use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::NodeRagConfig;
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
        /// Optional extra environment variables injected into the subprocess.
        /// Merged on top of the current process environment.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        env: std::collections::HashMap<String, String>,
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
    /// Per-agent RAG configuration overrides.
    /// When present, these override the global `[rag]` settings for this agent.
    #[serde(default)]
    pub rag: Option<NodeRagConfig>,
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
            if let McpTransportSpec::Stdio { ref mut command, .. } = server.transport {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_transport_stdio_deserialization() {
        let yaml = r#"
name: test-server
transport: stdio
command:
  - python3
  - tool.py
env:
  FOO: bar
"#;
        let spec: McpServerSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.name, "test-server");
        match spec.transport {
            McpTransportSpec::Stdio { command, env } => {
                assert_eq!(command, vec!["python3", "tool.py"]);
                assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn mcp_transport_http_deserialization() {
        let yaml = r#"
name: http-server
transport: http
uri: "http://localhost:9000"
"#;
        let spec: McpServerSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.name, "http-server");
        match spec.transport {
            McpTransportSpec::Http { uri } => assert_eq!(uri, "http://localhost:9000"),
            _ => panic!("expected Http"),
        }
    }

    #[test]
    fn mcp_server_spec_default_no_servers() {
        let yaml = r#"
id: agent1
prompt: "prompts/test.md"
output_schema: {}
"#;
        let spec: AgentSpec = serde_yaml::from_str(yaml).unwrap();
        assert!(spec.mcp_servers.is_empty());
        assert!(spec.tools.is_empty());
        assert!(spec.inputs.is_empty());
        assert!(spec.outputs.is_empty());
        assert!(spec.description.is_none());
    }

    #[test]
    fn agent_spec_with_tools_deserialization() {
        let yaml = r#"
id: agent1
prompt: "prompts/test.md"
tools:
  - name: search
    description: "search tool"
    command:
      - python3
      - search.py
    args_schema: {}
output_schema: {}
"#;
        let spec: AgentSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.tools.len(), 1);
        assert_eq!(spec.tools[0].name, "search");
        assert_eq!(spec.tools[0].command, vec!["python3", "search.py"]);
    }

    #[test]
    fn agent_spec_with_rag_deserialization() {
        let yaml = r#"
id: agent1
prompt: "prompts/test.md"
output_schema: {}
rag:
  enabled: false
  top_k: 10
  index_path: "hypotheses[*].statement"
  inject_format: "Prior: {{text}}"
"#;
        let spec: AgentSpec = serde_yaml::from_str(yaml).unwrap();
        let rag = spec.rag.unwrap();
        assert!(!rag.enabled);
        assert_eq!(rag.top_k, Some(10));
        assert_eq!(rag.index_path.as_deref(), Some("hypotheses[*].statement"));
        assert_eq!(rag.inject_format.as_deref(), Some("Prior: {{text}}"));
    }

    #[test]
    fn agent_spec_rag_enabled_defaults_to_true() {
        use crate::config::NodeRagConfig;
        let json = serde_json::json!({});
        let rag: NodeRagConfig = serde_json::from_value(json).unwrap();
        assert!(rag.enabled);
        assert!(rag.top_k.is_none());
    }
}
