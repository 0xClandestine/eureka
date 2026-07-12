# Eureka

Eureka is a graph-based AI runtime for autonomous research and long-running
application workflows. A run is a directed graph of LLM agents and subprocess
control nodes connected by typed ports and artifacts. The graph topology,
prompts, tools, and control-node commands are defined in one YAML manifest;
the Rust runtime validates and executes it with a concurrent, budget-aware
scheduler.

Eureka is designed for workflows such as:

```text
intake → classify → review → summarize
```

as well as iterative research loops such as the shipped co-scientist graph:

```text
generation → reflection → ranking → evolution → proximity → supervisor
     ↑___________________________________________________________|
```

## Current status

### Implemented

- YAML-defined graph topology with typed ports and JSON artifacts.
- LLM agent nodes with configurable prompts, output schemas, models, and tools.
- Python, shell, or arbitrary executable control nodes using JSON stdin/stdout.
- Port-kind validation, reachability checks, sink checks, governed-cycle checks,
  and optional input ports.
- Concurrent node activation with configurable in-flight limits.
- Feedback edges and synchronized execution rounds.
- Cost, token, wall-clock, and round budget backstops.
- Per-run SQLite paths for control-node and agent-tool state.
- Durable lifecycle records through the JSON-backed `RunStore` or SQLite.
- Runtime-owned SQLite run/checkpoint tables with revision checks.
- Durable scheduler checkpoints and process-restart resume at scheduler boundaries.
- Terminal artifact persistence in checkpoints.
- Human artifact injection into paused runs.
- `RunManager` lifecycle service for background execution and recovery.
- HTTP endpoints for graph data, live state, SSE events, durable status, and
  managed run lifecycle (`/runs`).
- Optional durable scheduler event traces in JSONL format.

The runtime does not resume an activation interrupted halfway through an LLM or
subprocess call; recovery starts from the latest completed scheduler boundary.

## Quick start

Build the release binary:

```bash
cargo build --release
```

Validate the shipped graph before making a model call:

```bash
cargo run --release -- validate example/coscientist.yml
```

Run a research session. The default provider is OpenRouter:

```bash
export OPENROUTER_API_KEY=sk-or-...
cargo run --release -- run \
  "Identify novel catalysts for CO₂ reduction" \
  --domain chemistry \
  --port 7773
```

Open `http://127.0.0.1:7773` for the live UI. Use `--port 0` to disable the
HTTP server.

The release binary is:

```text
target/release/eureka-cli
```

## Configuration

Configuration is layered in this order:

```text
built-in defaults → eureka.toml → EUREKA_* environment variables → CLI flags
```

A minimal configuration looks like:

```toml
graph = "example/coscientist.yml"

[provider]
kind = "openrouter"
generation_model = "deepseek/deepseek-v4-flash"

# Optional per-model pricing used by the cost budget backstop.
# [provider.pricing]
# input_per_million = 0.27
# output_per_million = 1.10

[scheduler]
max_in_flight = 8

[budget]
max_cost_usd = 25.0
max_tokens = 5_000_000
max_wallclock = "45m"
max_rounds = 100

[tracing]
enabled = false
include_artifacts = true
```

Set the API key matching the provider configured in `[provider]`:

| Provider | Environment variable | Example model |
|---|---|---|
| `anthropic` | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| `openai` | `OPENAI_API_KEY` | `gpt-4o` |
| `openrouter` | `OPENROUTER_API_KEY` | `deepseek/deepseek-v4-flash` |
| `gemini` | `GEMINI_API_KEY` | `gemini-2.0-flash` |
| `groq` | `GROQ_API_KEY` | `llama-3.3-70b-versatile` |
| `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| `mistral` | `MISTRAL_API_KEY` | provider-specific |
| `cohere` | `COHERE_API_KEY` | provider-specific |
| `perplexity` | `PERPLEXITY_API_KEY` | provider-specific |
| `together` | `TOGETHER_API_KEY` | provider-specific |
| `xai` | `XAI_API_KEY` | provider-specific |
| `ollama` | none | `llama3.1:8b` |

## Graph manifests

A manifest defines agents, control nodes, and edges. A compact example:

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

Important concepts:

- An **artifact** is `{ kind, data }`. Artifact kinds are opaque strings.
- A **port** declares the artifact kind accepted or emitted at that endpoint.
- Inputs are required by default; use `required: false` for optional inputs.
- A **forward edge** delivers work in the current round.
- A **feedback edge** delivers work in the next round and closes an iterative
  cycle.
- A **source node** receives the initial `Goal` artifact supplied to the run.
- A **sink node** has no downstream consumer for its emitted artifact.

See [`example/coscientist.yml`](example/coscientist.yml) for the complete
shipped graph.

## Control-node protocol

A control node is an executable declared in the manifest. Eureka starts it for
an activation, writes one JSON call envelope to stdin, and reads zero or more
JSON emit envelopes from stdout.

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

Emit envelope, one per output line:

```json
{
  "port": "out",
  "artifact": { "kind": "Classification", "data": { "category": "..." } }
}
```

Each control node receives these run-scoped environment variables:

```text
EUREKA_SESSION_ID       stable run identifier
EUREKA_NODE_ID          manifest node ID
EUREKA_ROUND            current scheduler round
EUREKA_CONFIG           JSON-encoded node config
EUREKA_DB_PATH          optional per-run SQLite path
EUREKA_DB_SCHEMA_VERSION runtime database contract version
EUREKA_DB_NAMESPACE     advisory plugin namespace, normally the node ID
```

Agent shell tools receive the same run-scoped environment. This allows both
control nodes and tools to use the per-run SQLite database consistently.

Plugins should keep their own tables namespaced and must not modify tables
owned by the future runtime persistence layer, which will use the `eureka_*`
namespace.

Reference implementations are in [`example/control/`](example/control/):

- `supervisor.py` — cross-round context and supervisor state.
- `ranker.py` — Elo ratings and ranking state.
- `proximity.py` — in-memory proximity graph generation.

## Persistence and run files

The CLI stores run files under:

```text
<graph directory>/.eureka/sessions/
```

A run may contain:

```text
<run_id>.sqlite        # control/plugin and tool state
<run_id>.json          # durable lifecycle metadata
<run_id>.traces.jsonl  # optional scheduler event trace
```

The current SQLite integration provides a shared per-run database path to
executable nodes. The Rust scheduler does not yet persist its activation
queues, input buffers, or checkpoints in SQLite. Consequently, the database
currently provides cross-round plugin memory, not full scheduler recovery.

See [`docs/db-session-spec.md`](docs/db-session-spec.md) for the planned
persistence abstractions, runtime-owned schema, checkpoint model, pause/resume
semantics, human input, and application service API.

## HTTP and observability

When started with `--port PORT`, the CLI server exposes:

| Endpoint | Purpose |
|---|---|
| `GET /api/graph` | Validated graph topology. |
| `GET /api/state` | In-memory live state for the active run. |
| `GET /api/events` | Server-sent event stream of scheduler events. |
| `GET /api/run` | Durable lifecycle metadata for the run. |

The live state and SSE endpoints are intended for active-run UIs. The durable
run endpoint is suitable for status polling after the process or UI restarts,
but does not currently restore execution.

Enable JSONL tracing with:

```toml
[tracing]
enabled = true
include_artifacts = false
```

Trace files are written next to the run database as:

```text
.eureka/sessions/<run_id>.traces.jsonl
```

See [`docs/tracing-spec.md`](docs/tracing-spec.md) for the file format,
configuration, durability behavior, and planned extensions.

## Library entry point

The primary library entry point is `Session`:

```rust,no_run
use eureka::config::EurekaConfig;
use eureka::Session;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = EurekaConfig::load(Some(std::path::Path::new("eureka.toml")))?;
let mut session = Session::new(
    config,
    &uuid::Uuid::now_v7().to_string(),
    None,
)?;

let stats = session
    .run(serde_json::json!({
        "goal": "Find promising catalysts for CO₂ reduction",
        "domain": "chemistry"
    }))
    .await?;

println!("completed {} rounds", stats.rounds_completed);
# Ok(())
# }
```

For lifecycle metadata, use `Session::run_with_store` with a `RunStore`. For
HTTP applications, prefer the higher-level run-management abstractions as they
are implemented from [`docs/db-session-spec.md`](docs/db-session-spec.md).

## Development

Run the test suite:

```bash
cargo test --release
```

Build the library and CLI:

```bash
cargo build --release
```

Run linting:

```bash
cargo clippy --all-targets --release
```

The repository has two complementary contributor references:

- [`AGENTS.md`](AGENTS.md) — architecture, invariants, repository conventions,
  and troubleshooting notes.
- [`example/COSCIENTIST.md`](example/COSCIENTIST.md) — design notes for the
  shipped AI co-scientist graph.

## Repository layout

```text
crates/eureka/       runtime library
crates/eureka-cli/   CLI and HTTP server
example/             shipped graph, prompts, controls, and tools
docs/                HTTP, persistence, and tracing specifications
eureka.toml          example runtime configuration
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
