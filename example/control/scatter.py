#!/usr/bin/env python3
"""Scatter — explodes a hypotheses array into individual HypothesisItem artifacts.

Receives { hypotheses: [...] } and emits one artifact per item on the "item"
port. Each artifact carries the hypothesis plus batch routing metadata
(_batch_id, _batch_index, _batch_total) so the downstream gather node knows
when the full batch has been collected.

A batch_id is generated fresh for each invocation so that successive rounds
(supervisor → scatter → critic) produce independent, non-colliding batches.

Input  (stdin):  JSON call envelope  { "port": "in", "artifact": {...} }
Output (stdout): N JSON emit envelopes on "item", one per hypothesis
"""

import json
import sys
import uuid


def main() -> None:
    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        return

    artifact_data = envelope.get("artifact", {}).get("data", {})
    hypotheses = artifact_data.get("hypotheses", [])

    if not hypotheses:
        # Nothing to scatter — emit nothing. The scheduler treats a zero-emit
        # activation as complete and the pending count drains to zero, ending
        # the run gracefully.
        return

    batch_id = str(uuid.uuid4())
    total = len(hypotheses)

    for i, hypothesis in enumerate(hypotheses):
        emit = {
            "port": "item",
            "artifact": {
                "kind": "HypothesisItem",
                "data": {
                    "hypothesis": hypothesis,
                    "_batch_id": batch_id,
                    "_batch_index": i,
                    "_batch_total": total,
                },
            },
        }
        sys.stdout.write(json.dumps(emit) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
