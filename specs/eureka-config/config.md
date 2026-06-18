# Spec: Configuration

> **Status:** Stable
> **Crate:** `eureka-config`
> **Files:** `model.rs`

## Purpose

`EurekaConfig` is the single configuration struct for a research run. It is loaded from
`eureka.toml`, with defaults applied for any missing fields. It controls which graph to
run, which LLM provider to use, scheduler concurrency, and budget limits.

## Design

### Structure

```rust
pub struct EurekaConfig {
    pub graph:     String,          // path to graph.json
    pub provider:  ProviderConfig,
    pub scheduler: SchedulerConfig,
    pub budget:    BudgetConfig,
}
```

### Graph Path

`graph` defaults to `"graphs/coscientist/graph.json"`. The agents directory is
**not configured** — it is always derived as `<graph_dir>/agents/`. Changing the
graph automatically selects the correct agents, plugins, and tools.

### Provider

```rust
pub struct ProviderConfig {
    pub kind:             ProviderKind,                       // openrouter | anthropic | openai | ...
    pub generation_model: Option<String>,                     // default model for all agents
    pub agent_models:     HashMap<String, String>,            // per-agent overrides
}
```

`generation_model` is the default model used for every agent. `agent_models` is a
map from agent name to model ID — agents named in this map use the specified model
instead of `generation_model`. Agents not in the map fall back to `generation_model`.

```toml
[provider]
kind = "openrouter"
generation_model = "deepseek/deepseek-v4-flash"

[provider.agent_models]
reflection  = "anthropic/claude-opus-4-5"   # use a stronger model for review
meta_review = "anthropic/claude-opus-4-5"
```

Default: `openrouter` with model `deepseek/deepseek-v4-flash`, empty `agent_models`.

Multiple clients are created lazily — one per distinct model ID — and cached for
reuse across agents that share the same model.

Supported providers:

| `kind` | API key env var |
|---|---|
| `openrouter` | `OPENROUTER_API_KEY` |
| `anthropic` | `ANTHROPIC_API_KEY` |
| `openai` | `OPENAI_API_KEY` |
| `gemini` | `GEMINI_API_KEY` |
| `groq` | `GROQ_API_KEY` |
| `mistral` | `MISTRAL_API_KEY` |
| `cohere` | `COHERE_API_KEY` |
| `deepseek` | `DEEPSEEK_API_KEY` |
| `perplexity` | `PERPLEXITY_API_KEY` |
| `together` | `TOGETHER_API_KEY` |
| `xai` | `XAI_API_KEY` |
| `ollama` | `OLLAMA_API_BASE_URL` (optional, defaults to `http://localhost:11434`) |

### Scheduler

```rust
pub struct SchedulerConfig {
    pub max_in_flight: usize,  // default: 8
}
```

Controls how many node activations run concurrently.

### Budget

```rust
pub struct BudgetConfig {
    pub max_cost_usd:  f64,     // default: 25.0
    pub max_tokens:    u64,     // default: 5_000_000
    pub max_wallclock: String,  // default: "45m"
    pub max_rounds:    u32,     // default: 100
}
```

The budget is a **hard safety backstop** only. It fires if cost, token, or wall-clock
limits are exceeded, or as a last-resort round cap. The primary round controller is
the `round-governor` plugin configured in `graph.json` — set `max_rounds` in the
governor node's `config` block there. The `budget.max_rounds` value only fires if
the governor plugin fails to halt.

```toml
[budget]
max_cost_usd = 25.0
max_tokens   = 5_000_000
max_wallclock = "45m"
max_rounds   = 100      # hard cap; governor plugin controls normal termination
```

### Loading

```rust
EurekaConfig::load()              -> Result<EurekaConfig, ConfigError>  // stub — returns default()
EurekaConfig::from_toml(toml_str) -> Result<EurekaConfig, ConfigError>
EurekaConfig::default()           -> EurekaConfig
```

`EurekaConfig::load()` is a stub that currently returns `Ok(Self::default())`. Full
figment-based layered loading (file → env → CLI) is not yet implemented. For now,
`from_toml()` is called directly in the CLI.

### Duration Parsing

`to_graph_budget()` converts `max_wallclock` to seconds via the internal
`parse_duration()` helper. Supported suffix forms:

| Input | Seconds |
|---|---|
| `"30s"` | 30.0 |
| `"45m"` | 2700.0 |
| `"2h"` | 7200.0 |
| `"30"` | 30.0 (bare number, interpreted as seconds) |

If `parse_duration()` returns `None` (unrecognised format), `to_graph_budget()`
falls back to `2700.0` (45 minutes).

## Invariants

- All fields have defaults — an empty `eureka.toml` is valid.
- `agents_dir` does not exist in `EurekaConfig`. Derive it from `graph`.
- `StoreConfig` / `StoreBackend` do not exist — `eureka-store` has been removed.
- `convergence_epsilon` / `elo_plateau_epsilon` do not exist — convergence detection
  was removed. The round-governor plugin handles termination.

## Non-Goals

- `EurekaConfig` does not validate that the `graph` path exists.
- Environment variable overrides are not yet implemented.
