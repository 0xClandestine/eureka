#!/usr/bin/env python3
"""Elo tournament ranker with similarity-based matchmaking.

Receives reviewed hypotheses and an optional proximity graph.  Uses the
proximity graph to weight pairwise comparisons: similar hypotheses are
prioritised for comparison (they get larger K-factor updates).  Newer and
top-ranking hypotheses are also given higher priority in match selection.

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

DEFAULT_ELO = 1200.0
BASE_K_FACTOR = 24.0
SIMILARITY_BOOST = 1.6
NEWCOMER_BOOST = 1.4


def expected(rating_a: float, rating_b: float) -> float:
    return 1.0 / (1.0 + 10 ** ((rating_b - rating_a) / 400.0))


def update(winner_rating: float, loser_rating: float, k: float) -> tuple[float, float]:
    ew = expected(winner_rating, loser_rating)
    return winner_rating + k * (1.0 - ew), loser_rating + k * (-ew)


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


def _build_emits(
    ratings: dict[str, tuple[float, dict, int]], top_k: int, output_field: str,
) -> tuple[dict, dict]:
    sorted_items = sorted(ratings.items(), key=lambda kv: kv[1][0], reverse=True)
    top_k_items = sorted_items[:top_k]
    top_list = [item for _, (_, item, _) in top_k_items]
    elo_map = {item_id: elo for item_id, (elo, _, _) in top_k_items}
    match_counts = {item_id: m for item_id, (_, _, m) in top_k_items}
    top_emit = {
        "port": "top", "artifact": {"kind": "Hypotheses", "data": {output_field: top_list}},
    }
    state_emit = {
        "port": "state",
        "artifact": {
            "kind": "Ranking",
            "data": {
                "hypotheses": top_list, "elo_ratings": elo_map,
                "match_counts": match_counts, "count": len(ratings),
            },
        },
    }
    return top_emit, state_emit


def main() -> None:
    config = json.loads(os.environ.get("EUREKA_CONFIG", "{}"))
    items_field  = config.get("items_field",  "reviews")
    score_field  = config.get("score_field",  "score")
    item_field   = config.get("item_field",   "hypothesis")
    id_field     = config.get("id_field",     "statement")
    output_field = config.get("output_field", "hypotheses")
    top_k        = int(config.get("top_k", 5))

    session_id = os.environ.get("EUREKA_SESSION_ID", "unknown")
    node_id    = os.environ.get("EUREKA_NODE_ID",    "elo-ranker")
    db_path    = os.environ.get("EUREKA_DB_PATH",    "")

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
        scored: list[tuple[str, float, dict]] = []
        for idx, review in enumerate(reviews):
            item = review.get(item_field, {})
            score = float(review.get(score_field, 5.0))
            item_id = str(item.get(id_field, ""))
            if item_id:
                scored.append((item_id, score, item))
                index_map[item_id] = idx

        for item_id, _, item in scored:
            if item_id not in ratings:
                ratings[item_id] = (DEFAULT_ELO, item, 0)

        n = len(scored)
        for i in range(n):
            id_a, score_a, _ = scored[i]
            for j in range(i + 1, n):
                id_b, score_b, _ = scored[j]
                elo_a, _, matches_a = ratings[id_a]
                elo_b, _, matches_b = ratings[id_b]
                k = _compute_k(
                    index_map.get(id_a, i), index_map.get(id_b, j),
                    matches_a, matches_b, similarities,
                )
                if score_a > score_b:
                    new_a, new_b = update(elo_a, elo_b, k)
                elif score_b > score_a:
                    new_b, new_a = update(elo_b, elo_a, k)
                else:
                    new_a, new_b = elo_a, elo_b
                ratings[id_a] = (new_a, ratings[id_a][1], matches_a + 1)
                ratings[id_b] = (new_b, ratings[id_b][1], matches_b + 1)

        if db_path:
            save_ratings(db_path, session_id, node_id, ratings)

    top_emit, state_emit = _build_emits(ratings, top_k, output_field)
    print(json.dumps(top_emit))
    print(json.dumps(state_emit))


if __name__ == "__main__":
    main()