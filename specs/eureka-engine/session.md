# Spec: Session

> **Status:** Stable
> **Crate:** `eureka-engine`
> **Files:** `session.rs`, `run.rs`

## Purpose

`Session` is the top-level object for a single research run. It holds a `GraphSpec`,
a `NodeRegistry`, and an `EurekaConfig`. Calling `session.run(goal)` constructs all
nodes, starts the scheduler, handles events (broadcasting to the UI and persisting to
the DB), and returns `RunStats` when complete.

## Design

### Struct

```rust
pub struct Session {
    config:            EurekaConfig,
    spec:              GraphSpec,
    registry:          NodeRegistry,
    stats:             Option<RunStats>,
    session_id:        uuid::Uuid,
    event_broadcaster: Option<broadcast::Sender<SchedulerEvent>>,
    db:                Option<Arc<SessionDb>>,
}
```

### Constructors

All four constructors validate the graph at construction time (not at `run()` time).
If validation fails, an `EngineError::Graph` is returned immediately.

```rust
/// Load GraphSpec from config.graph path, generate a new UUIDv7 session_id.
pub fn new(config: EurekaConfig, registry: NodeRegistry) -> Result<Self, EngineError>;

/// Load GraphSpec from config.graph path, use the supplied session_id.
/// Use this when the ID is needed before session creation (e.g. DB setup).
pub fn new_with_id(
    config:     EurekaConfig,
    registry:   NodeRegistry,
    session_id: uuid::Uuid,
) -> Result<Self, EngineError>;

/// Accept an already-loaded GraphSpec, generate a new UUIDv7 session_id.
pub fn with_spec(
    config:   EurekaConfig,
    spec:     GraphSpec,
    registry: NodeRegistry,
) -> Result<Self, EngineError>;

/// Accept an already-loaded GraphSpec and a supplied session_id.
pub fn with_spec_and_id(
    config:     EurekaConfig,
    spec:       GraphSpec,
    registry:   NodeRegistry,
    session_id: uuid::Uuid,
) -> Result<Self, EngineError>;
```

`new` and `new_with_id` delegate to `with_spec` / `with_spec_and_id` after loading the
file. `with_spec` delegates to `with_spec_and_id` with a freshly generated
`uuid::Uuid::now_v7()`. Graph format is detected by file extension: `.json` →
`GraphSpec::from_json`, anything else → `GraphSpec::from_toml`.

### Optional Attachments

```rust
/// Attach a session database to persist events and completion stats.
pub fn set_db(&mut self, db: Arc<SessionDb>);

/// Attach a broadcast sender so the UI server receives every SchedulerEvent.
pub fn set_event_broadcaster(&mut self, tx: broadcast::Sender<SchedulerEvent>);
```

Both are optional; if absent, the corresponding sink is skipped silently.

### Accessors

```rust
pub const fn session_id(&self) -> uuid::Uuid;
pub const fn stats(&self)      -> Option<&RunStats>;
pub const fn spec(&self)       -> &GraphSpec;
```

`stats()` returns `None` until `run()` completes.

### Run Sequence

```rust
pub async fn run(&mut self, goal: serde_json::Value) -> Result<RunStats, EngineError>;
```

1. Construct all nodes: for each `GraphNodeSpec` in `spec.nodes`, call
   `registry.construct(&node_spec) → BoxedNode`. Returns `EngineError::UnknownNodeKind`
   if the kind is not registered, or propagates any other `EngineError` from the
   constructor.
2. Create `Scheduler::new(spec, nodes, budget, max_in_flight)`.
   - `budget` comes from `config.to_graph_budget()`.
   - `max_in_flight` comes from `config.scheduler.max_in_flight`.
3. Spawn a Tokio task that receives `SchedulerEvent`s and:
   - Forwards each event to `event_broadcaster` (if set) via `tx.send(event.clone())`.
   - On `ActivationCompleted`: for each output in `outputs`, calls
     `db.write_event(round, node_id, port, kind, data_str)` (if DB is set).
   - Logs all variants at appropriate tracing levels.
4. Find source nodes via `spec.source_node_ids()`.
5. Inject `Artifact { kind: "Goal", data: goal }` into every source node as initial
   input.
6. Call `scheduler.run(initial_artifacts).await` → `RunStats`.
7. Store stats in `self.stats`.
8. Abort the event task.
9. Call `db.complete_session(stats.rounds_completed)` (if DB is set).
10. Return `RunStats`.

### Event Handling Detail

The event task handles all five `SchedulerEvent` variants:

| Variant | Action |
|---|---|
| `ActivationStarted { node_id, node_kind, round }` | `info!` log |
| `ActivationCompleted { node_id, node_kind, round, emit_count, outputs }` | `info!` log + DB writes |
| `ActivationFailed { node_id, node_kind, round, error }` | `error!` log |
| `CycleCompleted { round }` | `info!` log |
| `RunHalted { reason, total_rounds }` | `info!` log |

DB writes for `ActivationCompleted` extract `"port"`, `"kind"`, and `"data"` keys from
each element of `outputs: Vec<serde_json::Value>`. Serialization failures fall back to
`"null"`. Write failures are logged as warnings but do not abort the run.

### RunStats

Defined in `eureka_graph::control`:

```rust
pub struct RunStats {
    pub total_cost_usd:   f64,   // placeholder zero — token counting not implemented
    pub total_tokens:     u64,   // placeholder zero — token counting not implemented
    pub elapsed_secs:     f64,
    pub rounds_completed: u32,
}
```

### Goal Shape

`run()` takes `serde_json::Value`. The convention is:

```json
{ "goal": "Discover a novel CO2 reduction catalyst", "domain": "catalysis" }
```

The goal is wrapped in `Artifact { kind: "Goal", data: goal }` and injected into all
source nodes (those identified by `spec.source_node_ids()`).

## Invariants

- Graph validation (`validate_graph`) runs at construction time, before any nodes are
  constructed or LLM calls are made. `run()` does not re-validate.
- A `Session` can only be run once — the scheduler takes ownership of the node map.
- `session_id` is set at construction and never changes.
- `set_db` and `set_event_broadcaster` must be called before `run()` to take effect.

## Non-Goals

- `Session` does not persist state between runs.
- `Session` does not support checkpointing or resuming a partially-completed run.
- `Session` does not manage the UI server — that is started separately in the CLI.
- `stop_session()` on the DB is not called by `Session`; interrupted runs are only
  marked stopped if the CLI catches a signal and calls it directly.
