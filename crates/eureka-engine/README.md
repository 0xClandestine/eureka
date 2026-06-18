# eureka-engine

> Wiring: node registry, session lifecycle, and run orchestration.

**Status:** Active | **Depends on:** `eureka-graph`, `eureka-agents`, `eureka-config` | **Used by:** `eureka-cli`

The engine contains no research logic — that lives in the graph spec and agent files.
It provides the mechanism to wire a `NodeRegistry` to a `GraphSpec`, validate the
combination, and run it as a `Session`.

## Specs

| Spec | Description |
|------|-------------|
| [node-registry](../../specs/eureka-engine/node-registry.md) | `NodeRegistry`, constructor pattern, build order |
| [session](../../specs/eureka-engine/session.md) | `Session` lifecycle, event handling, DB integration, `RunStats`, goal shape |
| [run-orchestration](../../specs/eureka-engine/run-orchestration.md) | `RunConfig`, `RunResult`, `run_single`, `run_batch` |

## Modules

```mermaid
graph TD
    error["error.rs\nEngineError"]
    registry["registry.rs\nNodeRegistry · NodeConstructor"]
    run["run.rs\nRunConfig · RunResult · run_single · run_batch"]
    session["session.rs\nSession"]
    graph["eureka-graph\nScheduler · validate_graph · PortRegistry"]

    error --> registry
    error --> session
    registry --> session
    run --> session
    session --> graph
```

| File | Key exports |
|------|-------------|
| `error.rs` | `enum EngineError` |
| `registry.rs` | `struct NodeRegistry`, `type NodeConstructor` |
| `run.rs` | `struct RunConfig`, `struct RunResult`, `fn run_single`, `fn run_batch` |
| `session.rs` | `struct Session` |

## Public API

### NodeRegistry

```rust
pub struct NodeRegistry { /* ... */ }

impl NodeRegistry {
    pub fn new() -> Self;
    pub fn register(
        &mut self,
        kind:        impl Into<String>,
        constructor: NodeConstructor,
        ports:       PortSpec,
    );
    pub fn construct(&self, spec: &GraphNodeSpec) -> Result<BoxedNode, EngineError>;
    pub fn has_kind(&self, kind: &str) -> bool;
    pub fn port_registry(&self) -> &PortRegistry;
}
```

### Session

Graph validation runs at construction time. `run()` does not re-validate.

```rust
impl Session {
    // Load GraphSpec from config.graph path:
    pub fn new(config: EurekaConfig, registry: NodeRegistry) -> Result<Self, EngineError>;
    pub fn new_with_id(config: EurekaConfig, registry: NodeRegistry, session_id: uuid::Uuid) -> Result<Self, EngineError>;

    // Accept an already-loaded GraphSpec:
    pub fn with_spec(config: EurekaConfig, spec: GraphSpec, registry: NodeRegistry) -> Result<Self, EngineError>;
    pub fn with_spec_and_id(config: EurekaConfig, spec: GraphSpec, registry: NodeRegistry, session_id: uuid::Uuid) -> Result<Self, EngineError>;

    // Optional attachments (call before run()):
    pub fn set_db(&mut self, db: Arc<SessionDb>);
    pub fn set_event_broadcaster(&mut self, tx: broadcast::Sender<SchedulerEvent>);

    // Accessors:
    pub const fn session_id(&self) -> uuid::Uuid;
    pub const fn stats(&self)      -> Option<&RunStats>;
    pub const fn spec(&self)       -> &GraphSpec;

    pub async fn run(&mut self, goal: serde_json::Value) -> Result<RunStats, EngineError>;
}
```

**Run sequence:** construct all nodes → create `Scheduler` → spawn event task
(broadcast + DB writes) → inject `Goal` artifact into source nodes →
await scheduler quiescence → abort event task → call `db.complete_session()` →
return `RunStats`.

### RunStats

Defined in `eureka_graph::control` (used directly; not re-exported from `eureka-engine`):

```rust
pub struct RunStats {
    pub total_cost_usd:   f64,   // placeholder zero — token counting not implemented
    pub total_tokens:     u64,   // placeholder zero — token counting not implemented
    pub elapsed_secs:     f64,
    pub rounds_completed: u32,
}
```

### Batch Helpers (run.rs)

```rust
pub struct RunConfig {
    pub goal:             serde_json::Value,
    pub config_overrides: Option<EurekaConfig>,
}

pub struct RunResult {
    pub session_id: uuid::Uuid,
    pub stats:      RunStats,
}

pub async fn run_single(config: EurekaConfig, registry: NodeRegistry, goal: serde_json::Value)
    -> Result<RunResult, EngineError>;

pub async fn run_batch(base_config: EurekaConfig, registry: NodeRegistry, runs: Vec<RunConfig>)
    -> Result<Vec<RunResult>, EngineError>;
```

## Testing

Tests cover: registry register/construct/lookup, session validation failure on an
invalid graph, `RunStats` field access, and session construction with a minimal spec.
