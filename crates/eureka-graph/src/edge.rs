//! Edge types — typed connections between nodes in the graph.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A directed edge connecting one output port to one input port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    /// The source node ID.
    pub from_node: String,
    /// The source output port name.
    pub from_port: String,
    /// The target node ID.
    pub to_node: String,
    /// The target input port name.
    pub to_port: String,
    /// Whether this edge closes a cycle (feedback edge).
    #[serde(default)]
    pub feedback: bool,
}

impl Edge {
    /// Create a new edge.
    #[must_use]
    pub fn new(
        from_node: impl Into<String>,
        from_port: impl Into<String>,
        to_node: impl Into<String>,
        to_port: impl Into<String>,
    ) -> Self {
        Self {
            from_node: from_node.into(),
            from_port: from_port.into(),
            to_node: to_node.into(),
            to_port: to_port.into(),
            feedback: false,
        }
    }

    /// Mark this edge as feedback (closes a cycle).
    #[must_use]
    pub const fn feedback(mut self) -> Self {
        self.feedback = true;
        self
    }

    /// Get the full source port ID (`node.port`).
    #[must_use]
    pub fn source_id(&self) -> String {
        format!("{}.{}", self.from_node, self.from_port)
    }

    /// Get the full target port ID (`node.port`).
    #[must_use]
    pub fn target_id(&self) -> String {
        format!("{}.{}", self.to_node, self.to_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_creation() {
        let edge = Edge::new("generation", "out", "reflection", "in");
        assert_eq!(edge.from_node, "generation");
        assert_eq!(edge.source_id(), "generation.out");
        assert_eq!(edge.target_id(), "reflection.in");
        assert!(!edge.feedback);
    }

    #[test]
    fn test_feedback_edge() {
        let edge = Edge::new("ranking", "state", "meta_review", "in").feedback();
        assert!(edge.feedback);
    }
}
