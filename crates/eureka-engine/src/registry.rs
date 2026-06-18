//! Node registry — maps node kind strings to node constructors.
//!
//! The registry is populated at startup with all registered node kinds
//! (agents, tools, control nodes) and used both for validation and for
//! instantiating nodes from a `GraphSpec`.

use std::collections::HashMap;
use std::sync::Arc;

use eureka_graph::node::BoxedNode;
use eureka_graph::port::PortSpec;
use eureka_graph::validate::PortRegistry;

use crate::error::EngineError;

/// A function that constructs a boxed node from a `GraphNodeSpec`.
pub type NodeConstructor =
    Arc<dyn Fn(&eureka_graph::spec::GraphNodeSpec) -> Result<BoxedNode, EngineError> + Send + Sync>;

/// The node registry — maps node kind strings to constructors and port specs.
#[derive(Clone)]
pub struct NodeRegistry {
    /// Maps node kind to constructor.
    constructors: HashMap<String, NodeConstructor>,
    /// Maps node kind to port spec (for validation).
    port_registry: PortRegistry,
}

impl NodeRegistry {
    /// Create an empty node registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            constructors: HashMap::new(),
            port_registry: PortRegistry::new(),
        }
    }

    /// Register a node kind with its constructor and port spec.
    pub fn register(
        &mut self,
        kind: impl Into<String>,
        constructor: NodeConstructor,
        port_spec: PortSpec,
    ) {
        let kind: String = kind.into();
        self.port_registry.register(kind.clone(), port_spec);
        self.constructors.insert(kind, constructor);
    }

    /// Register a node kind from a concrete `BoxedNode` instance.
    ///
    /// This is useful for nodes that are always constructed the same way
    /// (e.g., control nodes with no configuration).
    pub fn register_instance(&mut self, kind: impl Into<String>, node: BoxedNode) {
        let kind: String = kind.into();
        let ports = node.ports();
        self.port_registry.register(kind.clone(), ports);
        self.constructors
            .insert(kind, Arc::new(move |_spec| Ok(node.clone())));
    }

    /// Get the port registry for validation.
    #[must_use]
    pub const fn port_registry(&self) -> &PortRegistry {
        &self.port_registry
    }

    /// Check if a node kind is registered.
    #[must_use]
    pub fn has_kind(&self, kind: &str) -> bool {
        self.constructors.contains_key(kind)
    }

    /// Get all registered node kinds.
    #[must_use]
    pub fn registered_kinds(&self) -> Vec<String> {
        self.constructors.keys().cloned().collect()
    }

    /// Construct a boxed node from a graph node spec.
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if the node kind is not registered or
    /// construction fails.
    pub fn construct(
        &self,
        spec: &eureka_graph::spec::GraphNodeSpec,
    ) -> Result<BoxedNode, EngineError> {
        let constructor = self.constructors.get(&spec.kind).ok_or_else(|| {
            EngineError::UnknownNodeKind(format!(
                "Node kind '{}' is not registered. Registered kinds: {:?}",
                spec.kind,
                self.registered_kinds()
            ))
        })?;
        constructor(spec)
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for NodeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeRegistry")
            .field("registered_kinds", &self.registered_kinds())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use eureka_graph::node::{Emit, Node, NodeCtx, NodeError, PortMsg};
    use eureka_graph::port::{PortDirection, PortSpecEntry};
    use eureka_graph::spec::GraphNodeSpec;
    use std::sync::Arc;

    struct TestNode;

    #[async_trait]
    impl Node for TestNode {
        fn ports(&self) -> PortSpec {
            PortSpec::new(
                vec![PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Goal".to_string(),
                    required: true,
                }],
                vec![PortSpecEntry {
                    name: "out".into(),
                    direction: PortDirection::Output,
                    kind: "Hypotheses".to_string(),
                    required: false,
                }],
            )
        }

        async fn process(&self, _ctx: &NodeCtx, _msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
            Ok(vec![])
        }
    }

    #[test]
    fn test_registry() {
        let mut registry = NodeRegistry::new();
        let ports = PortSpec::new(vec![], vec![]);
        registry.register(
            "test.node",
            Arc::new(|_spec| Ok(BoxedNode::new(TestNode))),
            ports,
        );

        assert!(registry.has_kind("test.node"));
        assert!(!registry.has_kind("nonexistent"));

        let spec = GraphNodeSpec {
            id: "test".into(),
            kind: "test.node".into(),
            config: serde_json::Value::Null,
            description: None,
        };
        let node = registry.construct(&spec).unwrap();
        assert_eq!(node.ports().input_names().len(), 1);
    }
}
