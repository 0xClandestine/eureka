//! Graph specification — the topology of a research run as data.

use serde::{Deserialize, Serialize};

use crate::edge::Edge;

/// A node definition in a `GraphSpec`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeSpec {
    /// Unique node identifier within this graph.
    pub id: String,
    /// The kind of node.
    ///
    /// For LLM agents loaded from `agents/` this is the agent name (e.g. `"generation"`).
    /// For built-in control nodes this is the full kind string (e.g. `"control.governor"`).
    pub kind: String,
    /// Optional per-instance configuration overrides (e.g. `temperature`).
    #[serde(default)]
    pub config: serde_json::Value,
    /// Optional human-readable description of this node's role in the graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A complete graph specification loaded from JSON.
///
/// This is the central data structure that makes "topology is data" real.
/// A `GraphSpec` defines all nodes and edges; the validator checks its
/// structural correctness at load time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSpec {
    /// A human-readable name for this graph topology.
    pub name: Option<String>,
    /// Optional description of this topology.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// All nodes in the graph.
    pub nodes: Vec<GraphNodeSpec>,
    /// All directed edges connecting node ports.
    pub edges: Vec<Edge>,
    /// Optional metadata (version, author, reference).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl GraphSpec {
    /// Load a `GraphSpec` from a TOML string.
    ///
    /// # Errors
    ///
    /// Returns a `GraphError` if the TOML is malformed.
    pub fn from_toml(toml_str: &str) -> Result<Self, GraphError> {
        toml::from_str(toml_str).map_err(|e| GraphError::ParseError(e.to_string()))
    }

    /// Load a `GraphSpec` from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns a `GraphError` if the JSON is malformed.
    pub fn from_json(json_str: &str) -> Result<Self, GraphError> {
        serde_json::from_str(json_str).map_err(|e| GraphError::ParseError(e.to_string()))
    }

    /// Serialize this `GraphSpec` to TOML.
    ///
    /// # Errors
    ///
    /// Returns a `GraphError` if serialization fails.
    pub fn to_toml(&self) -> Result<String, GraphError> {
        toml::to_string(self).map_err(|e| GraphError::ParseError(e.to_string()))
    }

    /// Get the set of node IDs in this graph.
    #[must_use]
    pub fn node_ids(&self) -> Vec<String> {
        self.nodes.iter().map(|n| n.id.clone()).collect()
    }

    /// Get the kind of a node by ID.
    #[must_use]
    pub fn node_kind(&self, id: &str) -> Option<&str> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.kind.as_str())
    }

    /// Get all edges originating from a node.
    #[must_use]
    pub fn edges_from(&self, node_id: &str) -> Vec<&Edge> {
        self.edges
            .iter()
            .filter(|e| e.from_node == node_id)
            .collect()
    }

    /// Get all edges targeting a node.
    #[must_use]
    pub fn edges_to(&self, node_id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.to_node == node_id).collect()
    }

    /// Get all nodes that have no non-feedback inbound edges (source nodes).
    ///
    /// Feedback edges are excluded so that nodes which only receive cyclic
    /// inputs (like `generation`, which gets insights from `meta_review`) are
    /// correctly identified as sources that receive the initial goal injection.
    #[must_use]
    pub fn source_node_ids(&self) -> Vec<String> {
        let has_forward_inbound: std::collections::HashSet<String> = self
            .edges
            .iter()
            .filter(|e| !e.feedback)
            .map(|e| e.to_node.clone())
            .collect();
        self.nodes
            .iter()
            .filter(|n| !has_forward_inbound.contains(&n.id))
            .map(|n| n.id.clone())
            .collect()
    }

    /// Get all nodes that have no outbound edges (potential sink nodes).
    #[must_use]
    pub fn sink_node_ids(&self) -> Vec<String> {
        let has_outbound: std::collections::HashSet<String> =
            self.edges.iter().map(|e| e.from_node.clone()).collect();
        self.nodes
            .iter()
            .filter(|n| !has_outbound.contains(&n.id))
            .map(|n| n.id.clone())
            .collect()
    }
}

/// Errors related to graph specification loading and validation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GraphError {
    /// The graph specification could not be parsed.
    #[error("Failed to parse graph specification: {0}")]
    ParseError(String),
    /// A port kind mismatch was detected.
    #[error("Port kind mismatch: {0}")]
    PortKindMismatch(String),
    /// A node has a dangling (unconnected) required input port.
    #[error("Dangling port: {0}")]
    DanglingPort(String),
    /// A node is unreachable from any source.
    #[error("Unreachable node: {0}")]
    UnreachableNode(String),
    /// A cycle lacks a governing node.
    #[error("Ungoverned cycle detected: {0}")]
    UngovernedCycle(String),
    /// No sink node emits a terminal artifact.
    #[error("No sink node found: {0}")]
    MissingSink(String),
    /// An edge references an unknown node.
    #[error("Unknown node referenced: {0}")]
    UnknownNode(String),
    /// An edge references an unknown port.
    #[error("Unknown port: {0}")]
    UnknownPort(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_spec_from_toml() {
        let toml_str = r#"name = "test-graph"

[[nodes]]
id = "generation"
kind = "agent.generation"

[[nodes]]
id = "reflection"
kind = "agent.reflection"

[[edges]]
from_node = "generation"
from_port = "out"
to_node = "reflection"
to_port = "in"
"#;
        let spec = GraphSpec::from_toml(toml_str).unwrap();
        assert_eq!(spec.nodes.len(), 2);
        assert_eq!(spec.edges.len(), 1);
    }

    #[test]
    fn test_node_ids() {
        let spec = GraphSpec {
            name: Some("test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "a".into(),
                    kind: "agent.test".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "b".into(),
                    kind: "agent.test".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![Edge::new("a", "out", "b", "in")],
            metadata: serde_json::Value::Null,
        };
        assert_eq!(spec.node_ids(), vec!["a", "b"]);
        assert_eq!(spec.edges_from("a").len(), 1);
        assert_eq!(spec.edges_to("b").len(), 1);
    }

    #[test]
    fn test_source_and_sink_nodes() {
        let spec = GraphSpec {
            name: None,
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "source".into(),
                    kind: "agent.test".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "middle".into(),
                    kind: "agent.test".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "sink".into(),
                    kind: "agent.test".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![
                Edge::new("source", "out", "middle", "in"),
                Edge::new("middle", "out", "sink", "in"),
            ],
            metadata: serde_json::Value::Null,
        };
        assert_eq!(spec.source_node_ids(), vec!["source"]);
        assert_eq!(spec.sink_node_ids(), vec!["sink"]);
    }
}
