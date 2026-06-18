# Spec: Graph Model

> **Status:** Stable
> **Crate:** `eureka-graph`
> **Files:** `artifact.rs`, `node.rs`, `port.rs`, `edge.rs`, `spec.rs`

## Purpose

Defines the fundamental data types that make up a Eureka execution graph: artifacts
(messages flowing between nodes), nodes (processing units), ports (typed connectors),
edges (directed connections), and the loadable graph specification.

## Motivation

The core design constraint is that graph topology must be a runtime configuration, not
compile-time structure. This rules out statically-typed pipelines. Instead, a single
generic `Artifact` type carries all inter-node messages, with the type information
encoded as an open string (`ArtifactKind`). Type safety is recovered at graph-load
time by the validator, not at Rust compile time.

## Design

### Artifact

```rust
pub type ArtifactKind = String;

pub struct Artifact {
    pub kind: ArtifactKind,
    pub data: serde_json::Value,
}

impl Artifact {
    pub fn new<T: Serialize>(kind: impl Into<String>, value: &T)
        -> Result<Self, serde_json::Error>;
    pub fn deserialize_as<T: DeserializeOwned>(&self)
        -> Result<T, serde_json::Error>;
}

impl Display for Artifact { /* prints "Artifact(<kind>)" */ }
```

`ArtifactKind` is a plain `String` alias — not an enum. This keeps the kind system open:
graphs and agents can introduce new kinds without modifying the Rust codebase. The
`data` field is an untyped `serde_json::Value` — each node is responsible for parsing
what it expects out of `data` according to its own schema.

`Artifact::new` serializes a typed value into the `data` field.
`Artifact::deserialize_as` deserializes `data` into a typed value (clones the JSON internally).

**Why not an enum?** An enum of variants would require every possible artifact type to be
known at compile time, defeating the "topology is data" goal. An open string + JSON value
defers type interpretation to the node receiving it.

### Node

```rust
pub trait Node: Send + Sync {
    fn ports(&self) -> PortSpec;
    async fn process(&self, ctx: &NodeCtx, msg: PortMsg)
        -> Result<Vec<Emit>, NodeError>;
}

pub struct BoxedNode {
    inner: Arc<dyn Node>,
}
```

`BoxedNode` is a newtype wrapper (not a type alias) around `Arc<dyn Node>`. It exposes
`BoxedNode::new(node: impl Node + 'static)`, `ports()`, and `process()` as forwarding
methods. The inner `Arc` lets the scheduler clone node references across async tasks.

Nodes are stateless from the graph's perspective — the scheduler calls `process` and
receives emitted artifacts. Any state a node needs (e.g., Elo ratings in `RankingNode`)
lives inside the node behind interior mutability.

`NodeCtx` carries per-activation metadata and is constructed with `NodeCtx::new(node_id,
node_kind, round, cancel)`:

```rust
pub struct NodeCtx {
    pub node_id:   String,
    pub node_kind: String,
    pub round:     u32,
    pub cancel:    tokio_util::sync::CancellationToken,
}
```

`NodeCtx::is_cancelled() -> bool` delegates to `cancel.is_cancelled()`.

`PortMsg` is the incoming message: `{ port: PortId, artifact: Artifact }`.

`Emit` is one outbound message: `{ port: PortId, artifact: Artifact }`.
`Emit::new(port: impl Into<String>, artifact: Artifact) -> Self` is a convenience constructor.

#### NodeError

```rust
pub enum NodeError {
    Agent(String),
    PortKind { port: String, expected: &'static str, got: String },
    Timeout(String),
    Cancelled,
    Internal(String),
}
```

`NodeError` is returned from `Node::process`. A failed node activation does not abort
the run — the scheduler emits `SchedulerEvent::ActivationFailed` and continues.

### Port

```rust
pub type PortId = String;  // convention: "<node_id>.<port_name>"

pub enum PortDirection { Input, Output }

pub struct PortSpecEntry {
    pub name:      String,
    pub direction: PortDirection,
    pub kind:      ArtifactKind,
    pub required:  bool,          // serde default = false
}

pub struct PortSpec {
    pub inputs:  Vec<PortSpecEntry>,
    pub outputs: Vec<PortSpecEntry>,
}
```

`PortSpec` is the contract a node declares — what kinds flow into and out of which named
ports. The validator checks every edge against the port specs of its endpoints.

`PortSpec::new(inputs, outputs)` is a `const fn` constructor.

#### PortSpec query methods

| Method | Returns |
|--------|---------|
| `input_kind(name)` | `Option<ArtifactKind>` |
| `output_kind(name)` | `Option<ArtifactKind>` |
| `is_input_required(name)` | `bool` |
| `required_inputs()` | `Vec<String>` — names of required input ports |
| `input_names()` | `Vec<String>` |
| `output_names()` | `Vec<String>` |

### Edge

```rust
pub struct Edge {
    pub from_node: String,
    pub from_port: String,
    pub to_node:   String,
    pub to_port:   String,
    pub feedback:  bool,
}
```

`feedback: true` marks a back-edge in the graph (cycle). The scheduler uses this flag
to increment the round counter when traversing such an edge.

### GraphSpec

```rust
pub struct GraphNodeSpec {
    pub id:          String,
    pub kind:        String,
    pub config:      serde_json::Value,   // serde default = Null
    pub description: Option<String>,      // skipped in serialization if None
}

pub struct GraphSpec {
    pub name:        Option<String>,
    pub description: Option<String>,      // skipped in serialization if None
    pub nodes:       Vec<GraphNodeSpec>,
    pub edges:       Vec<Edge>,
    pub metadata:    serde_json::Value,   // serde default = Null (not Option)
}
```

Loaded from a JSON string via `GraphSpec::from_json(json_str)` or from TOML via
`GraphSpec::from_toml(toml_str)`. Both return `Result<Self, GraphError>` with
`GraphError::ParseError` on malformed input. `GraphSpec::to_toml()` serializes back to
TOML.

The spec is a pure data structure — it has no behavior and holds no live nodes.

#### Query methods

| Method | Returns |
|--------|---------|
| `node_ids()` | `Vec<String>` — all node IDs |
| `node_kind(id)` | `Option<&str>` — kind of a node by ID |
| `edges_from(node_id)` | `Vec<&Edge>` — all outbound edges from a node |
| `edges_to(node_id)` | `Vec<&Edge>` — all inbound edges to a node |
| `source_node_ids()` | `Vec<String>` — nodes with no non-feedback inbound edges |
| `sink_node_ids()` | `Vec<String>` — nodes with no outbound edges |

`source_node_ids()` excludes feedback edges so that cyclic nodes that receive initial
goal injection (e.g. `generation`) are correctly identified as sources.

## Invariants

- `ArtifactKind` comparisons are case-sensitive string equality.
- Every `Edge` must reference node IDs and port names that exist in the `GraphSpec` (enforced by the validator, not at struct construction time).
- A node emitting on an unknown port name is a runtime routing miss — the artifact is silently dropped.
- `BoxedNode` wraps `Arc<dyn Node>` internally; nodes must be `Send + Sync + 'static`.
- `GraphSpec.metadata` defaults to `serde_json::Value::Null` (not `None`) when absent from JSON/TOML.
- `Edge::feedback` defaults to `false` via `#[serde(default)]`; the builder method `Edge::feedback()` sets it to `true` (consuming `self`).

## Non-Goals

- `Artifact` does not validate `data` against any schema at construction time.
- `GraphSpec` does not check edge validity — that is the validator's job.
- The graph model has no concept of "running" — that belongs to the scheduler.

## Open Questions

- Should `Artifact` carry a timestamp or provenance chain for debugging?
- Should `PortSpec` support variadic ports (e.g., `control.merge` takes N inputs)?
