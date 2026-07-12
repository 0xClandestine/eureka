# Eureka Durable Runs, Database Access, and Resume — Specification

**Status:** Proposed (database-environment contract partially implemented)

**Scope:** Eureka runtime library, scheduler, control-node protocol, agent tools,
CLI, and application HTTP integration.

## 1. Summary

Eureka currently has two useful but disconnected persistence mechanisms:

1. The CLI creates one SQLite file per run and exposes its path to subprocess
   control nodes through `EUREKA_DB_PATH`.
2. Rust persists lifecycle metadata in one JSON file per run through
   `FileRunStore`.

The shipped Python control nodes use the SQLite file for cross-round state such
as Elo ratings, proximity/context memory, and supervisor statistics. However,
the Rust runtime does not own that database, does not persist scheduler state in
it, and cannot resume a graph after the process exits.

This specification makes persistence cohesive without weakening Eureka's core
model:

- graph topology remains manifest-defined;
- artifacts and ports remain the primary graph data model;
- control plugins retain a namespaced database area for domain state;
- the Rust runtime owns run lifecycle, checkpoints, artifacts, and migrations;
- pause/resume and human input become durable operations;
- application services receive a small, stable run-management API.

The design intentionally does **not** turn Eureka into a shared mutable-context
workflow engine. It adds durable execution state around the existing artifact
scheduler.

## 2. Goals

### 2.1 Runtime goals

- Persist a run before execution starts.
- Persist lifecycle transitions atomically.
- Persist enough scheduler state to resume after a clean pause or process exit.
- Support pause, resume, cancel, and human artifact injection.
- Preserve partial statistics and terminal outputs.
- Detect concurrent updates to the same run.
- Validate that a checkpoint belongs to the same graph/configuration.
- Make database access consistent for control nodes and agent tools.
- Keep plugin-owned tables isolated from runtime-owned tables.
- Provide an in-memory backend for tests and a local SQLite backend for the CLI.
- Leave a path to PostgreSQL without coupling graph execution to one database.

### 2.2 Application goals

- Provide a `RunManager`-style API for HTTP services.
- Support status polling, event streaming, and final-result retrieval.
- Allow applications to attach metadata without putting it into graph artifacts.
- Provide examples for background execution and human review.

### 2.3 Compatibility goals

- Existing manifests continue to work unchanged.
- Existing control nodes continue to use `EUREKA_DB_PATH`.
- Existing plugin tables remain valid.
- `Session::run(goal)` remains available as a convenience API.
- Existing JSONL traces remain readable.
- A run without a configured persistent backend still works in memory.

## 3. Non-goals

- Replacing typed artifact flow with a global mutable context.
- Making every artifact durable by default for all workloads.
- Providing a general-purpose ORM to control plugins.
- Allowing plugins to mutate Rust-owned scheduler tables.
- Resuming an activation that was interrupted halfway through an LLM call or
  subprocess call. Resume occurs at a scheduler checkpoint boundary.
- Distributed scheduling across multiple workers in the first version.
- Guaranteeing exactly-once execution for external side effects. Nodes and
  plugins must remain idempotent at checkpoint boundaries.

## 4. Current-state audit

### 4.1 Existing per-run SQLite integration

The CLI currently creates:

```text
<graph_dir>/.eureka/sessions/<run_id>.sqlite
```

and passes the path to every executable node as:

```text
EUREKA_DB_PATH=<path>
```

The shared `RunEnvironment` contract also provides:

```text
EUREKA_SESSION_ID
EUREKA_NODE_ID
EUREKA_ROUND
EUREKA_CONFIG
EUREKA_DB_SCHEMA_VERSION
EUREKA_DB_NAMESPACE
```

Control nodes and LLM agent shell tools receive this same environment
contract. `EUREKA_DB_NAMESPACE` defaults to the invoking node ID and is an
advisory plugin namespace; runtime-owned tables remain reserved.

The Python controls use SQLite directly and define their own tables. This is
already sufficient for cross-round domain memory.

### 4.2 Existing Rust persistence

`RunStore` and `FileRunStore` persist a `RunRecord` as:

```text
<graph_dir>/.eureka/sessions/<run_id>.json
```

The record contains the graph path, goal, lifecycle status, statistics, and an
optional error. It does not contain scheduler buffers or pending artifacts.

### 4.3 Current limitations

- SQLite is created and owned implicitly by external processes.
- Rust does not run migrations or initialize runtime tables.
- There is no runtime-owned artifact/checkpoint schema.
- `FileRunStore` and the SQLite file have no shared transaction boundary.
- A process restart loses the scheduler's in-memory activation state.
- `Resume` is not a complete persisted resume operation.
- Rust does not yet run migrations or expose runtime-owned database tables.
- There is no version/compare-and-swap protection for run updates.
- There is no run listing/query API.
- Final artifacts are not persisted as a first-class result.

## 5. Terminology

- **Run:** One invocation of a graph with one run ID and initial goal.
- **Round:** A synchronized scheduler cycle. Forward edges stay in the round;
  feedback edges enter the next round.
- **Activation:** One node invocation with a joined set of input artifacts.
- **Checkpoint:** A durable scheduler boundary from which execution can resume.
- **Runtime state:** State owned by Rust: lifecycle, scheduler queues, input
  buffers, artifacts, stats, and graph identity.
- **Plugin state:** Domain-specific state owned by an external control node.
- **Run metadata:** Application-owned labels and correlation information that do
  not affect graph routing.
- **Terminal result:** Artifacts emitted at terminal/sink nodes and the final
  statistics.

## 6. Persistence architecture

### 6.1 One logical persistence boundary

A run has one logical persistence backend. The default local backend is SQLite:

```text
.eureka/
└── sessions/
    ├── <run_id>.sqlite       # runtime + plugin state
    ├── <run_id>.traces.jsonl # event trace
    └── <run_id>.json         # optional compatibility projection
```

The SQLite database becomes authoritative for runtime lifecycle and checkpoint
state. The JSON record remains supported as a simple export/compatibility
projection, but it must not become a second independently authoritative store.

### 6.2 Ownership boundaries

Rust owns tables prefixed with `eureka_`:

```text
eureka_schema_migrations
eureka_runs
eureka_checkpoints
eureka_checkpoint_inputs
eureka_checkpoint_activations
eureka_artifacts
eureka_run_outputs
eureka_run_metadata
```

Plugins own tables outside the `eureka_` namespace. They may create and mutate
those tables freely, but must not write runtime tables.

The runtime must document that plugin tables should use a unique prefix, for
example:

```text
ranker_elo_ratings
supervisor_context_memory
proximity_graphs
```

Existing example tables may retain their current names during migration, but
new examples should use node-specific names.

### 6.3 SQLite configuration

The SQLite backend should configure:

- WAL mode where supported;
- a busy timeout;
- foreign keys enabled;
- synchronous mode appropriate for the selected durability policy;
- one migration transaction at startup;
- bounded connection count for the local process.

The runtime must never delete plugin tables during migration.

## 7. Rust persistence abstractions

The public API should separate capabilities while allowing one backend to
implement them atomically.

### 7.1 Identifiers and revisions

```rust
pub type RunId = uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision(pub u64);
```

Every mutable run record and checkpoint has a revision. Writes require the
expected revision and return a conflict error when it does not match.

### 7.2 Run repository

```rust
#[async_trait]
pub trait RunRepository: Send + Sync {
    async fn create(&self, run: NewRun) -> Result<RunRecord, PersistenceError>;
    async fn get(&self, id: RunId) -> Result<Option<RunRecord>, PersistenceError>;
    async fn update(
        &self,
        id: RunId,
        expected: Revision,
        update: RunUpdate,
    ) -> Result<RunRecord, PersistenceError>;
    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>, PersistenceError>;
    async fn delete(&self, id: RunId) -> Result<(), PersistenceError>;
}
```

`RunUpdate` should support status, error, statistics, timestamps, and result
references without allowing callers to overwrite the run ID or graph identity.

### 7.3 Checkpoint repository

```rust
#[async_trait]
pub trait CheckpointRepository: Send + Sync {
    async fn save(
        &self,
        checkpoint: RunCheckpoint,
        expected: Revision,
    ) -> Result<Revision, PersistenceError>;
    async fn latest(&self, id: RunId) -> Result<Option<RunCheckpoint>, PersistenceError>;
    async fn get(
        &self,
        id: RunId,
        revision: Revision,
    ) -> Result<Option<RunCheckpoint>, PersistenceError>;
    async fn delete(&self, id: RunId) -> Result<(), PersistenceError>;
}
```

Checkpoints are immutable records. `latest()` returns the most recent complete
checkpoint, never a partially written one.

### 7.4 Artifact repository

Artifacts should be persisted only when required by the selected durability
policy. The interface is:

```rust
#[async_trait]
pub trait ArtifactRepository: Send + Sync {
    async fn append(&self, artifact: StoredArtifact) -> Result<(), PersistenceError>;
    async fn get(&self, id: ArtifactId) -> Result<Option<StoredArtifact>, PersistenceError>;
    async fn list_for_run(&self, run: RunId) -> Result<Vec<StoredArtifact>, PersistenceError>;
}
```

The first implementation may store JSON inline. Large payload/object-store
support can be added later through a content-addressed reference.

### 7.5 Combined persistence facade

A backend should expose a combined facade for atomic transitions:

```rust
#[async_trait]
pub trait RunPersistence: Send + Sync {
    type Transaction: Send;

    async fn begin(&self) -> Result<Self::Transaction, PersistenceError>;
    async fn load_run(&self, id: RunId) -> Result<Option<RunRecord>, PersistenceError>;
    async fn load_checkpoint(&self, id: RunId) -> Result<Option<RunCheckpoint>, PersistenceError>;
    async fn commit_checkpoint(
        &self,
        tx: Self::Transaction,
        run: RunUpdate,
        checkpoint: RunCheckpoint,
    ) -> Result<(), PersistenceError>;
}
```

The exact Rust transaction type may remain backend-specific. The invariant is
that lifecycle status and checkpoint revision advance together.

### 7.6 Compatibility adapter

Keep the current API through an adapter:

```rust
pub struct FileRunStore { ... }
```

It can implement `RunRepository` and continue to expose its current simple
`save/get/delete` methods temporarily. New code should use `RunRepository` or
`RunPersistence` rather than writing JSON records directly.

## 8. Runtime data model

### 8.1 Run record

```rust
pub struct RunRecord {
    pub id: RunId,
    pub graph_path: String,
    pub graph_hash: String,
    pub config_hash: String,
    pub goal: serde_json::Value,
    pub status: RunStatus,
    pub revision: Revision,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub stats: RunStats,
    pub error: Option<RunError>,
    pub metadata: serde_json::Value,
}
```

Statuses:

```rust
pub enum RunStatus {
    Created,
    Running,
    Pausing,
    Paused,
    Completing,
    Completed,
    Failed,
    Cancelled,
}
```

`Pausing` and `Completing` make transitions observable and prevent a second
request from incorrectly starting a concurrent operation.

### 8.2 Checkpoint

A checkpoint must represent a scheduler boundary, not an in-flight task:

```rust
pub struct RunCheckpoint {
    pub run_id: RunId,
    pub revision: Revision,
    pub graph_hash: String,
    pub config_hash: String,
    pub round: u32,
    pub current_round_pending: u64,
    pub pending_inputs: Vec<PendingInput>,
    pub ready_activations: Vec<ReadyActivation>,
    pub stats: RunStats,
    pub reason: CheckpointReason,
}
```

```rust
pub struct PendingInput {
    pub node_id: String,
    pub round: u32,
    pub port: String,
    pub artifact: Artifact,
}

pub struct ReadyActivation {
    pub node_id: String,
    pub round: u32,
    pub inputs: Vec<PortMsg>,
}
```

The persisted representation must be deterministic and versioned. Runtime
implementation details such as Tokio task IDs must never be persisted.

### 8.3 Stored outputs

On completion, the runtime stores terminal outputs:

```rust
pub struct RunOutput {
    pub run_id: RunId,
    pub node_id: String,
    pub port: String,
    pub artifact: Artifact,
    pub round: u32,
}
```

The default policy stores sink outputs and may optionally store all activation
outputs for debugging/tracing.

## 9. Scheduler changes

### 9.1 Extract serializable scheduler state

The scheduler currently keeps input buffers, pending counts, current round, and
activation results in local variables inside `Scheduler::run`. Refactor these
into an explicit internal state object:

```rust
struct SchedulerState {
    current_round: u32,
    round_pending: HashMap<u32, usize>,
    input_buffer: HashMap<(String, u32), HashMap<String, Artifact>>,
    ready_activations: VecDeque<Activation>,
    stats: RunStats,
}
```

Only the serializable portions are checkpointed. `JoinSet`, channels, semaphores,
and cancellation tokens remain runtime-only.

### 9.2 Checkpoint boundaries

A checkpoint may be written:

- before the first activation;
- after a complete round;
- after a clean pause;
- after cancellation, if all in-flight tasks have stopped;
- before and after human artifact injection;
- at completion or failure.

The first implementation should checkpoint after each completed round and on
pause. Per-activation checkpointing can be configurable for interactive flows.

### 9.3 Pause semantics

Pause is a state transition, not merely a signal:

1. Transition `Running → Pausing` using compare-and-swap.
2. Stop dispatching new activations.
3. Cancel/await in-flight activations.
4. Capture remaining input buffers and ready activations.
5. Write a complete checkpoint and transition `Pausing → Paused`.
6. Emit `RunPaused`.

If an activation cannot be stopped safely, the run becomes `Failed` or
`Cancelled` according to the configured policy; it must not be marked `Paused`
without a complete checkpoint.

### 9.4 Resume semantics

Resume must:

1. Load the run and latest checkpoint.
2. Verify graph/config hashes.
3. CAS `Paused → Running`.
4. Reconstruct the scheduler state.
5. Rebuild nodes and runtime handles.
6. Continue from ready activations and buffered inputs.
7. Persist the next checkpoint/status transition.

A stale revision or graph/config mismatch returns a structured error.

### 9.5 Human input

Human input is an artifact injection operation:

```rust
pub struct InputRequest {
    pub node_id: String,
    pub port: String,
    pub artifact: Artifact,
}
```

The runtime verifies that the node and port exist and that artifact kind matches
the port declaration before adding it to the checkpoint. A paused run may then
be resumed explicitly or automatically according to application policy.

## 10. Database access contract for plugins and tools

### 10.1 Control nodes

Keep the existing variables, now assembled by `RunEnvironment`:

```text
EUREKA_DB_PATH
EUREKA_SESSION_ID
EUREKA_NODE_ID
EUREKA_ROUND
EUREKA_CONFIG
```

Add:

```text
EUREKA_DB_SCHEMA_VERSION
EUREKA_DB_NAMESPACE
```

`EUREKA_DB_PATH` refers to the shared per-run database. Plugins must use their
own table namespace and must not modify `eureka_*` tables.

### 10.2 Agent tools

Agent shell tools now receive the same run-scoped environment values as control
nodes, including `EUREKA_DB_PATH`. This makes a tool's capabilities consistent
regardless of whether it is invoked by a control node or an LLM.

The tool environment is explicit and documented. It does not silently inherit
all of the Rust process environment. The environment-aware `LlmClient` method
retains a compatibility default for custom clients; the built-in `RigClient`
passes the environment to `CommandTool`.

### 10.3 Rust nodes

The public `NodeCtx` should expose a read-only run context rather than a raw
SQLite connection:

```rust
pub struct RunContext {
    pub run_id: RunId,
    pub round: u32,
    pub db_path: Option<PathBuf>,
    pub metadata: serde_json::Value,
}
```

A database handle should be injected through a separate capability trait only
for nodes that opt in. Nodes must not be able to bypass run transactions or
write scheduler tables directly.

## 11. Runtime SQLite schema

The initial migration should create tables similar to:

```sql
CREATE TABLE eureka_runs (
    id TEXT PRIMARY KEY,
    graph_path TEXT NOT NULL,
    graph_hash TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    goal_json TEXT NOT NULL,
    status TEXT NOT NULL,
    revision INTEGER NOT NULL,
    stats_json TEXT NOT NULL,
    error_json TEXT,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);

CREATE TABLE eureka_checkpoints (
    run_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    round INTEGER NOT NULL,
    graph_hash TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    state_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (run_id, revision),
    FOREIGN KEY (run_id) REFERENCES eureka_runs(id)
);

CREATE TABLE eureka_run_outputs (
    run_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    port TEXT NOT NULL,
    round INTEGER NOT NULL,
    artifact_json TEXT NOT NULL,
    PRIMARY KEY (run_id, sequence),
    FOREIGN KEY (run_id) REFERENCES eureka_runs(id)
);

CREATE TABLE eureka_run_metadata (
    run_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    PRIMARY KEY (run_id, key),
    FOREIGN KEY (run_id) REFERENCES eureka_runs(id)
);
```

The exact schema may change during implementation, but these invariants are
required:

- all runtime tables are prefixed `eureka_`;
- checkpoint rows are immutable;
- revisions are monotonically increasing per run;
- a checkpoint references the graph/config hash used to create it;
- status/checkpoint transitions are atomic;
- deleting a run either cascades deliberately or is rejected while active.

## 12. Backend plan

### Phase 1: local and test backends

- `InMemoryRunPersistence` for unit tests.
- `SqliteRunPersistence` for the CLI and local development.
- `FileRunStore` compatibility adapter.

### Phase 2: production backend

- `PostgresRunPersistence` with the same repository/facade traits.
- Optimistic locking through the revision column.
- Transactional status/checkpoint/output writes.

The graph runtime should not depend on PostgreSQL-specific types. Backend errors
must map to a common `PersistenceError`.

## 13. Run manager and HTTP API

Add a high-level service around session construction and persistence:

```rust
pub struct RunManager<P> {
    persistence: Arc<P>,
    ...
}
```

Required operations:

```rust
create_run(request)
get_run(id)
list_runs(filter)
pause_run(id)
resume_run(id)
submit_input(id, input)
cancel_run(id, reason)
get_outputs(id)
delete_run(id)
```

Recommended HTTP endpoints:

```text
POST   /runs
GET    /runs
GET    /runs/{id}
GET    /runs/{id}/outputs
GET    /runs/{id}/events
POST   /runs/{id}/pause
POST   /runs/{id}/resume
POST   /runs/{id}/input
POST   /runs/{id}/cancel
DELETE /runs/{id}
```

The existing `/api/run`, `/api/state`, and `/api/events` endpoints remain
compatible and can be implemented as a UI-oriented projection of this API.

## 14. Concurrency and correctness

The same run must not be executed concurrently by two managers.

Required protections:

- in-process per-run mutex for the local manager;
- revision compare-and-swap in persistence;
- `Running`, `Pausing`, `Paused`, and terminal statuses validated as a state
  machine;
- no resume from `Completed`, `Failed`, or `Cancelled` without an explicit
  restart operation;
- no input injection into a non-paused run unless explicitly allowed;
- no checkpoint restoration against a changed graph/config hash.

The runtime must define whether a failed node's side effects are retried. The
initial policy should be at-least-once activation semantics, with idempotency
expected from plugins and application nodes.

## 15. Migration and compatibility

### 15.1 Existing control databases

On first opening an existing `<run_id>.sqlite`:

- create runtime `eureka_*` tables if absent;
- leave all non-`eureka_` tables untouched;
- do not rename or drop existing example tables;
- expose the runtime schema version to plugins;
- keep `EUREKA_DB_PATH` unchanged.

### 15.2 Existing JSON run records

If a JSON run record exists but no SQLite `eureka_runs` row exists, import it
once and mark the source as a compatibility import. Future writes go to the
SQLite backend.

### 15.3 Existing library callers

- `Session::run(goal)` continues to run without persistence.
- `Session::run_with_store(...)` remains supported during transition.
- New application code should use `RunManager`.

## 16. Observability

Emit events for all durable transitions:

```text
RunCreated
RunStarted
CheckpointSaved
RunPausing
RunPaused
RunResumed
InputAccepted
RunCompleted
RunFailed
RunCancelled
```

Existing activation and cycle events remain unchanged. JSONL traces should
include run transition events and checkpoint revisions so an operator can
correlate durable state with the event stream.

`GET /api/state` remains an ephemeral live snapshot. `GET /api/run` and the
future `/runs/{id}` endpoint read durable state.

## 17. Testing requirements

### Persistence tests

- create/get/update/delete run;
- revision conflict;
- atomic status/checkpoint update;
- interrupted write does not produce a partial checkpoint;
- migration preserves plugin tables;
- SQLite busy/concurrent access behavior;
- in-memory and SQLite backends share behavior.

### Scheduler tests

- checkpoint after a completed round;
- pause with active work;
- pause with buffered multi-input state;
- resume restores round and input buffers;
- human artifact injection resumes a blocked node;
- graph/config hash mismatch is rejected;
- terminal outputs are retrievable after restart;
- node failure persists a failed run;
- cancellation persists partial statistics.

### Protocol tests

- control nodes receive database path and schema version;
- agent tools receive the same run-scoped database environment;
- plugins can create namespaced tables;
- runtime migration does not delete plugin data.

### HTTP tests

- create and poll run;
- pause/resume;
- submit human input;
- conflict response for concurrent resume;
- retrieve final outputs;
- list/filter runs.

## 18. Delivery plan

### Milestone A — cohesive database foundation

1. Add `PersistenceError`, `RunId`, and revisions.
2. Introduce `RunRepository`, `CheckpointRepository`, and `RunPersistence`.
3. Implement `InMemoryRunPersistence`.
4. Implement `SqliteRunPersistence` and runtime migrations.
5. Adapt `FileRunStore` as a compatibility projection.
6. Keep existing plugin `EUREKA_DB_PATH` behavior unchanged.
7. Pass run-scoped environment to agent tools.

### Milestone B — durable scheduler state

1. Extract serializable scheduler state.
2. Add checkpoint serialization.
3. Persist checkpoints after rounds and on clean pause.
4. Add graph/config hashes.
5. Persist terminal outputs and partial stats.

### Milestone C — lifecycle manager

1. Add `RunManager`.
2. Add per-run locking and revision checks.
3. Implement resume from checkpoint.
4. Implement human artifact injection.
5. Implement cancellation and terminal transitions.

### Milestone D — application integration

1. Add complete run HTTP API.
2. Add an interactive workflow example with a human-review control node.
3. Add listing/filtering and output retrieval.
4. Add PostgreSQL backend if operational requirements justify it.

## 19. Acceptance criteria

This specification is complete when all of the following are true:

1. A CLI run creates one SQLite database containing both runtime tables and
   plugin state.
2. Existing Python control nodes continue to read/write their state unchanged.
3. Rust can load a paused run after process restart.
4. Resuming restores pending input buffers and the correct round.
5. A human artifact can be injected into a paused node and execution continues.
6. Concurrent resume/update attempts produce a conflict rather than lost data.
7. Final sink artifacts are retrievable through the persistence API and HTTP.
8. Agent tools and control nodes receive consistent run-scoped DB metadata.
9. A changed graph/config cannot silently consume an old checkpoint.
10. The in-memory backend passes the same lifecycle/checkpoint tests as SQLite.
11. Existing manifests and `Session::run(goal)` callers remain compatible.
12. The event stream and durable records agree on lifecycle transitions.
