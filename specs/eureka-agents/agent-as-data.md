# Spec: Agent as Data

> **Status:** Stable
> **Crate:** `eureka-agents`
> **Files:** `def.rs`, `node.rs`

## Purpose

Agents are defined entirely as data — a prose preamble (`.md`) and a schema/config
(`.json`) — rather than as Rust structs. A single generic `LlmAgentNode` can drive any
agent by loading its definition at startup. Changing agent behavior requires only editing
files, not recompiling.

## Design

### AgentDef

```rust
pub struct AgentDef {
    pub name:          String,
    pub description:   Option<String>,
    pub preamble:      String,             // contents of <name>.md
    pub inputs:        Vec<AgentPort>,     // declared input ports
    pub outputs:       Vec<AgentPort>,     // declared output ports
    pub config:        AgentConfig,
    pub output_schema: serde_json::Value,  // JSON Schema for the submit tool
    pub tools:         Vec<ToolDef>,       // shell tools; empty if none declared
}

pub struct AgentPort {
    pub kind: ArtifactKind,  // artifact kind consumed or produced
    pub port: String,        // port name as referenced in GraphSpec edges
}

pub struct AgentConfig {
    pub temperature:    f64,   // default: 0.7
    pub max_iterations: u32,   // default: 10 — max loop turns before forced halt
}
```

`output_schema` becomes the parameter schema for the `submit` tool in the agent loop.
The agent is responsible for calling `submit` with JSON that matches this schema.

### File Layout

Each agent is a pair of files in a graph's `agents/` directory:

```
graphs/<name>/agents/
  generation.md    ← system preamble / role prompt
  generation.json  ← port declarations, output schema, config
```

**`<name>.md`** — The system preamble passed verbatim to the LLM as the system message.
Defines the agent's role and any domain constraints. Does not need to mention the `submit`
tool — the framework appends that instruction automatically.

**`<name>.json`** — Declares the agent's interface:

```json
{
  "inputs": [
    { "port": "in",      "kind": "Goal"     },
    { "port": "context", "kind": "Insights" }
  ],
  "outputs": [
    { "port": "out", "kind": "Hypotheses" }
  ],
  "output_schema": {
    "type": "object",
    "properties": {
      "hypotheses": { "type": "array", "items": { "..." } }
    },
    "required": ["hypotheses"]
  },
  "config": {
    "temperature": 0.8,
    "max_iterations": 10
  }
}
```

### Loading

```rust
// Load one agent by name
let def = AgentDef::load(Path::new("graphs/coscientist/agents"), "generation")?;

// Load all agents in a directory (all .json + .md pairs)
let defs = AgentDef::load_all(Path::new("graphs/coscientist/agents"))?;
```

`load_all` scans for `.json` files that have a matching `.md` sibling. Files without
a pair are silently skipped. Results are sorted alphabetically by agent name before
being returned.

### LlmAgentNode

`LlmAgentNode` is the single generic node that drives any `AgentDef`:

```rust
pub struct LlmAgentNode {
    def:    Arc<AgentDef>,
    client: Arc<dyn LlmClient>,
}
```

On `process(ctx, msg)`:

1. Build an initial message from the incoming artifact's `data` (serialized as
   pretty-printed JSON). For multi-input agents (`inputs.len() > 1`), prefix the
   message with `"Port: <name>\n\n"` so the LLM knows which artifact kind it is
   receiving.
2. Call `client.run_agent_loop(preamble, output_schema, tools, initial_message,
   max_iterations, temperature)`. The agent reasons freely, optionally calling shell
   tools, then calls `submit` to deliver its structured output.
3. For **single-output** agents: wrap the returned `Value` in `Artifact { kind, data }`
   and emit on the one declared output port.
4. For **multi-output** agents: the returned `Value` must be a JSON object with a key
   per output port name. Each key's value is emitted as an `Artifact` on the
   corresponding port. Missing keys produce `Artifact { data: null }`.

### `AgentLoadError`

`AgentLoadError` is defined in `def.rs` and returned by `AgentDef::load` and
`AgentDef::load_all`. It is wrapped by `AgentError::Load` when load errors propagate
through the execution layer.

```rust
pub enum AgentLoadError {
    Io    { path: String, source: std::io::Error },   // file could not be read
    Parse { path: String, source: serde_json::Error }, // JSON malformed
    NoInputs  { name: String },  // definition declares no input ports
    NoOutputs { name: String },  // definition declares no output ports
}
```

`Io` is returned for both the `.md` and the `.json` file reads. `Parse` is returned
only for the `.json` parse step.

### Port Spec Derivation

`AgentDef::to_port_spec()` converts the JSON port declarations to a `PortSpec` for
registration in the `PortRegistry` and graph validation:

```rust
impl AgentDef {
    pub fn to_port_spec(&self) -> PortSpec { ... }
}
```

## Invariants

- Every `.json` file in `agents/` must have a corresponding `.md` file.
- `inputs` in the JSON file uses the array form `"inputs": [...]`.
  The legacy singular `"input": {}` form is still accepted for backward compatibility.
- `temperature` defaults to `0.7`; `max_iterations` defaults to `10` if absent.
- Agent names default to the filename stem (e.g., `generation.json` → name `"generation"`),
  but can be overridden by a `"name"` field in the `.json` file.
- `output_schema` is the schema for `submit`'s parameters, not a response format hint.

## Non-Goals

- `AgentDef` does not validate JSON schema correctness.
- `LlmAgentNode` does not retry on LLM failure — that is the client's responsibility.
- Agents cannot be hot-reloaded without a process restart.

## Open Questions

- Should `max_iterations` be overridable per graph node config (in `graph.json`) rather
  than only in the agent `.json` file?
- Should agents have access to their round number and prior outputs for self-reflection?
