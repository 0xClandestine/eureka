# Spec: Control Types

> **Status:** Stable
> **Crate:** `eureka-graph`
> **Files:** `control.rs`

## Purpose

Defines the types that govern run lifecycle: the `ControlSignal` enum that flows through
feedback edges, the `Budget` struct that sets hard resource limits, and the `RunStats`
struct that tracks accumulated usage.

## Design

### ControlSignal

```rust
pub enum ControlSignal {
    Continue { round: u32 },
    Halt     { reason: String },
    Pause,
}

impl Display for ControlSignal { /* "Continue(round=N)", "Halt(reason)", "Pause" */ }
```

`ControlSignal` is the artifact kind that flows on feedback edges from a governor node.
Governor plugins emit JSON-encoded `ControlSignal` values; the scheduler routes them
back into the cycle or triggers halt logic.

### Budget

```rust
pub struct Budget {
    pub max_cost_usd:       f64,   // default 25.0
    pub max_tokens:         u64,   // default 5_000_000
    pub max_wallclock_secs: f64,   // default 2700.0 (45 minutes)
    pub max_rounds:         u32,   // default 12
}

impl Default for Budget { ... }
```

`Budget` is constructed by the session from `EurekaConfig.budget` and passed into
`Scheduler::new`. The defaults are conservative values suitable for a single research
run.

There are no `convergence_epsilon` or `convergence_window` fields — convergence
detection is the responsibility of the governor plugin.

### RunStats

```rust
#[derive(Default)]
pub struct RunStats {
    pub total_cost_usd:   f64,
    pub total_tokens:     u64,
    pub elapsed_secs:     f64,
    pub rounds_completed: u32,
}

impl RunStats {
    pub fn is_budget_exhausted(&self, budget: &Budget) -> Option<String>;
}
```

`is_budget_exhausted` checks the four limits in order (cost → tokens → wall-clock →
rounds). Returns `Some(human_readable_reason)` on the first exceeded limit, `None` if
all limits are within bounds. The scheduler calls this on a 100 ms timer tick.

`RunStats` is returned from `Scheduler::run()` at the end of the run with final values.

## Invariants

- `Budget::default()` values are suitable for a production run; callers should override
  them from `EurekaConfig` to respect user-configured limits.
- `total_cost_usd` and `total_tokens` remain at zero until the session wires up a usage
  tracker — `RigClient` does not yet populate them.
- `rounds_completed` is incremented by the scheduler each time an artifact crosses a
  `feedback: true` edge.

## Non-Goals

- `ControlSignal` does not encode which node should handle it — routing is done by the
  graph's edge definitions.
- `Budget` does not enforce limits itself — it is a data type checked by `RunStats::is_budget_exhausted`.
