use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::graph::port::{PortDef, PortSpec};

/// A subprocess-backed control node definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlSpec {
    /// Unique node ID within the graph (e.g. `"ranking"`).
    pub id: String,
    /// Node kind string used for registry lookup (e.g. `"elo-ranker"`).
    pub kind: String,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Subprocess argv. `command[0]` is the binary; relative paths are
    /// resolved against the YAML's parent directory.
    pub command: Vec<String>,
    /// Input port declarations.
    #[serde(default)]
    pub inputs: Vec<PortDef>,
    /// Output port declarations.
    #[serde(default)]
    pub outputs: Vec<PortDef>,
    /// Per-instance configuration (e.g. `max_rounds`).
    #[serde(default)]
    pub config: serde_json::Value,
    /// Maximum wall-clock seconds to wait for the subprocess.
    #[serde(default = "default_control_timeout")]
    pub timeout_secs: u32,
}

/// Default subprocess timeout for control nodes (seconds).
const fn default_control_timeout() -> u32 {
    60
}

impl ControlSpec {
    /// Resolve relative paths in the command.
    pub(super) fn resolve_paths(&mut self, base: &Path) {
        if let Some(bin) = self.command.first_mut() {
            let p = Path::new(bin.as_str());
            if p.is_relative() && !p.exists() {
                let resolved = base.join(bin.as_str());
                if resolved.exists() {
                    *bin = resolved.to_string_lossy().to_string();
                }
            }
        }
    }

    /// Build the config JSON value for the graph node spec.
    #[must_use]
    pub fn node_config(&self) -> serde_json::Value {
        self.config.clone()
    }

    /// Build a `PortSpec` for node registration.
    #[must_use]
    pub fn to_port_spec(&self) -> PortSpec {
        let inputs = self.inputs.iter().map(PortDef::to_input_spec).collect();
        let outputs = self.outputs.iter().map(PortDef::to_output_spec).collect();
        PortSpec::new(inputs, outputs)
    }
}

/// A shell tool available to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Name the LLM uses to call this tool.
    pub name: String,
    /// Description shown to the LLM in the tool schema.
    pub description: String,
    /// Subprocess argv. Relative paths resolved against the YAML file.
    pub command: Vec<String>,
    /// JSON Schema for the tool's arguments.
    pub args_schema: serde_json::Value,
    /// Maximum wall-clock seconds to wait for the subprocess.
    #[serde(default = "default_tool_timeout")]
    pub timeout_secs: u32,
}

/// Default subprocess timeout for shell tools (seconds).
const fn default_tool_timeout() -> u32 {
    30
}

impl ToolSpec {
    /// Resolve relative binary paths against the manifest base directory.
    pub(super) fn resolve_paths(&mut self, base: &Path) {
        if let Some(bin) = self.command.first_mut() {
            let p = Path::new(bin.as_str());
            if p.is_relative() && !p.exists() {
                let resolved = base.join(bin.as_str());
                if resolved.exists() {
                    *bin = resolved.to_string_lossy().to_string();
                }
            }
        }
    }
}
