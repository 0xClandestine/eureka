//! # Eureka
//!
//! Graph-based AI co-scientist runtime. Every concern lives in its own module
//! and composes into `Session`, the single entry point for running a research
//! graph.
//!
//! ## Module map
//!
//! | Module | What it owns |
//! |---|---|
//! | [`graph`] | Pure topology: nodes, edges, ports, artifacts, validation |
//! | [`scheduler`] | Event-driven graph executor |
//! | [`manifest`] | YAML graph format — loads a file into a [`graph::GraphSpec`] |
//! | [`agents`] | LLM-backed nodes: `AgentDef`, `LlmClient`, `LlmAgentNode` |
//! | [`control`] | Subprocess-backed control nodes and the shared process runner |
//! | [`db`] | SQLite session persistence |
//! | [`config`] | Typed, layered runtime configuration |
//! | [`session`] | Assembles all of the above into a single runnable session |

pub mod agents;
pub mod config;
pub mod control;
pub mod error;
pub mod graph;
pub mod manifest;
pub mod scheduler;
pub mod session;

pub use error::EngineError;
pub use session::Session;
