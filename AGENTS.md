# AGENTS.md — Eureka

Guide for AI coding agents (and humans) working in this repository.

## What this is

Eureka is a **graph-based AI co-scientist runtime** written in Rust. A research
run is a directed graph of *nodes* (LLM agents + subprocess control nodes)
connected by typed *edges*. Topology, prompts, tools, and control-node
definitions all live in a single YAML manifest (`<graph_dir>/<graph>.yml`).
The Rust runtime loads the manifest, validates the graph, and executes it with
an event-driven scheduler. There is no per-agent Rust code — agents are data.

This repo reproduces Google Research's "AI co-scientist" topology as the
shipped example: `example/coscientist.yml` with Python control nodes under
`example/control/` and an arXiv search tool under `example/tools/`.

## Repository layout

```
Cargo.toml                 # workspace (resolver 3), strict clippy lints
deny.toml                  # cargo deny config (licenses/bans/advisories)
rust-toolchain.toml        # pinned to 1.96.0
eureka.toml                # example runtime config (provider, budget, scheduler)
crates/
  eureka/                  # the library (graph, scheduler, agents, control, manifest, session, config)
    src/
      lib.rs               # module map + re-exports (start here)
      config.rs            # layered config: DEFAULT_TOML → file → env (figment)
      error.rs             # EngineError
      graph/               # PURE topology: artifact, edge, node, port, spec, validate, control
      scheduler/           # event-driven executor (scheduler.rs, event.rs, error.rs)
      agents/              # LLM nodes: def, client (rig-backed), node, tools
      control/             # subprocess-backed control nodes + shared process runner
      manifest/            # YAML loader: graph, agent, control, prompt
      session.rs           # assembles everything; entry point: Session::new / Session::run
  eureka-cli/              # the `eureka-cli` binary
    src/
      main.rs              # clap CLI: run | validate | list
      commands/            # run.rs, validate.rs, list.rs
      server.rs            # axum UI server (graph/state/SSE events)
    tests/coscientist_validate.rs   # end-to-end validation test (currently broken — see Gotchas)
example/                   # shipped co-scientist graph
  coscientist.yml          # the manifest (agents + control + edges)
  prompts/*.md            # agent system prompts
  control/*.py             # subprocess control nodes (supervisor, ranker, proximity)
  tools/arxiv_search.py    # shell tool used by agents
  COSCIENTIST.md           # long-form design doc (not code)
```

## Build, test, lint

```bash
cargo build --release                 # binary is target/release/eureka-cli  (NOT "eureka")
cargo test --release                  # unit + integration tests
cargo test --release -p eureka-cli --test coscientist_validate   # the e2e validation test
cargo clippy --release                # lib + bins only
cargo clippy --all-targets --release  # includes tests — currently FAILS (see Gotchas)
cargo run --release -- validate example/coscientist.yml          # validate the shipped graph
cargo run --release -- run "<goal>" --port 0                    # run (needs API key env)
```

The workspace sets `pedantic`/`nursery` to `warn`, `unwrap_used`/`expect_used`
to `deny`, and `unsafe_code` to `forbid`. Test code uses `.unwrap()` heavily,
so `cargo clippy --all-targets` currently fails under the deny policy — fix by
allowing `unwrap_used` in `#[cfg(test)]` modules or switching tests to
`?`/`expect`-free helpers.

## Architecture (how a run flows)

1. **Config** (`config.rs`): `EurekaConfig::load` merges `DEFAULT_TOML` → file
   (`eureka.toml` or `--config`) → env (`EUREKA_*`). `DEFAULT_TOML` is the
   single source of truth for defaults; sub-structs intentionally have no
   `Default`-via-serde fallbacks.
2. **Manifest** (`manifest/graph.rs`): `GraphManifest::load` reads the YAML,
   resolves relative prompt/command paths against the YAML's parent dir, and
   `to_graph_spec()` produces a pure `GraphSpec` (nodes + edges).
3. **Validation** (`graph/validate.rs`): before any model call, the graph is
   checked for: port-kind match, no dangling required inputs, reachability,
   unknown node kinds, governed cycles (every SCC must contain a node with a
   `halt` output port), and sink presence. Tarjan SCC is implemented inline.
4. **Session** (`session.rs`): `Session::new` loads + validates. `Session::run`
   constructs each node from the manifest (`build_agent_node` / `build_control_node`),
   injects the goal artifact into source nodes, and drives the scheduler.
5. **Scheduler** (`scheduler/scheduler.rs`): a single `tokio::select!` loop that
   pops activations off a channel, runs the node, routes emissions along
   outbound edges (enqueueing new activations), tracks `pending`, and enforces
   budget backstops (wall-clock via a 100ms ticker; rounds via feedback-edge
   crossings). Emits `SchedulerEvent`s on an mpsc channel.
6. **Agent node** (`agents/node.rs` + `agents/client.rs`): `LlmAgentNode::process`
   serializes the inbound artifact to JSON, runs an agentic loop via `rig-core`
   (`AgentBuilder` + a `submit` tool + shell `CommandTool`s), and splits the
   returned JSON across the agent's output ports.
7. **Control node** (`control/node.rs` + `control/process.rs`): spawns a
   subprocess, writes a JSON call envelope to stdin, reads line-delimited JSON
   emit envelopes from stdout. Env vars `EUREKA_SESSION_ID`, `EUREKA_NODE_ID`,
   `EUREKA_ROUND`, `EUREKA_CONFIG`, `EUREKA_DB_PATH` are injected.
8. **Observability** (`eureka-cli/src/server.rs`): an axum server on
   `127.0.0.1:{port}` exposes `/api/graph`, `/api/state`, and `/api/events`
   (SSE). A background task (`track_live_state`) folds scheduler events into a
   `LiveState` snapshot.

## Key concepts / invariants

- **Topology is data.** Adding an agent or control node never requires Rust
  changes — only a YAML edit. The `kind` string on a `GraphNodeSpec` is the
  agent `id` (for agents) or the control node's `kind` (for control nodes).
- **Artifact = `(kind: String, data: JSON)`.** Kinds are opaque strings
  matched at edge endpoints by the validator. There is no central enum of kinds.
- **Ports**: `PortDef` in YAML is `{port, kind}`. `to_input_spec` marks every
  input `required: true`; there is currently no way to declare an optional
  input via the manifest (the `required` field exists on `PortSpecEntry` but is
  never set false through the manifest path).
- **Feedback edges** (`feedback: true`) close cycles and are excluded from
  source-node detection so cyclic-but-source nodes (e.g. `generation`) still
  receive the initial goal.
- **Round counting**: the scheduler increments `rounds_completed` whenever an
  emission crosses a feedback edge. "Round" ≈ "feedback crossing count", not a
  clean cycle index.
- **Budget**: `RunStats` has `total_cost_usd`/`total_tokens` fields but the
  scheduler never populates them — only `elapsed_secs` and `rounds_completed`
  are tracked. Cost/token backstops are currently non-functional.
- **Provider switching**: `ProviderKind` enum + `build_llm_client` map to
  `rig-core` provider clients. Set the matching `*_API_KEY` env var.

## Conventions

- Module-level docs (`//!`) on every file; `missing_docs` is `warn`.
- `unwrap`/`expect` are `deny` in non-test code — use `?`, `ok_or`, `unwrap_or`,
  or return `EngineError`/`SchedulerError`/`NodeError`/`ControlError`.
- Errors use `thiserror`. Wrap with context via `anyhow::Context` at CLI
  boundaries only.
- Re-exports live in each module's `mod.rs` and the top-level `lib.rs`.
- Tests are inline (`#[cfg(test)] mod tests`) per file.

## Gotchas (things that are currently broken or surprising)

These are known issues — verify before relying on the affected behavior:

1. **Default graph path is wrong.** `eureka.toml` and `config.rs::DEFAULT_TOML`
   both point to `coscientist/coscientist.yml`, but the shipped graph is at
   `example/coscientist.yml`. `eureka run` from the repo root fails to find the
   manifest. Workaround: `--config` pointing at a corrected file, or run from
   `example/` with an adjusted config.
2. **Integration test path is wrong.**
   `crates/eureka-cli/tests/coscientist_validate.rs` looks for
   `<repo>/coscientist/coscientist.yml` (via `CARGO_MANIFEST_DIR/../../coscientist`),
   which does not exist. The test fails. It should point at `example/`.
3. **Control-node command[0] is mis-resolved.** `Session::build_control_node`
   rewrites *any* relative `command[0]` to `graph_dir/<arg>`, so `[python3, …]`
   becomes `<graph_dir>/python3`, which does not exist and fails to spawn.
   Control nodes already spawn with `current_dir = graph_dir`, so this rewrite
   is unnecessary and harmful and should be removed.
4. **Agent-tool subprocess cwd is not set.** `CommandTool` calls
   `run_subprocess` with `current_dir = None`, so relative script paths in tool
   commands (e.g. `tools/arxiv_search.py`) resolve against the *eureka process*
   cwd, not the graph dir. Tools fail unless you `cd` into the graph dir first.
5. **No cross-round persistence is wired.** `run.rs` passes `db_path = None`
   to `Session::new`, so `EUREKA_DB_PATH` is never set. The Python control
   nodes (supervisor context memory, Elo rating persistence, proximity graph
   cache) all no-op without it. The `db` module referenced in `lib.rs` does not
   exist.
6. **No input joining.** `Node::process` handles one `PortMsg` at a time.
   Multi-input agents (e.g. `generation` with `in` + `context`) run a fresh LLM
   call per input arrival — they never see both inputs together. Optional
   secondary inputs (e.g. `ranking.graph`) re-trigger the node and cause
   spurious re-emits. There is no barrier/join primitive in the runtime.
7. **Scheduler is sequential.** Despite `max_in_flight`/`Semaphore`, the loop
   awaits one node at a time; true concurrency is never realized. The semaphore
   is decorative.
8. **`cargo clippy --all-targets` fails** due to `unwrap_used` deny + test
   `unwrap()`s.
9. **`list` command advertises non-existent nodes** (`control.router`,
   `control.merge`, `control.broadcast`) that are not implemented anywhere.
10. **String-byte slicing panic risk**: `process.rs` slices stdout at a fixed
    byte offset (`&stdout[..MAX_OUTPUT_BYTES]`), which can panic on non-ASCII
    output that splits a multi-byte char.
