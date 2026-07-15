# Eureka SPEC Index

Maps source modules to their governing Active SPECs. When modifying any file listed here, read the corresponding SPEC(s) before making changes.

| Source path | Governing SPEC(s) |
|-------------|-------------------|
| `crates/eureka/src/graph/artifact.rs` | [SPEC-0002](SPEC-0002.md) |
| `crates/eureka/src/graph/port.rs` | [SPEC-0003](SPEC-0003.md) |
| `crates/eureka/src/graph/edge.rs` | [SPEC-0004](SPEC-0004.md) |
| `crates/eureka/src/graph/node.rs` | [SPEC-0005](SPEC-0005.md) |
| `crates/eureka/src/graph/spec.rs` | [SPEC-0006](SPEC-0006.md) |
| `crates/eureka/src/graph/validate.rs` | [SPEC-0007](SPEC-0007.md) |
| `crates/eureka/src/graph/mod.rs` | SPEC-0002 – SPEC-0007 |
| `crates/eureka/src/lib.rs` | SPEC-0001 (module map only) |
| `crates/eureka/src/time.rs` | SPEC-0009 (timestamp utilities used by scheduler) |
| `crates/eureka/src/manifest/mod.rs` | [SPEC-0008](SPEC-0008.md) |
| `crates/eureka/src/manifest/graph.rs` | [SPEC-0008](SPEC-0008.md) |
| `crates/eureka/src/manifest/agent.rs` | [SPEC-0008](SPEC-0008.md) |
| `crates/eureka/src/manifest/control.rs` | [SPEC-0008](SPEC-0008.md) |
| `crates/eureka/src/manifest/prompt.rs` | [SPEC-0008](SPEC-0008.md) |
| `crates/eureka/src/scheduler/executor.rs` | [SPEC-0009](SPEC-0009.md), [SPEC-0014](SPEC-0014.md) |
| `crates/eureka/src/scheduler/event.rs` | [SPEC-0009](SPEC-0009.md) |
| `crates/eureka/src/scheduler/error.rs` | [SPEC-0009](SPEC-0009.md) |
| `crates/eureka/src/scheduler/mod.rs` | [SPEC-0009](SPEC-0009.md) |
| `crates/eureka/src/agent/mod.rs` | [SPEC-0010](SPEC-0010.md) |
| `crates/eureka/src/agent/def.rs` | [SPEC-0010](SPEC-0010.md) |
| `crates/eureka/src/agent/node.rs` | [SPEC-0010](SPEC-0010.md) |
| `crates/eureka/src/agent/client.rs` | [SPEC-0010](SPEC-0010.md) |
| `crates/eureka/src/agent/tool.rs` | [SPEC-0010](SPEC-0010.md), [SPEC-0011](SPEC-0011.md) |
| `crates/eureka/src/agent/error.rs` | [SPEC-0010](SPEC-0010.md) |
| `crates/eureka/src/control/mod.rs` | [SPEC-0011](SPEC-0011.md) |
| `crates/eureka/src/control/node.rs` | [SPEC-0011](SPEC-0011.md) |
| `crates/eureka/src/control/process.rs` | [SPEC-0011](SPEC-0011.md) |
| `crates/eureka/src/control/def.rs` | [SPEC-0011](SPEC-0011.md) |
| `crates/eureka/src/control/error.rs` | [SPEC-0011](SPEC-0011.md) |
| `crates/eureka/src/session/mod.rs` | [SPEC-0012](SPEC-0012.md) |
| `crates/eureka/src/session/runner.rs` | [SPEC-0012](SPEC-0012.md) |
| `crates/eureka/src/session/builder.rs` | [SPEC-0012](SPEC-0012.md) |
| `crates/eureka/src/config/mod.rs` | [SPEC-0013](SPEC-0013.md) |
| `crates/eureka/src/config/budget.rs` | [SPEC-0013](SPEC-0013.md), [SPEC-0014](SPEC-0014.md) |
| `crates/eureka/src/config/provider.rs` | [SPEC-0013](SPEC-0013.md) |
| `crates/eureka/src/config/scheduler.rs` | [SPEC-0013](SPEC-0013.md) |
| `crates/eureka/src/config/agent.rs` | [SPEC-0013](SPEC-0013.md) |
| `crates/eureka/src/config/tracing.rs` | [SPEC-0013](SPEC-0013.md) |
| `crates/eureka/src/error.rs` | [SPEC-0015](SPEC-0015.md) |
| `crates/eureka/src/config/rag.rs` | [SPEC-0013](SPEC-0013.md), [SPEC-0018](SPEC-0018.md) |
| `crates/eureka/src/manager.rs` | [SPEC-0017](SPEC-0017.md) |
| `crates/eureka/src/rag/mod.rs` | [SPEC-0018](SPEC-0018.md) |
| `crates/eureka/src/rag/embedding.rs` | [SPEC-0018](SPEC-0018.md) |
| `crates/eureka/src/rag/handle.rs` | [SPEC-0018](SPEC-0018.md) |
| `crates/eureka/src/rag/indexer.rs` | [SPEC-0018](SPEC-0018.md) |
| `crates/eureka/src/rag/init.rs` | [SPEC-0018](SPEC-0018.md) |
| `crates/eureka/src/persistence/mod.rs` | [SPEC-0015](SPEC-0015.md) |
| `crates/eureka/src/persistence/store.rs` | [SPEC-0015](SPEC-0015.md) |
| `crates/eureka/src/persistence/record.rs` | [SPEC-0015](SPEC-0015.md) |
| `crates/eureka/src/persistence/checkpoint.rs` | [SPEC-0015](SPEC-0015.md) |
| `crates/eureka/src/persistence/environment.rs` | [SPEC-0011](SPEC-0011.md), [SPEC-0015](SPEC-0015.md) |
| `crates/eureka/src/persistence/sqlite.rs` | [SPEC-0015](SPEC-0015.md) |
| `crates/eureka/src/persistence/memory.rs` | [SPEC-0015](SPEC-0015.md) |
| `crates/eureka-cli/src/server.rs` | [SPEC-0019](SPEC-0019.md) |
| `crates/eureka/src/manifest/agent.rs` | [SPEC-0008](SPEC-0008.md), [SPEC-0021](SPEC-0021.md) |
| `crates/eureka/src/agent/def.rs` | [SPEC-0010](SPEC-0010.md), [SPEC-0021](SPEC-0021.md) |
| `AGENTS.md` | [SPEC-0001](SPEC-0001.md) |
| `specs/` | [SPEC-0001](SPEC-0001.md) |

## SPEC registry

| SPEC | Title | Status | Type |
|------|-------|--------|------|
| [SPEC-0001](SPEC-0001.md) | Meta-SPEC — Format and Process | Draft  | Process |
| [SPEC-0002](SPEC-0002.md) | Artifact System | Draft  | Core |
| [SPEC-0003](SPEC-0003.md) | Port System | Draft  | Core |
| [SPEC-0004](SPEC-0004.md) | Edge System | Draft  | Core |
| [SPEC-0005](SPEC-0005.md) | Node Interface | Draft  | Core |
| [SPEC-0006](SPEC-0006.md) | Graph Specification | Draft  | Core |
| [SPEC-0007](SPEC-0007.md) | Graph Validation | Draft  | Core |
| [SPEC-0008](SPEC-0008.md) | Manifest Format | Draft  | Interface |
| [SPEC-0009](SPEC-0009.md) | Scheduler Protocol | Draft  | Core |
| [SPEC-0010](SPEC-0010.md) | Agent Node Protocol | Draft  | Core |
| [SPEC-0011](SPEC-0011.md) | Control Node Protocol | Draft  | Interface |
| [SPEC-0012](SPEC-0012.md) | Session Lifecycle | Draft  | Core |
| [SPEC-0013](SPEC-0013.md) | Configuration Schema | Draft  | Interface |
| [SPEC-0014](SPEC-0014.md) | Budget and Cost Tracking | Draft  | Core |
| [SPEC-0015](SPEC-0015.md) | Engine Error Types | Draft  | Core |
| [SPEC-0016](SPEC-0016.md) | Persistence Schema | Draft  | Interface |
| [SPEC-0017](SPEC-0017.md) | RunManager | Draft  | Core |
| [SPEC-0018](SPEC-0018.md) | RAG Integration | Draft  | Core |
| [SPEC-0019](SPEC-0019.md) | HTTP API and Observability | Draft  | Interface |
| [SPEC-0021](SPEC-0021.md) | MCP Tool Integration       | Draft  | Interface |
