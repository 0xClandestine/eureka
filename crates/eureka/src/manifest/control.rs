use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::graph::port::PortDef;

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

/// Resolve a relative command binary path against a base directory.
fn resolve_binary(bin: &mut String, base: &Path) {
    let p = Path::new(bin.as_str());
    if p.is_relative() && !p.exists() {
        let resolved = base.join(bin.as_str());
        if resolved.exists() {
            *bin = resolved.to_string_lossy().to_string();
        }
    }
}

impl ControlSpec {
    /// Resolve relative paths in the command.
    pub(super) fn resolve_paths(&mut self, base: &Path) {
        if let Some(bin) = self.command.first_mut() {
            resolve_binary(bin, base);
        }
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
            resolve_binary(bin, base);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_spec_default_timeout() {
        let yaml = r#"
name: search
description: "search"
command:
  - python3
  - search.py
args_schema: {}
"#;
        let spec: ToolSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.timeout_secs, 30);
    }

    #[test]
    fn tool_spec_explicit_timeout() {
        let yaml = r#"
name: search
description: "search"
command:
  - python3
  - search.py
args_schema: {}
timeout_secs: 120
"#;
        let spec: ToolSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.timeout_secs, 120);
    }

    #[test]
    fn tool_spec_timeout_zero() {
        let yaml = r#"
name: search
description: "search"
command:
  - python3
  - search.py
args_schema: {}
timeout_secs: 0
"#;
        let spec: ToolSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.timeout_secs, 0);
    }

    #[test]
    fn control_spec_default_timeout() {
        let yaml = r#"
id: ranker
kind: elo-ranker
command:
  - python3
  - rank.py
"#;
        let spec: ControlSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.timeout_secs, 60);
        assert!(spec.inputs.is_empty());
        assert!(spec.outputs.is_empty());
        assert_eq!(spec.config, serde_json::Value::Null);
    }

    #[test]
    fn control_spec_explicit_timeout_and_description() {
        let yaml = r#"
id: ranker
kind: elo-ranker
description: "ranks hypotheses"
command:
  - python3
  - rank.py
timeout_secs: 300
"#;
        let spec: ControlSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.timeout_secs, 300);
        assert_eq!(spec.description, Some("ranks hypotheses".into()));
    }

    #[test]
    fn control_spec_with_ports() {
        let yaml = r#"
id: ranker
kind: elo-ranker
command:
  - python3
  - rank.py
inputs:
  - port: in
    kind: Hypotheses
    required: true
outputs:
  - port: out
    kind: Rankings
"#;
        let spec: ControlSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.inputs.len(), 1);
        assert_eq!(spec.inputs[0].port, "in");
        assert_eq!(spec.inputs[0].kind, "Hypotheses");
        assert_eq!(spec.inputs[0].required, Some(true));
        assert_eq!(spec.outputs.len(), 1);
        assert_eq!(spec.outputs[0].port, "out");
        assert_eq!(spec.outputs[0].kind, "Rankings");
        assert_eq!(spec.outputs[0].required, None);
    }

    #[test]
    fn control_spec_with_config() {
        let yaml = r#"
id: ranker
kind: elo-ranker
command:
  - python3
  - rank.py
config:
  max_rounds: 5
  threshold: 0.8
"#;
        let spec: ControlSpec = serde_yaml::from_str(yaml).unwrap();
        let cfg = spec.config;
        assert_eq!(cfg["max_rounds"], 5);
        assert_eq!(cfg["threshold"], 0.8);
    }

    #[test]
    fn tool_spec_serde_round_trip() {
        let original = ToolSpec {
            name: "my_tool".into(),
            description: "does stuff".into(),
            command: vec!["python3".into(), "tool.py".into()],
            args_schema: serde_json::json!({"type": "object"}),
            timeout_secs: 45,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ToolSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.name, original.name);
        assert_eq!(restored.command, original.command);
        assert_eq!(restored.timeout_secs, original.timeout_secs);
    }

    #[test]
    fn control_spec_serde_round_trip() {
        let original = ControlSpec {
            id: "r1".into(),
            kind: "elo".into(),
            description: Some("desc".into()),
            command: vec!["py".into(), "r.py".into()],
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({"k": 1}),
            timeout_secs: 90,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ControlSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.kind, original.kind);
        assert_eq!(restored.timeout_secs, original.timeout_secs);
        assert_eq!(restored.config, original.config);
    }

    #[test]
    fn tool_spec_deserialize_negative_timeout_wraps_to_u32_max() {
        let yaml = r#"
name: search
description: "search"
command:
  - python3
  - search.py
args_schema: {}
timeout_secs: -1
"#;
        let result: Result<ToolSpec, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "negative timeout should fail deserialization on u32");
    }
}
