# Eureka Plugin System

**Status:** Stable
**Crate:** `eureka-plugins`
**Files:** `crates/eureka-plugins/src/{lib,manifest,registry,node,error}.rs`, `graphs/coscientist/plugins/*/plugin.json`, `graphs/coscientist/plugins/*/*.py`

---

## Purpose

Plugins are the extensibility mechanism for Eureka. A plugin is a subprocess-based program — written in any language — that can act as a **control node** in a graph, a **tool** available to agents, or both.

The `eureka-plugins` crate handles:
- `PluginManifest` / `NodeRoleConfig` / `ToolRoleConfig` / `PortDef` — serde types for `plugin.json`
- `PluginRegistry` — discovers and loads manifests from the filesystem
- `PluginEntry` — a loaded plugin (manifest + directory path)
- `ControlPluginNode` — implements the `Node` trait by spawning the plugin subprocess
- `PluginError` — error variants for discovery and invocation failures

> **Note:** The `tool` role (`PluginTool`) is declared in manifests but a `ToolDyn` implementation is not yet present in this crate. That is tracked as an open gap in `specs/INDEX.md`.

---

## Design

### Roles

A plugin declares one or more roles in its manifest:

- **`node`** — The plugin can be used as a graph node. The runtime calls it with an artifact on stdin and reads emitted artifacts from stdout.
- **`tool`** — The plugin can be used as an agent tool. The manifest declares `description` and `args_schema` for the LLM; invocation is not yet implemented.

A single plugin can have both roles.

### Runtime

All plugins in v1 use the **`process`** runtime: a subprocess spawned per invocation. The manifest specifies the command to run.

### Discovery Order

`PluginRegistry::discover(graph_dir)` scans two directories in order:

1. **User-global** — `~/.eureka/plugins/<name>/plugin.json` (lower priority; scanned first)
2. **Graph-local** — `<graph_dir>/plugins/<name>/plugin.json` (higher priority; scanned second, overrides global)

The second scan overwrites entries from the first. There is no built-in install-path search; graph-local plugins are the primary mechanism. Missing directories are silently skipped.

---

## Manifest Format

Every plugin lives in its own directory containing a `plugin.json` manifest:

```
plugins/
  elo-ranker/
    plugin.json
    ranker.py
  jaccard-dedup/
    plugin.json
    dedup.py
  round-governor/
    plugin.json
    governor.py
```

### Top-level `plugin.json` Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Unique plugin identifier. Must match the directory name. |
| `version` | string | yes | SemVer string. |
| `description` | string | yes | Human-readable description. |
| `runtime` | string | yes | Must be `"process"` in v1. |
| `command` | `string[]` | yes | Argv for the subprocess. `command[0]` is the binary. All paths are relative to the plugin directory. |
| `roles` | `string[]` | yes | Subset of `["node", "tool"]`. |
| `node` | object | if `"node"` in roles | Node role configuration (see below). |
| `tool` | object | if `"tool"` in roles | Tool role configuration (see below). |

`PluginManifest::has_node_role()` returns `true` iff `"node"` appears in `roles`.
`PluginManifest::has_tool_role()` returns `true` iff `"tool"` appears in `roles`.

### `node` Object (`NodeRoleConfig`)

| Field | Type | Default | Description |
|---|---|---|---|
| `inputs` | `PortDef[]` | — | Input ports this node accepts. Required. |
| `outputs` | `PortDef[]` | — | Output ports this node may emit on. Required. |
| `timeout_secs` | integer | 60 | Maximum wall-clock seconds to wait for the subprocess before returning `NodeError::Timeout`. |

`config_schema` may appear in `plugin.json` files as documentation/validation metadata, but it is **not** deserialized by the Rust `NodeRoleConfig` struct and has no effect at runtime.

### `PortDef`

Each entry in `inputs` and `outputs` is a `PortDef`:

| Field | Type | Description |
|---|---|---|
| `port` | string | Port name used in graph edges (e.g. `"in"`, `"top"`, `"unique"`). |
| `kind` | string | Artifact kind accepted or emitted on this port (e.g. `"Hypotheses"`, `"Ranking"`). |

Example:

```json
{
  "inputs":  [{"port": "in",    "kind": "Reviews"},
              {"port": "cycle", "kind": "Control"}],
  "outputs": [{"port": "top",   "kind": "Hypotheses"},
              {"port": "state", "kind": "Ranking"}]
}
```

### `tool` Object (`ToolRoleConfig`)

| Field | Type | Default | Description |
|---|---|---|---|
| `description` | string | — | Shown to the LLM as the tool description. |
| `args_schema` | JSON Schema | — | Parameter schema given to the LLM. |
| `timeout_secs` | integer | 30 | Maximum wall-clock seconds to wait for the subprocess. |

---

## `PluginRegistry`

```rust
pub struct PluginRegistry { ... }

impl PluginRegistry {
    pub fn empty() -> Self
    pub fn discover(graph_dir: &Path) -> Result<Self, PluginError>
    pub fn len(&self) -> usize
    pub fn is_empty(&self) -> bool
    pub fn get(&self, name: &str) -> Option<&PluginEntry>
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PluginEntry)>
}
```

`PluginEntry` holds:
- `manifest: PluginManifest` — the parsed `plugin.json`
- `plugin_dir: PathBuf` — absolute path to the directory containing `plugin.json` and the scripts

---

## `ControlPluginNode`

Constructor:

```rust
pub fn new(
    manifest: PluginManifest,
    plugin_dir: PathBuf,
    session_id: String,
    db_path: Option<PathBuf>,
    config: serde_json::Value,
) -> Self
```

Implements `Node`. `ports()` is derived from `manifest.node.inputs` and `manifest.node.outputs`. All input ports have `required: true`; all output ports have `required: false`.

---

## Node Invocation Protocol

When the scheduler activates a `ControlPluginNode`:

1. Environment variables are set (see below).
2. A **call envelope** is written to the subprocess stdin (UTF-8 JSON, no trailing newline; stdin is closed immediately after the write to signal EOF):

```json
{"port": "in", "artifact": {"kind": "Hypotheses", "data": { ... }}}
```

3. The subprocess writes zero or more **emit envelopes** to stdout, one per line:

```json
{"port": "top",   "artifact": {"kind": "Hypotheses", "data": { ... }}}
{"port": "state", "artifact": {"kind": "Ranking",    "data": { ... }}}
```

4. Stdout is captured up to **64 KB** (`MAX_OUTPUT_BYTES`). Lines beyond that limit are silently truncated.
5. The timeout enforced is `node.timeout_secs` from the manifest (default 60 s). On expiry, `NodeError::Timeout` is returned.
6. On non-zero exit, stderr (up to 500 characters) is included in a `NodeError::Internal` message.

A plugin may emit on multiple ports in one invocation by printing multiple JSON lines.

If `artifact.data` is absent in an emit envelope, it defaults to `null`.

---

## Environment Variables

Every `ControlPluginNode` subprocess receives:

| Variable | Value |
|---|---|
| `EUREKA_SESSION_ID` | Session UUID string |
| `EUREKA_NODE_ID` | Node ID from `graph.json` |
| `EUREKA_ROUND` | Current round number (integer, as a string) |
| `EUREKA_CONFIG` | JSON-encoded node `config` object from `graph.json` (defaults to `"{}"`) |
| `EUREKA_DB_PATH` | Absolute path to the session SQLite database (only set if `db_path` is `Some`) |

`EUREKA_DB_PATH` is **not set** (absent from the environment) when no database path was provided to `ControlPluginNode::new`. Plugins must handle the absent-variable case gracefully.

---

## `PluginError` Variants

| Variant | When |
|---|---|
| `ManifestRead { path, source }` | `plugin.json` could not be read from disk. |
| `InvalidManifest { path, message }` | `plugin.json` is not valid JSON or fails serde deserialization. |
| `DirectoryScan { path, source }` | A plugin search directory exists but could not be read (e.g., permission error). |
| `MissingNodeConfig { name }` | A manifest declares the `"node"` role but the `node` object is absent. |

---

## Built-in Plugins

Three reference plugins ship with the coscientist graph at `graphs/coscientist/plugins/`. They replace the previously hardcoded Rust `GovernorNode`, `RankingNode`, and `ProximityNode`.

### `round-governor`

**File:** `governor.py`
**Ports:** input `in` (Hypotheses) → output `continue` (Hypotheses) **or** `halt` (Control)
**Timeout:** 5 s
**Config:** `max_rounds` (integer, default 12)

Reads `EUREKA_ROUND` and `max_rounds` from `EUREKA_CONFIG`.

- If `round < max_rounds`: emits the input artifact data unchanged on port `"continue"` as a `Hypotheses` artifact. This passes the deduped hypotheses into the next reflection–ranking cycle.
- Otherwise: emits `{"signal": "halt", "round": <round_num>}` on port `"halt"` as a `Control` artifact. The scheduler terminates naturally when no downstream consumer exists.

The `"continue"` port emits `kind: "Hypotheses"` (not `kind: "Control"`).

### `elo-ranker`

**File:** `ranker.py`
**Ports:** inputs `in` (Reviews), `cycle` (Control) → outputs `top` (Hypotheses), `state` (Ranking)
**Timeout:** 15 s
**Config:** `items_field` (default `"reviews"`), `score_field` (default `"score"`), `item_field` (default `"hypothesis"`), `id_field` (default `"statement"`), `output_field` (default `"hypotheses"`), `top_k` (default 5), `k_factor` (default 32.0)

Performs pairwise Elo ranking over a batch of reviewed hypotheses.

**`in` port behaviour:** Reads reviews from `artifact.data[items_field]`. Each review must have `item_field` (the hypothesis object) and `score_field` (float). Items are identified by `item.id_field`. Pairwise Elo updates are applied: for each unique pair, the higher-scored item wins. Draws (equal scores) produce no rating change. Updated ratings are persisted to the session DB (see below). Always emits on both `top` and `state`.

**`cycle` port behaviour:** Skips Elo processing entirely; re-emits current top-K from whatever ratings are already stored.

**DB persistence:** When `EUREKA_DB_PATH` is set, ratings are loaded from and saved to an `elo_ratings` table (created on first use):

```sql
CREATE TABLE IF NOT EXISTS elo_ratings (
    session_id TEXT NOT NULL,
    node_id    TEXT NOT NULL,
    item_id    TEXT NOT NULL,
    item_json  TEXT NOT NULL,
    elo        REAL NOT NULL DEFAULT 1000.0,
    matches    INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, node_id, item_id)
)
```

Default Elo for new items: **1200.0**. DB errors are silently swallowed; the plugin continues with in-memory ratings.

**Emit shapes:**

`top` port:
```json
{"port": "top", "artifact": {"kind": "Hypotheses", "data": {"hypotheses": [...top-k items...]}}}
```

`state` port:
```json
{"port": "state", "artifact": {"kind": "Ranking", "data": {
  "hypotheses":  [...top-k items...],
  "elo_ratings": {"<item_id>": <elo_score>, ...},
  "count":       <total items in ratings>
}}}
```

`elo_ratings` in the `state` emit contains only the top-K items (not all rated items).

### `jaccard-dedup`

**File:** `dedup.py`
**Ports:** input `in` (Hypotheses) → output `unique` (Hypotheses)
**Timeout:** 10 s
**Config:** `items_field` (default `"hypotheses"`), `text_field` (default `"statement"`), `threshold` (default 0.65), `output_field` (default `"hypotheses"`)

Removes items from the input array whose `text_field` is too similar to an item already accepted into the output. Similarity is word-level Jaccard:

```
jaccard(a, b) = |words(a) ∩ words(b)| / |words(a) ∪ words(b)|
```

Both strings are lowercased and split on whitespace before comparison. Two empty strings have similarity 1.0. An item is dropped if `jaccard(item, any_accepted) >= threshold`.

Items are processed in input order. The first item is always kept. Stateless (no DB access).

**Emit shape:**

```json
{"port": "unique", "artifact": {"kind": "Hypotheses", "data": {"hypotheses": [...unique items...]}}}
```

---

## Invariants

- Discovery is idempotent for a given pair of directories.
- Graph-local plugins always shadow user-global plugins with the same name.
- Subdirectories without a `plugin.json` are silently skipped during discovery.
- The subprocess `current_dir` is set to `plugin_dir`, so relative paths in `command` resolve correctly.
- Node output is capped at 64 KB; lines after the cap are silently dropped.
- `EUREKA_DB_PATH` is only set in the subprocess environment when a DB path was provided; plugins must tolerate the variable being absent.
- `config_schema` in `plugin.json` is documentation/convention only; it is not parsed or enforced by the Rust runtime.

---

## Non-Goals

- **Wasm or in-process runtimes** — only `"process"` is supported in v1.
- **Hot reload** — plugins are discovered once at session start.
- **Tool role invocation** (`PluginTool`) — `ToolDyn` impl for plugin-backed agent tools is not yet implemented (open gap).
- **Config schema validation** — `config_schema` in `plugin.json` is not validated at graph load time by the Rust runtime.
- **Built-in install-path** — there is no third `<eureka-install>/plugins/` search location; only user-global and graph-local are scanned.
