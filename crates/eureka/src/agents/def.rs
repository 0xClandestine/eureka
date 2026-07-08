//! Agent definition — the runtime type for an LLM-powered agent.
//!
//! Agents are loaded at startup from the YAML manifest's `agents[]` array,
//! not from separate files. Each agent carries its preamble, port specs,
//! LLM config, output schema, and shell tool definitions.

pub use crate::config::AgentConfig;
pub use crate::graph::port::PortDef as AgentPort;

use crate::graph::port::PortSpec;
use serde::{Deserialize, Serialize};

/// A shell tool the agent may call during its loop.
///
/// When the LLM calls this tool, the framework runs `command` as a subprocess,
/// writes the args JSON to stdin, and returns stdout to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    /// Name the LLM uses to call this tool. Must not be `"submit"`.
    pub name: String,
    /// Description shown to the LLM in the tool schema.
    pub description: String,
    /// Subprocess argv. `command[0]` is the binary. Tokens of the form
    /// `{{arg_name}}` are substituted from the LLM's call arguments.
    pub command: Vec<String>,
    /// JSON Schema for the arguments the LLM must supply.
    pub args_schema: serde_json::Value,
    /// Maximum wall-clock seconds to wait for the subprocess.
    #[serde(default = "default_tool_timeout")]
    pub timeout_secs: u32,
}

/// Default subprocess timeout for agent tools (seconds).
const fn default_tool_timeout() -> u32 {
    30
}

/// A fully loaded agent definition.
///
/// Produced by the session from a YAML `AgentSpec` + prompt file.
#[derive(Debug, Clone)]
pub struct AgentDef {
    /// The agent's name (matches the node ID from the manifest).
    pub name: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// The system preamble loaded from the prompt file.
    pub preamble: String,
    /// Input port specifications.
    pub inputs: Vec<AgentPort>,
    /// Output port specifications.
    pub outputs: Vec<AgentPort>,
    /// LLM configuration for this agent.
    pub config: AgentConfig,
    /// JSON Schema the LLM must conform to (becomes the `submit` tool's parameter schema).
    pub output_schema: serde_json::Value,
    /// Shell tools available to the agent during its loop. May be empty.
    pub tools: Vec<ToolDef>,
}

impl AgentDef {
    /// Build a `PortSpec` for this agent (used by the node registry).
    #[must_use]
    pub fn to_port_spec(&self) -> PortSpec {
        let inputs = self.inputs.iter().map(AgentPort::to_input_spec).collect();
        let outputs = self.outputs.iter().map(AgentPort::to_output_spec).collect();
        PortSpec::new(inputs, outputs)
    }
}
