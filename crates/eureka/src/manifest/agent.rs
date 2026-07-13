use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::graph::port::PortDef;

use super::control::ToolSpec;
use super::prompt::PromptPath;

/// An LLM-powered agent definition, loaded from the YAML manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Unique node ID within the graph (e.g. `"generation"`).
    pub id: String,
    /// Optional human-readable description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Path to the system prompt file (e.g. `"prompts/generation.md"`).
    /// Resolved relative to the YAML file.
    pub prompt: PromptPath,
    /// Input port declarations.
    #[serde(default)]
    pub inputs: Vec<PortDef>,
    /// Output port declarations.
    #[serde(default)]
    pub outputs: Vec<PortDef>,
    /// LLM configuration.
    #[serde(default)]
    pub config: serde_json::Value,
    /// Shell tools available to the agent.
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    /// JSON Schema for the agent's structured output.
    pub output_schema: serde_json::Value,
}

impl AgentSpec {
    /// Resolve relative paths against the manifest base directory.
    pub(super) fn resolve_paths(&mut self, base: &Path) {
        self.prompt.resolve(base);
        for tool in &mut self.tools {
            tool.resolve_paths(base);
        }
    }

    /// Build the LLM config JSON value for the graph node spec.
    #[must_use]
    pub fn llm_config(&self) -> serde_json::Value {
        self.config.clone()
    }

    /// Build a `PortSpec` for node registration.
    #[must_use]
    pub fn to_port_spec(&self) -> crate::graph::port::PortSpec {
        crate::graph::port::PortSpec::from_defs(&self.inputs, &self.outputs)
    }
}
