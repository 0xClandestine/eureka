#!/usr/bin/env python3
"""Round governor plugin.

Reads the current round from EUREKA_ROUND and max_rounds from EUREKA_CONFIG.

- If round < max_rounds: forwards the deduped hypotheses on the 'continue'
  port so they enter the next reflection–ranking cycle.
- Otherwise: emits a Control artifact on the 'halt' port to signal session
  end (no downstream consumer needed — the scheduler terminates naturally).

Input  (stdin):  JSON call envelope  { "port": "in", "artifact": { "kind": "Hypotheses", ... } }
Output (stdout): one JSON emit envelope on "continue" (Hypotheses) or "halt" (Control)
"""

import json
import os
import sys


def main() -> None:
    config = json.loads(os.environ.get("EUREKA_CONFIG", "{}"))
    max_rounds = int(config.get("max_rounds", 12))
    round_num = int(os.environ.get("EUREKA_ROUND", "0"))

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        envelope = {}

    input_data = envelope.get("artifact", {}).get("data", {})

    if round_num < max_rounds:
        # Pass the deduped hypotheses through so reflection can re-review them.
        emit = {
            "port": "continue",
            "artifact": {
                "kind": "Hypotheses",
                "data": input_data,
            },
        }
    else:
        # Halt — the session ends naturally when this emits with no consumer.
        emit = {
            "port": "halt",
            "artifact": {
                "kind": "Control",
                "data": {"signal": "halt", "round": round_num},
            },
        }

    print(json.dumps(emit))


if __name__ == "__main__":
    main()
