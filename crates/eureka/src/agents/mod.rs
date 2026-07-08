//! # Agent
//!
//! LLM-powered graph nodes. An agent receives a typed input artifact, calls
//! an LLM with a system preamble and a JSON Schema, and emits structured
//! output artifacts. All agent definitions are loaded at runtime from the
//! YAML manifest — no per-agent Rust types are needed.

pub mod client;
pub mod def;
pub mod error;
pub mod node;
pub mod tools;

pub use client::{LlmClient, RigClient};
pub use def::{AgentConfig, AgentDef, AgentPort, ToolDef};
pub use error::AgentError;
pub use node::LlmAgentNode;
pub use tools::CommandTool;
