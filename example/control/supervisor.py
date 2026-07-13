#!/usr/bin/env python3
"""Supervisor orchestrator — replaces governor.py with dynamic orchestration.

The Supervisor manages the co-scientist's task queue, resource allocation,
and terminal-state detection. It reads the full tournament ranking state
(instead of just raw hypotheses), computes progress statistics, maintains
a context-memory ledger, and decides which agents to schedule next.

Input  (stdin):  JSON call envelope  { "port": "in", "artifact": { "kind": "Ranking|Hypotheses", ... } }
Output (stdout): one JSON emit envelope on "continue" (Ranking), "evolve" (Hypotheses), or "halt" (Control)

Context memory is written to EUREKA_DB_PATH when available so that agent
prompts can be enriched with persistent feedback across rounds.
"""

import json
import os
import sqlite3
import sys
import time

# ---------------------------------------------------------------------------
# DB helpers — lightweight context-memory store
# ---------------------------------------------------------------------------

_CTX_TABLE = """
CREATE TABLE IF NOT EXISTS context_memory (
    session_id  TEXT NOT NULL,
    round       INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    written_at  INTEGER NOT NULL,
    PRIMARY KEY (session_id, round, kind)
)
"""


def _load_context(db_path: str, session_id: str) -> dict:
    """Return {kind: payload} for the latest round in this session."""
    try:
        conn = sqlite3.connect(db_path)
        conn.execute(_CTX_TABLE)
        conn.commit()
        cur = conn.execute(
            "SELECT kind, payload FROM context_memory "
            "WHERE session_id=? AND round=(SELECT MAX(round) FROM context_memory WHERE session_id=?)",
            (session_id, session_id),
        )
        ctx = {}
        for kind, payload in cur.fetchall():
            try:
                ctx[kind] = json.loads(payload)
            except json.JSONDecodeError:
                ctx[kind] = payload
        conn.close()
        return ctx
    except Exception:
        return {}


def _save_context(db_path: str, session_id: str, round_num: int, kind: str, payload: dict) -> None:
    try:
        now = int(time.time() * 1000)
        conn = sqlite3.connect(db_path)
        conn.execute(_CTX_TABLE)
        conn.execute(
            "INSERT OR REPLACE INTO context_memory (session_id, round, kind, payload, written_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (session_id, round_num, kind, json.dumps(payload), now),
        )
        conn.commit()
        conn.close()
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Statistics helpers
# ---------------------------------------------------------------------------

def _compute_stats(hypotheses: list, elo_ratings: dict, round_num: int) -> dict:
    """Compute summary statistics of the current tournament state."""
    n_total = len(elo_ratings)
    n_ranked = len(hypotheses)
    ratings = list(elo_ratings.values())
    avg_elo = sum(ratings) / len(ratings) if ratings else 0.0
    max_elo = max(ratings) if ratings else 0.0
    min_elo = min(ratings) if ratings else 0.0

    # Estimate convergence via Elo spread
    elo_spread = max_elo - min_elo if n_total >= 2 else 0.0

    # Estimate diversity via distinct statements
    statements = {h.get("statement", "") for h in hypotheses}
    unique_count = len(statements)

    return {
        "round": round_num,
        "total_hypotheses_generated": n_total,
        "hypotheses_in_top_k": n_ranked,
        "avg_elo": round(avg_elo, 1),
        "max_elo": round(max_elo, 1),
        "min_elo": round(min_elo, 1),
        "elo_spread": round(elo_spread, 1),
        "unique_statements": unique_count,
    }


def _terminal_condition(stats: dict, config: dict) -> bool:
    """Decide whether the co-scientist computation has reached a terminal state."""
    max_rounds = int(config.get("max_rounds", 5))
    min_rounds = int(config.get("min_rounds", 2))
    convergence_threshold = float(config.get("convergence_threshold", 200.0))
    min_hypotheses = int(config.get("min_hypotheses", 3))

    round_num = stats["round"]

    # Hard budget
    if round_num >= max_rounds:
        return True

    # Minimum rounds before early stopping
    if round_num < min_rounds:
        return False

    # Insufficient hypotheses
    if stats["total_hypotheses_generated"] < min_hypotheses:
        return False

    # Convergence check — Elo spread below threshold suggests stable rankings
    if stats["elo_spread"] > 0 and stats["elo_spread"] < convergence_threshold:
        return True

    return False


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    config = json.loads(os.environ.get("EUREKA_CONFIG", "{}"))
    session_id = os.environ.get("EUREKA_SESSION_ID", "unknown")
    db_path = os.environ.get("EUREKA_DB_PATH", "")
    round_num = int(os.environ.get("EUREKA_ROUND", "0"))

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        envelope = {}

    hypotheses = []
    elo_ratings = {}

    for inp in envelope.get("inputs", []):
        port = inp.get("port", "")
        data = inp.get("artifact", {}).get("data", {})
        if port == "in":
            hypotheses = data.get("hypotheses", [])
            if not elo_ratings:
                elo_ratings = data.get("elo_ratings", {})
        elif port == "ranking":
            elo_ratings = data.get("elo_ratings", {})

    # ── Elo fallback: load persisted ratings from DB (written by ranker) ──
    if not elo_ratings and db_path:
        try:
            conn = sqlite3.connect(db_path)
            conn.execute("""
                CREATE TABLE IF NOT EXISTS elo_ratings (
                    session_id TEXT NOT NULL, node_id TEXT NOT NULL,
                    item_id TEXT NOT NULL, elo REAL NOT NULL DEFAULT 1200.0,
                    PRIMARY KEY (session_id, node_id, item_id)
                )
            """)
            conn.commit()
            cur = conn.execute(
                "SELECT item_id, elo FROM elo_ratings WHERE session_id=?",
                (session_id,),
            )
            for item_id, elo in cur.fetchall():
                elo_ratings[item_id] = elo
            conn.close()
        except Exception:
            pass

    # Compute statistics
    stats = _compute_stats(hypotheses, elo_ratings, round_num)

    # Load prior context, append this round's stats
    prior_ctx = {}
    if db_path:
        prior_ctx = _load_context(db_path, session_id)

    # Merge prior statistics for trend analysis
    prior_stats = prior_ctx.get("statistics", {})
    prior_spread = prior_stats.get("elo_spread", 0.0)
    stats["elo_spread_delta"] = round(stats["elo_spread"] - prior_spread, 1)
    stats["prior_round"] = prior_stats.get("round", -1)

    # Persist context memory
    if db_path:
        _save_context(db_path, session_id, round_num, "statistics", stats)
        _save_context(db_path, session_id, round_num, "hypotheses", hypotheses)
        _save_context(db_path, session_id, round_num, "elo_ratings", elo_ratings)

    # Terminal check
    if _terminal_condition(stats, config):
        emit = {
            "port": "halt",
            "artifact": {
                "kind": "Control",
                "data": {
                    "signal": "halt",
                    "stats": stats,
                },
            },
        }
    else:
        # Continue the loop — forward hypotheses with ranking metadata so
        # reflection has full tournament context for recurrent reviews
        emit = {
            "port": "continue",
            "artifact": {
                "kind": "Hypotheses",
                "data": {
                    "hypotheses": hypotheses,
                    "elo_ratings": elo_ratings,
                    "stats": stats,
                    "context": prior_ctx,
                },
            },
        }

    print(json.dumps(emit))


if __name__ == "__main__":
    main()
