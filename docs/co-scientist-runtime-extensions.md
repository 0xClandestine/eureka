# Co-Scientist Runtime Extensions

Document outlining the Rust changes required to bring the Eureka runtime
into conformance with the AI co-scientist architecture described in
[`example/COSCIENTIST.md`](../example/COSCIENTIST.md).

---

## Table of Contents

1. [Overview](#overview)
2. [Current Limitations](#current-limitations)
3. [Tier 1: Artifact Queues per Input Port](#tier-1-artifact-queues-per-input-port)
4. [Tier 2: Scatter–Gather Map Node](#tier-2-scattergather-map-node)
5. [Tier 3: Dynamic Edge Routing](#tier-3-dynamic-edge-routing)
6. [Tier 4: Per-Agent Resource Limits](#tier-4-per-agent-resource-limits)
7. [Implementation Order and Dependencies](#implementation-order-and-dependencies)
8. [File Inventory](#file-inventory)

---

## Overview

The current Eureka runtime executes a **static, dataflow-driven graph**: every
node fires exactly when its required input ports are populated, routing follows
a fixed set of YAML edges, and there is one global concurrency cap.

The AI co-scientist paper describes a **dynamic, task-queue-driven
architecture**: the Supervisor agent decides which specialised agents to
schedule, how many workers to allocate, and when to terminate.  Agents run
concurrently over individual hypotheses, not over monolithic batch artifacts.
The engine should provide generic primitives for this — not hardcode a
specific agent role.

Bringing the runtime to that model requires four layered changes.

---

## Current Limitations

### 1. Single artifact per port per round

`crates/eureka/src/scheduler/scheduler.rs` buffers inputs as:

```rust
// (node_id, round) → { port_name → Artifact }
let mut input_buffer: HashMap<(String, u32), HashMap<String, Artifact>> = HashMap::new();
```

Each port holds **one** artifact. When two sources emit to the same
`(node, round, port)`, the later arrival silently overwrites the earlier one.
This prevents:

- multiple replica agents from fanning into one downstream node
- injecting several expert artifacts onto one port in one round
- accumulating multi-source input

### 2. No fan-out primitive

There is no `replicas: N` or `scatter: hypotheses` declaration.  To run four
Generation agents in parallel the user must manually copy-paste four agent
blocks, four tool definitions, and four edge declarations into the YAML.

### 3. No dynamic scheduling

Every edge is prescriptive.  The Supervisor control node can only emit
`continue` or `halt` along a fixed feedback edge — it cannot decide to run
Generation twice, skip Proximity when the frontier is diverse, or re-inject
the original Goal in later rounds.

### 4. No per-agent concurrency control

```rust
in_flight_sem: Arc<Semaphore>,
```

One global `max_in_flight` cap.  A slow agent (e.g. Reflection with
`max_iterations=12`) competes for the same pool as a fast agent (e.g.
SafetyReview with no tools).  There is no way to reserve capacity for critical-
path nodes or to prevent one agent type from starving others.

---

## Tier 1: Artifact Queues per Input Port

**Blocks:** nothing.  
**Unlocks:** scatter/gather, multi-source fan-in, expert injection at scale.

### 1a. Change the input buffer from map to queue

**File:** `crates/eureka/src/scheduler/scheduler.rs`

The inner `HashMap<String, Artifact>` becomes `HashMap<String, VecDeque<Artifact>>`.
The type alias changes from:

```rust
HashMap<(String, u32), HashMap<String, Artifact>>
```

to:

```rust
HashMap<(String, u32), HashMap<String, VecDeque<Artifact>>>
```

`deliver_input` pushes to the back of the deque instead of inserting:

```rust
// Before
bucket.insert(port.to_string(), artifact);

// After
bucket
    .entry(port.to_string())
    .or_default()
    .push_back(artifact);
```

### 1b. Activation modes for input ports

**File:** `crates/eureka/src/graph/port.rs`

Add a new field and enum to `PortDef`:

```rust
/// How arrivals on this port produce activations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub enum PortActivation {
    /// Fire once when all required ports are present (current behaviour).
    #[default]
    Once,
    /// Fire once per artifact arriving on this port, provided all other
    /// required ports are also present.  Each activation drains one
    /// artifact from this port's queue and one from each other required
    /// port's queue (matching their front elements).
    Each,
}

pub struct PortDef {
    pub port: String,
    pub kind: ArtifactKind,
    pub required: Option<bool>,
    /// Activation mode.  Defaults to `Once`.
    #[serde(default)]
    pub activation: Option<PortActivation>,
}
```

In YAML:

```yaml
inputs:
  - port: in
    kind: Hypotheses
    activation: each       # one activation per hypothesis
  - port: context
    kind: Insights
    required: false
    activation: once       # optional; default
```

The YAML keys `items_field` and corresponding `max_parallel` go here too.

### 1c. `deliver_input` semantics for `Each`

When a port is marked `Each`:

1. Push the artifact to its deque.
2. Check whether **all** required ports have at least one item in their deque.
3. If yes, **pop one** artifact from each required deque and dispatch one
   activation.
4. Repeat until any required deque is empty.
5. Return the number of activations dispatched (may be > 1).

This allows a single `deliver_input` call to dispatch multiple activations
when a batch of hypotheses arrives at an `Each` port, bypassing the current
return-at-most-one behaviour.

### 1d. Checkpoint compatibility

**File:** `crates/eureka/src/run.rs`

`PendingInput` currently holds one artifact. Extend it:

```rust
pub struct PendingInput {
    pub node_id: String,
    pub round: u32,
    pub port: String,
    /// Ordered queue of pending artifacts.  Length ≥ 1.
    pub artifacts: Vec<Artifact>,
}
```

The `persist_checkpoint` method serialises the full deque per port. Resume
reconstructs the deque in order.

### 1e. Estimated impact

| Metric | Value |
|---|---|
| Files touched | 6 |
| New lines | ~120 |
| Changed lines | ~40 |
| Breaking change | No (defaults preserve `Once` behaviour) |

---

## Tier 2: Scatter–Gather Map Node

**Blocks:** Tier 1.  
**Unlocks:** parallel generation, parallel reflection, parallel evolution.

### 2a. YAML declaration

**File:** `example/coscientist.yml` (example — implementation in manifest code)

A new top-level key in the manifest:

```yaml
map_nodes:
  - id: parallel_generation
    description: Run N generation workers concurrently over individual hypotheses
    worker_kind: generation
    inputs:
      - port: in
        kind: Goal
        activation: each
        items_field: hypotheses
    outputs:
      - port: out
        kind: Hypotheses
    max_parallel: 4
```

`items_field` tells the map node which JSON key inside the inbound artifact's
`data` contains the array to scatter.  `max_parallel` caps concurrent worker
activations for this map node specifically (separate from the global
`max_in_flight`).

### 2b. Manifest types

**New file:** `crates/eureka/src/manifest/map.rs`

```rust
use crate::graph::port::PortDef;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapNodeSpec {
    /// Unique node ID.
    pub id: String,
    /// Optional description.
    pub description: Option<String>,
    /// The agent kind to instantiate as a worker (must be a declared agent).
    pub worker_kind: String,
    /// Scatter input port (activation: each, with items_field).
    pub inputs: Vec<PortDef>,
    /// Gather output port.
    pub outputs: Vec<PortDef>,
    /// Maximum concurrent worker activations for this map.
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
}

const fn default_max_parallel() -> usize { 4 }
```

**Modify:** `crates/eureka/src/manifest/graph.rs`

Add `map_nodes` to `GraphManifest`:

```rust
pub struct GraphManifest {
    // ... existing fields ...
    #[serde(default)]
    pub map_nodes: Vec<MapNodeSpec>,
}
```

### 2c. Runtime node

**New file:** `crates/eureka/src/graph/map.rs`

```rust
pub struct MapNode {
    /// The worker node implementation (shared across activations).
    worker: BoxedNode,
    /// Field name to extract from the scatter artifact's data.
    items_field: String,
    /// Max concurrent worker calls within one map activation.
    max_parallel: usize,
}
```

`MapNode` implements `Node`.  Its `process()` method:

1. Extract `data[items_field]` from the scatter artifact.
2. Assert it is a JSON array.
3. Create a `tokio::sync::Semaphore` with `max_parallel` permits.
4. For each element in the array, spawn a task that acquires a permit, wraps
   the element as a `PortMsg`, calls `worker.process(ctx, vec![msg])`, and
   sends the result through an `mpsc` channel.
5. `join_all` the tasks.
6. Collect emissions into a single array artifact.
7. Emit the gathered array on the node's output port.

The existing concurrency test
(`test_activations_run_concurrently_not_sequentially`) in the scheduler already
validates that independent nodes run in parallel.  Map internals work similarly
but within one graph node.

### 2d. Session construction

**File:** `crates/eureka/src/session.rs`

When `Session::construct_node` encounters a `MapNodeSpec`:

1. Look up the worker kind in the manifest's agent list.
2. Construct the worker `LlmAgentNode` once.
3. Wrap it in `MapNode::new(worker, items_field, max_parallel)`.

The worker node is `Arc`-backed (`BoxedNode`), so concurrent `process()` calls
on the same worker are safe.  The LLM client is already behind `Arc`.

### 2e. Graph validation

**File:** `crates/eureka/src/graph/validate.rs`

New checks for map nodes:

- The scatter input port's `items_field` must exist in the upstream node's
  output schema.
- The worker kind must be a declared agent.
- The gather output kind must match the worker's output kind.
- `max_parallel` must be ≥ 1.
- Map nodes participate in standard reachability and port-kind checks.

### 2f. Estimated impact

| Metric | Value |
|---|---|
| New files | 2 (`manifest/map.rs`, `graph/map.rs`) |
| Files touched | 5 |
| New lines | ~350 |
| Changed lines | ~40 |
| Breaking change | No |

---

## Tier 3: Dynamic Edge Routing

**Blocks:** nothing (can land independently).  
**Unlocks:** any control node can influence graph routing at runtime;
co-scientist Supervisor, workflow orchestrators, conditional pipelines,
goal replay, and long-running iterative systems.

### Design principle

The scheduler must not know about specific agent roles ("Supervisor",
"Governor", etc.).  Instead it should provide a **generic dynamic routing
primitive**: a node can tag its emissions with a `route_to` hint that the
scheduler resolves in addition to (or in place of) the static YAML edges.

The co-scientist Supervisor *uses* this primitive; a simple linear
pipeline ignores it.  Both graphs run on the same generic engine.

### 3a. Route hints on emissions

**File:** `crates/eureka/src/graph/node.rs`

```rust
pub struct Emit {
    pub port: PortId,
    pub artifact: Artifact,
    /// Optional runtime routing override.  When present, the scheduler
    /// delivers this artifact to the specified target in addition to (or
    /// instead of) the static edges matching `port`.
    pub route_to: Option<RouteHint>,
}

pub struct RouteHint {
    /// Target node ID.
    pub node_id: String,
    /// Target input port on that node.
    pub port: String,
    /// Priority relative to other activations (0 = lowest, 255 = highest).
    /// The scheduler may use this to order ready activations.
    pub priority: u8,
    /// Whether to suppress static-edge routing for this emission.
    ///
    /// When `false` (default), the artifact follows both static edges AND
    /// this hint.  When `true`, only the hint target receives it.
    pub suppress_static: bool,
}

impl Default for RouteHint {
    fn default() -> Self {
        Self { node_id: String::new(), port: String::new(), priority: 128, suppress_static: false }
    }
}
```

Any node can set `route_to`.  LLM agents usually won't.  Control nodes do it
through their emit envelope.

### 3b. Control-node emit envelope extension

**File:** `crates/eureka/src/control/node.rs`

The control node currently reads JSON lines from subprocess stdout, each of
the form `{"port": "...", "artifact": {...}}`.  Extend the schema:

```json
{
  "port": "continue",
  "artifact": {"kind": "Hypotheses", "data": {...}},
  "route_to": {
    "node_id": "generation",
    "port": "in",
    "priority": 200,
    "suppress_static": false
  }
}
```

`route_to` is optional.  When absent the emission follows static edges only.
No Rust-side scheduling-mode flag is needed — the control node decides at
runtime whether to use dynamic routing.

### 3c. Scheduler resolution

**File:** `crates/eureka/src/scheduler/scheduler.rs`

In `route_emission`, after processing static edges, check `emit.route_to`:

```rust
fn route_emission(&self, from_node_id: &str, emit: &Emit, /* ... */) -> Result<usize, SchedulerError> {
    let mut enqueued = 0usize;

    // Static edges (unless suppressed by route hint).
    if !emit.route_to.as_ref().is_some_and(|h| h.suppress_static) {
        enqueued += self.route_via_static_edges(from_node_id, emit, outbound, round, /* ... */)?;
    }

    // Dynamic route hint.
    if let Some(hint) = &emit.route_to {
        enqueued += self.deliver_input(
            &hint.node_id, &hint.port, emit.artifact.clone(),
            round, input_buffer, in_flight, next_activation_id, handles, tasks,
        )?;
    }

    Ok(enqueued)
}
```

No special channel, no `SupervisorEvent` enum, no mode switch.  The scheduler
treats `route_to` the same way it treats static edges — route the artifact,
buffer it, dispatch when ready.

### 3d. Goal replay

A common pattern in iterative graphs: a later-round node needs to re-inject
the original Goal artifact (with new context) so a source agent fires again.

Rather than baking this into the scheduler, expose the initial Goal through
`NodeCtx`:

**File:** `crates/eureka/src/graph/node.rs`

```rust
pub struct NodeCtx {
    // ... existing fields ...
    /// The original Goal artifact supplied to this run.  Available to all
    /// nodes so they can replay it via `route_to` without the scheduler
    /// holding special Supervisor knowledge.
    pub initial_goal: Option<Artifact>,
}
```

The scheduler sets `initial_goal` when seeding the run.  Any node — not just
a Supervisor — can clone it and emit it with `route_to: { node_id: "generation", port: "in" }`.

### 3e. Priority-aware activation ordering

Static graphs fire activations in FIFO order.  With dynamic routing, a node
might want its emission handled before other queued work.

**File:** `crates/eureka/src/scheduler/scheduler.rs`

When an activation is ready to dispatch, insert it into a priority queue
rather than spawning immediately:

```rust
// Current: spawn as soon as ready
spawn_activation(activation, node.clone(), handles, tasks);

// New: push to priority queue, flush highest-priority first
ready_queue.push(activation, priority);
// ... at end of deliver_input batch, drain ready_queue in priority order:
while let Some(act) = ready_queue.pop() {
    spawn_activation(act, node.clone(), handles, tasks);
}
```

Priority comes from `RouteHint::priority`.  Static-edge deliveries use
default priority (128).  This ensures dynamic routing does not starve static
edges, and an orchestrator node can bump high-urgency work above the queue.

### 3f. Validation

**File:** `crates/eureka/src/graph/validate.rs`

Dynamic routing targets are not checked at load time (the target may not
exist in the static graph, or the port kind may be unknown).  Validation
remains structural only.  Runtime delivery to an unknown node or port
returns `SchedulerError`, same as a broken static edge.

### 3g. Session wiring — no changes needed

Dynamic routing is an emission-level feature.  The `Session` continues to
inject the Goal into source nodes as before.  If a graph wants a
centralised orchestrator, the orchestrator node is declared in YAML like
any other agent or control node.  The Goal reaches it through a static edge
from a source node, and it emits dynamic `route_to` hints from there.

No `SchedulingMode` enum.  No supervisor-specific injection path.

### 3h. Estimated impact

| Metric | Value |
|---|---|
| Files touched | 3 (`node.rs`, `control/node.rs`, `scheduler.rs`) |
| New lines | ~180 |
| Changed lines | ~30 |
| Breaking change | No (`route_to` defaults to `None`) |

---

## Tier 4: Per-Agent Resource Limits

**Blocks:** nothing (can be done independently).  
**Unlocks:** fair allocation, cost control per agent type.

### 4a. Configuration

**File:** `crates/eureka/src/config.rs`

```rust
pub struct SchedulerConfig {
    pub max_in_flight: usize,
    /// Per-agent-kind concurrency caps (keyed by agent kind string).
    /// When absent, the agent shares the global max_in_flight pool.
    #[serde(default)]
    pub agent_limits: HashMap<String, usize>,
}
```

```toml
[scheduler.agent_limits]
generation = 2
reflection = 3
evolution = 4
```

### 4b. Resource pool

**File:** `crates/eureka/src/scheduler/scheduler.rs`

Replace the single `in_flight_sem` with:

```rust
struct ResourcePool {
    global: Arc<Semaphore>,
    per_kind: HashMap<String, Arc<Semaphore>>,
}

impl ResourcePool {
    fn new(max_in_flight: usize, limits: &HashMap<String, usize>) -> Self {
        let mut per_kind = HashMap::new();
        for (kind, limit) in limits {
            per_kind.insert(kind.clone(), Arc::new(Semaphore::new(*limit)));
        }
        Self {
            global: Arc::new(Semaphore::new(max_in_flight)),
            per_kind,
        }
    }

    async fn acquire(&self, kind: &str) -> Option<ResourceGuard> {
        // Try to acquire the per-kind permit first.
        if let Some(sem) = self.per_kind.get(kind) {
            let kind_permit = sem.acquire_owned().await.ok()?;
            let global_permit = self.global.acquire_owned().await.ok()?;
            Some(ResourceGuard { _kind: kind_permit, _global: global_permit })
        } else {
            // No per-kind limit: only need the global permit.
            let global_permit = self.global.acquire_owned().await.ok()?;
            Some(ResourceGuard { _kind: std::mem::ManuallyDrop::new(()), _global: global_permit })
        }
    }
}
```

Agents without an entry in `agent_limits` share the global pool freely.
Agents with an entry must acquire both permits.

### 4c. Estimated impact

| Metric | Value |
|---|---|
| Files touched | 2 (`config.rs`, `scheduler.rs`) |
| New lines | ~100 |
| Changed lines | ~20 |
| Breaking change | No |

---

## Implementation Order and Dependencies

```
  Tier 1 ──► Tier 2
  (queues)   (map)

  Tier 3 ──► (can land at any point, orthogonal to 1 and 2)
  (dynamic routing)

  Tier 4 ──► (can land at any point)
  (limits)
```

**Recommended sequence:**

1. **Tier 1** — low risk, unlocks multi-source fan-in immediately.  Validates
   that the deque approach does not regress existing single-artifact semantics.

2. **Tier 4** — independent of the others.  Provides immediate operational value
   for the current static graph (prevent Reflection from starving SafetyReview).

3. **Tier 2** — adds the map primitive.  Requires Tier 1 for multi-artifact
   fan-in semantics.  At this point the current co-scientist graph can declare
   `replicas` and achieve true parallel generation without copy-pasting agent
   blocks.

4. **Tier 3** — dynamic edge routing.  Orthogonal to Tier 2 and can land
   earlier if desired.  Enables any control node to influence routing at
   runtime without the scheduler knowing about specific agent roles.

---

## File Inventory

### New files

| File | Tier | Purpose |
|---|---|---|
| `crates/eureka/src/manifest/map.rs` | 2 | `MapNodeSpec` types |
| `crates/eureka/src/graph/map.rs` | 2 | `MapNode` runtime implementation |

### Modified files

| File | Tiers | Changes |
|---|---|---|
| `crates/eureka/src/graph/port.rs` | 1 | `PortActivation` enum, `activation` field on `PortDef` |
| `crates/eureka/src/scheduler/scheduler.rs` | 1,2,3,4 | Queue buffer, map dispatch, route-hint resolution, resource pool |
| `crates/eureka/src/run.rs` | 1 | `PendingInput.artifacts: Vec<Artifact>` |
| `crates/eureka/src/config.rs` | 4 | `agent_limits` on `SchedulerConfig` |
| `crates/eureka/src/session.rs` | 2 | Map node construction |
| `crates/eureka/src/graph/node.rs` | 3 | `RouteHint` struct, `route_to` field on `Emit`, `initial_goal` on `NodeCtx` |
| `crates/eureka/src/manifest/graph.rs` | 2 | `map_nodes` field on `GraphManifest` |
| `crates/eureka/src/manifest/mod.rs` | 2 | Re-export `MapNodeSpec` |
| `crates/eureka/src/graph/validate.rs` | 2 | Map node validation rules |
| `crates/eureka/src/control/node.rs` | 3 | Parse `route_to` key in emit envelope |
