use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::graph::port::PortDef;

use super::control::ToolSpec;
use super::prompt::PromptPath;

/// MCP transport variant — how this server is reached.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpTransportSpec {
    /// Spawn a local subprocess; communicate over stdin/stdout.
    Stdio {
        /// Subprocess argv. `command[0]` is the binary.
        /// Resolved against the manifest base directory at load time.
        command: Vec<String>,
    },
    /// Connect to an already-running HTTP server.
    Http {
        /// Full HTTP URI, e.g. `"http://localhost:9000"`.
        uri: String,
    },
}

/// An MCP server declaration inside an agent's manifest entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSpec {
    /// Unique label for this server within the agent.  Used in logs and errors.
    pub name: String,
    /// Transport type and its required parameters.
    #[serde(flatten)]
    pub transport: McpTransportSpec,
}

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
    /// MCP servers to connect at session-build time.  Defaults to empty.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerSpec>,
}

impl AgentSpec {
    /// Resolve relative paths against the manifest base directory.
    pub(super) fn resolve_paths(&mut self, base: &Path) {
        self.prompt.resolve(base);
        for tool in &mut self.tools {
            tool.resolve_paths(base);
        }
        // Resolve `command[0]` of stdio servers: if the binary exists at the
        // resolved path, rewrite to absolute; otherwise leave for PATH lookup.
        for server in &mut self.mcp_servers {
            if let McpTransportSpec::Stdio { ref mut command } = server.transport {
                if let Some(bin) = command.first_mut() {
                    let candidate = base.join(&*bin);
                    if candidate.exists() {
                        *bin = candidate.to_string_lossy().into_owned();
                    }
                }
            }
        }
    }
}
