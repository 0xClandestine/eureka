#!/usr/bin/env python3
"""Shared context memory accessible to all agents in a co-scientist run.

Reads/writes structured JSON data to the per-run SQLite database. Agents use
this to share literature notes, rejected hypotheses, intermediate findings,
and cross-round memory without bloating graph artifacts.

Usage (JSON on stdin):
  {"action": "write", "key": "lit_notes", "value": {...}}
  {"action": "read",  "key": "lit_notes"}
  {"action": "list",  "prefix": "lit_"}
  {"action": "read_round", "round": 1}
  {"action": "delete", "key": "lit_notes"}

Environment:
  EUREKA_DB_PATH       per-run SQLite database (required)
  EUREKA_SESSION_ID    session UUID
  EUREKA_NODE_ID       calling agent's node ID
  EUREKA_ROUND         current scheduler round
"""

import json
import os
import sqlite3
import sys
import time

DB_PATH = os.environ.get("EUREKA_DB_PATH", "")
SESSION_ID = os.environ.get("EUREKA_SESSION_ID", "unknown")
NODE_ID = os.environ.get("EUREKA_NODE_ID", "unknown")
ROUND_NUM = int(os.environ.get("EUREKA_ROUND", "0"))

_TABLE_SQL = """
CREATE TABLE IF NOT EXISTS agent_context (
    session_id  TEXT NOT NULL,
    namespace   TEXT NOT NULL DEFAULT 'shared',
    key         TEXT NOT NULL,
    value_json  TEXT NOT NULL,
    source_node TEXT NOT NULL,
    round       INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (session_id, namespace, key)
)
"""


def _connect() -> sqlite3.Connection:
    if not DB_PATH:
        raise SystemExit(json.dumps({"error": "EUREKA_DB_PATH not set"}))
    conn = sqlite3.connect(DB_PATH, timeout=5.0)
    conn.execute("PRAGMA busy_timeout = 5000")
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute(_TABLE_SQL)
    conn.commit()
    return conn


def action_write(args: dict) -> dict:
    key = args["key"]
    value = args["value"]
    namespace = args.get("namespace", "shared")
    now = int(time.time() * 1000)
    conn = _connect()
    conn.execute(
        """INSERT INTO agent_context
           (session_id, namespace, key, value_json, source_node, round, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT(session_id, namespace, key) DO UPDATE SET
           value_json = excluded.value_json,
           source_node = excluded.source_node,
           round = excluded.round,
           updated_at = excluded.updated_at""",
        (SESSION_ID, namespace, key, json.dumps(value), NODE_ID, ROUND_NUM, now, now),
    )
    conn.commit()
    conn.close()
    return {"ok": True, "key": key, "namespace": namespace, "round": ROUND_NUM}


def action_read(args: dict) -> dict:
    key = args["key"]
    namespace = args.get("namespace", "shared")
    conn = _connect()
    row = conn.execute(
        """SELECT value_json, source_node, round, updated_at
           FROM agent_context
           WHERE session_id = ? AND namespace = ? AND key = ?""",
        (SESSION_ID, namespace, key),
    ).fetchone()
    conn.close()
    if row is None:
        return {"found": False, "key": key}
    return {
        "found": True,
        "key": key,
        "namespace": namespace,
        "value": json.loads(row[0]),
        "source_node": row[1],
        "round": row[2],
        "updated_at": row[3],
    }


def action_list(args: dict) -> dict:
    prefix = args.get("prefix", "")
    namespace = args.get("namespace", "shared")
    conn = _connect()
    rows = conn.execute(
        """SELECT key, source_node, round, updated_at
           FROM agent_context
           WHERE session_id = ? AND namespace = ?
             AND key LIKE ?
           ORDER BY updated_at DESC
           LIMIT 100""",
        (SESSION_ID, namespace, f"{prefix}%"),
    ).fetchall()
    conn.close()
    return {
        "keys": [
            {"key": r[0], "source_node": r[1], "round": r[2], "updated_at": r[3]}
            for r in rows
        ],
        "count": len(rows),
        "prefix": prefix,
    }


def action_read_round(args: dict) -> dict:
    round_num = args["round"]
    conn = _connect()
    rows = conn.execute(
        """SELECT namespace, key, value_json, source_node
           FROM agent_context
           WHERE session_id = ? AND round = ?
           ORDER BY namespace, key""",
        (SESSION_ID, round_num),
    ).fetchall()
    conn.close()
    entries = {}
    for namespace, key, value_json, source in rows:
        try:
            entries[f"{namespace}/{key}"] = json.loads(value_json)
        except json.JSONDecodeError:
            entries[f"{namespace}/{key}"] = value_json
    return {"round": round_num, "entries": entries, "count": len(rows)}


def action_delete(args: dict) -> dict:
    key = args["key"]
    namespace = args.get("namespace", "shared")
    conn = _connect()
    conn.execute(
        "DELETE FROM agent_context WHERE session_id = ? AND namespace = ? AND key = ?",
        (SESSION_ID, namespace, key),
    )
    conn.commit()
    conn.close()
    return {"ok": True, "deleted": key}


def main() -> None:
    try:
        args = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        args = {}

    action = args.get("action", "read")

    try:
        if action == "write":
            result = action_write(args)
        elif action == "read":
            result = action_read(args)
        elif action == "list":
            result = action_list(args)
        elif action == "read_round":
            result = action_read_round(args)
        elif action == "delete":
            result = action_delete(args)
        else:
            result = {"error": f"unknown action: {action}"}
    except Exception as exc:
        result = {"error": str(exc)}

    print(json.dumps(result))


if __name__ == "__main__":
    main()