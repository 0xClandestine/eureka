#!/usr/bin/env python3
"""Elo ranker plugin.

Performs pairwise Elo ranking over a batch of reviewed hypotheses. When
EUREKA_DB_PATH is set, Elo state is loaded from and saved back to the
session SQLite database, making ratings persistent across rounds.

Input  (stdin):  JSON call envelope  { "port": "in"|"cycle", "artifact": {...} }
Output (stdout): two JSON emit envelopes — one on "top", one on "state"

The "cycle" port re-emits the current top-K without processing new reviews.
"""

import json
import os
import sqlite3
import sys

DEFAULT_ELO = 1200.0
K_FACTOR_DEFAULT = 32.0


# ---------------------------------------------------------------------------
# Elo maths
# ---------------------------------------------------------------------------

def expected(ra: float, rb: float) -> float:
    return 1.0 / (1.0 + 10 ** ((rb - ra) / 400.0))


def update(winner: float, loser: float, k: float) -> tuple[float, float]:
    ew = expected(winner, loser)
    el = expected(loser, winner)
    return winner + k * (1.0 - ew), loser + k * (0.0 - el)


# ---------------------------------------------------------------------------
# DB persistence (optional — gracefully skipped if DB is unavailable)
# ---------------------------------------------------------------------------

def load_ratings(db_path: str, session_id: str, node_id: str) -> dict[str, tuple[float, dict]]:
    """Return {item_id: (elo, item_json)} from the DB, or {} on error."""
    try:
        conn = sqlite3.connect(db_path)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS elo_ratings (
                session_id TEXT NOT NULL,
                node_id    TEXT NOT NULL,
                item_id    TEXT NOT NULL,
                item_json  TEXT NOT NULL,
                elo        REAL NOT NULL DEFAULT 1000.0,
                matches    INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (session_id, node_id, item_id)
            )
        """)
        conn.commit()
        cur = conn.execute(
            "SELECT item_id, elo, item_json FROM elo_ratings WHERE session_id=? AND node_id=?",
            (session_id, node_id),
        )
        result = {}
        for item_id, elo, item_json in cur.fetchall():
            try:
                item = json.loads(item_json)
            except json.JSONDecodeError:
                item = {}
            result[item_id] = (elo, item)
        conn.close()
        return result
    except Exception:  # noqa: BLE001
        return {}


def save_ratings(
    db_path: str,
    session_id: str,
    node_id: str,
    ratings: dict[str, tuple[float, dict]],
) -> None:
    """Upsert all ratings into the DB. Silently swallows errors."""
    import time
    now = int(time.time() * 1000)
    try:
        conn = sqlite3.connect(db_path)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS elo_ratings (
                session_id TEXT NOT NULL,
                node_id    TEXT NOT NULL,
                item_id    TEXT NOT NULL,
                item_json  TEXT NOT NULL,
                elo        REAL NOT NULL DEFAULT 1000.0,
                matches    INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (session_id, node_id, item_id)
            )
        """)
        for item_id, (elo, item) in ratings.items():
            conn.execute(
                """INSERT INTO elo_ratings (session_id, node_id, item_id, item_json, elo, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?)
                   ON CONFLICT(session_id, node_id, item_id)
                   DO UPDATE SET elo=excluded.elo, updated_at=excluded.updated_at""",
                (session_id, node_id, item_id, json.dumps(item), elo, now),
            )
        conn.commit()
        conn.close()
    except Exception:  # noqa: BLE001
        pass


# ---------------------------------------------------------------------------
# Main logic
# ---------------------------------------------------------------------------

def build_emits(
    ratings: dict[str, tuple[float, dict]],
    top_k: int,
    output_field: str,
) -> tuple[dict, dict]:
    """Return (top_emit, state_emit) from current ratings."""
    sorted_items = sorted(ratings.items(), key=lambda kv: kv[1][0], reverse=True)
    top_k_items = sorted_items[:top_k]

    top_list = [item for _, (_, item) in top_k_items]
    elo_map = {item_id: elo for item_id, (elo, _) in top_k_items}

    top_emit = {
        "port": "top",
        "artifact": {
            "kind": "Hypotheses",
            "data": {output_field: top_list},
        },
    }
    state_emit = {
        "port": "state",
        "artifact": {
            "kind": "Ranking",
            "data": {
                "hypotheses": top_list,
                "elo_ratings": elo_map,
                "count": len(ratings),
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
    k_factor     = float(config.get("k_factor", K_FACTOR_DEFAULT))

    session_id = os.environ.get("EUREKA_SESSION_ID", "unknown")
    node_id    = os.environ.get("EUREKA_NODE_ID",    "elo-ranker")
    db_path    = os.environ.get("EUREKA_DB_PATH",    "")

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        envelope = {}

    port = envelope.get("port", "in")

    # Load persisted ratings
    ratings: dict[str, tuple[float, dict]] = {}
    if db_path:
        ratings = load_ratings(db_path, session_id, node_id)

    if port == "in":
        artifact_data = envelope.get("artifact", {}).get("data", {})
        reviews = artifact_data.get(items_field, [])

        # Collect (id, score, item) triples
        scored: list[tuple[str, float, dict]] = []
        for review in reviews:
            item = review.get(item_field, {})
            score = float(review.get(score_field, 5.0))
            item_id = str(item.get(id_field, ""))
            if item_id:
                scored.append((item_id, score, item))

        # Initialize unseen items
        for item_id, _, item in scored:
            if item_id not in ratings:
                ratings[item_id] = (DEFAULT_ELO, item)

        # Pairwise Elo updates
        pairs = [(item_id, score) for item_id, score, _ in scored]
        for i in range(len(pairs)):
            for j in range(i + 1, len(pairs)):
                id_a, score_a = pairs[i]
                id_b, score_b = pairs[j]
                elo_a = ratings[id_a][0]
                elo_b = ratings[id_b][0]
                if score_a > score_b:
                    new_a, new_b = update(elo_a, elo_b, k_factor)
                elif score_b > score_a:
                    new_b, new_a = update(elo_b, elo_a, k_factor)
                else:
                    new_a, new_b = elo_a, elo_b  # draw
                ratings[id_a] = (new_a, ratings[id_a][1])
                ratings[id_b] = (new_b, ratings[id_b][1])

        # Persist updated ratings
        if db_path:
            save_ratings(db_path, session_id, node_id, ratings)

    # Both "in" and "cycle" emit top-K
    top_emit, state_emit = build_emits(ratings, top_k, output_field)
    print(json.dumps(top_emit))
    print(json.dumps(state_emit))


if __name__ == "__main__":
    main()
