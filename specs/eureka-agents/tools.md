# Spec: Agent Tools

> **Status:** Stable
> **Crate:** `eureka-agents`
> **Files:** `def.rs`, `tools.rs`, `client.rs`

## Purpose

Each agent can declare a set of tools in its `.json` file. Tools are shell commands —
the agent calls them by name during its loop, and the framework spawns the subprocess,
feeds the call arguments as JSON on stdin, and returns stdout back to the agent. This
lets agents gather external information before calling `submit`.

The `submit` tool is always present and implicit — it is never declared in `tools`.

## Design

### `ToolDef` in the Agent JSON

```json
{
  "inputs":  [...],
  "outputs": [...],
  "output_schema": { ... },
  "config":  { "temperature": 0.9, "max_iterations": 12 },
  "tools": [
    {
      "name":        "arxiv_search",
      "description": "Search arXiv literature or fetch a paper's full text. Use mode='search' to find papers, mode='fetch' to retrieve full text.",
      "command":     ["python3", "graphs/coscientist/tools/arxiv_search.py"],
      "args_schema": {
        "type": "object",
        "required": ["mode"],
        "properties": {
          "mode":        { "type": "string", "enum": ["search", "fetch"] },
          "query":       { "type": "string",  "description": "Search terms (mode=search)" },
          "arxiv_id":    { "type": "string",  "description": "ArXiv paper ID e.g. '2404.01234' (mode=fetch)" },
          "max_results": { "type": "integer", "description": "Results per page, default 5, max 20 (mode=search)" },
          "page":        { "type": "integer", "description": "Zero-based page number for pagination, default 0 (mode=search)" }
        }
      },
      "timeout_secs": 60
    }
  ]
}
```

Agents with no tools omit the `"tools"` key entirely. They receive only `submit`.

### `ToolDef` Struct

```rust
pub struct ToolDef {
    pub name:         String,
    pub description:  String,
    /// argv[0] is the binary. Tokens of the form `{{arg_name}}` are interpolated
    /// from the LLM's tool call arguments before execution.
    pub command:      Vec<String>,
    /// JSON Schema for the arguments the LLM must supply when calling the tool.
    pub args_schema:  serde_json::Value,
    /// Maximum wall-clock time for the subprocess. Default: 30s.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,
}
```

`name` must not be `"submit"` (reserved). Names must be unique within an agent's tool list.

### Invocation Model

When the agent calls a tool, `CommandTool::call(args_json)` is invoked:

```
1. Template-substitute {{arg_name}} tokens in the command argv.
   e.g. ["cmd", "--query={{query}}"] + {query: "CO2 catalysts"}
     →  ["cmd", "--query=CO2 catalysts"]

2. Spawn the subprocess.
   - Working dir: process cwd (repo root when running `eureka` normally).
   - Stdin:  full args JSON written to stdin (scripts read structured input here).
   - Stdout: captured.
   - Stderr: captured; first 500 chars returned to agent on non-zero exit.

3. Wait up to timeout_secs.
   - On timeout: return the string `"Error: timed out after Ns"` to the agent.
     The subprocess is not explicitly killed — `wait_with_output` is abandoned and
     the child may continue running until its pipe is closed by the OS.

4. On exit code 0: return trimmed stdout as the tool result string (max 64 KB).
5. On non-zero exit: return `"Error (exit N): <stderr snippet>"` as the result,
   where the stderr snippet is at most 500 Unicode chars. The agent sees this and
   may retry or work around the error.
```

The result string is injected into the conversation as the tool response. The agent
continues its reasoning loop with this new context.

### `CommandTool` Implementation

`CommandTool` implements Rig's `ToolDyn` trait (not the static `Tool` trait, because tool
names are determined at runtime from the JSON, not at compile time).

```rust
pub struct CommandTool {
    def: Arc<ToolDef>,
}

impl ToolDyn for CommandTool {
    fn name(&self) -> String { self.def.name.clone() }

    fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        Box::pin(std::future::ready(ToolDefinition {
            name:        self.def.name.clone(),
            description: self.def.description.clone(),
            parameters:  self.def.args_schema.clone(),
        }))
    }

    fn call<'a>(&'a self, args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(self.execute(args))
    }
}
```

`CommandTool`s are passed to the Rig agent builder via `.tools(vec![...])`.
`Submit` is added first via `.tool(submit)`.

### `run_agent_loop` with Tools

```rust
// in RigClient::run_agent_loop
let command_tools: Vec<Box<dyn ToolDyn>> = tools
    .iter()
    .map(|t| Box::new(CommandTool::new(Arc::new(t.clone()))) as Box<dyn ToolDyn>)
    .collect();

let agent = AgentBuilder::new(self.model.clone())
    .preamble(&full_preamble)
    .temperature(temperature)
    .tool(submit)           // always present; the exit mechanism
    .tools(command_tools)   // declared in agent JSON; may be empty
    .build();

agent.prompt(initial_message).max_turns(max_iterations as usize).await
```

The `LlmClient::run_agent_loop` signature accepts `tools: &[ToolDef]` from `AgentDef`.

## Template Substitution Rules

- `{{name}}` in a command token is replaced with the string value of `args["name"]`.
- Non-string values (integer, array, object) are serialized as compact JSON.
- If `{{name}}` refers to a missing key, the token is replaced with an empty string.
- Template substitution is token-level: `["cmd", "--q={{query}}"]` is supported.
- The full args JSON is always written to stdin regardless of template substitution.

## The Coscientist Tool: `arxiv_search`

The coscientist graph ships one tool, `arxiv_search`, used by `generation`,
`reflection`, and `evolution`. It is implemented in
`graphs/coscientist/tools/arxiv_search.py`.

### Modes

**`search`** — searches `librarian-bots/arxiv-metadata-snapshot` via the HuggingFace
datasets server REST API. No local dataset download; filtering is server-side.

Input:
```json
{ "mode": "search", "query": "attention mechanism transformers", "max_results": 5, "page": 0 }
```

Output:
```json
{
  "papers": [
    {
      "arxiv_id": "1706.03762",
      "title": "Attention Is All You Need",
      "abstract": "...",
      "authors": "Vaswani, A., Shazeer, N., et al.",
      "categories": "cs.CL cs.LG",
      "year": "2017",
      "doi": ""
    }
  ],
  "total_found": 142,
  "page": 0,
  "max_results": 5,
  "has_more": true
}
```

Pagination: increment `page` to retrieve subsequent result sets. `has_more` is `true`
when `(page + 1) * max_results < total_found`.

**`fetch`** — fetches and converts a specific paper to markdown via `arxiv2md`.

Input:
```json
{ "mode": "fetch", "arxiv_id": "1706.03762" }
```

Output:
```json
{
  "arxiv_id": "1706.03762",
  "text": "# Attention Is All You Need\n\n...",
  "truncated": false
}
```

If `arxiv2md` is not installed, `fetch` falls back to returning the abstract from
the metadata snapshot, with a `note` field indicating the fallback.

### Dependencies

- `search` mode: stdlib only (`urllib`).
- `fetch` mode: `pip install arxiv2md` for full paper text.

## Security Model

- Tools run with the same OS user as the `eureka` process.
- Commands are resolved via `PATH` — no shell interpolation; each token is a separate
  argv element. This prevents shell injection from LLM-supplied argument values.
- Output is capped at **64 KB**. Excess output is truncated with a `[truncated]` notice.
- Timeout defaults to 30 seconds per tool call, configurable per `ToolDef`.
- There is no sandbox. Tools are trusted scripts co-located with the graph.

## Invariants

- `"submit"` is reserved and must not appear in `"tools"`.
- Tool names within one agent must be unique.
- `command` must be a non-empty array. `command[0]` is the binary name or path.
- Tools are loaded at agent load time (`AgentDef::load`), not at invocation time.
- Tool execution is synchronous within a loop turn — the agent waits for the subprocess.

## Non-Goals

- Tools are not sandboxed or containerized.
- There is no tool result caching across loop iterations.
- Tools cannot write to or read from other agents' state.
