# Tracing via JSON Files — Spec

## 1. Motivation

Every Eureka graph run currently produces `SchedulerEvent` entries on an
in-memory `mpsc` channel consumed by two ephemeral sinks:

1. `Session::run` — logs each event via `tracing` and forwards to a
   `broadcast::Sender` (SSE).
2. `track_live_state` — folds events into an in-memory `LiveState` struct
   for `GET /api/state`.

When the process exits, **all event history is lost**. Post-hoc analysis
(cost-per-round breakdown, agent latency histograms, tool-call audit log,
comparison across runs) is impossible without durable traces.

The only durable output today is the `.eureka/sessions/{id}.sqlite` DB
that the **Python control nodes** (supervisor, ranker, proximity) manage
for their own cross-round state — opaque to the Rust scheduler.

This spec adds **opt-in, file-based durable tracing** that captures every
`SchedulerEvent` as a JSONL file co-located with the session DB.

### Non-goals
- Checkpoint / resume (this spec does not persist graph state for restart).
- Replace the SSE endpoint or `LiveState`.
- Structured (OTEL) tracing spans — this is **event-level** only.

---

## 2. Design

### 2.1 Trace file layout

```
<graph_dir>/
  .eureka/
    sessions/
      {session_id}.sqlite        # existing control-node DB
      {session_id}.traces.jsonl  # NEW — scheduler event log
```

Every run gets exactly one trace file. If `EUREKA_TRACING` is enabled
and the `.eureka/sessions/` directory is writable, the file is opened at
session start and flushed on each event. If the directory cannot be
created, tracing is silently disabled (same fallback as the SQLite DB).

### 2.2 File format

**JSONL** — one JSON object per line, `\n` terminated. Each line is a
`SchedulerEvent` serialized with `serde` using the existing
`#[serde(tag = "type", rename_all = "camelCase")]` representation.

A **header line** (object with `"type": "traceHeader"`) is written first:

```jsonl
{"type":"traceHeader","sessionId":"01938a1f-...","graph":"example/coscientist.yml","startedAt":"2025-07-01T12:00:00.000Z","config":{"provider":"openrouter","model":"deepseek/deepseek-v4-flash","maxInFlight":8,"budget":{"maxCostUsd":25,"maxTokens":5000000,"maxWallclock":2700,"maxRounds":12}}}
{"type":"activationStarted","nodeId":"generation","nodeKind":"generation","round":0}
{"type":"toolCalled","nodeId":"generation","nodeKind":"generation","round":0,"tool":"arxiv_search","argsSummary":"search: \"transformer architecture\" max_results: 5"}
```

### 2.3 When the file is written

| When | Write |
|---|---|
| Session start | Header line (and flush) |
| Every `SchedulerEvent` | One line (buffered; flush on every event for crash safety) |
| Run completes (including budget halt / error) | Footer line with `RunStats` summary, then flush + sync |
| Cancel / drop | Best-effort footer line + flush |

**Crash safety**: flush after every event. JSONL is inherently
recoverable even on partial writes — each object is a complete line.

### 2.4 Tracing deduplication

The `SchedulerEvent` stream is already fanned out to the SSE broadcaster
in `session.rs`. The trace writer reads from the **same** `mpsc::Receiver`
that the event-logging task consumes. No new channel needed.

---

## 3. Configuration

### 3.1 New `[tracing]` section in `EurekaConfig`

```toml
[tracing]
# Write scheduler events to .eureka/sessions/{session_id}.traces.jsonl
enabled = false

# Maximum file size in bytes before rotation (0 = no rotation).
# When exceeded, the current file is renamed to .traces.jsonl.{N} and
# a new file is opened.
max_file_bytes = 0   # no rotation by default

# Whether to include artifact payloads in ActivationCompleted outputs.
# Artifacts can be large (lists of hypotheses, full paper bodies, etc.)
# and may double or triple the trace file size.
include_artifacts = true
```

### 3.2 `DEFAULT_TOML` default

```toml
[tracing]
enabled = false
max_file_bytes = 0
include_artifacts = true
```

### 3.3 Environment variable overrides

| Env var | Maps to |
|---|---|
| `EUREKA_TRACING_ENABLED` | `tracing.enabled` (`true` / `false`) |
| `EUREKA_TRACING_MAX_FILE_BYTES` | `tracing.max_file_bytes` |
| `EUREKA_TRACING_INCLUDE_ARTIFACTS` | `tracing.include_artifacts` |

### 3.4 CLI override (future)

The `run` subcommand can accept `--trace` / `--no-trace` as a convenience
override. Out of scope for initial implementation.

---

## 4. Implementation plan

### 4.1 New module: `crates/eureka/src/tracing/mod.rs`

A self-contained module with one public struct:

```rust
/// Writes SchedulerEvent entries to a JSONL file.
pub struct JsonlTraceWriter {
    path: PathBuf,
    writer: BufWriter<File>,
    include_artifacts: bool,
}

impl JsonlTraceWriter {
    /// Create and open the trace file. Writes the header line immediately.
    /// Returns `Ok(None)` if the file cannot be created (disk full, permission
    /// denied, etc.) — tracing is best-effort and must never fail a run.
    pub fn open(
        sessions_dir: &Path,
        session_id: &Uuid,
        config: &TracingConfig,
        graph_path: &str,
        provider: &str,
        model: &str,
        budget: &Budget,
        max_in_flight: usize,
    ) -> Result<Option<Self>>;

    /// Append one event as a JSON line. Flushes after every write.
    ///
    /// On I/O error, logs a warning and drops the event. The trace is
    /// corrupted-possible on disk but the run continues.
    pub fn write_event(&mut self, event: &SchedulerEvent);

    /// Write a footer with final `RunStats`, then flush + fsync.
    pub fn close(self, stats: &RunStats) -> Result<()>;
}
```

**Key invariants:**
- `open` returns `Ok(None)` on any I/O error — never `Err`.
- `write_event` catches I/O errors internally and logs via `tracing::warn!`.
- The `BufWriter` is **line-buffered** (flush after every `writeln!`).
- `close` performs an `fsync` for durability.

### 4.2 New config type: `TracingConfig`

Added to `crates/eureka/src/config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub max_file_bytes: u64,
    #[serde(default = "default_true")]
    pub include_artifacts: bool,
}

fn default_true() -> bool { true }

impl Default for TracingConfig {
    fn default() -> Self {
        Self { enabled: false, max_file_bytes: 0, include_artifacts: true }
    }
}
```

`EurekaConfig` gains a `tracing: TracingConfig` field.

### 4.3 Wire up in `Session::run`

In `session.rs`, the event-pumping task currently receives every
`SchedulerEvent` from `scheduler.event_receiver()` and:

1. Forwards to `broadcast` (SSE)
2. Logs via `tracing::info!`

Add a third step:

```rust
// In Session::run(), after creating `scheduler` and `events`:

let trace_writer = if self.config.tracing.enabled {
    let sessions_dir = self.graph_dir.join(".eureka").join("sessions");
    let provider = self.config.provider.kind.to_string();
    let model = self
        .config
        .provider
        .generation_model
        .clone()
        .unwrap_or_default();
    JsonlTraceWriter::open(
        &sessions_dir,
        &self.session_id,
        &self.config.tracing,
        &self.config.graph,
        &provider,
        &model,
        &self.config.budget,
        self.config.scheduler.max_in_flight,
    ).unwrap_or_else(|e| {
        tracing::warn!(error=%e, "Failed to open trace file; tracing disabled");
        None
    })
} else {
    None
};

let trace_writer = Arc::new(Mutex::new(trace_writer));

// In the event processing loop:
let tw = Arc::clone(&trace_writer);
tokio::spawn(async move {
    while let Some(event) = events.recv().await {
        // existing: broadcast, log
        // NEW:
        if let Some(ref mut w) = *tw.lock().await {
            w.write_event(&event);
        }
    }
});
```

**Alternatively (simpler):** make the event-handling task own the trace
writer directly (no Arc/Mutex). The task already runs sequentially in
its own spawn. Pass `trace_writer` into the inner loop.

### 4.4 Artifact filtering

When `include_artifacts = false`, `write_event` strips the `outputs` field
from `ActivationCompleted` events:

```rust
SchedulerEvent::ActivationCompleted { ref outputs, .. } => {
    let evt = if self.include_artifacts { event.clone() } else {
        let mut e = event.clone();
        if let SchedulerEvent::ActivationCompleted { ref mut outputs, .. } = e {
            *outputs = vec![];
        }
        e
    };
    writeln!(self.writer, "{}", serde_json::to_string(&evt)?)?;
}
```

### 4.5 Footer on completion

Before the event handle is aborted (`event_handle.abort()` in `Session::run`),
close the trace writer:

```rust
if let Some(mut writer) = trace_writer.take() {
    let _ = writer.close(&stats);
}
```

If the run halts via budget exhaustion, the `RunHalted` event is written
normally, then the footer is appended when the scheduler loop exits.

---

## 5. Edge cases & error handling

| Scenario | Behavior |
|---|---|
| Disk full at open | `open` returns `Ok(None)`; tracing disabled for run |
| Disk full mid-run | `write_event` logs a warning, drops the event; run continues. Subsequent writes likely also fail — consider a `stalled` flag to stop trying |
| Permission denied | Same as disk full |
| Process killed (SIGKILL) | Last line may be truncated. JSONL is recoverable — reader skips incomplete final line |
| Process killed during `close` | Best-effort; footer may be missing. The `RunHalted` event (written before close) plus `RunStats` in the events provide enough data |
| `include_artifacts = false` | Only `ActivationCompleted.outputs` is cleared. All other fields remain |
| File rotation | Deferred to v2. The `max_file_bytes` field is reserved in the config struct for future use |
| Session directory doesn't exist | `create_dir_all` attempted; if it fails, tracing is silently disabled (same as DB) |

---

## 6. File changes summary

| File | Change |
|---|---|
| `crates/eureka/src/config.rs` | Add `TracingConfig` struct and `tracing` field to `EurekaConfig`; update `DEFAULT_TOML` |
| `crates/eureka/src/tracing/mod.rs` | **New file** — `JsonlTraceWriter` |
| `crates/eureka/src/tracing.rs` | **New file** — `pub mod tracing;` re-export |
| `crates/eureka/src/lib.rs` | Add `pub mod tracing;` |
| `crates/eureka/src/session.rs` | Open trace writer in `run()`, wire into event loop, close on completion |
| `eureka.toml` | Add commented `[tracing]` section |

---

## 7. Tests

### Unit tests (`tracing/mod.rs`)

1. **`test_open_creates_file`** — opens writer, verifies `.traces.jsonl` exists
2. **`test_open_writes_header`** — reads first line, asserts
   `"type":"traceHeader"` with correct fields
3. **`test_write_event`** — writes an `ActivationStarted`, reads back,
   round-trips through `serde_json`
4. **`test_include_artifacts_false`** — `ActivationCompleted` with outputs is
   written with `"outputs":[]`
5. **`test_close_writes_footer`** — calls close, reads last line,
   asserts `"type":"runCompleted"` with stats fields
6. **`test_open_nonexistent_dir`** — returns `Ok(None)`, no panic
7. **`test_open_readonly_dir`** — returns `Ok(None)`, no panic

### Integration test (`eureka-cli/tests/`)

8. **`test_run_with_tracing`** — end-to-end: run a trivial 2-node graph with
   `tracing.enabled = true`, assert `.traces.jsonl` exists, has header +
   events + footer, `ActivationCompleted` contains outputs

---

## 8. Future extensions (out of scope)

- **File rotation** using `max_file_bytes`
- **`eureka trace` subcommand** — replay / visualize a trace file
- **OTLP / OpenTelemetry** — structured span export
- **Checkpoint / resume** from a trace
- **Gzip compression** of old trace files