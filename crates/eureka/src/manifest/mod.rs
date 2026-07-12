//! Graph manifest — the single YAML file that defines an entire graph.

/// Agent specification from the YAML manifest.
pub mod agent;
/// Control node specification from the YAML manifest.
pub mod control;
/// Graph manifest loader (`GraphManifest`).
pub mod graph;
/// Prompt path resolution.
pub mod prompt;

pub use agent::AgentSpec;
pub use control::{ControlSpec, ToolSpec};
pub use graph::GraphManifest;
pub use prompt::PromptPath;
