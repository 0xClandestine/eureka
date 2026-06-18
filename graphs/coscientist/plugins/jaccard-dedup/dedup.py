#!/usr/bin/env python3
"""Jaccard deduplication plugin.

Removes items from an array whose text field is too similar (by word-level
Jaccard similarity) to an item already accepted into the output.

Input  (stdin):  JSON call envelope  { "port": "in", "artifact": {...} }
Output (stdout): JSON emit envelope  { "port": "unique", "artifact": {...} }
"""

import json
import os
import sys


def jaccard(a: str, b: str) -> float:
    """Word-level Jaccard similarity between two strings."""
    set_a = set(a.lower().split())
    set_b = set(b.lower().split())
    if not set_a and not set_b:
        return 1.0
    union = set_a | set_b
    if not union:
        return 1.0
    return len(set_a & set_b) / len(union)


def main() -> None:
    config = json.loads(os.environ.get("EUREKA_CONFIG", "{}"))
    items_field = config.get("items_field", "hypotheses")
    text_field = config.get("text_field", "statement")
    threshold = float(config.get("threshold", 0.65))
    output_field = config.get("output_field", "hypotheses")

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        envelope = {}

    artifact_data = envelope.get("artifact", {}).get("data", {})
    items = artifact_data.get(items_field, [])

    unique: list = []
    seen_texts: list[str] = []

    for item in items:
        text = str(item.get(text_field, ""))
        is_dup = any(jaccard(text, seen) >= threshold for seen in seen_texts)
        if not is_dup:
            seen_texts.append(text)
            unique.append(item)

    emit = {
        "port": "unique",
        "artifact": {
            "kind": "Hypotheses",
            "data": {output_field: unique},
        },
    }
    print(json.dumps(emit))


if __name__ == "__main__":
    main()
