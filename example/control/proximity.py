#!/usr/bin/env python3
"""Proximity agent — builds a similarity graph over hypotheses.

Computes TF-IDF cosine similarity between hypothesis statements (falling
back to Jaccard for very short texts), constructs a proximity graph,
identifies clusters of related hypotheses, and emits a diverse frontier
with matchmaking information for the Ranking agent.

TF-IDF weights terms by their importance across the corpus, producing
better semantic clusters than raw word overlap.  All computation is
local — no network calls or external dependencies.

Input  (stdin):  JSON call envelope  { "port": "in", "artifact": {...} }
Output (stdout): two JSON emit envelopes — one on "unique" (Hypotheses),
                 one on "graph" (ProximityGraph).
"""

import json
import math
import os
import sys
from collections import Counter


# ── Similarity engines ─────────────────────────────────────────────────


def jaccard(a: str, b: str) -> float:
    """Jaccard similarity fallback for very short texts."""
    set_a = set(a.lower().split())
    set_b = set(b.lower().split())
    if not set_a and not set_b:
        return 1.0
    union = set_a | set_b
    return len(set_a & set_b) / len(union) if union else 1.0


def _tokenize(text: str) -> list[str]:
    """Simple tokenizer: lowercase, split on non-alphanumeric."""
    import re
    return [t for t in re.split(r"[^a-z0-9]+", text.lower()) if t and len(t) > 1]


def _build_tfidf(documents: list[str]) -> tuple[list[dict[str, float]], dict[str, float]]:
    """Build TF-IDF vectors for a corpus. Returns (vectors, idf_map).

    Each vector is a sparse dict {term: tfidf}.  IDF uses smoothed
    inverse document frequency: log((N + 1) / (df + 1)) + 1."""
    n = len(documents)
    if n == 0:
        return [], {}

    tokenized = [_tokenize(doc) for doc in documents]
    dfs: Counter[str] = Counter()
    for tokens in tokenized:
        dfs.update(set(tokens))

    idf: dict[str, float] = {}
    for term, df in dfs.items():
        idf[term] = math.log((n + 1) / (df + 1)) + 1.0

    vectors: list[dict[str, float]] = []
    for tokens in tokenized:
        tfs = Counter(tokens)
        doc_len = len(tokens) or 1
        vec = {term: (tf / doc_len) * idf.get(term, 0.0) for term, tf in tfs.items()}
        vectors.append(vec)

    return vectors, idf


def _cosine(a: dict[str, float], b: dict[str, float]) -> float:
    """Cosine similarity between two sparse TF-IDF vectors."""
    dot = sum(a.get(k, 0.0) * b.get(k, 0.0) for k in set(a) | set(b))
    norm_a = math.sqrt(sum(v * v for v in a.values()))
    norm_b = math.sqrt(sum(v * v for v in b.values()))
    if norm_a == 0.0 or norm_b == 0.0:
        return 0.0
    return dot / (norm_a * norm_b)


def tfidf_similarity(documents: list[str]) -> list[list[float]]:
    """Return pairwise cosine similarity matrix for the given documents."""
    n = len(documents)
    if n == 0:
        return []
    vectors, _ = _build_tfidf(documents)
    matrix: list[list[float]] = [[0.0] * n for _ in range(n)]
    for i in range(n):
        matrix[i][i] = 1.0
        for j in range(i + 1, n):
            sim = _cosine(vectors[i], vectors[j])
            matrix[i][j] = sim
            matrix[j][i] = sim
    return matrix


# ── Graph construction ─────────────────────────────────────────────────


def _similarity(texts: list[str], mode: str) -> list[list[float]]:
    """Compute pairwise similarity matrix."""
    if mode == "jaccard":
        n = len(texts)
        matrix: list[list[float]] = [[0.0] * n for _ in range(n)]
        for i in range(n):
            matrix[i][i] = 1.0
            for j in range(i + 1, n):
                sim = jaccard(texts[i], texts[j])
                matrix[i][j] = sim
                matrix[j][i] = sim
        return matrix
    # tfidf (default)
    return tfidf_similarity(texts)


def build_graph(items: list, text_field: str, threshold: float,
                similarity_mode: str = "tfidf") -> dict:
    """Build an undirected proximity graph.

    Returns:
        {
          "nodes": [{"id": 0, "label": "...", "statement": "..."}, ...],
          "edges": [{"source": 0, "target": 1, "weight": 0.85}, ...],
          "clusters": [[0, 3], [1, 2], ...]  # connected components
          "similarity_mode": "tfidf"
        }
    """
    n = len(items)
    texts = [str(item.get(text_field, "")) for item in items]
    sim_matrix = _similarity(texts, similarity_mode)

    nodes = [
        {
            "id": i,
            "label": texts[i][:80],
            "statement": texts[i],
        }
        for i in range(n)
    ]

    adjacency: list[list[int]] = [[] for _ in range(n)]
    edges: list[dict] = []
    for i in range(n):
        for j in range(i + 1, n):
            sim = round(sim_matrix[i][j], 3)
            if sim >= threshold:
                adjacency[i].append(j)
                adjacency[j].append(i)
                edges.append({"source": i, "target": j, "weight": sim})

    # Connected components (clusters) via DFS
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

    return {
        "nodes": nodes,
        "edges": edges,
        "clusters": clusters,
        "similarity_mode": similarity_mode,
    }


def select_frontier(items: list, text_field: str, threshold: float,
                    max_items: int, similarity_mode: str = "tfidf") -> list[int]:
    """Greedy diverse subset — one representative per cluster, then fill
    remaining slots with least-similar items to the frontier."""
    graph = build_graph(items, text_field, threshold, similarity_mode)
    clusters = graph["clusters"]
    n = len(items)
    selected: set[int] = set()

    for cluster in clusters:
        selected.add(cluster[0])

    texts = [str(item.get(text_field, "")) for item in items]

    if len(selected) < max_items and len(selected) < n:
        remaining = [i for i in range(n) if i not in selected]

        def min_sim_to_selected(idx: int) -> float:
            return min(
                (jaccard(texts[idx], texts[s]) for s in selected),
                default=1.0,
            )

        remaining.sort(key=min_sim_to_selected)
        for idx in remaining:
            if len(selected) >= max_items:
                break
            selected.add(idx)

    return sorted(selected)


# ── Main ───────────────────────────────────────────────────────────────


def main() -> None:
    config = json.loads(os.environ.get("EUREKA_CONFIG", "{}"))
    items_field = config.get("items_field", "hypotheses")
    text_field = config.get("text_field", "statement")
    threshold = float(config.get("threshold", 0.65))
    output_field = config.get("output_field", "hypotheses")
    max_frontier = int(config.get("max_frontier", 10))
    similarity_mode = config.get("similarity_mode", "tfidf")

    try:
        envelope = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        envelope = {}

    artifact_data = envelope.get("artifact", {}).get("data", {})
    items = artifact_data.get(items_field, [])

    graph = build_graph(items, text_field, threshold, similarity_mode)
    frontier_indices = select_frontier(
        items, text_field, threshold, max_frontier, similarity_mode,
    )
    unique_items = [items[i] for i in frontier_indices if i < len(items)]

    unique_emit = {
        "port": "unique",
        "artifact": {
            "kind": "Hypotheses",
            "data": {output_field: unique_items},
        },
    }
    print(json.dumps(unique_emit))

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