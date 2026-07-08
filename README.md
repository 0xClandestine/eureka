# Eureka

A graph-based **AI co-scientist** runtime written in Rust. A research run is a
directed graph of *nodes* — LLM agents and subprocess control nodes — connected
by typed *edges*. The topology, prompts, tools, and control-node definitions
all live in a single YAML manifest; the runtime loads it, validates the graph
structurally, and executes it with a concurrent, budget-aware scheduler. There
is no per-agent Rust code — agents are data.

Eureka ships with a reproduction of Google Research's [AI co-scientist](https://research.google/blog/ai-co-scientist/)
topology (`example/coscientist.yml`): generation → reflection → ranking →
evolution → proximity → supervisor, with feedback loops that let the system
refine hypotheses across rounds.

## Features

- **Topology is data.** Adding an agent or control node never requires Rust
  changes — only a YAML edit.
- **Build-time validation.** Before any model call, every graph is checked for
  port-kind matching, dangling required inputs, reachability, governed cycles
  (every SCC must contain a node with a `halt` output), and sink presence.
- **Concurrent execution.** Activations run as tokio tasks capped by
  `scheduler.max_in_flight`; the semaphore now actually parallelizes node
  execution. Cancellation aborts in-flight LLM calls cleanly.
- **Input joining.** A multi-input node fires once with *all* its available
  inputs (required first, then any optional that arrived) — not once per input.
- **Budget backstops.** Token usage flows back from each provider call (via
  rig's normalized `Usage`) and is aggregated into `RunStats`, so
  `max_cost_usd` / `max_tokens` / `max_wallclock` / `max_rounds` actually fire.
  Cost uses per-input/per-output rates.
- **Subprocess control nodes.** External programs (Python, shell, any
  executable) act as graph nodes via a simple JSON-envelope stdin/stdout
  protocol, with `EUREKA_DB_PATH` for cross-round SQLite persistence.
- **Real-time observability.** An axum server streams `SchedulerEvent`s over
  SSE (`/api/graph`, `/api/state`, `/api/events`).
- **12 LLM providers** via [`rig`](https://crates.io/crates/rig-core):
  Anthropic, OpenAI, OpenRouter, Gemini, Groq, Mistral, Cohere, DeepSeek,
  Perplexity, Together, xAI, and Ollama (local).

## Quick start

```bash
# Build (binary is target/release/eureka-cli)
cargo build --release

# Validate the shipped graph
./target/release/eureka-cli validate example/coscientist.yml

# List the agents/control-nodes/tools declared by the configured graph
./target/release/eureka-cli list

# Run a research session (needs an API key — OpenRouter by default)
export OPENROUTER_API_KEY=sk-or-...
./target/release/eureka-cli run "Identify novel catalysts for CO₂ reduction" \
  --domain chemistry --port 7773
```

Then open `http://127.0.0.1:7773` for the live UI (graph topology, active
nodes, per-node outputs, SSE event stream). Pass `--port 0` to disable it.

## Configuration

Eureka uses layered config: `DEFAULT_TOML` (bundled) → `eureka.toml` (or
`--config`) → `EUREKA_*` environment variables → CLI flags. The shipped
`eureka.toml`:

```toml
graph = "example/coscientist.yml"

[provider]
kind = "openrouter"
generation_model = "deepseek/deepseek-v4-flash"

# Per-agent model overrides (optional)
# [provider.agent_models]
# reflection  = "anthropic/claude-opus-4-5"

# Per-input/per-output pricing so the cost backstop can fire (optional)
# [provider.pricing]
# input_per_million  = 0.27
# output_per_million = 1.10

[scheduler]
max_in_flight = 8

[budget]
max_cost_usd = 25.0
max_tokens = 5_000_000
max_wallclock = "45m"   # accepts "30s", "45m", "2h", or a float
max_rounds = 100
```

Set the matching `*_API_KEY` env var for your provider. Other providers:

| `kind` | Env var | Example model |
|---|---|---|
| `anthropic` | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| `openai` | `OPENAI_API_KEY` | `gpt-4o` |
| `gemini` | `GEMINI_API_KEY` | `gemini-2.0-flash` |
| `groq` | `GROQ_API_KEY` | `llama-3.3-70b-versatile` |
| `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| `xai` | `XAI_API_KEY` | `grok-3-mini` |
| `ollama` | _(none)_ | `llama3.1:8b` |

…and `mistral`, `cohere`, `perplexity`, `together`.

> **Env-var note:** figment splits `EUREKA_*` keys on `_`, so
> `EUREKA_PROVIDER_KIND` → `provider.kind`. The `budget.max_wallclock` field
> contains an underscore, so set it via `EUREKA_BUDGET_MAXWALLCLOCK` (the
> underscore-free alias) or via the config file.

## Graph manifests

A graph is one YAML file. Agents are LLM nodes (prompt + ports + JSON-schema
output + shell tools); control nodes are subprocess-backed. Example:

```yaml
name: My Graph
agents:
  - id: generation
    prompt: prompts/generation.md
    inputs:
      - { port: in, kind: Goal }
      - { port: context, kind: Insights, required: false }   # optional input
    outputs:
      - { port: out, kind: Hypotheses }
    config:
      temperature: 0.9
      max_iterations: 15
    tools:
      - name: search_literature
        description: Search arXiv for papers matching a query
        command: [python3, tools/arxiv_search.py]
        args_schema: { type: object, required: [query], properties: { query: { type: string } } }
        timeout_secs: 30
    output_schema:
      type: object
      required: [hypotheses]
      properties:
        hypotheses: { type: array, items: { type: object } }

control:
  - id: supervisor
    kind: supervisor
    command: [python3, control/supervisor.py]
    inputs:  [{ port: in, kind: Hypotheses }]
    outputs:
      - { port: continue, kind: Hypotheses }
      - { port: halt, kind: Control }
    config: { max_rounds: 5 }

edges:
  - { from_node: generation, from_port: out, to_node: supervisor, to_port: in }
  - { from_node: supervisor, from_port: continue, to_node: generation, to_port: in, feedback: true }
```

Key concepts:

- **Artifact** = `(kind: String, data: JSON)`. Kinds are opaque strings matched
  at edge endpoints by the validator — there is no central enum.
- **Feedback edges** (`feedback: true`) close cycles and are excluded from
  source-node detection so cyclic-but-source nodes still receive the initial
  goal. They deliver to `round + 1`.
- **Optional inputs** use `required: false`; inputs are required by default.
- **Sink** = a node emitting an artifact kind no other node consumes.

See `example/coscientist.yml` for a full topology, and
[`AGENTS.md`](AGENTS.md) for the detailed architecture, build/test commands,
and invariants.

## Control-node protocol

Control nodes communicate via JSON on stdin/stdout:

- **Call** (stdin): one JSON envelope. Carries a backward-compatible top-level
  `port`/`artifact` (the first input) **and** an `inputs` array of
  `{port, artifact}` objects, one per populated input port:
  ```json
  {"port": "in", "artifact": {"kind": "Goal", "data": {...}},
   "inputs": [{"port": "in", "artifact": {...}}, {"port": "context", "artifact": {...}}]}
  ```
- **Emit** (stdout): zero or more envelopes, one per line:
  `{"port": "out", "artifact": {"kind": "...", "data": {...}}}`

Environment variables: `EUREKA_SESSION_ID`, `EUREKA_NODE_ID`, `EUREKA_ROUND`,
`EUREKA_CONFIG` (JSON-encoded node config), `EUREKA_DB_PATH` (per-session
SQLite path for cross-round persistence). See `example/control/*.py` for
reference implementations (Elo ranker, proximity graph, supervisor).

## Repository layout

```
crates/
  eureka/        # the library: graph, scheduler, agents, control, manifest, session, config
  eureka-cli/    # the `eureka-cli` binary (run | validate | list) + UI server
example/         # shipped co-scientist graph (manifest, prompts, control nodes, tools)
eureka.toml      # example runtime config
AGENTS.md        # deep guide for contributors and AI agents
```

## Build, test, lint

```bash
cargo build --release                  # binary: target/release/eureka-cli
cargo test --release                   # 71 tests
cargo clippy --all-targets --release  # zero errors (unwrap/expect allowed in tests)
```

The workspace sets `pedantic`/`nursery` to `warn`, `unwrap_used`/`expect_used`
to `deny` in production code (allowed in `#[cfg(test)]`), and
`unsafe_code` to `forbid`. Rust toolchain is pinned to 1.96.

## Architecture

```
config ──► manifest (YAML) ──► GraphSpec ──► validate ──► Session ──► Scheduler
                                                                │
                                              ┌─────────────────┴────────────────┐
                                              ▼                                  ▼
                                         LlmAgentNode                       ControlNode
                                         (rig agent loop)                   (subprocess)
```

- **`graph`** — pure topology primitives (no I/O, no domain): nodes, ports,
  artifacts, edges, spec, validator.
- **`scheduler`** — event-driven executor: input joining, concurrent
  activations (JoinSet), synchronized round model, budget backstops.
- **`agents`** — LLM-backed nodes: `AgentDef`, `RigClient` (type-erased agentic
  loop via rig), `CommandTool` (shell tools).
- **`control`** — subprocess-backed nodes + the shared process runner.
- **`manifest`** — YAML loader (graph, agent, control, prompt paths).
- **`session`** — assembles all of the above; single entry point.

See [`AGENTS.md`](AGENTS.md) for the module map, key invariants, and a catalog
of resolved issues.

## Status

Early. The runtime is feature-complete and all known gotchas are resolved (see
`AGENTS.md`), but the system has not been validated end-to-end against a live
LLM provider beyond smoke tests. Token/cost backstops depend on the provider
reporting usage via rig's normalized `Usage`; providers that don't report usage
report 0, in which case only the wall-clock and round backstops fire.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
