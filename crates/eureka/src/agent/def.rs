//! Agent definition — the runtime type for an LLM-powered agent.
//!
//! Agents are loaded at startup from the YAML manifest's `agents[]` array,
//! not from separate files. Each agent carries its preamble, port specs,
//! LLM config, output schema, shell tool definitions, and MCP server specs.

pub use crate::config::AgentConfig;
pub use crate::graph::port::PortDef;

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

/// Runtime transport for an MCP server connection.
#[derive(Debug, Clone)]
pub enum McpTransport {
    /// Stdio subprocess. `command[0]` is the binary.
    Stdio {
        /// Full argv (already path-resolved by `AgentSpec::resolve_paths`).
        command: Vec<String>,
        /// Extra environment variables injected into the subprocess.
        env: std::collections::HashMap<String, String>,
    },
    /// Streamable-HTTP server reachable at this URI.
    Http {
        /// Full HTTP URI (e.g. `"http://localhost:9000"`).
        uri: String,
    },
}

/// A resolved MCP server definition — runtime counterpart of `McpServerSpec`.
#[derive(Debug, Clone)]
pub struct McpServerDef {
    /// Unique label for this server within the agent.
    pub name: String,
    /// How to connect to the server.
    pub transport: McpTransport,
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
    pub inputs: Vec<PortDef>,
    /// Output port specifications.
    pub outputs: Vec<PortDef>,
    /// LLM configuration for this agent.
    pub config: AgentConfig,
    /// JSON Schema the LLM must conform to (becomes the `submit` tool's parameter schema).
    pub output_schema: serde_json::Value,
    /// Shell tools available to the agent during its loop. May be empty.
    pub tools: Vec<ToolDef>,
    /// MCP servers to connect at session-build time.  May be empty.
    pub mcp_servers: Vec<McpServerDef>,
}

impl AgentDef {
    /// Build a `PortSpec` for this agent (used by the node registry).
    #[must_use]
    pub fn to_port_spec(&self) -> crate::graph::port::PortSpec {
        crate::graph::port::PortSpec::from_defs(&self.inputs, &self.outputs)
    }
}
