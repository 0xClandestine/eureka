# Spec: LLM Client

> **Status:** Stable
> **Crate:** `eureka-agents`
> **Files:** `client.rs`

## Purpose

`LlmClient` is the type-erased interface between agent nodes and LLM providers. Rather than a single-shot JSON extraction, each agent runs a full **agentic loop**: it receives an input message, reasons across one or more turns, and terminates when it calls the `submit` tool with its structured output.

## Design

### The Agentic Loop

```
initial_message
     │
     ▼
┌─────────────────────────────────────────────────────┐
│  LLM turn                                           │
│  Tools available: submit(json), <CommandTools...>   │
└──────────┬──────────────────────────────────────────┘
           │
    tool called?
     ├─ submit(json)    ──► return json   ← loop exits
     ├─ CommandTool(…)  ──► spawn subprocess, return stdout, continue
     └─ text only       ──► continue (up to max_iterations)
```

The agent's `.md` preamble describes its role. The framework appends a standard instruction
to call `submit` when done. The `submit` tool's parameter schema is the agent's
`output_schema` from its `.json` definition — so the LLM provider enforces the output
shape at the tool-call level, not via fragile text parsing.

### `LlmClient` Trait

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn run_agent_loop(
        &self,
        preamble:        &str,
        output_schema:   &serde_json::Value,
        tools:           &[ToolDef],
        initial_message: &str,
        max_iterations:  u32,
        temperature:     f64,
    ) -> Result<serde_json::Value, AgentError>;
}
```

`tools` are the shell tools declared in the agent's `.json` file. Each becomes a
`CommandTool` added to the Rig agent builder alongside the implicit `submit` tool.

### `RigClient<M>`

The production implementation wraps any `rig_core::completion::CompletionModel`:

1. Creates a `Submit` tool with `output_schema` as its parameter schema.
2. Calls `build_preamble(preamble, tool_names)` to append a submit/tool instruction:
   - No tools: appends "call the `submit` tool with your structured output."
   - With tools: appends "You have access to tools: `<names>`. … call `submit` when ready."
3. Creates a `CommandTool` for each entry in `tools`.
4. Builds a Rig `Agent` with the augmented preamble, temperature, `submit` tool, and all `CommandTool`s.
5. Calls `.prompt(initial_message).max_turns(max_iterations).await`.
6. After the loop, reads the submit slot — if empty, returns `AgentError::ExtractionFailed`.

### `Submit` Tool

`Submit` is internal to `client.rs` and not part of the public API.

```rust
struct Submit {
    schema: serde_json::Value,              // becomes the tool's parameter schema
    result: Arc<Mutex<Option<Value>>>,      // written on call, read after loop
}

impl Tool for Submit {
    const NAME: &'static str = "submit";
    type Args  = serde_json::Value;         // accepts any JSON; shape enforced by provider
    type Output = String;
    type Error  = SubmitError;
}
```

`Submit` is created fresh per `run_agent_loop` call. It is always the first tool registered
in the `AgentBuilder`. `CommandTool` instances (one per declared tool) are added after it
via `.tools(command_tools)`.

### Provider Selection

`build_llm_client()` in `run.rs` constructs a `RigClient` for each distinct model ID
needed. A `client_cache: HashMap<String, Arc<dyn LlmClient>>` ensures only one `RigClient`
is built per model ID — agents sharing the same model reuse the same instance. The
resolved model ID comes from `ProviderConfig.agent_models[agent_name]` if present,
otherwise from `ProviderConfig.generation_model`, otherwise from the compile-time
fallback `"deepseek/deepseek-v4-flash"` (OpenRouter default only).

| ProviderKind | Env var required |
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
| `ollama` | `OLLAMA_API_BASE_URL` _(optional; defaults to `http://localhost:11434`)_ |

The only hardcoded default model is `"deepseek/deepseek-v4-flash"` when the provider
is `openrouter` and `generation_model` is unset. All other providers require an explicit
`generation_model` in `eureka.toml`.

## Invariants

- `run_agent_loop` returns `AgentError::ExtractionFailed` if `max_iterations` is exhausted
  without a `submit` call.
- `output_schema` is used verbatim as the submit tool's parameter schema — agent `.json`
  files fully control the output shape.
- The `.md` preamble is never modified; the submit instruction is appended as a separate
  paragraph so preambles don't need to mention tools.
- At most one `RigClient` is created per distinct model ID; agents sharing the same model
  ID share one client. `RigClient` must be `Send + Sync` to allow concurrent use.

## `build_preamble`

`build_preamble` is a private function in `client.rs` that augments the agent's system
preamble with submit/tool instructions:

```rust
fn build_preamble(preamble: &str, tool_names: &[&str]) -> String
```

- When `tool_names` is empty: appends a single instruction to call `submit` with
  structured output and not to write raw JSON.
- When `tool_names` is non-empty: appends an instruction listing the available tool
  names and directing the agent to call `submit` when ready.

The original preamble is never modified; the instruction is appended as a separate
paragraph separated by two newlines.

## Non-Goals

- `LlmClient` does not track token usage or cost (see open gaps in `INDEX.md`).
- `LlmClient` does not support streaming responses.
- `LlmClient` does not manage conversation history across graph-level rounds — each
  `process()` call starts a fresh loop.

## Open Questions

- Should different agent roles use different models? Per-agent overrides are supported
  via `ProviderConfig.agent_models` and the client-cache in `build_registry`. The
  `ranking_model` field no longer exists in `ProviderConfig` — overrides are keyed by
  agent name (e.g. `agent_models.reflection = "..."`).
- Should `run_agent_loop` return iteration count alongside the value for cost estimation?
