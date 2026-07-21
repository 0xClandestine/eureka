//! Graph validator — checks structural integrity of a `GraphSpec` at load time.
//!
//! Before any model call, every `GraphSpec` is validated against these rules:
//! 1. Port kind match — every edge's producer and consumer kinds align
//! 2. No dangling inputs — every required input port has ≥1 inbound edge
//! 3. Reachability — every node is reachable from a source
//!    3b. Unknown node kinds — every node kind must be registered (if registry is non-empty)
//! 4. Sink presence — at least one node emits a terminal artifact
//!
//! Cycle termination is an operational property governed by budget config
//! (`max_rounds`, `max_cost_usd`, etc.) — not a topological requirement.
//! Nodes may emit on a `halt` port for early exit, but it is never required.

use std::collections::{HashMap, HashSet, VecDeque};

use super::edge::Edge;
use super::port::PortSpec;
use super::spec::{GraphError, GraphSpec};

/// The result of validating a `GraphSpec` against a node registry.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether the graph is structurally valid.
    pub valid: bool,
    /// Any validation errors found.
    pub errors: Vec<GraphError>,
}

impl ValidationResult {
    /// Create a new valid result.
    #[must_use]
    pub const fn valid() -> Self {
        Self { valid: true, errors: vec![] }
    }

    /// Create a new invalid result with errors.
    #[must_use]
    pub const fn invalid(errors: Vec<GraphError>) -> Self {
        Self { valid: false, errors }
    }
}

/// A registry mapping node kind strings to their port specifications.
///
/// Populated from the manifest at startup and used by the validator to check
/// port kinds without needing to instantiate nodes.
#[derive(Clone, Default)]
pub struct PortRegistry {
    /// Registered port specs keyed by node kind string.
    pub(super) specs: HashMap<String, PortSpec>,
}

impl PortRegistry {
    /// Create an empty port registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a node kind with its port spec.
    pub fn register(&mut self, kind: impl Into<String>, spec: PortSpec) {
        self.specs.insert(kind.into(), spec);
    }

    /// Get the port spec for a node kind.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<&PortSpec> {
        self.specs.get(kind)
    }
}

/// Validate a `GraphSpec` against a `PortRegistry`.
///
/// Returns a `ValidationResult` with any structural errors found.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn validate_graph(spec: &GraphSpec, registry: &PortRegistry) -> ValidationResult {
    let mut errors: Vec<GraphError> = Vec::new();

    let node_ids: HashSet<&str> = spec.nodes.iter().map(|n| n.id.as_str()).collect();

    // Check all referenced nodes exist
    for edge in &spec.edges {
        if !node_ids.contains(edge.from_node.as_str()) {
            errors.push(GraphError::UnknownNode(format!(
                "Edge source '{}' not found in node list",
                edge.from_node
            )));
        }
        if !node_ids.contains(edge.to_node.as_str()) {
            errors.push(GraphError::UnknownNode(format!(
                "Edge target '{}' not found in node list",
                edge.to_node
            )));
        }
    }

    if !errors.is_empty() {
        return ValidationResult::invalid(errors);
    }

    // Build adjacency and reverse-adjacency
    let mut adjacency: HashMap<&str, Vec<&Edge>> = HashMap::new();
    let mut reverse_adjacency: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &spec.edges {
        adjacency.entry(edge.from_node.as_str()).or_default().push(edge);
        reverse_adjacency.entry(edge.to_node.as_str()).or_default().push(edge);
    }

    // 1. Port kind match check
    for edge in &spec.edges {
        let from_kind = spec.node_kind(&edge.from_node);
        let to_kind = spec.node_kind(&edge.to_node);

        if let (Some(fk), Some(tk)) = (from_kind, to_kind) {
            let from_spec = registry.get(fk);
            let to_spec = registry.get(tk);

            if let (Some(fs), Some(ts)) = (from_spec, to_spec) {
                let output_kind = fs.output_kind(&edge.from_port);
                let input_kind = ts.input_kind(&edge.to_port);

                match (output_kind, input_kind) {
                    (Some(ok), Some(ik)) if ok != ik => {
                        errors.push(GraphError::PortKindMismatch(format!(
                            "Edge {}:{} → {}:{}: output kind {:?} != input kind {:?}",
                            edge.from_node, edge.from_port, edge.to_node, edge.to_port, ok, ik
                        )));
                    }
                    (None, _) => {
                        errors.push(GraphError::UnknownPort(format!(
                            "Output port '{}.{}' not found on node '{}' (kind: {})",
                            edge.from_node, edge.from_port, edge.from_node, fk
                        )));
                    }
                    (_, None) => {
                        errors.push(GraphError::UnknownPort(format!(
                            "Input port '{}.{}' not found on node '{}' (kind: {})",
                            edge.to_node, edge.to_port, edge.to_node, tk
                        )));
                    }
                    _ => {}
                }
            }
        }
    }

    // 2. No dangling required inputs (except source nodes that accept Goal)
    for node in &spec.nodes {
        if let Some(kind) = registry.get(&node.kind) {
            let required = kind.required_inputs();
            for req_port in required {
                let accepts_goal =
                    kind.inputs.iter().any(|p| p.name == req_port && p.kind == "Goal");
                if accepts_goal {
                    continue;
                }

                let has_inbound = reverse_adjacency
                    .get(node.id.as_str())
                    .is_some_and(|edges| edges.iter().any(|e| e.to_port == req_port));
                if !has_inbound {
                    errors.push(GraphError::DanglingPort(format!(
                        "Node '{}' (kind: {}) requires input '{}' but has no inbound edges",
                        node.id, node.kind, req_port
                    )));
                }
            }
        }
    }

    // 3. Reachability — every node reachable from a source
    let source_candidates: Vec<&str> = spec
        .nodes
        .iter()
        .filter(|n| {
            registry.get(&n.kind).is_some_and(|ps| ps.inputs.iter().any(|p| p.kind == "Goal"))
        })
        .map(|n| n.id.as_str())
        .collect();

    let sources: Vec<&str> = if source_candidates.is_empty() {
        // Only non-feedback inbound edges count for source detection: a node
        // that receives inputs exclusively on feedback edges is still a source.
        let has_non_feedback_inbound: HashSet<&str> =
            spec.edges.iter().filter(|e| !e.feedback).map(|e| e.to_node.as_str()).collect();
        spec.nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| !has_non_feedback_inbound.contains(id))
            .collect()
    } else {
        source_candidates
    };

    if sources.is_empty() {
        errors.push(GraphError::UnreachableNode(
            "Graph has no source nodes (no node accepts a Goal artifact, \
             and all nodes have inbound edges)"
                .into(),
        ));
    } else {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        for s in &sources {
            visited.insert(s);
            queue.push_back(s);
        }
        while let Some(current) = queue.pop_front() {
            if let Some(edges) = adjacency.get(current) {
                for edge in edges {
                    let next = edge.to_node.as_str();
                    if visited.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
        for node in &spec.nodes {
            if !visited.contains(node.id.as_str()) {
                errors.push(GraphError::UnreachableNode(format!(
                    "Node '{}' (kind: {}) is not reachable from any source node",
                    node.id, node.kind
                )));
            }
        }
    }

    // 3b. Unknown node kinds
    if !registry.specs.is_empty() {
        for node in &spec.nodes {
            if registry.get(&node.kind).is_none() {
                errors.push(GraphError::UnknownNode(format!(
                    "Node kind '{}' (id: '{}') is not registered. \
                     Check that the plugin is installed under <graph_dir>/plugins/.",
                    node.kind, node.id
                )));
            }
        }
    }

    // 4. Sink presence
    if !registry.specs.is_empty() {
        let all_input_kinds: HashSet<&str> = registry
            .specs
            .values()
            .flat_map(|ps| ps.inputs.iter().map(|p| p.kind.as_str()))
            .collect();

        let has_terminal_output = spec.nodes.iter().any(|node| {
            registry.get(&node.kind).is_some_and(|ps| {
                ps.outputs.iter().any(|p| !all_input_kinds.contains(p.kind.as_str()))
            })
        });

        if !has_terminal_output {
            errors.push(GraphError::MissingSink(
                "No node produces a terminal output (an artifact kind that no other node \
                 consumes). At least one sink node is required."
                    .into(),
            ));
        }
    }

    if errors.is_empty() {
        ValidationResult::valid()
    } else {
        ValidationResult::invalid(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::{Emit, Node, NodeCtx, NodeError, PortMsg};
    use crate::graph::port::{PortDirection, PortSpec, PortSpecEntry};
    use crate::graph::spec::GraphNodeSpec;
    use async_trait::async_trait;

    struct MockAgent {
        ports: PortSpec,
    }

    #[async_trait]
    impl Node for MockAgent {
        fn ports(&self) -> PortSpec {
            self.ports.clone()
        }

        async fn process(
            &self,
            _ctx: &NodeCtx,
            _inputs: Vec<PortMsg>,
        ) -> Result<(Vec<Emit>, crate::graph::node::NodeUsage), NodeError> {
            Ok((vec![], crate::graph::node::NodeUsage::default()))
        }
    }

    fn make_agent_spec(input_kind: &str, output_kind: &str) -> PortSpec {
        PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: input_kind.to_string(),
                required: true,
            }],
            vec![PortSpecEntry {
                name: "out".into(),
                direction: PortDirection::Output,
                kind: output_kind.to_string(),
                required: false,
            }],
        )
    }

    #[test]
    fn test_valid_graph() {
        let mut registry = PortRegistry::new();
        registry.register("agent.gen", make_agent_spec("Goal", "Hypotheses"));
        registry.register("agent.ref", make_agent_spec("Hypotheses", "Reviews"));
        registry.register(
            "control.governor",
            PortSpec::new(
                vec![PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Reviews".to_string(),
                    required: true,
                }],
                vec![
                    PortSpecEntry {
                        name: "continue".into(),
                        direction: PortDirection::Output,
                        kind: "Control".to_string(),
                        required: false,
                    },
                    PortSpecEntry {
                        name: "halt".into(),
                        direction: PortDirection::Output,
                        kind: "Control".to_string(),
                        required: false,
                    },
                ],
            ),
        );

        let spec = GraphSpec {
            name: Some("test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "gen".into(),
                    kind: "agent.gen".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "ref".into(),
                    kind: "agent.ref".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "gov".into(),
                    kind: "control.governor".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![Edge::new("gen", "out", "ref", "in"), Edge::new("ref", "out", "gov", "in")],
            frontend: None,
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        assert!(result.valid, "expected valid graph but got errors: {:?}", result.errors);
    }

    #[test]
    fn test_port_kind_mismatch() {
        let mut registry = PortRegistry::new();
        registry.register("agent.gen", make_agent_spec("Goal", "Hypotheses"));
        registry.register("agent.ref", make_agent_spec("Reviews", "Insights"));

        let spec = GraphSpec {
            name: None,
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "gen".into(),
                    kind: "agent.gen".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "ref".into(),
                    kind: "agent.ref".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![Edge::new("gen", "out", "ref", "in")],
            frontend: None,
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| matches!(e, GraphError::PortKindMismatch(_))));
    }

    #[test]
    fn test_dangling_input() {
        let mut registry = PortRegistry::new();
        registry.register("agent.ref", make_agent_spec("Reviews", "Hypotheses"));

        let spec = GraphSpec {
            name: None,
            description: None,
            nodes: vec![GraphNodeSpec {
                id: "ref".into(),
                kind: "agent.ref".into(),
                config: serde_json::Value::Null,
                description: None,
            }],
            edges: vec![],
            frontend: None,
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| matches!(e, GraphError::DanglingPort(_))));
    }

    #[test]
    fn test_unknown_node() {
        let registry = PortRegistry::new();
        let spec = GraphSpec {
            name: None,
            description: None,
            nodes: vec![],
            edges: vec![Edge::new("ghost", "out", "phantom", "in")],
            frontend: None,
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| matches!(e, GraphError::UnknownNode(_))));
    }
}
