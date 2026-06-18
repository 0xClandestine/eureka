# Specs Index

Every feature has a spec. Every crate has a subfolder. Status: 🔴 Missing · 🟡 Draft · 🟢 Stable

## eureka-graph

> GraphSpec, scheduler, validation, artifact, ports, edges, control node traits

| Module | Spec | Status |
|---|---|---|
| Graph types (GraphSpec, GraphNodeSpec, Node, BoxedNode, Edge, Artifact, Port) | [graph-model.md](eureka-graph/graph-model.md) | 🟢 Stable |
| Scheduler (event-driven async execution, budget, signals) | [scheduler.md](eureka-graph/scheduler.md) | 🟢 Stable |
| Graph validation (port/edge type checking, unknown kinds, governed cycles) | [graph-validation.md](eureka-graph/graph-validation.md) | 🟢 Stable |
| Control types (ControlSignal, Budget, RunStats) | [control.md](eureka-graph/control.md) | 🟢 Stable |

## eureka-agents

> AgentDef, AgentConfig, LlmClient, LlmAgentNode, ToolDef, CommandTool, AgentError

| Module | Spec | Status |
|---|---|---|
| Agent definition (AgentDef, AgentPort, AgentConfig, file loading, AgentLoadError) | [agent-as-data.md](eureka-agents/agent-as-data.md) | 🟢 Stable |
| LLM client (RigClient, agentic loop, submit tool, build_preamble) | [llm-client.md](eureka-agents/llm-client.md) | 🟢 Stable |
| Agent tools (ToolDef, CommandTool, invocation model, template substitution, arxiv_search) | [tools.md](eureka-agents/tools.md) | 🟢 Stable |
| Error types (AgentError, AgentLoadError) | [error-types.md](eureka-agents/error-types.md) | 🟢 Stable |
| Token counting / cost tracking | — | 🔴 Missing |

## eureka-engine

> NodeRegistry, Session, run orchestration

| Module | Spec | Status |
|---|---|---|
| Node registry (register, construct, port lookup) | [node-registry.md](eureka-engine/node-registry.md) | 🟢 Stable |
| Session (constructors, event handling, DB integration, RunStats) | [session.md](eureka-engine/session.md) | 🟢 Stable |
| Run orchestration (RunConfig, RunResult, run_single, run_batch) | [run-orchestration.md](eureka-engine/run-orchestration.md) | 🟢 Stable |

## eureka-config

> EurekaConfig, ProviderConfig, BudgetConfig

| Module | Spec | Status |
|---|---|---|
| Configuration (EurekaConfig, per-agent models, budget backstop, TOML loading) | [config.md](eureka-config/config.md) | 🟢 Stable |

## eureka-cli

> CLI binary, commands, control plugins, UI server, graph layout convention

| Module | Spec | Status |
|---|---|---|
| CLI commands (run, validate, list, eval) | [cli.md](eureka-cli/cli.md) | 🟢 Stable |
| UI server (axum REST + SSE) | [ui-server.md](eureka-cli/ui-server.md) | 🟢 Stable |
| build_registry / build_registry_for_validation | [build-registry.md](eureka-cli/build-registry.md) | 🟢 Stable |
| Control node implementations (plugin-based: elo-ranker, jaccard-dedup, round-governor) | [control-nodes.md](eureka-cli/control-nodes.md) | 🟢 Stable |
| Graph package layout (agents/, plugins/, tools/ directories) | [graph-layout.md](eureka-cli/graph-layout.md) | 🟢 Stable |
| Human gate injection (POST /api/inject) | — | 🔴 Missing |

## eureka-plugins

> Plugin system — subprocess-based control nodes and agent tools

| Module | Spec | Status |
|---|---|---|
| Plugin manifest, discovery, invocation protocol | [plugins.md](eureka-plugins/plugins.md) | 🟢 Stable |
| Built-in plugins (elo-ranker, jaccard-dedup, round-governor) | [plugins.md](eureka-plugins/plugins.md) | 🟢 Stable |

## eureka-db

> Session database — SQLite persistence for scientific work

| Module | Spec | Status |
|---|---|---|
| Session DB (schema, lifecycle, write protocol, DbError, SessionDb API) | [session-db.md](eureka-db/session-db.md) | 🟢 Stable |

## eureka-ui

> EurekaUI macOS Swift app

| Module | Spec | Status |
|---|---|---|
| EurekaUI (views, models, SSE data flow) | [eureka-ui.md](eureka-ui/eureka-ui.md) | 🟢 Stable |

## xtask

> Build automation

| Module | Spec | Status |
|---|---|---|
| xtask (validate-graphs, doc, lint) | [xtask.md](xtask/xtask.md) | 🟢 Stable |

---

## Open Gaps

| Feature | Owning Crate | Notes |
|---|---|---|
| Token counting / cost tracking | `eureka-agents` + `eureka-db` | `RunStats.total_cost_usd` and `.total_tokens` are hardcoded zero; `RigClient` does not track usage |
| Human gate injection | `eureka-cli` | `POST /api/inject` not implemented; `HumanGateNode` stub removed with hardcoded control nodes |
| Debate and jury graphs | `eureka-cli` | `graphs/debate/` and `graphs/jury/` do not exist |
| `PluginTool` (tool role for plugins) | `eureka-plugins` | `ToolDyn` impl for plugin-backed agent tools deferred; `ToolDef` would need a `"plugin"` key |
| Merge, Router, HumanGate control nodes | `eureka-plugins` | Not yet ported to plugins; previously existed as Rust stubs |
| `eureka sessions` subcommands | `eureka-cli` | `list`, `show`, `resume`, `export`, `delete` per session-db.md spec not implemented |
