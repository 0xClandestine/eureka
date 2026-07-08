#!/usr/bin/env python3
"""Proximity agent — builds a similarity graph over hypotheses.

Replaces dedup.py. Computes pairwise word-level Jaccard similarity,
constructs a proximity graph, identifies clusters of related hypotheses,
and emits a diverse frontier with matchmaking information for the Ranking
agent to prioritize similar-pair comparisons in tournament matches.

Input  (stdin):  JSON call envelope  { "port": "in", "artifact": {...} }
Output (stdout): two JSON emit envelopes — one on "unique" (Hypotheses),
                 one on "graph" (ProximityGraph).
"""

import json
import os
import sys


def jaccard(a: str, b: str) -> float:
    set_a = set(a.lower().split())
    set_b = set(b.lower().split())
    if not set_a and not set_b:
        return 1.0
    union = set_a | set_b
    return len(set_a & set_b) / len(union) if union else 1.0


def build_graph(items: list, text_field: str, threshold: float) -> dict:
    """Build an undirected proximity graph.

    Returns:
        {
          "nodes": [{"id": 0, "label": "...", "statement": "..."}, ...],
          "edges": [{"source": 0, "target": 1, "weight": 0.85}, ...],
          "clusters": [[0, 3], [1, 2], ...]  # connected components
        }
    """
    n = len(items)
    nodes = [
        {
            "id": i,
            "label": str(item.get(text_field, ""))[:80],
            "statement": str(item.get(text_field, "")),
        }
        for i, item in enumerate(items)
    ]

    # Pairwise similarity
    adjacency: list[list[int]] = [[] for _ in range(n)]
    edges: list[dict] = []
    for i in range(n):
        for j in range(i + 1, n):
            ti = str(items[i].get(text_field, ""))
            tj = str(items[j].get(text_field, ""))
            sim = jaccard(ti, tj)
            if sim >= threshold:
                adjacency[i].append(j)
                adjacency[j].append(i)
                edges.append({"source": i, "target": j, "weight": round(sim, 3)})

    # Connected components (clusters)
    visited = [False] * n
    clusters: list[list[int]] = []

    def dfs(v: int, comp: list[int]) -> None:
        visited[v] = True
        comp.append(v)
        for nb in adjacency[v]:
            if not visited[nb]:
                dfs(nb, comp)

    for v in range(n):
        if not visited[v]:
            comp: list[int] = []
            dfs(v, comp)
            clusters.append(comp)

    return {"nodes": nodes, "edges": edges, "clusters": clusters}


def select_frontier(items: list, text_field: str, threshold: float, max_items: int) -> list[int]:
    """Greedy diverse subset — keep first representative from each cluster,
    then fill remaining slots with least-similar items to the frontier."""
    graph = build_graph(items, text_field, threshold)
    clusters = graph["clusters"]
    n = len(items)
    selected: set[int] = set()

    # One representative per cluster
    for cluster in clusters:
        selected.add(cluster[0])

    # If we can add more, pick items least similar to already-selected
    if len(selected) < max_items and len(selected) < n:
        remaining = [i for i in range(n) if i not in selected]
        # Sort by minimum similarity to selected set
        def min_sim_to_selected(idx: int) -> float:
            ti = str(items[idx].get(text_field, ""))
            return min(
                (jaccard(ti, str(items[s].get(text_field, ""))) for s in selected),
                default=1.0,
            )
        remaining.sort(key=min_sim_to_selected)
        for idx in remaining:
            if len(selected) >= max_items:
                break
            selected.add(idx)

    return sorted(selected)


def main() -> None:
    config = json.loads(os.environ.get("EUREKA_CONFIG", "{}"))
    items_field = config.get("items_field", "hypotheses")
    text_field = config.get("text_field", "statement")
    threshold = float(config.get("threshold", 0.65))
    output_field = config.get("output_field", "hypotheses")
    max_frontier = int(config.get("max_frontier", 10))

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        envelope = {}

    artifact_data = envelope.get("artifact", {}).get("data", {})
    items = artifact_data.get(items_field, [])

    # Build full proximity graph
    graph = build_graph(items, text_field, threshold)

    # Select diverse frontier
    frontier_indices = select_frontier(items, text_field, threshold, max_frontier)
    unique_items = [items[i] for i in frontier_indices if i < len(items)]

    # Emit diverse frontier
    unique_emit = {
        "port": "unique",
        "artifact": {
            "kind": "Hypotheses",
            "data": {output_field: unique_items},
        },
    }
    print(json.dumps(unique_emit))

    # Emit proximity graph (used by Ranking for matchmaking)
    graph_emit = {
        "port": "graph",
        "artifact": {
            "kind": "ProximityGraph",
            "data": {
                "graph": graph,
                "items_field": items_field,
                "text_field": text_field,
            },
        },
    }
    print(json.dumps(graph_emit))


if __name__ == "__main__":
    main()