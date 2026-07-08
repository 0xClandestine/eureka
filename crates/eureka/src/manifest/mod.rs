//! Graph manifest — the single YAML file that defines an entire graph.

pub mod agent;
pub mod control;
pub mod graph;
pub mod prompt;

pub use agent::AgentSpec;
pub use control::{ControlSpec, ToolSpec};
pub use graph::GraphManifest;
pub use prompt::PromptPath;
