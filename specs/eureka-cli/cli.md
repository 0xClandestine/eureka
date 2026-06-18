# Spec: CLI

> **Status:** Stable
> **Crate:** `eureka-cli`
> **Files:** `main.rs`, `commands/`

## Purpose

The `eureka` binary is the primary entry point. It provides four subcommands for
running, validating, listing, and A/B-testing graph topologies.

## Commands

### `eureka run <goal>`

Run a full research session.

```
ARGS:
  <goal>                   Research goal (title string)

OPTIONS:
  -d, --description <s>    Extended description of the research question
      --domain <s>         Domain of study [default: general]
  -r, --max-rounds <n>     Override maximum rounds from config
  -o, --output <path>      Write RunStats to JSON file
  -v, --verbose            More detailed log output
      --port <n>           UI server port [default: 7773] (0 = disable)
  -c, --config <path>      Config file [default: eureka.toml] (global flag)
```

**`RunArgs` struct** (constructed in `main.rs`, consumed by `commands/run.rs`):

| Field | Type | Source |
|---|---|---|
| `config_path` | `String` | `--config` |
| `goal` | `String` | positional |
| `description` | `Option<String>` | `--description` |
| `domain` | `String` | `--domain` (default `"general"`) |
| `max_rounds` | `Option<u32>` | `--max-rounds` |
| `output` | `Option<String>` | `--output` |
| `verbose` | `bool` | `--verbose` |
| `port` | `u16` | `--port` (default `7773`) |

**Sequence in `execute(RunArgs)`:**
1. Load `EurekaConfig` from `--config` file; fall back to `EurekaConfig::default()` if missing.
2. If `--max-rounds` was given, override `config.budget.max_rounds`.
3. Generate a `uuid::Uuid::now_v7()` session ID.
4. Construct the goal as `serde_json::json!({ "goal", "description", "domain" })`.
5. Call `SessionDb::create(session_id, graph_id, goal_json)` — non-fatal; `db` is `Option<Arc<SessionDb>>`.
6. Call `build_registry(&config, session_id, db_path)` — loads agents and plugins.
7. Call `Session::new_with_id(config, registry, session_id)`.
8. Call `session.set_db(db)` if DB creation succeeded.
9. Create a `broadcast::channel(256)` and call `session.set_event_broadcaster(event_tx.clone())`.
10. If `port > 0`: create `Arc<Mutex<LiveState>>`, spawn `track_live_state(rx, live)`, call
    `start_server(session.spec().clone(), event_tx, live, port)`.
11. Call `session.run(goal).await` — blocks until complete or budget exhausted.
12. Write `RunStats` as pretty JSON to `--output` path if provided.

### `eureka validate <graph>`

Validate a graph specification file without running it.

```
ARGS:
  <graph>    Path to graph.json or graph.toml
```

Builds a `PortRegistry` by calling `build_registry_for_validation(graph_dir)`, which
loads real agent definitions from `<graph_dir>/agents/` and real plugin manifests via
`PluginRegistry::discover(graph_dir)`. No node kinds are hardcoded.

Runs `validate_graph(&spec, &registry)` and prints a summary. Exits non-zero if the
graph is invalid.

Output:
```
Graph: AI Co-Scientist
  Nodes: 7
  Edges: 8
✓ Graph specification is valid.
```

### `eureka list`

Print a static catalogue of available agents, control nodes, tools, and shipped graph
specs. Does not load any files — content is hardcoded in `commands/list.rs`.

The catalogue includes:
- **Scientific agents:** `generation`, `reflection`, `ranking`, `evolution`,
  `proximity`, `meta_review`
- **Control nodes:** `control.governor`, `control.router`, `control.merge`,
  `control.broadcast`, `control.human_gate`
- **Tools:** `web_search`, `fetch`
- **Shipped graph specs:** `graphs/coscientist/graph.json`,
  `graphs/debate/graph.json`, `graphs/jury/graph.json`

> Note: the control node names printed here (`control.governor`, etc.) are aspirational
> labels from an earlier design. The actual plugin node kinds in use are
> `round-governor`, `elo-ranker`, and `jaccard-dedup`. The `debate` and `jury` graph
> files do not currently exist on disk.

### `eureka eval <graph_a> <graph_b> <goal>`

A/B compare two graph topologies on the same goal.

```
ARGS:
  <graph_a>    First graph spec path
  <graph_b>    Second graph spec path
  <goal>       Research goal string

OPTIONS:
  -o, --output <path>    Write JSON comparison to file
  -c, --config <path>    Config file [default: eureka.toml] (global flag, currently unused)
```

**Current implementation:** uses `EurekaConfig::default()` regardless of `--config`;
runs both sessions with a minimal stub registry (`build_eval_registry()`) containing
only a single `test.node` entry backed by `TestNode` (emits a static "Evaluation
complete." artifact). Both sessions run sequentially.

Output JSON (written to `--output` if provided):

```json
{
  "graph_a": "path/to/a.json",
  "graph_b": "path/to/b.json",
  "stats_a": { "rounds": 1, "elapsed_secs": 0.01, "total_cost_usd": 0.0, "total_tokens": 0 },
  "stats_b": { "rounds": 1, "elapsed_secs": 0.01, "total_cost_usd": 0.0, "total_tokens": 0 }
}
```

## Invariants

- The UI server is always started before `session.run()`.
- `--port 0` completely disables the server (no axum binding, broadcast sender is dropped).
- Config file absence is non-fatal for `run` — defaults are used with a warning logged.
- Session DB creation failure is non-fatal — the session continues without persistence.
- `build_registry_for_validation()` (validate) is data-driven: it reads the same agent
  `.json` files and plugin manifests as `build_registry()` (run). There is no hardcoded
  port table.

## Non-Goals

- `eureka eval` does not currently use real LLM agents or the full `build_registry()` pipeline.
- `eureka list` does not read the filesystem; it cannot reflect dynamic agent or plugin changes.
