#!/usr/bin/env python3
"""Elo tournament ranker with similarity-based matchmaking and optional LLM debates.

Receives reviewed hypotheses and an optional proximity graph.  Uses the
proximity graph to weight pairwise comparisons: similar hypotheses are
prioritised for comparison (they get larger K-factor updates).  Newer and
top-ranking hypotheses are also given higher priority in match selection.

When llm_debate is enabled in config and an LLM API key is available,
pairwise tournament matches are decided by multi-turn scientific debates
judged by the LLM, rather than simple score comparisons.

Persists Elo state to EUREKA_DB_PATH so ratings are stable across rounds.

Input  (stdin):  JSON call envelope  { "port": "in", "artifact": {...} }
                 The "cycle" port leaves ratings unchanged.

Output (stdout): two JSON emit envelopes — "top" (Hypotheses), "state" (Ranking)
"""

import json
import os
import sqlite3
import sys
import time
import urllib.request
import urllib.error

DEFAULT_ELO = 1200.0
BASE_K_FACTOR = 24.0
SIMILARITY_BOOST = 1.6
NEWCOMER_BOOST = 1.4
LLM_TIMEOUT_SECS = 45

# ── Elo maths ──────────────────────────────────────────────────────────


def expected(rating_a: float, rating_b: float) -> float:
    return 1.0 / (1.0 + 10 ** ((rating_b - rating_a) / 400.0))


def update(winner_rating: float, loser_rating: float, k: float) -> tuple[float, float]:
    ew = expected(winner_rating, loser_rating)
    return winner_rating + k * (1.0 - ew), loser_rating + k * (-ew)


# ── DB helpers ─────────────────────────────────────────────────────────


def _connect(db_path: str) -> sqlite3.Connection:
    conn = sqlite3.connect(db_path)
    conn.execute("""
        CREATE TABLE IF NOT EXISTS elo_ratings (
            session_id TEXT NOT NULL,
            node_id    TEXT NOT NULL,
            item_id    TEXT NOT NULL,
            item_json  TEXT NOT NULL,
            elo        REAL NOT NULL DEFAULT 1200.0,
            matches    INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER)*1000),
            PRIMARY KEY (session_id, node_id, item_id)
        )
    """)
    conn.commit()
    return conn


def load_ratings(db_path: str, session_id: str, node_id: str) -> dict[str, tuple[float, dict, int]]:
    try:
        conn = _connect(db_path)
        cur = conn.execute(
            "SELECT item_id, elo, item_json, matches FROM elo_ratings "
            "WHERE session_id=? AND node_id=?",
            (session_id, node_id),
        )
        result = {}
        for item_id, elo, item_json, matches in cur.fetchall():
            try:
                item = json.loads(item_json)
            except json.JSONDecodeError:
                item = {}
            result[item_id] = (elo, item, matches)
        conn.close()
        return result
    except Exception:
        return {}


def save_ratings(
    db_path: str, session_id: str, node_id: str,
    ratings: dict[str, tuple[float, dict, int]],
) -> None:
    now = int(time.time() * 1000)
    try:
        conn = _connect(db_path)
        for item_id, (elo, item, matches) in ratings.items():
            conn.execute(
                """INSERT INTO elo_ratings
                   (session_id, node_id, item_id, item_json, elo, matches, updated_at, created_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(session_id, node_id, item_id)
                   DO UPDATE SET elo=excluded.elo, matches=excluded.matches,
                                 item_json=excluded.item_json, updated_at=excluded.updated_at""",
                (session_id, node_id, item_id, json.dumps(item), elo, matches, now, now),
            )
        conn.commit()
        conn.close()
    except Exception:
        pass


def _load_proximity(db_path: str, session_id: str) -> dict[str, float]:
    try:
        conn = sqlite3.connect(db_path)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS context_memory (
                session_id  TEXT NOT NULL, round INTEGER NOT NULL,
                kind TEXT NOT NULL, payload TEXT NOT NULL, written_at INTEGER NOT NULL,
                PRIMARY KEY (session_id, round, kind)
            )
        """)
        conn.commit()
        cur = conn.execute(
            "SELECT payload FROM context_memory "
            "WHERE session_id=? AND kind='proximity_graph' "
            "ORDER BY round DESC LIMIT 1",
            (session_id,),
        )
        row = cur.fetchone()
        conn.close()
        if row is None:
            return {}
        data = json.loads(row[0])
        edges = data.get("graph", {}).get("edges", [])
        result: dict[str, float] = {}
        for e in edges:
            key = f"{e['source']}::{e['target']}"
            result[key] = float(e.get("weight", 0.0))
        return result
    except Exception:
        return {}


def _compute_k(
    idx_a: int, idx_b: int, matches_a: int, matches_b: int,
    similarities: dict[str, float],
) -> float:
    k = BASE_K_FACTOR
    key_ab = f"{idx_a}::{idx_b}"
    key_ba = f"{idx_b}::{idx_a}"
    sim = similarities.get(key_ab, similarities.get(key_ba, 0.0))
    if sim >= 0.5:
        k *= 1.0 + (sim - 0.5) * (SIMILARITY_BOOST - 1.0) * 2.0
    if matches_a <= 2 or matches_b <= 2:
        k *= NEWCOMER_BOOST
    return k


# ── LLM debate ─────────────────────────────────────────────────────────

_DEBATE_PROMPT = """You are a rigorous scientific debate judge. Two hypotheses
are presented below with their peer reviews. Compare them on:

1. **Novelty** — which is more genuinely new, not just a recombination?
2. **Correctness** — which has sounder logic, fewer errors?
3. **Feasibility** — which can be tested more realistically?
4. **Impact** — which would matter more if proven true?

Consider the review scores but do not simply compare numbers — apply your
own scientific judgement. Return ONLY a JSON object with no extra text:

{"winner": "A"|"B"|"draw", "confidence": 0.0-1.0, "reasoning": "..."}

HYPOTHESIS A:
{a_hypothesis}

REVIEW OF A (score {a_score}):
{a_review}

HYPOTHESIS B:
{b_hypothesis}

REVIEW OF B (score {b_score}):
{b_review}
"""


def _llm_debate(
    hyp_a: dict, review_a: str, score_a: float,
    hyp_b: dict, review_b: str, score_b: float,
    api_base: str, model: str, api_key: str,
) -> dict:
    """Run a pairwise LLM debate. Returns {"winner": "A"|"B"|"draw", "confidence": ..., "reasoning": ...}."""
    prompt = _DEBATE_PROMPT.format(
        a_hypothesis=json.dumps(hyp_a, indent=2),
        a_review=review_a[:2000],
        a_score=score_a,
        b_hypothesis=json.dumps(hyp_b, indent=2),
        b_review=review_b[:2000],
        b_score=score_b,
    )

    body = json.dumps({
        "model": model,
        "messages": [
            {"role": "system", "content": "You are a scientific debate judge. Respond with JSON only."},
            {"role": "user", "content": prompt},
        ],
        "temperature": 0.3,
        "max_tokens": 512,
        "response_format": {"type": "json_object"},
    }).encode("utf-8")

    req = urllib.request.Request(
        f"{api_base}/chat/completions",
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        },
    )

    try:
        with urllib.request.urlopen(req, timeout=LLM_TIMEOUT_SECS) as resp:
            data = json.loads(resp.read().decode("utf-8"))
        content = data["choices"][0]["message"]["content"]
        return json.loads(content)
    except Exception as exc:
        return {"winner": "draw", "confidence": 0.0, "reasoning": f"LLM debate failed: {exc}"}


def _resolve_api(config: dict) -> tuple[str, str, str] | None:
    """Resolve LLM API base URL, model, and key from config + env.

    Returns (api_base, model, api_key) or None if unavailable."""
    model = config.get("llm_model", os.environ.get("EUREKA_LLM_MODEL", ""))
    api_key_env = config.get("llm_api_key_env", "")
    if not api_key_env:
        # Try common provider env vars
        for var in ("OPENROUTER_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY"):
            if os.environ.get(var):
                api_key_env = var
                break
    api_key = os.environ.get(api_key_env, "") if api_key_env else ""
    if not api_key or not model:
        return None
    api_base = config.get("llm_api_base", "https://openrouter.ai/api/v1")
    return (api_base, model, api_key)


# ── Build emits ────────────────────────────────────────────────────────


def _build_emits(
    ratings: dict[str, tuple[float, dict, int]], top_k: int, output_field: str,
    debate_transcripts: list[dict] | None = None,
) -> tuple[dict, dict]:
    sorted_items = sorted(ratings.items(), key=lambda kv: kv[1][0], reverse=True)
    top_k_items = sorted_items[:top_k]
    top_list = [item for _, (_, item, _) in top_k_items]
    elo_map = {item_id: elo for item_id, (elo, _, _) in top_k_items}
    match_counts = {item_id: m for item_id, (_, _, m) in top_k_items}
    top_emit = {
        "port": "top", "artifact": {"kind": "Hypotheses", "data": {output_field: top_list}},
    }
    state_data: dict = {
        "hypotheses": top_list, "elo_ratings": elo_map,
        "match_counts": match_counts, "count": len(ratings),
    }
    if debate_transcripts:
        state_data["debate_transcripts"] = debate_transcripts
    state_emit = {
        "port": "state",
        "artifact": {"kind": "Ranking", "data": state_data},
    }
    return top_emit, state_emit


# ── Main ───────────────────────────────────────────────────────────────


def main() -> None:
    config = json.loads(os.environ.get("EUREKA_CONFIG", "{}"))
    items_field  = config.get("items_field",  "reviews")
    score_field  = config.get("score_field",  "score")
    item_field   = config.get("item_field",   "hypothesis")
    id_field     = config.get("id_field",     "statement")
    output_field = config.get("output_field", "hypotheses")
    top_k        = int(config.get("top_k", 5))

    # LLM debate config
    llm_debate_enabled = config.get("llm_debate", False)
    debate_transcripts: list[dict] = []

    session_id = os.environ.get("EUREKA_SESSION_ID", "unknown")
    node_id    = os.environ.get("EUREKA_NODE_ID",    "elo-ranker")
    db_path    = os.environ.get("EUREKA_DB_PATH",    "")

    api_info = _resolve_api(config) if llm_debate_enabled else None

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        envelope = {}

    port = envelope.get("port", "in")

    ratings: dict[str, tuple[float, dict, int]] = {}
    if db_path:
        ratings = load_ratings(db_path, session_id, node_id)

    similarities: dict[str, float] = {}
    if db_path:
        similarities = _load_proximity(db_path, session_id)

    if port == "in":
        artifact_data = envelope.get("artifact", {}).get("data", {})
        reviews = artifact_data.get(items_field, [])

        index_map: dict[str, int] = {}
        scored: list[tuple[str, float, dict, str, float]] = []
        for idx, review in enumerate(reviews):
            item = review.get(item_field, {})
            score = float(review.get(score_field, 5.0))
            reasoning = review.get("reasoning", "")
            item_id = str(item.get(id_field, ""))
            if item_id:
                scored.append((item_id, score, item, reasoning, score))
                index_map[item_id] = idx

        for item_id, _, item, _, _ in scored:
            if item_id not in ratings:
                ratings[item_id] = (DEFAULT_ELO, item, 0)

        n = len(scored)

        # Pairwise comparison loop
        for i in range(n):
            id_a, score_a, item_a, review_a, _ = scored[i]
            for j in range(i + 1, n):
                id_b, score_b, item_b, review_b, _ = scored[j]
                elo_a, _, matches_a = ratings[id_a]
                elo_b, _, matches_b = ratings[id_b]
                k = _compute_k(
                    index_map.get(id_a, i), index_map.get(id_b, j),
                    matches_a, matches_b, similarities,
                )

                winner = None
                debate_info = None

                # ── Try LLM debate ──
                if api_info:
                    api_base, model, api_key = api_info
                    debate_result = _llm_debate(
                        item_a, review_a, score_a,
                        item_b, review_b, score_b,
                        api_base, model, api_key,
                    )
                    debate_info = {
                        "hypothesis_a": id_a,
                        "hypothesis_b": id_b,
                        "winner": debate_result.get("winner", "draw"),
                        "confidence": debate_result.get("confidence", 0.0),
                        "reasoning": debate_result.get("reasoning", ""),
                    }
                    debate_transcripts.append(debate_info)
                    w = debate_info["winner"]
                    if w == "A":
                        winner = "a"
                    elif w == "B":
                        winner = "b"

                # ── Fall back to score comparison ──
                if winner is None:
                    if score_a > score_b:
                        winner = "a"
                    elif score_b > score_a:
                        winner = "b"

                if winner == "a":
                    new_a, new_b = update(elo_a, elo_b, k)
                elif winner == "b":
                    new_b, new_a = update(elo_b, elo_a, k)
                else:
                    new_a, new_b = elo_a, elo_b

                ratings[id_a] = (new_a, ratings[id_a][1], matches_a + 1)
                ratings[id_b] = (new_b, ratings[id_b][1], matches_b + 1)

        if db_path:
            save_ratings(db_path, session_id, node_id, ratings)

    top_emit, state_emit = _build_emits(ratings, top_k, output_field, debate_transcripts)
    print(json.dumps(top_emit))
    print(json.dumps(state_emit))


if __name__ == "__main__":
    main()