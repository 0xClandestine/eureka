use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::graph::edge::Edge;
use crate::graph::spec::{GraphError, GraphNodeSpec, GraphSpec};

use super::agent::AgentSpec;
use super::control::ControlSpec;

/// A complete graph definition loaded from a single YAML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphManifest {
    /// A human-readable name for this graph.
    pub name: Option<String>,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// LLM agent definitions.
    #[serde(default)]
    pub agents: Vec<AgentSpec>,

    /// Subprocess-backed control node definitions.
    #[serde(default)]
    pub control: Vec<ControlSpec>,

    /// Directed edges between node ports.
    #[serde(default)]
    pub edges: Vec<Edge>,

    /// Optional metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl GraphManifest {
    /// Load a manifest from a YAML file path.
    ///
    /// All relative paths (prompts, commands) are resolved against the YAML's
    /// parent directory.
    ///
    /// # Errors
    ///
    /// Returns a [`GraphError`] if the file cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, GraphError> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            GraphError::ParseError(format!(
                "Failed to read graph manifest '{}': {e}",
                path.display()
            ))
        })?;
        let mut manifest: Self = serde_yaml::from_str(&raw).map_err(|e| {
            GraphError::ParseError(format!("Failed to parse graph manifest YAML: {e}"))
        })?;

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for agent in &mut manifest.agents {
            agent.resolve_paths(base);
        }
        for ctrl in &mut manifest.control {
            ctrl.resolve_paths(base);
        }

        Ok(manifest)
    }

    /// Convert the manifest's topology into a [`GraphSpec`] for the scheduler.
    ///
    /// Every agent and control node becomes a `GraphNodeSpec` keyed by its `id`.
    #[must_use]
    pub fn to_graph_spec(&self) -> GraphSpec {
        let mut nodes = Vec::with_capacity(self.agents.len() + self.control.len());

        for agent in &self.agents {
            nodes.push(GraphNodeSpec {
                id: agent.id.clone(),
                kind: agent.id.clone(),
                config: agent.llm_config(),
                description: agent.description.clone(),
            });
        }
        for ctrl in &self.control {
            nodes.push(GraphNodeSpec {
                id: ctrl.id.clone(),
                kind: ctrl.kind.clone(),
                config: ctrl.node_config(),
                description: ctrl.description.clone(),
            });
        }

        GraphSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            nodes,
            edges: self.edges.clone(),
            metadata: self.metadata.clone(),
        }
    }

    /// Get the set of all node IDs (agents + control).
    #[must_use]
    pub fn all_node_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.agents.iter().map(|a| a.id.clone()).collect();
        ids.extend(self.control.iter().map(|c| c.id.clone()));
        ids
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::graph::edge::Edge;
    use crate::graph::port::PortDef;

    use super::super::control::ControlSpec;
    use super::super::prompt::PromptPath;
    use super::*;

    fn write_yml(dir: &Path, yml: &str) -> PathBuf {
        let path = dir.join("test.yml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(yml.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_load_minimal_manifest() {
        let dir = TempDir::new().unwrap();
        let path = write_yml(
            dir.path(),
            r#"
name: test
agents:
  - id: gen
    prompt: prompts/gen.md
    inputs:
      - port: in
        kind: Goal
    outputs:
      - port: out
        kind: Hypotheses
    output_schema:
      type: object
edges:
  - from_node: gen
    from_port: out
    to_node: sink
    to_port: in
"#,
        );
        let prompts = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(prompts.join("gen.md"), "test prompt").unwrap();

        let manifest = GraphManifest::load(&path).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("test"));
        assert_eq!(manifest.agents.len(), 1);
        assert_eq!(manifest.agents[0].id, "gen");
        assert_eq!(manifest.edges.len(), 1);
    }

    #[test]
    fn test_to_graph_spec() {
        let manifest = GraphManifest {
            name: Some("test".into()),
            description: None,
            agents: vec![AgentSpec {
                id: "gen".into(),
                description: None,
                prompt: PromptPath {
                    raw: "prompts/gen.md".into(),
                    absolute: PathBuf::from("/tmp/prompts/gen.md"),
                },
                inputs: vec![PortDef {
                    port: "in".into(),
                    kind: "Goal".into(),
                    required: None,
                }],
                outputs: vec![PortDef {
                    port: "out".into(),
                    kind: "Hypotheses".into(),
                    required: None,
                }],
                config: serde_json::json!({}),
                tools: vec![],
                output_schema: serde_json::json!({ "type": "object" }),
            }],
            control: vec![],
            edges: vec![],
            metadata: serde_json::json!({}),
        };

        let spec = manifest.to_graph_spec();
        assert_eq!(spec.nodes.len(), 1);
        assert_eq!(spec.nodes[0].id, "gen");
    }

    #[test]
    fn test_optional_ports_preserve_required_flag() {
        let manifest: GraphManifest = serde_yaml::from_str(
            r#"
name: optional
agents:
  - id: gen
    prompt: p.md
    inputs:
      - { port: in, kind: Goal }
      - { port: context, kind: Insights, required: false }
    outputs: [{ port: out, kind: Test }]
    output_schema: { type: object }
"#,
        )
        .unwrap();
        assert_eq!(manifest.agents[0].inputs[1].required, Some(false));
    }

    #[test]
    fn test_control_and_agents_combined() {
        let manifest = GraphManifest {
            name: None,
            description: None,
            agents: vec![AgentSpec {
                id: "gen".into(),
                description: None,
                prompt: PromptPath {
                    raw: "p.md".into(),
                    absolute: PathBuf::from("p.md"),
                },
                inputs: vec![PortDef {
                    port: "in".into(),
                    kind: "Goal".into(),
                    required: None,
                }],
                outputs: vec![PortDef {
                    port: "out".into(),
                    kind: "Hypotheses".into(),
                    required: None,
                }],
                config: serde_json::json!({}),
                tools: vec![],
                output_schema: serde_json::json!({ "type": "object" }),
            }],
            control: vec![ControlSpec {
                id: "gov".into(),
                kind: "round-governor".into(),
                description: None,
                command: vec!["python3".into(), "control/gov.py".into()],
                inputs: vec![PortDef {
                    port: "in".into(),
                    kind: "Hypotheses".into(),
                    required: None,
                }],
                outputs: vec![
                    PortDef {
                        port: "continue".into(),
                        kind: "Hypotheses".into(),
                        required: None,
                    },
                    PortDef {
                        port: "halt".into(),
                        kind: "Control".into(),
                        required: None,
                    },
                ],
                config: serde_json::json!({}),
                timeout_secs: 10,
            }],
            edges: vec![Edge {
                from_node: "gen".into(),
                from_port: "out".into(),
                to_node: "gov".into(),
                to_port: "in".into(),
                feedback: false,
            }],
            metadata: serde_json::json!({}),
        };

        let ids = manifest.all_node_ids();
        assert!(ids.contains(&"gen".to_string()));
        assert!(ids.contains(&"gov".to_string()));
    }
}
