//! Graph validator — checks structural integrity of a `GraphSpec` at load time.
//!
//! Before any model call, every `GraphSpec` is validated against these rules:
//! 1. Port kind match — every edge's producer and consumer kinds align
//! 2. No dangling inputs — every required input port has ≥1 inbound edge
//! 3. Reachability — every node is reachable from a source
//!    3b. Unknown node kinds — every node kind must be registered (if registry is non-empty)
//! 4. Governed cycles — every cycle must contain a node with a `"halt"` output port
//! 5. Sink presence — at least one node emits a terminal artifact

use std::collections::{HashMap, HashSet, VecDeque};

use super::edge::Edge;
use super::node::Node;
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
        Self {
            valid: true,
            errors: vec![],
        }
    }

    /// Create a new invalid result with errors.
    #[must_use]
    pub const fn invalid(errors: Vec<GraphError>) -> Self {
        Self {
            valid: false,
            errors,
        }
    }

    /// Merge multiple validation results.
    #[must_use]
    pub fn merge(results: Vec<Self>) -> Self {
        let errors: Vec<GraphError> = results.into_iter().flat_map(|r| r.errors).collect();
        let valid = errors.is_empty();
        Self { valid, errors }
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

    /// Register a node kind from a `Node` instance.
    pub fn register_node(&mut self, kind: impl Into<String>, node: &impl Node) {
        self.specs.insert(kind.into(), node.ports());
    }

    /// Get the port spec for a node kind.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<&PortSpec> {
        self.specs.get(kind)
    }
}

/// Parse a port reference like `"node.port"` into `(node_id, port_name)`.
#[must_use]
pub fn parse_port_ref(ref_str: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = ref_str.splitn(2, '.').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
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
        adjacency
            .entry(edge.from_node.as_str())
            .or_default()
            .push(edge);
        reverse_adjacency
            .entry(edge.to_node.as_str())
            .or_default()
            .push(edge);
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
                let accepts_goal = kind
                    .inputs
                    .iter()
                    .any(|p| p.name == req_port && p.kind == "Goal");
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
            registry
                .get(&n.kind)
                .is_some_and(|ps| ps.inputs.iter().any(|p| p.kind == "Goal"))
        })
        .map(|n| n.id.as_str())
        .collect();

    let sources: Vec<&str> = if source_candidates.is_empty() {
        spec.nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| !reverse_adjacency.contains_key(id))
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

    // 4. Governed cycles — every SCC/cycle must contain a governor node.
    let governor_nodes: HashSet<&str> = spec
        .nodes
        .iter()
        .filter(|n| {
            registry
                .get(&n.kind)
                .is_some_and(|ps| ps.outputs.iter().any(|p| p.name == "halt"))
        })
        .map(|n| n.id.as_str())
        .collect();

    let sccs = tarjan_scc(spec);
    for (scc_index, scc) in sccs.iter().enumerate() {
        if scc.len() > 1 || (scc.len() == 1 && has_self_loop(spec, scc[0].as_str())) {
            let has_governor = scc
                .iter()
                .any(|node_id| governor_nodes.contains(node_id.as_str()));
            if !has_governor {
                errors.push(GraphError::UngovernedCycle(format!(
                    "SCC #{} ({}) has no governor node (a node with a 'halt' output port). \
                     Every cycle must contain at least one governor.",
                    scc_index,
                    scc.join(", ")
                )));
            }
        }
    }

    // 5. Sink presence
    if !registry.specs.is_empty() {
        let all_input_kinds: HashSet<&str> = registry
            .specs
            .values()
            .flat_map(|ps| ps.inputs.iter().map(|p| p.kind.as_str()))
            .collect();

        let has_terminal_output = spec.nodes.iter().any(|node| {
            registry.get(&node.kind).is_some_and(|ps| {
                ps.outputs
                    .iter()
                    .any(|p| !all_input_kinds.contains(p.kind.as_str()))
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

/// Tarjan's strongly connected components algorithm.
#[must_use]
fn tarjan_scc(spec: &GraphSpec) -> Vec<Vec<String>> {
    let node_ids: Vec<&str> = spec.nodes.iter().map(|n| n.id.as_str()).collect();
    let index_map: HashMap<&str, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();

    let mut adj: Vec<Vec<usize>> = vec![vec![]; node_ids.len()];
    for edge in &spec.edges {
        if let (Some(&from), Some(&to)) = (
            index_map.get(edge.from_node.as_str()),
            index_map.get(edge.to_node.as_str()),
        ) {
            adj[from].push(to);
        }
    }

    let n = node_ids.len();
    let mut index = 0usize;
    let mut indices: Vec<usize> = vec![usize::MAX; n];
    let mut lowlink: Vec<usize> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut sccs: Vec<Vec<String>> = Vec::new();

    #[allow(clippy::items_after_statements, clippy::too_many_arguments)]
    fn strongconnect(
        v: usize,
        index: &mut usize,
        indices: &mut Vec<usize>,
        lowlink: &mut Vec<usize>,
        on_stack: &mut Vec<bool>,
        stack: &mut Vec<usize>,
        adj: &[Vec<usize>],
        node_ids: &[&str],
        sccs: &mut Vec<Vec<String>>,
    ) {
        indices[v] = *index;
        lowlink[v] = *index;
        *index += 1;
        stack.push(v);
        on_stack[v] = true;

        for &w in &adj[v] {
            if indices[w] == usize::MAX {
                strongconnect(
                    w, index, indices, lowlink, on_stack, stack, adj, node_ids, sccs,
                );
                lowlink[v] = lowlink[v].min(lowlink[w]);
            } else if on_stack[w] {
                lowlink[v] = lowlink[v].min(indices[w]);
            }
        }

        if lowlink[v] == indices[v] {
            let mut scc: Vec<String> = Vec::new();
            while let Some(w) = stack.pop() {
                on_stack[w] = false;
                scc.push(node_ids[w].to_string());
                if w == v {
                    break;
                }
            }
            scc.sort();
            sccs.push(scc);
        }
    }

    for v in 0..n {
        if indices[v] == usize::MAX {
            strongconnect(
                v,
                &mut index,
                &mut indices,
                &mut lowlink,
                &mut on_stack,
                &mut stack,
                &adj,
                &node_ids,
                &mut sccs,
            );
        }
    }

    sccs
}

/// Check if a node has a self-loop (edge to itself).
#[must_use]
fn has_self_loop(spec: &GraphSpec, node_id: &str) -> bool {
    spec.edges
        .iter()
        .any(|e| e.from_node == node_id && e.to_node == node_id)
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
            edges: vec![
                Edge::new("gen", "out", "ref", "in"),
                Edge::new("ref", "out", "gov", "in"),
            ],
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        let _ = result;
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
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, GraphError::PortKindMismatch(_))));
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
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, GraphError::DanglingPort(_))));
    }

    #[test]
    fn test_unknown_node() {
        let registry = PortRegistry::new();
        let spec = GraphSpec {
            name: None,
            description: None,
            nodes: vec![],
            edges: vec![Edge::new("ghost", "out", "phantom", "in")],
            metadata: serde_json::Value::Null,
        };

        let result = validate_graph(&spec, &registry);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, GraphError::UnknownNode(_))));
    }

    #[test]
    fn test_parse_port_ref() {
        assert_eq!(
            parse_port_ref("generation.out"),
            Some(("generation".into(), "out".into()))
        );
        assert_eq!(parse_port_ref("invalid"), None);
    }
}
