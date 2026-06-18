# Spec: Node Registry

> **Status:** Stable
> **Crate:** `eureka-engine`
> **Files:** `registry.rs`

## Purpose

`NodeRegistry` maps node kind strings (e.g., `"generation"`, `"control.ranking"`) to
factory functions that construct `BoxedNode` instances from a `GraphNodeSpec`. It also
maintains a `PortRegistry` for graph validation.

## Motivation

Because graph topology is loaded at runtime, node construction must also happen at
runtime given only a string kind identifier. The registry is the binding between the
string names used in `graph.json` and the Rust types that implement those nodes.

## Design

### Types

```rust
pub type NodeConstructor = Arc<dyn Fn(&GraphNodeSpec) -> Result<BoxedNode, EngineError> + Send + Sync>;

pub struct NodeRegistry {
    constructors:  HashMap<String, NodeConstructor>,
    port_registry: PortRegistry,
}
```

### Methods

```rust
impl NodeRegistry {
    pub fn new() -> Self;

    /// Register a configurable node kind. The constructor receives the full
    /// GraphNodeSpec (including spec.config) and returns a BoxedNode.
    pub fn register(
        &mut self,
        kind:        impl Into<String>,
        constructor: NodeConstructor,
        port_spec:   PortSpec,
    );

    /// Register a node kind from a pre-built BoxedNode instance.
    ///
    /// Useful for nodes that are always constructed identically (e.g. control
    /// nodes with no per-instance configuration). The node's ports() is called
    /// once to populate the PortRegistry; the clone of the BoxedNode is
    /// returned for every construct() call.
    pub fn register_instance(&mut self, kind: impl Into<String>, node: BoxedNode);

    /// Construct a node from a GraphNodeSpec. Returns Err if the kind is unknown.
    pub fn construct(&self, spec: &GraphNodeSpec) -> Result<BoxedNode, EngineError>;

    /// Check if a kind is registered.
    pub fn has_kind(&self, kind: &str) -> bool;

    /// Return all registered kind strings (order is arbitrary).
    pub fn registered_kinds(&self) -> Vec<String>;

    /// Return the port registry for use with validate_graph().
    pub fn port_registry(&self) -> &PortRegistry;
}
```

### Registration Pattern

Control nodes read their configuration from `spec.config` inside the constructor
closure, making each instance independently configurable:

```rust
registry.register(
    "control.governor",
    Arc::new(|spec| {
        let max_rounds = spec.config
            .get("max_rounds")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as u32;
        Ok(BoxedNode::new(GovernorNode { max_rounds }))
    }),
    governor_port_spec(),
);
```

LLM agent nodes are registered the same way, but their constructor closes over an
`Arc<AgentDef>` and `Arc<dyn LlmClient>` rather than reading from config:

```rust
registry.register(
    def.name.clone(),
    Arc::new(move |_spec| Ok(BoxedNode::new(LlmAgentNode::new(
        Arc::clone(&def_arc),
        Arc::clone(&client_arc),
    )))),
    def.to_port_spec(),
);
```

### Build Order

`build_registry()` in `commands/run.rs`:
1. Derives the agents directory as `<graph_dir>/agents/`.
2. Loads all `AgentDef`s from that directory.
3. Builds the LLM client (once, shared).
4. Registers each `AgentDef` as an `LlmAgentNode`.
5. Registers all built-in control nodes.

## Invariants

- Each `kind` string should be registered at most once. Re-registration silently
  overwrites the previous entry (HashMap semantics); no panic occurs.
- The `port_registry` and `constructors` map are always in sync: every registered
  constructor has a corresponding `PortSpec` in the `PortRegistry`.
- `construct()` should only be called after `validate_graph()` has confirmed all
  node kinds in the graph are present in the registry.

## Non-Goals

- The registry does not support unregistering or replacing node kinds at runtime.
- The registry does not validate `spec.config` contents — that is each constructor's job.
