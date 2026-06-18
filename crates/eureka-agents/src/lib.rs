//! # Eureka Agents
//!
//! An agent is a loop: it receives a typed input artifact, calls an LLM with
//! a system preamble and a JSON Schema, and emits one or more structured output
//! artifacts. Agent definitions are loaded at runtime from `agents/` — a pair
//! of files per agent:
//!
//! - `<name>.md` — the system preamble
//! - `<name>.json` — the output JSON Schema plus port and config declarations
//!
//! A single [`LlmAgentNode`] handles all LLM agents; no per-agent Rust types.

pub mod client;
pub mod def;
pub mod error;
pub mod node;
pub mod tools;

pub use client::{LlmClient, RigClient};
pub use def::{AgentConfig, AgentDef, AgentLoadError, AgentPort, ToolDef};
pub use error::AgentError;
pub use node::LlmAgentNode;
pub use tools::CommandTool;
