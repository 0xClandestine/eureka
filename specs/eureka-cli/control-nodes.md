# Spec: Control Nodes

> **Status:** Stable (plugin-based)
> **Crate:** `eureka-plugins` (runtime), `eureka-cli` (registration)
> **Files:** `graphs/coscientist/plugins/`, `commands/run.rs`

## Purpose

Control nodes handle non-LLM operations: budget enforcement, Elo tournament ranking,
hypothesis deduplication, fan-in merging, conditional routing, and human-in-the-loop
gating. They behave identically to LLM agent nodes from the scheduler's perspective —
they receive an artifact on an input port and emit artifacts on output ports.

## Architecture

Control nodes for the coscientist graph are implemented as **subprocess plugins**
rather than compiled Rust code. Each plugin is a directory under
`graphs/coscientist/plugins/` containing a `plugin.json` manifest and a Python script.
The `ControlPluginNode` Rust type handles subprocess lifecycle uniformly for all of
them. See [plugins.md](../eureka-plugins/plugins.md) for the invocation protocol.

This design means control behavior is tunable and replaceable without recompiling
Eureka — swap a Python script, ship a new graph directory.

## Built-in Coscientist Plugins

### `round-governor`

Enforces a round limit. Reads `max_rounds` from `EUREKA_CONFIG` (set from the node's
`config` block in `graph.json`, default 12).

```
Input:  in       (Hypotheses)
Output: continue (Hypotheses)  ← pass-through for next reflection round
        halt     (Control)     ← session ends naturally with no consumer
```

On each activation: if `EUREKA_ROUND < max_rounds`, emit the input hypotheses
unchanged on `continue` so they enter the next reflection–ranking cycle. Otherwise,
emit a halt signal on `halt`. The scheduler terminates naturally when `halt` emits
with no downstream consumer.

`continue` carries `Hypotheses` (not `Control`) so the downstream `reflection` node
receives real hypothesis data for the next review round.

Config:

| Field | Default | Meaning |
|---|---|---|
| `max_rounds` | `12` | Number of evolution cycles before halting |

### `elo-ranker`

Runs a pairwise Elo tournament over reviewed hypotheses. Elo state is persisted to the
session SQLite database across rounds via `EUREKA_DB_PATH`.

```
Input:  in    (Reviews)     ← batch of reviewed hypotheses with scores
        cycle (Control)     ← (unused; retained for legacy compat)
Output: top   (Hypotheses)  ← top-k by Elo rating
        state (Ranking)     ← full Elo ratings table + top-k list
```

Both inputs emit `top` and `state`. On `in`, new Elo ratings are computed; on `cycle`,
existing ratings are re-emitted unchanged.

Config:

| Field | Default | Meaning |
|---|---|---|
| `items_field` | `"reviews"` | Key in input data containing the review array |
| `score_field` | `"score"` | Key within each review holding the numeric score |
| `item_field` | `"hypothesis"` | Key within each review holding the hypothesis object |
| `id_field` | `"statement"` | Key within the hypothesis used as stable identity |
| `output_field` | `"hypotheses"` | Key used in output data |
| `top_k` | `5` | Number of hypotheses to emit on `top` |
| `k_factor` | `32.0` | Elo K-factor |

**Elo math:** Initial rating 1200. For each pair of reviews, the higher-scored one is
the winner. `K=32` updates:

```
expected(a, b) = 1 / (1 + 10^((b - a) / 400))
new_winner = winner + 32 * (1 - expected(winner, loser))
new_loser  = loser  + 32 * (0 - expected(loser, winner))
```

`state` artifact shape:
```json
{
  "hypotheses": [...],
  "elo_ratings": { "<statement>": 1342.5, ... },
  "count": 12
}
```

> Note: `elo_ratings` contains only the **top-k** entries (not all tracked items). `count`
> reflects the total number of items in the Elo table across all rounds.

### `jaccard-dedup`

Deduplicates a hypothesis set using word-level Jaccard similarity. Stateless — runs
fresh on each activation with no persistent state.

```
Input:  in     (Hypotheses)
Output: unique (Hypotheses)
```

Config:

| Field | Default | Meaning |
|---|---|---|
| `items_field` | `"hypotheses"` | Key in input data containing the hypothesis array |
| `text_field` | `"statement"` | Key within each hypothesis holding comparison text |
| `threshold` | `0.65` | Jaccard similarity above which two hypotheses are duplicates |
| `output_field` | `"hypotheses"` | Key used in output data |

**Jaccard similarity:** Tokenize both statements into lowercased word sets, compute
`|A ∩ B| / |A ∪ B|`. If two hypotheses exceed the threshold, the later one is dropped.

> Note: word-level similarity, not semantic/embedding similarity.

## Unimplemented Control Nodes

The following control behaviors are not yet implemented as plugins. They existed as
Rust stubs in an earlier version and have been removed pending proper plugin implementations.

| Behavior | Intended kind | Status |
|---|---|---|
| Fan-in merge of parallel streams | `merge` | Not implemented |
| Conditional routing | `router` | Not implemented |
| Human review gate | `human_gate` | Not implemented |

## Governor Contract

Any node that can govern a cycle must declare an output port named **`halt`**. The
graph validator uses this port name as the behavioral contract — not the node's kind
string. This allows both native and plugin governors to satisfy the cycle-governance
rule uniformly.

## Invariants

- All plugin control nodes read config from `EUREKA_CONFIG` with explicit defaults —
  missing config keys never cause crashes.
- `elo-ranker` Elo state is per-session, persisted to the session DB.
- `jaccard-dedup` is stateless — deduplication is performed fresh each activation.
- `round-governor`'s `halt` output is intentionally unwired in the coscientist graph;
  the session terminates naturally when the scheduler's pending counter reaches zero.

## Non-Goals

- Plugin control nodes do not make LLM calls.
- `elo-ranker` does not perform head-to-head LLM debate — it uses review scores as
  the proxy for pairwise comparison.
