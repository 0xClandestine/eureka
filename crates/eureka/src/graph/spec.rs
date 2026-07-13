//! Graph specification — the topology of a research run as data.

use serde::{Deserialize, Serialize};

use super::edge::Edge;

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

/// A complete graph specification.
///
/// This is the central data structure that makes "topology is data" real.
/// The only way to load a `GraphSpec` is via [`crate::manifest::GraphManifest::load`]
/// + `to_graph_spec()`.
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
    /// Get the kind of a node by ID.
    #[must_use]
    pub fn node_kind(&self, id: &str) -> Option<&str> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.kind.as_str())
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
}

/// Errors related to graph specification loading and validation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GraphError {
    /// The graph specification could not be parsed or loaded.
    #[error("{0}")]
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
    }
}
