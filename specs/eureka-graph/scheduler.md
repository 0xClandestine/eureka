# Spec: Scheduler

> **Status:** Stable
> **Crate:** `eureka-graph`
> **Files:** `scheduler.rs`, `control.rs`

## Purpose

The `Scheduler` is the event-driven execution engine that routes `Artifact` messages
between nodes along the edges defined by a `GraphSpec`. It manages concurrency,
termination detection, round counting, budget enforcement, and cancellation.

## Motivation

A research graph is not a static pipeline — it contains feedback loops (e.g., the
governor feeds a control signal back to the ranker). A simple sequential executor
cannot handle cycles. The scheduler uses an event queue and a pending-work counter to
drive execution and detect quiescence.

## Design

### Construction

```rust
pub fn Scheduler::new(
    spec: GraphSpec,
    nodes: HashMap<String, BoxedNode>,
    budget: Budget,
    max_in_flight: usize,
) -> Self
```

`max_in_flight` caps simultaneous LLM calls via a `tokio::sync::Semaphore`.

### Startup (`run`)

```rust
pub async fn run(
    &mut self,
    initial_artifacts: HashMap<String, Vec<Artifact>>,
) -> Result<RunStats, SchedulerError>
```

1. The caller passes a map of `node_id → Vec<Artifact>` for initial injection (typically
   a single `Goal` artifact on the generation node).
2. The scheduler builds an outbound routing table: `node_id → Vec<Edge>`.
3. Initial artifacts are enqueued as activations on the first input port of each target
   node (using `node.ports().input_names().first()`).
4. Enters the activation loop.

An initial `Artifact` of kind `Goal` is conventionally injected by the session, but the
scheduler itself does not know about `Goal` — it injects whatever `initial_artifacts`
it is given.

### Activation Loop

```
pending = number of activations enqueued at startup

loop:
  if pending == 0: break

  select:
    cancel token fired → return current stats
    signal received    → handle Cancel/Pause
    activation dequeued:
      pending -= 1
      emit ActivationStarted
      acquire semaphore permit
      result = node.process(ctx, msg)
      release permit
      if Ok(emits):
        emit ActivationCompleted
        for each Emit:
          route_emission → enqueue downstream activations
          pending += enqueued_count
          if crossed_feedback_edge: rounds_completed += 1; emit CycleCompleted
      if Err:
        emit ActivationFailed
        continue (graceful degradation — run is not aborted)
    100ms timer tick:
      check budget; if exhausted, emit RunHalted, trigger cancel
```

The activation channel has capacity 256. The semaphore permit is held only for the
duration of the `node.process()` call.

### Termination Detection

Termination is detected by a `pending: usize` counter (not channel closure). The counter
starts at the number of initial activations enqueued. When an activation is dequeued,
`pending` is decremented immediately (before processing). When downstream activations
are enqueued, `pending` is incremented by the number of new activations. When `pending`
reaches 0 at the top of the loop, the run is complete.

This avoids the "last message in flight" race that channel-close detection suffers.

### Round Counter

The round number is incremented each time the scheduler routes an artifact across an
edge marked `feedback: true`. This corresponds to one complete cycle through the graph
(e.g., governor → ranker → ... → governor).

### Signal Channel

The scheduler exposes `signal_sender() -> mpsc::Sender<SchedulerSignal>`. The sender
is stored in the scheduler struct so it is not dropped before the run completes.
External code (e.g., the UI server or a budget watcher) can send:

- `SchedulerSignal::Cancel` — triggers the cancellation token, ending the run.
- `SchedulerSignal::Pause` — logged but does not currently block execution (reserved).

There is no `Resume` signal variant.

`scheduler.cancellation_token() -> CancellationToken` exposes the same token for
callers that want to wait on cancellation independently.

### Control Signals

```rust
pub enum ControlSignal {
    Continue { round: u32 },
    Halt     { reason: String },
    Pause,
}
```

`ControlSignal` is the payload that control nodes (e.g., `GovernorNode`) place inside
an `Artifact` and emit through feedback edges. The scheduler does not interpret
`ControlSignal` — it routes by artifact kind string only. Control nodes receive a
`ControlSignal` artifact, inspect it, and decide whether to emit on their `continue`
or `halt` output ports.

`ControlSignal` implements `Display`: `"Continue(round=N)"`, `"Halt(<reason>)"`, `"Pause"`.

### Budget Enforcement

```rust
pub struct Budget {
    pub max_cost_usd:       f64,   // default 25.0
    pub max_tokens:         u64,   // default 5_000_000
    pub max_wallclock_secs: f64,   // default 2700.0 (45 min)
    pub max_rounds:         u32,   // default 12
}
```

`Budget` has no `convergence_epsilon` or `convergence_window` fields — those do not
exist in the code.

Budget is checked on a **100 ms timer interval** (not after each activation). When any
limit is exceeded, `RunHalted` is emitted, and the cancellation token is triggered.
The graph's governor plugin is the primary round controller; `max_rounds` in `Budget`
acts as a hard backstop in case the governor fails.

`RunStats::is_budget_exhausted(budget) -> Option<String>` returns `Some(reason)` when
any of the four limits is exceeded, `None` otherwise.

```rust
pub struct RunStats {
    pub total_cost_usd:    f64,
    pub total_tokens:      u64,
    pub elapsed_secs:      f64,
    pub rounds_completed:  u32,
}
```

`RunStats` is the value returned by `Scheduler::run()` on successful completion.
`elapsed_secs` is updated at each budget check tick and once more just before returning.

### Events

```rust
pub enum SchedulerEvent {
    ActivationStarted   { node_id: String, node_kind: String, round: u32 },
    ActivationCompleted {
        node_id: String, node_kind: String, round: u32,
        emit_count: usize,
        outputs: Vec<serde_json::Value>,  // [{port, kind, data}, ...]
    },
    ActivationFailed    { node_id: String, node_kind: String, round: u32, error: String },
    CycleCompleted      { round: u32 },
    RunHalted           { reason: String, total_rounds: u32 },
}
```

All variants are `#[serde(tag = "type", rename_all = "camelCase")]`.
`ActivationCompleted.outputs` is a `Vec<serde_json::Value>`, each element
`{ "port": "...", "kind": "...", "data": { ... } }`.

Events are sent on a **`mpsc` channel** (capacity 1024), not a broadcast channel.
The receiver is obtained via `scheduler.event_receiver() -> mpsc::Receiver<SchedulerEvent>`,
which takes the `Option<Receiver>` out of the scheduler (can only be called once).
The UI server consumes this channel to produce the SSE stream.

### SchedulerError

```rust
pub enum SchedulerError {
    NodeNotFound(String),
    Internal(String),
    Cancelled,
}
```

`run()` returns `Err(SchedulerError::NodeNotFound)` if an activation targets a node ID
not present in the `nodes` map. `Internal` covers channel send failures.
`Cancelled` is defined but currently not returned — cancellation returns `Ok(stats)`.

## Invariants

- `pending` never goes negative (decremented before dispatch, incremented on enqueue).
- The round counter only increments when `route_emission` traverses a `feedback: true` edge.
- `signal_sender()` is stored in the `Scheduler` struct so it is held alive for the entire run.
- `event_receiver()` may only be called once per `Scheduler` instance; the second call returns an immediately-closed channel.
- A failed node activation does not abort the run — the error is emitted as `ActivationFailed` and execution continues.
- The `Budget` struct has no convergence fields. Convergence detection, if any, is the responsibility of the governor plugin.

## Non-Goals

- The scheduler does not interpret artifact content — only kind strings for routing.
- The scheduler does not retry failed activations.
- Priority ordering of activations is not guaranteed (FIFO within concurrent tasks).

## Open Questions

- Should parallel activations of the same node be serialized or allowed?
- Should `CycleCompleted` fire once per governor emit, or once per round increment?
