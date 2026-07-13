# Server-Driven Architecture for AI Agent Systems: Eureka

> **Status:** Concept paper — July 2026
>
> Applies server-driven UI principles to AI agent orchestration: the YAML
> manifest is the single source of truth; the Rust runtime is the rendering
> engine; the scheduler is the layout pass for computation graphs.

---

## 1. Why Bother? The Problem with Hardcoded Agent Graphs

Traditional AI agent frameworks (LangChain, AutoGen, CrewAI) wire agents
together in imperative code.  Adding a new agent, reordering a pipeline, or
inserting a safety gate means editing Python or TypeScript source, re-running
tests, and redeploying the whole system.

In Eureka we asked: *what if agent topology were data, not code?*  The same
question that drove server-driven UI for mobile apps — "why should changing a
banner require an app store review?" — applies here.  Changing a prompt,
reordering the review pipeline, or adding a proximity node shouldn't require
recompiling Rust or restarting a server.

---

## 2. Core Philosophy: The SDUI Principles Applied to Agents

| SDUI Principle | Eureka Equivalent |
|---|---|
| **Start with the screen** | Design the graph topology first (YAML), build nodes to match |
| **Keep the client simple** | Agents are pure functions of `(input artifacts) → (output artifacts)` |
| **Serve ready-to-use content** | Artifacts carry typed, structured JSON — no raw strings that clients must parse |
| **Speak a common language** | The port-type system (`Goal`, `Hypotheses`, `Reviews`, `Ranking`) is the design system |
| **Single source of truth** | One YAML manifest defines agents, control nodes, edges, tools, and prompts |

### 2.1 "Start with the Graph" — Topology-First Design

In Eureka, you don't write code to connect agents.  You write a YAML file:

```yaml
edges:
  - from_node: generation
    from_port: out
    to_node: safety_review
    to_port: in
```

The Rust runtime reads this and builds the execution graph.  Changing the
pipeline — inserting a new node, reordering stages, adding feedback loops —
is a YAML edit.  No Rust code changes, no recompilation.

This is the agent equivalent of SDUI's "dream query": design the ideal
scientific reasoning graph, then let the runtime figure out how to execute it.

### 2.2 "Keep the Node Simple" — Pure Functions

Every Eureka node (LLM agent or Python control process) is a pure function:

```
fn process(inputs: Vec<PortMsg>) → (Vec<Emit>, NodeUsage)
```

The node never knows about the broader graph.  It doesn't decide where
its output goes or what comes next.  The scheduler handles all routing.

This is the agent equivalent of SDUI's "dumb renderer": the client (node)
just renders its inputs into outputs; the server (scheduler) decides layout
and flow.

### 2.3 "Serve Artifacts, Not Raw Data" — Typed Ports

Artifacts in Eureka carry a `kind` tag (`Hypotheses`, `Reviews`, `Ranking`)
that the validator checks at graph-load time.  Downstream nodes receive
structured JSON matching their declared input schema.  No parsing raw LLM
output, no string manipulation in the graph.

This is the agent equivalent of SDUI's "product info, not domain data":
the server pre-formats content so the client just renders it.

### 2.4 "The Port System IS the Design System"

In SDUI, a design system provides a shared vocabulary of components
(`Button`, `Carousel`, `Banner`).  In Eureka, the port-kind system provides
a shared vocabulary of reasoning artifacts:

```
Goal → Hypotheses → Reviews → Ranking → Insights → (feedback)
```

Every edge connects compatible port kinds, validated at manifest load.
Adding a new node kind (e.g., `safety_review`) with `kind: Hypotheses` input
and output means it slots into the graph wherever `Hypotheses` flows.

---

## 3. Architecture: How Eureka Implements Server-Driven Agent Orchestration

### 3.1 The Blueprint

```
┌─────────────────────────────────────────────────────────┐
│  YAML Manifest (the "server")                           │
│  ┌──────────┐  ┌─────────┐  ┌──────────┐              │
│  │ agents   │  │ control │  │ edges    │              │
│  │ (LLM)    │  │ nodes   │  │ (topology)│             │
│  └──────────┘  └─────────┘  └──────────┘              │
│  ┌──────────┐  ┌─────────┐                             │
│  │ prompts  │  │ tools   │                             │
│  └──────────┘  └─────────┘                             │
└──────────────────────┬──────────────────────────────────┘
                       │ load + validate
                       ▼
┌─────────────────────────────────────────────────────────┐
│  Rust Runtime (the "rendering engine")                  │
│  ┌───────────────────────────────────────────────────┐ │
│  │  Scheduler                                        │ │
│  │  ┌─────────┐  ┌──────────┐  ┌────────────────┐  │ │
│  │  │ event   │  │ port     │  │ budget/round   │  │ │
│  │  │ loop    │  │ routing  │  │ enforcement    │  │ │
│  │  └─────────┘  └──────────┘  └────────────────┘  │ │
│  └───────────────────────────────────────────────────┘ │
│  ┌──────────┐  ┌──────────┐  ┌────────────────────┐   │
│  │ LLM pool │  │ subproc  │  │ checkpoint/persist │   │
│  │ (rig)    │  │ runner   │  │ (SQLite)           │   │
│  └──────────┘  └──────────┘  └────────────────────┘   │
└──────────────────────┬──────────────────────────────────┘
                       │ SSE events / HTTP API
                       ▼
┌─────────────────────────────────────────────────────────┐
│  Observability Layer (the "UI viewport")                │
│  GET /api/graph  — static topology                     │
│  GET /api/state  — live round/active-nodes/elapsed     │
│  GET /api/events — SSE firehose of SchedulerEvent      │
└─────────────────────────────────────────────────────────┘
```

### 3.2 The Component Registry: PortRegistry + Manifest

In SDUI, the client has a component registry mapping type strings to native
widgets.  In Eureka, the `PortRegistry` maps node kinds to their port
signatures, and the manifest maps agent/control `id` to implementation:

```rust
// Component registry for agents — the "design system" for reasoning
let port_registry = build_port_registry(&manifest);
validate_graph(&spec, &port_registry)?;  // all edges type-check

// Instantiation (like component rendering)
for node_spec in &spec.nodes {
    let node: BoxedNode = match node_spec.kind {
        // LLM agent from manifest
        kind if manifest.has_agent(kind) =>
            build_agent_node(&manifest.agent(kind)),
        // Python control node from manifest
        kind =>
            build_control_node(&manifest.control(kind)),
    };
}
```

Like SDUI's `componentRegistry["ProductCarousel"]`, Eureka resolves
`"generation"` → `LlmAgentNode` or `"elo-ranker"` → `ControlNode` at
runtime from the manifest — never from hardcoded Rust.

### 3.3 The API Conversation: Artifact Routing

The scheduler is the "layout pass."  When a node emits an artifact, the
scheduler looks up outbound edges and delivers it to downstream input ports:

```
generation emits Artifact { kind: "Hypotheses", data: [...] }
    → route along edges[generation.out → safety_review.in]
    → safety_review receives PortMsg { port: "in", artifact: ... }
```

This is identical to SDUI's rendering loop:
1. Server sends `{ type: "carousel", data: {...} }`
2. Client looks up `"carousel"` in registry
3. Client renders the carousel widget with the data

In Eureka:
1. Node emits `Emit { port: "out", artifact: {...} }`
2. Scheduler looks up edges for `(node_id, "out")`
3. Scheduler delivers `PortMsg` to downstream input ports

### 3.4 Expert-in-the-Loop: Late Human Injection

SDUI's killer feature is updating the UI without redeployment.  Eureka's
equivalent is expert injection: a human scientist can pause a running graph,
inject a hypothesis or review into any port, and resume — the scheduler picks
up the new artifact as if it came from a node.

```bash
eureka session pause <id>
eureka session inject <id> generation in \
  '{"hypotheses":[{"statement":"Novel mechanism",...}]}'
eureka session resume <id>
```

This is the agent equivalent of SDUI's "holiday banner" scenario: change the
content flowing through the system without restarting the graph or changing
any code.

### 3.5 Dynamic Personalization: Per-Agent Config + Feedback Edges

SDUI personalizes UI per user segment.  Eureka personalizes agent behavior
per agent ID via `agent_overrides` in `eureka.toml`:

```toml
[agent.overrides.generation]
temperature = 0.9
max_iterations = 15
```

And the graph itself adapts via **feedback edges** — the `meta_review` agent
sends `Insights` back to `generation`, `reflection`, and `evolution` for the
next round.  This is SDUI's "dynamic configuration" applied to reasoning:
the graph changes what it emphasizes based on prior results.

---

## 4. Putting It Together: The Eureka SDUI Flow

### Step-by-step

| Step | SDUI for Mobile | SDUI for Agents (Eureka) |
|------|----------------|--------------------------|
| **1. Configure** | Product manager sets banner config on server | Scientist writes YAML manifest + prompts |
| **2. Fetch** | App calls `GET /screen/home` | CLI: `eureka start "Research goal"` |
| **3. Render** | Client parses JSON, renders `Banner` widget | Scheduler loads manifest, validates graph, constructs nodes |
| **4. Execute** | User taps button → action model fires | Scheduler routes artifacts along edges, nodes process inputs |
| **5. Observe** | Analytics track engagement | SSE events stream to UI dashboard |
| **6. Update** | Banner config changed, appears on next fetch | Graph topology changed, next run executes new pipeline |

### A concrete change: adding a safety review gate

**Without SDUI (traditional):**
1. Edit Python orchestrator to add `safety_review()` call
2. Update generation output schema to pass through safety node
3. Update reflection input to accept safety-filtered hypotheses
4. Re-run tests, fix type errors, redeploy

**With Eureka (SDUI):**
```yaml
# Insert one edge pair
edges:
  - from_node: generation
    from_port: out
    to_node: safety_review    # was: to_node: reflection
    to_port: in
  - from_node: safety_review
    from_port: out
    to_node: reflection
    to_port: in
```

Plus a prompt file.  No code changes.  The validator catches port mismatches.

---

## 5. Adoption Roadmap

### 5.1 Where SDUI for Agents Excels

- **Multi-round reasoning** — feedback edges, context memory, tournament ranking
- **Human-in-the-loop** — pause/inject/resume for expert review
- **Multi-agent pipelines** — any DAG or governed cyclic graph from a single YAML
- **Experimentation** — swap prompts, models, or ranking strategies per run

### 5.2 Where Hardcoded Orchestration Still Wins

- **Single-agent, single-turn** — a `ChatCompletion` call needs no graph
- **Tight coupling** — if node A always and only feeds node B, a direct function call is simpler
- **Extreme latency sensitivity** — the scheduler adds ~100µs overhead per activation

### 5.3 Incremental Adoption

1. **Start with one pipeline**: Write a 2-node graph (generation → reflection) in YAML
2. **Add a control node**: Replace a Python `if` with a `supervisor` control node
3. **Add feedback**: Wire `meta_review.insights → generation.context` as a feedback edge
4. **Go multi-agent**: Add evolution, proximity, ranking — all data, no code

---

## 6. FAQ

**Is Eureka only for co-scientist workflows?**
No. The runtime is domain-agnostic. Any workflow that can be modeled as a
directed graph of LLM agents + Python control nodes works. Replace the
prompts and control scripts with your domain logic.

**Does this replace LangChain?**
It complements it. Eureka handles graph-level orchestration, checkpointing,
budget enforcement, and human-in-the-loop. Individual agents can use any
LLM provider via rig. Control nodes can call any Python library.

**What's the cost of the indirection?**
~100µs routing overhead per activation. For LLM calls that take seconds,
this is negligible. For high-frequency control nodes (1000/sec), use the
`halt` port to terminate early instead of micro-optimized loops.

**Can I add new node types without Rust changes?**
Yes.  LLM agents need only a YAML entry + a prompt file.  Control nodes
need a Python script that reads stdin JSON and writes stdout JSON.  No
Rust code is needed for either.

**How does this compare to server-driven UI for mobile?**
It's the same architecture applied to a different domain.  Instead of
`Button`/`Carousel` components you have `generation`/`reflection` nodes.
Instead of `navigate_to_screen` actions you have artifact routing along
edges.  Instead of a component registry you have a `PortRegistry`.