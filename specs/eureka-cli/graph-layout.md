# Spec: Graph Layout

> **Status:** Stable
> **Files:** `graphs/*/graph.json`, `graphs/*/agents/`, `graphs/*/plugins/`, `graphs/*/tools/`

## Purpose

Defines the on-disk convention for graph packages: each graph lives in its own
directory containing the topology spec, agent files, plugin scripts, and agent tools.
A graph package is self-contained — copying its directory to another machine is
sufficient to run it.

## Layout

```
graphs/
  <graph-name>/
    graph.json              ← GraphSpec (nodes, edges, metadata)
    agents/
      <agent>.md            ← System preamble for the LLM agent
      <agent>.json          ← Schema, port declarations, tools, temperature
    plugins/
      <plugin-name>/
        plugin.json         ← Plugin manifest (ports, command, roles)
        <script>            ← Executable (Python, shell, etc.)
    tools/
      <tool>.py             ← Agent tool scripts called via CommandTool
```

### graph.json

A `GraphSpec` serialized as JSON. Required fields: `nodes`, `edges`. Optional:
`name`, `description`, `metadata`.

```json
{
  "name": "AI Co-Scientist",
  "description": "...",
  "nodes": [
    { "id": "generation", "kind": "generation",   "description": "..." },
    { "id": "ranking",    "kind": "elo-ranker",   "config": { "top_k": 5 } },
    { "id": "governor",   "kind": "round-governor","config": { "max_rounds": 5 } }
  ],
  "edges": [
    { "from_node": "generation", "from_port": "out",
      "to_node":   "reflection", "to_port":   "in" },
    { "from_node": "governor",   "from_port": "continue",
      "to_node":   "reflection", "to_port":   "in",   "feedback": true }
  ]
}
```

### Agent Files (`agents/`)

Each LLM-driven node kind needs a matching `.md` + `.json` pair in `agents/`.

**`<name>.md`** — The system prompt. Written in plain prose. Describes the agent's
role and any tools it should use.

**`<name>.json`** — Port declarations, output schema, per-agent config, and tool list:

```json
{
  "inputs":  [{ "kind": "Goal", "port": "in" }, { "kind": "Insights", "port": "context" }],
  "outputs": [{ "kind": "Hypotheses", "port": "out" }],
  "config":  { "temperature": 0.9, "max_iterations": 12 },
  "output_schema": { ... },
  "tools": [
    {
      "name":        "arxiv_search",
      "description": "Search arXiv literature or fetch a paper's full text.",
      "command":     ["python3", "graphs/coscientist/tools/arxiv_search.py"],
      "args_schema": { ... },
      "timeout_secs": 60
    }
  ]
}
```

### Plugin Files (`plugins/`)

Each plugin node kind needs a directory under `plugins/` containing a `plugin.json`
manifest and the executable script(s). Plugin nodes do not need agent files.

```json
{
  "name": "round-governor",
  "version": "1.0.0",
  "runtime": "process",
  "command": ["python3", "governor.py"],
  "roles": ["node"],
  "node": {
    "inputs":  [{ "port": "in",       "kind": "Hypotheses" }],
    "outputs": [{ "port": "continue", "kind": "Hypotheses" },
                { "port": "halt",     "kind": "Control"    }]
  }
}
```

Graph-local plugins shadow user-global plugins (`~/.eureka/plugins/`) of the same
name. See [plugins.md](../eureka-plugins/plugins.md) for the full discovery order.

### Tool Files (`tools/`)

Agent tool scripts live in `tools/`. They are referenced by path from the agent's
`command` field and are called by `CommandTool` at LLM tool-call time.

Scripts read JSON args from stdin and write a JSON result to stdout:

```
stdin  ← {"mode": "search", "query": "...", "max_results": 5, "page": 0}
stdout → {"papers": [...], "total_found": 42, "has_more": true}
```

## Directory Resolution

When running a graph, the CLI derives subdirectories automatically:

```
agents_dir  = parent(config.graph) / "agents"
plugins_dir = parent(config.graph) / "plugins"
tools_dir   = parent(config.graph) / "tools"
```

For `config.graph = "graphs/coscientist/graph.json"`:
- `agents_dir  = "graphs/coscientist/agents"`
- `plugins_dir = "graphs/coscientist/plugins"`
- `tools_dir   = "graphs/coscientist/tools"`

None of these directories are configured explicitly — all are derived from `graph`.

## Tool Path Convention

Tool `command` paths are relative to the process working directory (the repo root
when running `eureka` normally). The full path must be specified:

```json
"command": ["python3", "graphs/coscientist/tools/arxiv_search.py"]
```

## Shipped Graphs

| Directory | Description |
|---|---|
| `graphs/coscientist/` | Full co-scientist topology (generation → reflection → ranking → evolution → proximity → governor → meta-review) |

## Adding a New Graph

1. Create `graphs/<name>/graph.json`.
2. Create `graphs/<name>/agents/<agent>.md` and `<agent>.json` for each LLM node.
3. Create `graphs/<name>/plugins/<plugin>/plugin.json` + scripts for any control nodes.
4. Create `graphs/<name>/tools/<tool>.py` for any agent tools.
5. Reference it: `graph = "graphs/<name>/graph.json"` in `eureka.toml`.
6. Validate: `eureka validate graphs/<name>/graph.json`.

## Invariants

- Every `.md` file in `agents/` must have a matching `.json` file (and vice versa).
- Every LLM node kind referenced in `graph.json` must have a corresponding agent pair.
- Every plugin node kind referenced in `graph.json` must have a `plugin.json` manifest.
  Missing plugins are caught at startup by graph validation.
- `graph.json` uses JSON (not TOML) as the canonical format.
- Tool scripts are not validated at startup — errors surface at runtime when the LLM
  first calls the tool.
