# Spec: Run Orchestration

> **Status:** Stable
> **Crate:** `eureka-engine`
> **Files:** `run.rs`

## Purpose

`run.rs` provides thin convenience functions that wrap `Session` for the two common
invocation patterns: a single research run (`run_single`) and a sequential batch of
runs with optional per-run config overrides (`run_batch`). Both return typed result
structs that bundle the session ID and final `RunStats`.

## Design

### Types

```rust
/// Configuration for a single run within a batch.
pub struct RunConfig {
    /// The research goal as a JSON value.
    pub goal: serde_json::Value,
    /// If set, overrides the shared base config for this run only.
    pub config_overrides: Option<EurekaConfig>,
}

/// The result of a completed research run.
pub struct RunResult {
    /// The UUIDv7 session identifier.
    pub session_id: uuid::Uuid,
    /// Final run statistics returned by the scheduler.
    pub stats: RunStats,
}
```

`RunStats` is defined in `eureka_graph::control`. It is not re-exported from `eureka-engine`; callers that need it should import it directly from `eureka_graph::control`.

### Functions

```rust
/// Execute a single research run.
///
/// Creates a Session via Session::new, runs it with the provided goal,
/// and returns a RunResult containing the session ID and RunStats.
pub async fn run_single(
    config:   EurekaConfig,
    registry: NodeRegistry,
    goal:     serde_json::Value,
) -> Result<RunResult, EngineError>;

/// Execute multiple research runs sequentially.
///
/// Each run uses its own config_overrides if provided, falling back to
/// base_config. Runs execute one at a time; the function returns early
/// with Err on the first failure.
pub async fn run_batch(
    base_config: EurekaConfig,
    registry:    NodeRegistry,
    runs:        Vec<RunConfig>,
) -> Result<Vec<RunResult>, EngineError>;
```

### Behaviour

- `run_single` calls `Session::new(config, registry)` (which loads and validates the
  graph), then `session.run(goal)`, then assembles `RunResult` from
  `session.session_id()` and the returned `RunStats`.
- `run_batch` iterates `runs` in order. For each entry, it selects
  `run_config.config_overrides.unwrap_or_else(|| base_config.clone())` and delegates
  to `run_single`. `registry` is cloned for each run.
- Neither function attaches a DB or event broadcaster — that is the caller's
  responsibility when those features are needed (the CLI uses `Session` directly).

## Invariants

- `run_batch` is sequential, not concurrent — runs do not overlap.
- `registry` must be `Clone`; each call to `run_single` from `run_batch` receives its
  own clone.
- `run_batch` stops at the first error; already-completed runs are not rolled back.

## Non-Goals

- `run_batch` does not support parallel execution of runs.
- These helpers do not set up the DB or UI event broadcaster; use `Session` directly
  when those are required.
