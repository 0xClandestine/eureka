# HTTP run and session integration

Eureka runs are autonomous by default, but applications can expose the same
lifecycle through HTTP. The runtime provides two pieces for this pattern:

- `FileRunStore` / `RunStore` persist the run lifecycle as JSON.
- `start_server_with_run_store` exposes the current record at `GET /api/run`.

The graph remains declarative; the HTTP layer owns authentication, request
validation, and any application-specific persistence.

## Starting a run

```rust,no_run
use std::sync::Arc;
use eureka::{FileRunStore, RunStore, Session};
use eureka::config::EurekaConfig;

let config = EurekaConfig::load(Some("eureka.toml".as_ref()))?;
let run_id = uuid::Uuid::now_v7();
let sessions_dir = std::path::Path::new(".eureka/sessions");
let store: Arc<dyn RunStore> = Arc::new(FileRunStore::new(sessions_dir));
let mut session = Session::new(config.clone(), &run_id.to_string(), None)?;

let goal = serde_json::json!({
    "goal": "Find promising catalysts for CO2 reduction",
    "domain": "chemistry"
});

// This writes a `running` record before the first node is activated and a
// `completed`, `paused`, or `failed` record when the scheduler returns.
let stats = session.run_with_store(goal, Some(store.as_ref())).await?;
println!("completed {} rounds", stats.rounds_completed);
```

For a web service, create the store and session once per request or keep them
in application state. The store uses an atomic temporary-file rename, so a
reader sees either the previous complete record or the new complete record.

## Polling status

When the CLI UI server is started with a run store, clients can poll:

```text
GET /api/run
```

Example response:

```json
{
  "id": "019...",
  "graph": "example/coscientist.yml",
  "goal": { "goal": "Find promising catalysts for CO2 reduction" },
  "status": "completed",
  "stats": {
    "total_cost_usd": 0.42,
    "total_tokens": 12000,
    "total_input_tokens": 9000,
    "total_output_tokens": 3000,
    "elapsed_secs": 18.4,
    "rounds_completed": 4
  },
  "error": null
}
```

`GET /api/state` remains the low-latency in-memory view for active nodes and
latest outputs. `GET /api/run` is the durable lifecycle metadata view and
remains available after the UI process restarts. It does **not** yet restore
scheduler queues or resume a graph from a checkpoint; true durable recovery is
specified in [`db-session-spec.md`](db-session-spec.md) but is not implemented.

## Database and trace files

The CLI co-locates per-run files under `.eureka/sessions/`:

```text
<run_id>.sqlite        # control-node/plugin state
<run_id>.json          # lifecycle metadata compatibility record
<run_id>.traces.jsonl  # optional scheduler event trace
```

Control nodes and agent shell tools receive the run-scoped environment
contract, including `EUREKA_DB_PATH`, `EUREKA_DB_SCHEMA_VERSION`, and
`EUREKA_DB_NAMESPACE`. See [`db-session-spec.md`](db-session-spec.md) for the
planned runtime-owned database/checkpoint abstraction and
[`tracing-spec.md`](tracing-spec.md) for JSONL tracing configuration.

## Pause and resume signals

The scheduler exposes `SchedulerSignal::Pause` and `SchedulerSignal::Resume`.
A pause emits `RunPaused`, stops in-flight work, and records the run as
`paused`. The current `Resume` signal is an observability/lifecycle signal; it
does not reconstruct scheduler state. Applications should not advertise true
restart recovery yet. Checkpoint-backed resume is specified in
[`db-session-spec.md`](db-session-spec.md).

For production services, serialize requests for the same run ID. The file
store prevents torn writes, but it does not provide optimistic locking between
concurrent application requests.
