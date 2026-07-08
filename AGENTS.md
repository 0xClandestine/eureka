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
- **Round counting**: the scheduler tracks a synchronized `current_round`.
  Forward edges deliver to the same round; feedback edges deliver to
  `round + 1`. A round is "complete" only when its outstanding-activation
  count hits zero, at which point the scheduler advances to the next round
  that has buffered work and emits a single `CycleCompleted`. So
  `rounds_completed` counts fully drained cycles, not per-emit feedback
  crossings — a single emit fanning out to many feedback edges increments the
  round by at most one.
- **Budget**: `RunStats` aggregates `total_cost_usd`/`total_tokens`/
  `total_input_tokens`/`total_output_tokens` from each `Node::process`'s
  `NodeUsage` return. The cost/token backstops (`max_cost_usd`/
  `max_tokens`) now fire. Cost uses per-input/per-output rates from
  `ProviderConfig::pricing` (`input_per_million` / `output_per_million`);
  tokens come from rig's normalized `Usage` (providers that don't report
  usage report 0, in which case only the wall-clock/round backstops fire).
- **Concurrency**: activations run as concurrent tokio tasks in a `JoinSet`,
  capped by `scheduler.max_in_flight`. `abort_all` on cancel/drop cancels
  in-flight `process()` calls (dropping in-flight LLM HTTP requests).
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

## Gotchas

No outstanding known limitations at this time. The items below that were
previously tracked as gotchas have all been resolved; see the
"Previously fixed" section. Token counts still depend on the provider
reporting usage via rig's normalized `Usage` — providers that don't report
usage report 0, in which case only the wall-clock and round backstops fire.

## Previously fixed (were gotchas, now resolved)

- ~~Round model was fuzzy~~ — fixed; the scheduler now tracks a synchronized
  `current_round` with per-round pending counts. Forward edges stay in the
  same round, feedback edges deliver to `round + 1`, and a round completes
  only when its pending count hits zero — so `rounds_completed` counts fully
  drained cycles, not per-emit feedback crossings.
- ~~Control nodes received only the first input~~ — fixed; the call envelope
  now carries an `inputs` array of every populated input port (plus the
  backward-compatible top-level `port`/`artifact` for legacy scripts).
- ~~`graph::control` re-exported `config::Budget`/`parse_duration`~~ — fixed;
  `RunStats`/`ControlSignal` now live in `config`, the `graph` module has zero
  production references to `config`, and the re-exports were dropped.
- ~~Cost estimate was a flat per-model rate~~ — fixed; replaced the single
  `cost_per_million_tokens` rate with a proper `ProviderConfig::pricing`
  (`input_per_million` / `output_per_million`), so cost reflects the real
  split. `NodeUsage::from_rig_usage` takes `Option<&Pricing>` and computes
  `input*in_rate + output*out_rate`.
- ~~Control-node `command[0]` mis-resolved to `graph_dir/python3`~~ — fixed;
  commands are used verbatim and the node spawns with `current_dir = graph_dir`.
- ~~Agent tools ran with the eureka process cwd~~ — fixed; tool subprocesses
  now run with `current_dir = graph_dir`.
- ~~Default graph path `coscientist/coscientist.yml` didn't exist~~ — fixed;
  defaults point at `example/coscientist.yml`.
- ~~Integration test path pointed at a non-existent directory~~ — fixed.
- ~~No cross-round persistence wired~~ — fixed; `run.rs` computes a per-session
  SQLite path under `<graph_dir>/.eureka/sessions/<id>.sqlite` and passes it
  as `EUREKA_DB_PATH` to control nodes.
- ~~No input joining~~ — fixed; the scheduler buffers inputs per
  `(node, round)` and fires a node once with all required (plus available
  optional) inputs.
- ~~Timed-out subprocesses leaked orphans~~ — fixed; `kill_on_drop(true)` +
  explicit kill on timeout, and stdin/stdout/stderr are driven concurrently
  to avoid pipe deadlocks. Output truncation is now char-safe.
- ~~Tool named 'submit' could shadow the terminal tool~~ — fixed; rejected at
  `build_agent_node`.
- ~~Scheduler could deadlock on >256-way fan-out~~ — fixed; unbounded
  activation channel.
- ~~Redundant error nesting~~ — fixed; `GraphError::ParseError` Display no
  longer prepends a prefix, and validation errors render as a joined list.
- ~~`max_wallclock` unsettable via env var~~ — fixed; underscore-free aliases
  `maxwallclock`/`maxwallclocksecs`.
- ~~Cost/token budget backstops not enforced~~ — fixed; `Node::process`
  now returns a `NodeUsage` (tokens + cost), the scheduler aggregates it into
  `RunStats`, and `max_cost_usd`/`max_tokens` now fire. rig's extended prompt
  path supplies token usage; cost uses per-input/per-output `pricing`.
- ~~The scheduler was sequential~~ — fixed; activations run as concurrent
  tokio tasks in a `JoinSet` capped by `max_in_flight`, with `abort_all` on
  cancel/drop so in-flight LLM calls are cancelled cleanly.
- ~~Cancellation ignored during permit wait~~ — fixed; biased `select!`.
- ~~Optional inputs impossible to declare~~ — fixed; `required: false` in the
  manifest's `PortDef`.
- ~~`eureka list` advertised non-existent control nodes~~ — fixed; `list` now
  introspects the actual graph manifest.
- ~~`cargo clippy --all-targets` failed~~ — fixed; `unwrap_used`/`expect_used`
  are allowed in `#[cfg(test)]` builds.
