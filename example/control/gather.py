#!/usr/bin/env python3
"""Gather — collects individual ReviewItems back into a Reviews array.

The scheduler activates this node once per item (each advocate emit triggers
one activation). Items are accumulated in SQLite keyed by (session_id,
batch_id). When the count reaches _batch_total, the full batch is emitted as
{ reviews: [...] } on the "out" port and the batch rows are deleted.

All N activations run concurrently. An EXCLUSIVE SQLite transaction serialises
the insert+check+delete so exactly one activation wins the right to emit;
the rest write their item and exit silently.

Input  (stdin):  JSON call envelope carrying a single ReviewItem artifact
                 (fields: review, _batch_id, _batch_index, _batch_total)
Output (stdout): one JSON emit on "out" when batch is complete, else nothing
"""

import json
import os
import sqlite3
import sys

_TABLE = """
CREATE TABLE IF NOT EXISTS gather_batch (
    session_id  TEXT    NOT NULL,
    batch_id    TEXT    NOT NULL,
    idx         INTEGER NOT NULL,
    data        TEXT    NOT NULL,
    PRIMARY KEY (session_id, batch_id, idx)
)
"""


def main() -> None:
    session_id = os.environ.get("EUREKA_SESSION_ID", "unknown")
    db_path = os.environ.get("EUREKA_DB_PATH", "")

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        return

    data = envelope.get("artifact", {}).get("data", {})
    review = data.get("review", {})
    batch_id = data.get("_batch_id", "")
    batch_index = int(data.get("_batch_index", 0))
    batch_total = int(data.get("_batch_total", 1))

    if not batch_id or not db_path:
        # No batch metadata or no DB — emit immediately as a single-item batch.
        # Handles unit-test environments and degenerate single-hypothesis cases.
        emit = {
            "port": "out",
            "artifact": {
                "kind": "Reviews",
                "data": {"reviews": [review]},
            },
        }
        sys.stdout.write(json.dumps(emit) + "\n")
        return

    con = sqlite3.connect(db_path, timeout=60)
    try:
        con.execute("PRAGMA journal_mode=WAL")
        con.execute(_TABLE)

        # EXCLUSIVE lock: only one gather process at a time executes the
        # insert + count check + conditional delete block.  This prevents two
        # concurrent processes both reading count == total and both emitting.
        con.execute("BEGIN EXCLUSIVE")

        con.execute(
            "INSERT OR REPLACE INTO gather_batch "
            "(session_id, batch_id, idx, data) VALUES (?, ?, ?, ?)",
            (session_id, batch_id, batch_index, json.dumps(review)),
        )

        rows = con.execute(
            "SELECT data FROM gather_batch "
            "WHERE session_id=? AND batch_id=? ORDER BY idx",
            (session_id, batch_id),
        ).fetchall()
        count = len(rows)

        if count >= batch_total:
            reviews = [json.loads(r[0]) for r in rows]
            con.execute(
                "DELETE FROM gather_batch WHERE session_id=? AND batch_id=?",
                (session_id, batch_id),
            )
            con.execute("COMMIT")
            con.close()

            emit = {
                "port": "out",
                "artifact": {
                    "kind": "Reviews",
                    "data": {"reviews": reviews},
                },
            }
            sys.stdout.write(json.dumps(emit) + "\n")
        else:
            con.execute("COMMIT")
            con.close()
            # Still accumulating — emit nothing.

    except Exception:
        try:
            con.execute("ROLLBACK")
        except Exception:
            pass
        con.close()
        raise


if __name__ == "__main__":
    main()
