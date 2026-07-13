# Eureka

Eureka is a **graph-based AI runtime** for autonomous research and long-running application workflows, written in Rust. A run is a directed graph of LLM agents and subprocess control nodes connected by typed ports and JSON artifacts. The entire topology — prompts, tools, control-node commands, and edges — lives in a single YAML manifest. The Rust runtime validates the graph at load time, then executes it with a concurrent, budget-aware scheduler.

```
intake → classify → review → summarize
```

and iterative research loops such as the shipped co-scientist graph:

```mermaid
flowchart LR
    gen[Generation] --> ref[Reflection]
    ref --> rank[Ranking]
    rank --> evo[Evolution]
    evo --> prox[Proximity]
    prox --> sup[Supervisor]
    sup -- continue --> gen
    sup -.-> halt[Terminal Output]
```

## Current status

### Implemented

- YAML-defined graph topology with typed ports and JSON artifacts.
- LLM agent nodes with configurable prompts, output schemas, models, and tools.
- Subprocess control nodes (Python, shell, or any executable) using JSON stdin/stdout.
- Port-kind validation, reachability checks, sink checks, governed-cycle checks, and optional input ports.
- Concurrent node activation with configurable in-flight limits.
- Feedback edges and synchronized execution rounds.
- Cost, token, wall-clock, and round budget backstops.
- Per-run SQLite database for control-node and agent-tool state.
- Durable lifecycle records (JSON-backed `RunStore` or SQLite).
- Runtime-owned SQLite run/checkpoint tables with revision-based optimistic concurrency.
- Durable scheduler checkpoints with process-restart resume from the latest completed scheduler boundary.
- Terminal artifact persistence in checkpoints.
- Human artifact injection into paused runs.
- `RunManager` lifecycle service for background execution and recovery.
- HTTP endpoints for graph data, live state, SSE events, durable status, and managed run lifecycle (`/runs`).
- Optional durable scheduler event history in SQLite (opt-in via `[tracing]` config).

Recovery from an activation interrupted mid-LLM-call or mid-subprocess is unsupported; the runtime resumes from the latest completed scheduler checkpoint.

---

## Quick start

Build the release binary:

```bash
cargo build --release
```

Validate the shipped graph without making any model call:

```bash
cargo run --release -- validate example/coscientist.yml
```

Run a research session via the daemon:

```bash
export OPENROUTER_API_KEY=sk-or-...

# Start the background daemon
cargo run --release -- daemon start --port 7773 &

# Submit a run
cargo run --release -- start \
  "Identify novel catalysts for CO₂ reduction" \
  --domain chemistry
```

Open `http://127.0.0.1:7773` for the live UI. The release binary is `target/release/eureka-cli`.

---

## Architecture (layers)

```mermaid
flowchart TB
    subgraph CLI ["CLI & HTTP (eureka-cli)"]
        CLAP[clap: daemon | start | session | validate | list]
        AXUM[axum server: /api/graph, /api/state, /api/events, /runs/*]
    end

    subgraph CORE ["Runtime Library (eureka)"]
        CONFIG[config.rs: layered figment merge]
        MANIFEST[manifest: YAML → GraphSpec]
        VALIDATE[validate.rs: port-kind, SCC, reachability]
        SESSION[session.rs: assemble & run]
        SCHED[scheduler.rs: event-loop, budget backstops]
        AGENTS[agents: LlmAgentNode + rig-core LLM client]
        CTRL[control: ControlNode + subprocess runner]
        RUN[run.rs: RunRecord, CheckpointStore, SqliteRunPersistence]
    end

    CONFIG --> MANIFEST
    MANIFEST --> VALIDATE
    SESSION --> SCHED
    SESSION --> AGENTS
    SESSION --> CTRL
    SCHED --> AGENTS
    SCHED --> CTRL
    SCHED --> RUN
    CTRL --> RUN
    AGENTS --> RUN
```

---

## Configuration

```mermaid
flowchart LR
    D[DEFAULT_TOML] --> F[figment merge]
    F --> C[eureka.toml]
    C --> E[EUREKA_* env vars]
    E --> CLI[CLI flags]
    CLI --> OUT[EurekaConfig]
```

| Layer | Source | Example |
|---|---|---|
| Built-in defaults | `config.rs` → `DEFAULT_TOML` | `max_rounds = 12` |
| File | `eureka.toml` or `--config` | `graph = "example/coscientist.yml"` |
| Environment | `EUREKA_*` prefix | `EUREKA_BUDGET_MAXWALLCLOCK=30m` |
| CLI | Clap flags | `--max-rounds 50` (patched post-load) |

Minimal configuration:

```toml
graph = "example/coscientist.yml"

[provider]
kind = "openrouter"
generation_model = "deepseek/deepseek-v4-flash"

[scheduler]
max_in_flight = 8

[budget]
max_cost_usd = 25.0
max_tokens = 5_000_000
max_wallclock = "45m"
max_rounds = 12        # hard backstop; governor plugin is the primary controller

[tracing]
enabled = false
```

> **Note:** `max_rounds` defaults to `12` in the code. Increase it in your `eureka.toml` for longer runs. The graph's own governor plugin is the primary round controller — the config value is a safety backstop.

### API keys

| Provider | Env var | Example model |
|---|---|---|
| `anthropic` | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| `openai` | `OPENAI_API_KEY` | `gpt-4o` |
| `openrouter` | `OPENROUTER_API_KEY` | `deepseek/deepseek-v4-flash` |
| `gemini` | `GEMINI_API_KEY` | `gemini-2.0-flash` |
| `groq` | `GROQ_API_KEY` | `llama-3.3-70b-versatile` |
| `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| `mistral` | `MISTRAL_API_KEY` | `mistral-large-latest` |
| `cohere` | `COHERE_API_KEY` | `command-r-plus` |
| `perplexity` | `PERPLEXITY_API_KEY` | `sonar-pro` |
| `together` | `TOGETHER_API_KEY` | `meta-llama/Llama-3-70b-chat-hf` |
| `xai` | `XAI_API_KEY` | `grok-3-mini` |
| `ollama` | (none) | `llama3.1:8b` |

---

## Graph manifests

An agent and control nodes are declared in a single YAML file; edges connect their ports.

```yaml
name: Review workflow

agents:
  - id: classifier
    prompt: prompts/classifier.md
    inputs:
      - { port: in, kind: Intake }
    outputs:
      - { port: out, kind: Classification }
    output_schema:
      type: object
      required: [category]
      properties:
        category: { type: string }

control:
  - id: review
    kind: human_review
    command: [python3, control/review.py]
    inputs:
      - { port: in, kind: Classification }
    outputs:
      - { port: approved, kind: Decision }
      - { port: rejected, kind: Decision }

edges:
  - { from_node: classifier, from_port: out,
      to_node: review, to_port: in }
```

Key concepts:

- **Artifact** = `{ kind, data }`. Kinds are opaque strings compared at edge endpoints.
- **Port** = named endpoint declaring the artifact kind it accepts or emits.
- **Forward edge** → same round. **Feedback edge** (`feedback: true`) → next round.
- **Source node** = receives the initial `Goal` artifact. Feedback edges are excluded from source detection so cyclic nodes like `generation` still receive the goal.
- **Sink node** = produces an artifact kind no other node consumes.

---

## Control-node protocol

Control nodes communicate with the scheduler via a JSON envelope protocol on stdin/stdout.

```mermaid
sequenceDiagram
    participant S as Scheduler
    participant C as Control Process

    S->>C: Fork process
    S->>C: JSON call envelope (stdin)
    Note over S,C: {"port":"in","artifact":{...},"inputs":[...]}
    loop Can emit multiple outputs
        C-->>S: JSON emit envelope (stdout)
        Note over C,S: {"port":"out","artifact":{...}}
    end
    S->>C: Close stdin, wait for exit
    C-->>S: Exit 0
```

Call envelope:
```json
{
  "port": "in",
  "artifact": { "kind": "Intake", "data": { "text": "..." } },
  "inputs": [
    { "port": "in", "artifact": { "kind": "Intake", "data": { "text": "..." } } }
  ]
}
```

Emit envelope (one per line of stdout):
```json
{
  "port": "out",
  "artifact": { "kind": "Classification", "data": { "category": "..." } }
}
```

Environment variables passed to the subprocess:
```text
EUREKA_SESSION_ID       stable run identifier
EUREKA_NODE_ID          manifest node ID
EUREKA_ROUND            current scheduler round
EUREKA_CONFIG           JSON-encoded node config
EUREKA_DB_PATH          per-run SQLite path
EUREKA_DB_SCHEMA_VERSION  runtime contract version
EUREKA_DB_NAMESPACE     plugin namespace (normally node ID)
```

Agent shell tools receive the same environment contract.

---

## Persistence and run files

```
<graph directory>/.eureka/sessions/
  <run_id>.sqlite    # run metadata, checkpoints, event history, plugin/tool state
```

The SQLite integration provides:
- Shared per-run database path for control-node and agent-tool cross-round state.
- Durable scheduler checkpoints with pause/resume support.
- Scheduler event history (opt-in via `[tracing]`).
- Run lifecycle records with revision-based optimistic concurrency.
- Process-restart resume from the latest completed scheduler checkpoint.

See [`docs/db-session-spec.md`](docs/db-session-spec.md) for the planned persistence abstractions, runtime-owned schema, checkpoint model, pause/resume semantics, human input, and application service API.

---

## HTTP and observability

The CLI server exposes these endpoints (started with `daemon start --port PORT`):

| Endpoint | Purpose |
|---|---|
| `GET /api/graph` | Validated graph topology. |
| `GET /api/state` | In-memory live state for the active run. |
| `GET /api/events` | SSE stream of scheduler events. |
| `GET /api/events/history` | All scheduler events from SQLite. |
| `GET /api/run` | Durable lifecycle metadata for the run. |
| `POST /runs` | Create a new run via `RunManager`. |
| `GET /runs` | List all runs managed by the daemon. |
| `GET /runs/{id}` | Get run details. |
| `POST /runs/{id}/pause` | Pause a running run (graceful). |
| `POST /runs/{id}/resume` | Resume a paused run. |

Enable durable event history:

```toml
[tracing]
enabled = true
include_artifacts = false   # omit large artifact payloads
```

Events are stored in the per-run `eureka_events` table.

---

## Library entry point

```rust,no_run
use eureka::config::EurekaConfig;
use eureka::Session;

let config = EurekaConfig::load(Some(std::path::Path::new("eureka.toml")))?;
let mut session = Session::new(config, &uuid::Uuid::now_v7().to_string(), None)?;

let stats = session
    .run(serde_json::json!({
        "goal": "Find promising catalysts for CO₂ reduction",
        "domain": "chemistry"
    }))
    .await?;

println!("completed {} rounds", stats.rounds_completed);
```

For lifecycle metadata, use `Session::run_with_store` with a `RunStore`. For HTTP applications, prefer the higher-level run-management abstractions described in [`docs/db-session-spec.md`](docs/db-session-spec.md).

---

## Development

```bash
cargo test --release         # unit + integration tests
cargo build --release        # library + CLI
cargo clippy --all-targets --release  # lint (green)
```

### Contributor references

- [`AGENTS.md`](AGENTS.md) — architecture, invariants, repository conventions, and troubleshooting notes.
- [`example/COSCIENTIST.md`](example/COSCIENTIST.md) — design notes for the shipped AI co-scientist graph.

### Repository layout

```
crates/eureka/       runtime library
crates/eureka-cli/   CLI and HTTP server
example/             shipped graph, prompts, controls, and tools
docs/                HTTP, persistence, and tracing specifications
eureka.toml          example runtime configuration
```

---

## License

Dual-licensed under MIT or Apache-2.0, at your option.
