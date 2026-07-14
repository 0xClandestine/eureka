# AGENTS.md — Eureka

Agent guide to the Eureka runtime. Defines the SPEC system, agent compliance rules, and operational conventions.

---

## What this is

Eureka is a graph-based execution engine written in Rust. A run is a directed graph of nodes (LLM agents, subprocess control nodes) connected by typed edges carrying JSON artifacts. The topology, prompts, tools, and control-node definitions live in a single YAML manifest. The runtime loads the manifest, validates the graph, and executes it with an event-driven scheduler.

---

## Source is ground truth

When understanding the system, read the source code directly. Do not rely on documentation, prose descriptions, or code comments. SPECs (defined below) are derived from source — not the other way around. When in doubt about behavior, read the implementation.

---

## SPEC system

SPECs are normative specifications for every subsystem, protocol, and interface in Eureka. They are the shared contract between agents and humans. An Active SPEC is binding: code must conform to it. Deviations require explicit human approval.

### Location

```
specs/
  README.md        ← index: maps source files/modules → governing SPECs
  SPEC-0001.md     ← the meta-SPEC (this format and process, normatively)
  SPEC-NNNN.md
```

### Numbering

Four-digit zero-padded integers assigned sequentially. Numbers are never reused.

### Status lifecycle

```
Draft ──► Review ──► Active
Active ──► Superseded   (replaced by a newer SPEC)
Active ──► Deprecated   (retired, no replacement)
```

Only **Active** SPECs are binding. Draft and Review SPECs are proposals.

### Types

| Type | Meaning |
|------|---------|
| **Core** | Defines runtime data structures, invariants, and algorithms |
| **Interface** | Defines a protocol boundary: subprocess I/O, HTTP API, config schema |
| **Informational** | Describes rationale or design — not normative |
| **Process** | Defines agent or human workflow |

### SPEC format

Every SPEC MUST use exactly this template, no more, no less:

```
# SPEC-NNNN: Title

| Field      | Value                                               |
|------------|-----------------------------------------------------|
| Status     | Draft | Review | Active | Superseded | Deprecated  |
| Type       | Core | Interface | Informational | Process        |
| Created    | YYYY-MM-DD                                          |
| Requires   | SPEC-XXXX, SPEC-YYYY (or None)                      |
| Supersedes | SPEC-ZZZZ (or None)                                 |

## Abstract

One paragraph. What this SPEC defines and why it exists.

## Specification

Normative content. Use RFC 2119 keywords:
- MUST / MUST NOT   — absolute requirement
- SHOULD / SHOULD NOT — strong recommendation; deviation requires justification
- MAY               — optional

## Rationale

Design decisions and alternatives considered. Not normative.

## Reference Implementation

File paths and line references to the canonical implementation. No prose — only locations.
```

The `Rationale` and `Reference Implementation` sections MAY be omitted when not applicable. All other sections are REQUIRED.

### Writing a SPEC

1. Read the source files for the subsystem directly — not comments, not docs
2. Identify all public types, trait bounds, invariants, and observable behaviors
3. Express them as normative MUST/SHOULD/MAY statements in `## Specification`
4. Set `Status: Draft`
5. A human promotes it to `Review`, then to `Active`

Agents MUST NOT self-promote a SPEC beyond `Draft`.

---

## Agent compliance rules

### Before modifying any code

1. Open `specs/README.md`
2. Find every **Active** SPEC governing the module(s) being changed
3. Read each governing SPEC fully
4. Verify the change does not violate any MUST or MUST NOT

### If a change conflicts with an Active SPEC

**Stop immediately. Do not proceed.**

Report to the human:
- Which SPEC is in conflict (by number and title)
- The exact normative statement that would be violated
- The proposed resolution: either amend the SPEC or change the approach

Wait for explicit human approval before continuing.

### After a human-approved SPEC amendment

Update the SPEC in the same commit as the code change. If the amendment is substantial, move the SPEC status back to `Draft` pending re-review.

### Never

- Promote a SPEC status without human instruction
- Treat a Draft or Review SPEC as binding
- Use `docs/`, README prose, or code comments as authoritative sources of behavior — read the source

---

## Repository layout

```
Cargo.toml                    # workspace (resolver 3), strict clippy lints
deny.toml                     # cargo deny config (licenses/bans/advisories)
rust-toolchain.toml           # pinned to 1.96.0
eureka.toml                   # example runtime config (provider, budget, scheduler)
specs/                        # SPEC documents (see SPEC system above)
crates/
  eureka/                     # the library
    src/
      lib.rs                  # module map + re-exports
      config.rs               # layered config: DEFAULT_TOML → file → env (figment)
      error.rs                # EngineError enum
      graph/                  # pure topology: data structures only, no I/O
        artifact.rs           # Artifact { kind, data }
        control.rs            # ControlFlow, ControlDecision types
        edge.rs               # Edge { from, to, port, feedback }
        mod.rs               # re-exports
        node.rs               # Node trait, BoxedNode, Emit, NodeCtx, Usage
        port.rs               # PortDef, PortSpec, PortDirection
        spec.rs               # GraphSpec, GraphNodeSpec
        validate.rs           # Tarjan SCC, port-kind checks
      scheduler/              # event-driven executor
        executor.rs          # main loop, activation dispatch, budget ticks
        event.rs              # SchedulerEvent, SchedulerSignal types
        error.rs              # SchedulerError
      agent/                  # LLM-backed nodes
        client.rs            # LlmClient trait, RigClient (rig-core wrapper)
        def.rs               # AgentDef, ToolDef, AgentConfig
        error.rs             # AgentError
        mod.rs               # re-exports
        node.rs              # LlmAgentNode (the Node impl)
        tool.rs              # CommandTool (shell tool runner)
      control/                # subprocess-backed nodes
        error.rs              # ControlError
        mod.rs               # re-exports
        node.rs               # ControlNode, ControlNodeDef
        process.rs            # shared subprocess runner
      manifest/               # YAML loader
        agent.rs              # AgentSpec
        control.rs            # ControlSpec
        graph.rs              # GraphManifest (top-level loader)
        prompt.rs             # PromptLoader
      persistence/           # RunRecord, RunEnvironment, CheckpointStore, SqliteRunPersistence
      session/                # session module
        mod.rs                # Session struct
        builder.rs            # Session builder
        runner.rs             # Session::run
      manager.rs              # RunManager lifecycle service
      time.rs               # timestamp utilities

      rag/                    # RAG integration (opt-in via [rag] config section)
        mod.rs                # RagIndexHandle
        init.rs               # register_sqlite_vec()
        indexer.rs            # RagDocument, RagIndexer
        embedding.rs          # build_rag_components
  eureka-cli/                 # the CLI binary
    src/
      main.rs                 # clap: daemon | start | session | validate | list
      commands/
        daemon.rs             # daemon lifecycle
        list.rs               # print graph manifest contents
        mod.rs                # command exports
        session.rs            # session introspection and control
        validate.rs           # validate command
        run.rs
      server.rs               # axum HTTP server for observability
    tests/
      coscientist_validate.rs # end-to-end mock validation test
example/                      # shipped example graph
  coscientist.yml
  prompts/*.md
  control/*.py
  tools/arxiv_search.py
```

---

## Build, test, lint

```bash
cargo build --release
cargo test --release
cargo clippy --release                # lib + bins only
cargo clippy --all-targets --release  # includes tests
cargo run --release -- validate example/coscientist.yml
cargo run --release -- daemon start
cargo run --release -- start "<goal>" --domain chemistry
```

The workspace sets `pedantic`/`nursery` to `warn`, `unwrap_used`/`expect_used` to `deny`, and `unsafe_code` to `deny`. Test code relaxes denies via `#![cfg_attr(test, allow(...))]`.

---

## Conventions

- `unwrap`/`expect` are `deny` in non-test code — use `?`, `ok_or`, or return error types
- Errors use `thiserror`; wrap with `anyhow::Context` at CLI boundaries only
- Re-exports live in each module's `mod.rs` and the top-level `lib.rs`
- Tests are inline (`#[cfg(test)] mod tests`) per file
- Module-level docs (`//!`) on every file; `missing_docs` is `warn`
- Lint group priorities: `priority = -1` so individual overrides win
