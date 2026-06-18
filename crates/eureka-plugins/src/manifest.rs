//! Plugin manifest types — deserialized from `plugin.json`.

use serde::Deserialize;

/// A port name + artifact kind pair declared in a node role config.
#[derive(Debug, Clone, Deserialize)]
pub struct PortDef {
    /// The port name used in graph edges (e.g. `"in"`, `"top"`, `"state"`).
    pub port: String,
    /// The artifact kind accepted or emitted on this port (e.g. `"Reviews"`).
    pub kind: String,
}

/// Configuration for the `node` role of a plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeRoleConfig {
    /// Input ports this node accepts.
    pub inputs: Vec<PortDef>,
    /// Output ports this node may emit on.
    pub outputs: Vec<PortDef>,
    /// Maximum wall-clock seconds to wait for the subprocess. Default: 60.
    #[serde(default = "default_node_timeout_secs")]
    pub timeout_secs: u32,
}

/// Configuration for the `tool` role of a plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolRoleConfig {
    /// Description shown to the LLM.
    pub description: String,
    /// JSON Schema for the arguments the LLM must supply.
    pub args_schema: serde_json::Value,
    /// Maximum wall-clock seconds to wait for the subprocess. Default: 30.
    #[serde(default = "default_tool_timeout_secs")]
    pub timeout_secs: u32,
}

/// A fully parsed `plugin.json` manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier. Must match the plugin directory name.
    pub name: String,
    /// SemVer version string.
    pub version: String,
    /// Human-readable description.
    pub description: String,
    /// Subprocess runtime. Currently only `"process"` is supported.
    pub runtime: String,
    /// Argv for the subprocess. `command[0]` is the binary; all paths are
    /// relative to the plugin directory.
    pub command: Vec<String>,
    /// Active roles for this plugin. Subset of `["node", "tool"]`.
    pub roles: Vec<String>,
    /// Node role config. Required when `"node"` is in `roles`.
    #[serde(default)]
    pub node: Option<NodeRoleConfig>,
    /// Tool role config. Required when `"tool"` is in `roles`.
    #[serde(default)]
    pub tool: Option<ToolRoleConfig>,
}

impl PluginManifest {
    /// Returns `true` if this plugin declares the node role.
    #[must_use]
    pub fn has_node_role(&self) -> bool {
        self.roles.iter().any(|r| r == "node")
    }

    /// Returns `true` if this plugin declares the tool role.
    #[must_use]
    pub fn has_tool_role(&self) -> bool {
        self.roles.iter().any(|r| r == "tool")
    }
}

const fn default_node_timeout_secs() -> u32 {
    60
}

const fn default_tool_timeout_secs() -> u32 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_node_plugin() {
        let json = r#"{
            "name": "round-governor",
            "version": "1.0.0",
            "description": "Governs rounds.",
            "runtime": "process",
            "command": ["python3", "governor.py"],
            "roles": ["node"],
            "node": {
                "inputs": [{"port": "in", "kind": "Hypotheses"}],
                "outputs": [
                    {"port": "continue", "kind": "Hypotheses"},
                    {"port": "halt",     "kind": "Control"}
                ]
            }
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.name, "round-governor");
        assert!(m.has_node_role());
        assert!(!m.has_tool_role());
        let node = m.node.unwrap();
        assert_eq!(node.inputs.len(), 1);
        assert_eq!(node.outputs.len(), 2);
        assert_eq!(node.timeout_secs, 60);
    }

    #[test]
    fn test_parse_dual_role_plugin() {
        let json = r#"{
            "name": "elo-ranker",
            "version": "1.0.0",
            "description": "Elo ranking.",
            "runtime": "process",
            "command": ["python3", "ranker.py"],
            "roles": ["node", "tool"],
            "node": {
                "inputs":  [{"port": "in", "kind": "Reviews"}],
                "outputs": [
                    {"port": "top",   "kind": "Hypotheses"},
                    {"port": "state", "kind": "Ranking"}
                ]
            },
            "tool": {
                "description": "Rank items.",
                "args_schema": {"type": "object"}
            }
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.has_node_role());
        assert!(m.has_tool_role());
    }
}
