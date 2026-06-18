# Spec: Graph Validation

> **Status:** Stable
> **Crate:** `eureka-graph`
> **Files:** `validate.rs`

## Purpose

`validate_graph()` checks a `GraphSpec` against a `PortRegistry` before any LLM calls
are made. It proves six structural properties that guarantee the graph can run
meaningfully and safely.

## Motivation

Without pre-flight validation, a misconfigured graph (wrong port name, missing edge,
unguarded cycle) would either silently produce nothing or fail partway through an
expensive LLM run. By making structural errors detectable at load time, we shift the
error surface from startup to before any work is done.

## Design

### API

```rust
pub fn validate_graph(spec: &GraphSpec, registry: &PortRegistry) -> ValidationResult;

pub struct ValidationResult {
    pub valid:  bool,
    pub errors: Vec<GraphError>,
}

impl ValidationResult {
    pub const fn valid() -> Self;
    pub const fn invalid(errors: Vec<GraphError>) -> Self;
    pub fn merge(results: Vec<Self>) -> Self;
}

pub struct PortRegistry {
    specs: HashMap<String, PortSpec>,  // private field
}

impl PortRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, kind: impl Into<String>, spec: PortSpec);
    pub fn register_node(&mut self, kind: impl Into<String>, node: &impl Node);
    pub fn get(&self, kind: &str) -> Option<&PortSpec>;
}

pub fn parse_port_ref(ref_str: &str) -> Option<(String, String)>;
```

`parse_port_ref` splits a `"node.port"` string into `(node_id, port_name)` using
`splitn(2, '.')`. Returns `None` if there is no `.` in the input.

`ValidationResult::merge` flattens errors from multiple results; the merged result is
valid iff there are no errors.

All errors are collected before returning — the validator does not short-circuit on the
first failure (except: if unknown node references are found in edges, subsequent checks
are skipped to avoid cascading errors).

### Early Exit

If any edge references an unknown node ID, `validate_graph` returns immediately with
those `UnknownNode` errors before running the remaining checks. All other rules
(port kind match, dangling inputs, reachability, unknown node kinds, governed cycles,
sink presence) proceed in a single pass and accumulate errors.

### The Six Rules

The rules execute in this order, matching the code structure. All rules accumulate
errors in a single pass (except the early exit on unknown node references described
above). Rule 3b runs inside that same pass immediately after Rule 3 (Reachability).

#### 1. Port Kind Match

For every edge `(from_node, from_port) → (to_node, to_port)`:
- Look up `from_node.kind` in the registry → get its output port named `from_port`.
- Look up `to_node.kind` in the registry → get its input port named `to_port`.
- Assert `output_port.kind == input_port.kind`.

Violation: `"Edge generation.out → ranking.in: kind mismatch (Hypotheses vs Reviews)"`

#### 2. No Dangling Required Inputs

For every node kind in the spec, for every `required: true` input port that is not a
`Goal` source port: assert that at least one inbound edge targets that port.

`Goal` ports are exempt because they are externally injected by the session at startup
(not wired in the graph).

Violation: `"Node reflection.in (Hypotheses) has no inbound edges"`

#### 3. Reachability

BFS from all nodes that accept `Goal` input must visit every node in the spec.
Unreachable nodes can never activate and indicate a disconnected topology.

If no node accepts a `Goal` input (e.g. the registry is empty), falls back to treating
nodes with no inbound edges as sources.

Violation: `"Node evolution is unreachable from any Goal source"`

#### 3b. No Unknown Node Kinds

If the registry is non-empty, every node kind referenced in `graph.json` must be
present in the registry. This catches missing plugin installations before any LLM
work begins.

This check runs in the same pass as Reachability, **after** it (step 3b in the code).

Violation: `"Node kind 'elo-ranker' (id: 'ranking') is not registered. Check that the plugin is installed under <graph_dir>/plugins/."`

This check only runs when the registry is populated — an empty registry (e.g. during
isolated unit tests) skips it to avoid false positives.

#### 4. Governed Cycles

Find all strongly-connected components using Tarjan's algorithm. For any SCC of size
> 1 (i.e., a real cycle), or a single-node SCC with a self-loop, assert that at least
one node in the SCC is a **governor**.

A node is considered a governor if its registered port spec includes an output port
named **`"halt"`**. This is the plugin-agnostic behavioral contract — any node,
regardless of kind string, that can halt a cycle satisfies the rule.

This replaces the earlier convention of checking for `kind == "control.governor"`,
which was not portable to plugin-based governors.

Violation: `"SCC #0 (evolution, governor, proximity, ranking, reflection) has no governor node (a node with a 'halt' output port)"`

#### 5. Sink Presence

At least one node in the spec must emit an artifact kind that is not consumed as an
input by any other registered node. Without a terminal output, the run produces no
final result visible to the researcher.

Skipped when the registry is empty.

Violation: `"No node produces a terminal output (an artifact kind that no other node consumes)"`

In the coscientist graph, `meta_review`'s `Overview` output satisfies this rule —
`Overview` is not an input kind for any registered node.

## Error Types

All validation errors are `GraphError` variants (from `spec.rs`):

| Variant | When emitted |
|---------|-------------|
| `UnknownNode(String)` | An edge references a node ID not in `spec.nodes`, or (step 3b) a node kind absent from a non-empty registry |
| `UnknownPort(String)` | An edge references a port name absent from the node's registered `PortSpec` |
| `PortKindMismatch(String)` | Output kind ≠ input kind on a connected edge |
| `DanglingPort(String)` | A required non-Goal input port has no inbound edge |
| `UnreachableNode(String)` | A node cannot be reached via BFS from Goal-accepting sources |
| `UngovernedCycle(String)` | An SCC lacks a node with a `"halt"` output port |
| `MissingSink(String)` | No node produces an artifact kind unconsumed by any other node |
| `ParseError(String)` | JSON/TOML parse failure in `GraphSpec::from_json`/`from_toml` |

## Invariants

- Validation is pure — it has no side effects and does not modify the spec or registry.
- `validate_graph()` must be called before constructing any `BoxedNode` instances.
- Unknown node kinds produce an error that names the missing kind and hints at plugin installation.
- Governed-cycle detection uses Tarjan's SCC algorithm. A single-node SCC is only flagged if the node has a self-loop.

## Non-Goals

- Validation does not check the content of `config` objects within graph nodes.
- Validation does not guarantee the graph terminates in finite time.
- Validation does not type-check the JSON `data` inside `Artifact` values.
