# Spec: build_registry

> **Status:** Stable
> **Crate:** `eureka-cli`
> **Files:** `commands/run.rs`, `commands/validate.rs`

## Purpose

`build_registry` constructs the `NodeRegistry` used by a research session. It
discovers all agent definitions and plugin manifests in the graph's directory and
registers a factory closure for each. A parallel function,
`build_registry_for_validation`, builds a lightweight `PortRegistry` used by the
`validate` command.

## `build_registry`

```rust
pub fn build_registry(
    config: &EurekaConfig,
    session_id: &str,
    db_path: Option<PathBuf>,
) -> Result<NodeRegistry>
```

### Steps

1. **Derive directories** from `config.graph`:
   - `graph_dir  = parent(config.graph)`
   - `agents_dir = graph_dir / "agents"`

2. **Load agent definitions** via `AgentDef::load_all(&agents_dir)`. Each `.md` +
   `.json` pair in the directory becomes an `AgentDef`.

3. **Build LLM clients with per-model caching.** For each agent:
   - Determine `model_id`: first checks `config.provider.agent_models[agent_name]`;
     falls back to `config.provider.generation_model`; ultimate default:
     `"deepseek/deepseek-v4-flash"`.
   - Look up `model_id` in `client_cache: HashMap<String, Arc<dyn LlmClient>>`.
     If absent, call `build_llm_client(config, model_id)` and insert into cache.
   - This ensures one provider connection per distinct model ID, not one per agent.

4. **Register each agent** as an `LlmAgentNode` factory in the `NodeRegistry` under
   the agent's `name` field.

5. **Discover plugins** via `PluginRegistry::discover(graph_dir)`. Discovery failure
   is non-fatal — a warning is logged and the function continues without plugins.

6. **Register each plugin** that declares the `node` role as a `ControlPluginNode`
   factory. Plugins without a `node` role are skipped. The factory captures:
   - `manifest: PluginManifest`
   - `plugin_dir: PathBuf`
   - `session_id: String`
   - `db_path: Option<PathBuf>`
   - `spec.config: serde_json::Value` (from the graph node's config block)

### Provider / API Key Mapping

`build_llm_client` dispatches on `config.provider.kind`:

| `ProviderKind` | Environment variable |
|---|---|
| `Anthropic` | `ANTHROPIC_API_KEY` |
| `OpenAI` | `OPENAI_API_KEY` |
| `OpenRouter` | `OPENROUTER_API_KEY` |
| `Gemini` | `GEMINI_API_KEY` |
| `Groq` | `GROQ_API_KEY` |
| `Mistral` | `MISTRAL_API_KEY` |
| `Cohere` | `COHERE_API_KEY` |
| `DeepSeek` | `DEEPSEEK_API_KEY` |
| `Perplexity` | `PERPLEXITY_API_KEY` |
| `Together` | `TOGETHER_API_KEY` |
| `XAI` | `XAI_API_KEY` |
| `Ollama` | `OLLAMA_API_BASE_URL` (optional; defaults to `http://localhost:11434`) |

A missing required environment variable causes `build_registry` to return an error
immediately.

## `build_registry_for_validation`

```rust
fn build_registry_for_validation(graph_dir: &Path) -> Result<PortRegistry>
```

Builds a `PortRegistry` (port-spec-only, no factory closures) for the `validate`
command. No LLM clients are constructed.

### Steps

1. If `<graph_dir>/agents/` exists, load all `AgentDef`s via `AgentDef::load_all` and
   register each as `reg.register(def.name, def.to_port_spec())`.
2. Discover plugins via `PluginRegistry::discover(graph_dir)`. Unlike
   `build_registry`, discovery failure here returns an error (the caller wraps it with
   context).
3. For each plugin with the `node` role, construct a `PortSpec` from the manifest's
   `node.inputs` / `node.outputs` and register it.

Missing directories (`agents/` absent) are silently skipped so validation works on
partial graph packages.

## Invariants

- `build_registry` and `build_registry_for_validation` derive subdirectories from the
  same convention: `parent(graph_path) / "agents"` and `parent(graph_path) / "plugins"`.
- Both functions register the same port specs for the same agent/plugin names (since
  both call `AgentDef::to_port_spec()`), guaranteeing that `validate` and `run` agree
  on port definitions.
- Client caching in `build_registry` is keyed on the resolved `model_id` string, not
  the agent name — multiple agents using the same model share one provider connection.
- Plugin nodes with `has_node_role() == false` are silently skipped in both functions.

## Non-Goals

- `build_registry` does not validate the graph topology — that is done separately by
  `validate_graph` in `eureka-graph`.
- `build_registry_for_validation` does not construct any `Node` instances; it only
  populates port metadata.
