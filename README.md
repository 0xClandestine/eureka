# Eureka

Eureka is a **graph-based AI agent execution engine** written in Rust. You define a network of LLM agents and control nodes in a YAML manifest — topology, prompts, output schemas, tools, and subprocess commands all live there. The runtime validates the graph, then runs it with a concurrent, budget-aware scheduler.

The engine is wholly generic. Artifact kinds, port names, agent prompts, and control logic are all manifest-defined. No recompile is needed to add a new agent, change a prompt, or rewire the graph.

## Why Eureka

**Multi-agent loops are hard to get right.** Most orchestration libraries glue LLM calls together in code, which means topology, control flow, and prompts are scattered across application logic. When the graph needs to change — a new review step, a different ranking strategy, a tighter budget — you change code.

Eureka separates the two concerns:

- **Graph** — declared once in YAML: nodes, ports, edges, feedback cycles
- **Runtime** — generic Rust that validates and executes whatever graph you hand it

This means:
- A researcher can iterate on the topology and prompts without touching Rust
- Control nodes are any executable — Python, Go, a shell script, a compiled binary — no SDK required
- The scheduler handles concurrency, round synchronization, and budget discipline automatically
- Runs are durable: checkpoints survive process restarts, and paused runs accept external input
- Agents can query a built-in RAG index automatically populated from prior round outputs — no retrieval plumbing in the prompt logic

The shipped example is an **AI co-scientist** loop — generation → reflection → ranking → evolution — that reproduces the iterative hypothesis refinement topology from the Google co-scientist paper. It is one workload among many the engine can express.

## Quick start

```bash
cargo build --release
cargo run --release -- validate example/coscientist.yml

export OPENROUTER_API_KEY=sk-or-...
cargo run --release -- start "Identify novel catalysts for CO₂ reduction"
```

Open `http://127.0.0.1:7773` for the live observer UI.

## Configuration

Drop an `eureka.toml` next to your manifest:

```toml
graph = "example/coscientist.yml"

[provider]
kind = "openrouter"
generation_model = "deepseek/deepseek-v4-flash"

[budget]
max_cost_usd = 25.0
max_tokens   = 5_000_000
max_wallclock = "45m"
max_rounds   = 12
```

Layers apply in order: built-in defaults → `eureka.toml` → `EUREKA_*` env vars → CLI flags. Supported providers: Anthropic, OpenAI, OpenRouter, Gemini, Groq, DeepSeek, Mistral, Cohere, Perplexity, Together, xAI, Ollama.

To enable RAG — artifact outputs from each round are embedded and made available as dynamic context in subsequent agent turns:

```toml
[rag]
enabled           = true
embedding_provider = "openai"
embedding_model   = "text-embedding-3-small"
top_k             = 5
```

Embedding providers follow the same bring-your-own-key model as generation providers. `index_kinds` and `agent_ids` let you filter which artifacts are indexed and which agents receive retrieved context.

## Manifest format

Agents, control nodes, and edges in one YAML file:

```yaml
name: Review workflow

agents:
  - id: classifier
    prompt: prompts/classifier.md
    inputs:  [{ port: in,  kind: Intake }]
    outputs: [{ port: out, kind: Classification }]
    output_schema:
      type: object
      required: [category]
      properties:
        category: { type: string }

control:
  - id: governor
    kind: round_governor
    command: [./control/governor]   # any executable: compiled binary, Python, shell, …
    inputs:  [{ port: in,  kind: Classification }]
    outputs: [{ port: continue, kind: Control },
              { port: halt,     kind: Control }]

edges:
  - { from_node: classifier, from_port: out, to_node: governor, to_port: in }
  - { from_node: governor, from_port: continue, to_node: classifier, to_port: in, feedback: true }
```

- **Artifact** = `{ kind, data }`. Kinds are strings matched at edge endpoints.
- **Feedback edge** (`feedback: true`) delivers to the next round, enabling iterative loops.
- **Control nodes** spawn any executable. The runtime writes a JSON call envelope to stdin and reads JSON emit envelopes from stdout (one per line). The subprocess can be written in any language — there is no Eureka SDK or library to import.

## Library usage

```rust
use eureka::{Session, config::EurekaConfig};

let config = EurekaConfig::load(Some(Path::new("eureka.toml")))?;
let mut session = Session::new(config, &Uuid::now_v7().to_string(), None)?;
let stats = session.run(json!({ "goal": "Find CO₂ reduction catalysts" })).await?;
println!("{} rounds, ${:.4} spent", stats.rounds_completed, stats.total_cost_usd);
```

## Development

```bash
cargo test --release
cargo clippy --all-targets --release
```

```
crates/eureka/      runtime library
crates/eureka-cli/  CLI and HTTP server
example/            co-scientist graph, prompts, controls, and tools
specs/              normative system specifications
eureka.toml         example configuration
```

See [`AGENTS.md`](AGENTS.md) for architecture, SPECs, and contributor conventions.

---

Dual-licensed under MIT or Apache-2.0, at your option.
