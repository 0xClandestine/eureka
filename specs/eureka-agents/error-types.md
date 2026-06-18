# Spec: Agent Error Types

> **Status:** Stable
> **Crate:** `eureka-agents`
> **Files:** `error.rs`, `def.rs`

## Purpose

`eureka-agents` exposes two error types:

- `AgentError` — errors that occur during agent **execution** (LLM call failed, output
  unparseable, agent timed out, etc.). Returned by `LlmClient::run_agent_loop` and
  `LlmAgentNode::process`.
- `AgentLoadError` — errors that occur when **loading** an agent definition from disk
  (file not found, JSON malformed, no ports declared). Returned by `AgentDef::load`
  and `AgentDef::load_all`.

## Design

### `AgentError` (`error.rs`)

```rust
#[derive(Debug, Error)]
pub enum AgentError {
    /// The LLM returned output that could not be parsed as valid JSON,
    /// or the agent exhausted max_iterations without calling submit.
    #[error("Extraction failed: {0}")]
    ExtractionFailed(String),

    /// The LLM provider returned an error (network, auth, rate-limit, etc.).
    #[error("Provider error: {0}")]
    Provider(String),

    /// An agent definition could not be loaded (wraps AgentLoadError).
    #[error("Agent load error: {0}")]
    Load(#[from] AgentLoadError),

    /// A serde_json serialization or deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
```

`AgentError` implements `From<AgentLoadError>` and `From<serde_json::Error>` via the
`#[from]` derives, so both can be propagated with `?`.

`NodeError::Agent(String)` in the graph layer is constructed from `AgentError` via
`.to_string()` inside `LlmAgentNode::process`.

### `AgentLoadError` (`def.rs`)

```rust
#[derive(Debug, Error)]
pub enum AgentLoadError {
    /// A file could not be read (applies to both the .md and .json files).
    #[error("Failed to read '{path}': {source}")]
    Io { path: String, source: std::io::Error },

    /// The .json file could not be parsed.
    #[error("Failed to parse '{path}': {source}")]
    Parse { path: String, source: serde_json::Error },

    /// The agent definition declares no input ports (neither "inputs" nor "input").
    #[error("Agent '{name}' declares no input ports")]
    NoInputs { name: String },

    /// The agent definition declares no output ports ("outputs" is empty).
    #[error("Agent '{name}' declares no output ports")]
    NoOutputs { name: String },
}
```

`AgentLoadError` is defined in `def.rs` alongside `AgentDef` because the validation
that produces `NoInputs`/`NoOutputs` is part of the loading logic.

## When Each Variant Is Returned

| Variant | Trigger |
|---|---|
| `AgentLoadError::Io` | `fs::read_to_string` fails for `<name>.md` or `<name>.json` |
| `AgentLoadError::Parse` | `serde_json::from_str` fails for `<name>.json` |
| `AgentLoadError::NoInputs` | Both `"inputs"` array and legacy `"input"` field are absent or empty |
| `AgentLoadError::NoOutputs` | `"outputs"` array is present but empty |
| `AgentError::ExtractionFailed` | Agent loop exhausted `max_iterations` without calling `submit` |
| `AgentError::Provider` | Rig completion model returns an error; also on Mutex poison |
| `AgentError::Load` | `AgentLoadError` propagated via `?` |
| `AgentError::Serialization` | `serde_json::to_string_pretty` fails inside `LlmAgentNode::process` |

## Invariants

- `AgentLoadError` is always produced with a human-readable file path in the `path`
  field so diagnostics can pinpoint the offending file.
- `AgentError::Load` is a transparent wrapper — callers can match on the inner
  `AgentLoadError` with `if let AgentError::Load(inner) = err`.
- Neither error type implements `Clone`; they are not reusable values.

## Non-Goals

- Neither type carries structured retry metadata or error codes beyond the variants
  listed above.
- Token-budget exceeded and cost-limit errors are not yet represented (open gap).
