//! Port types — typed input/output ports for graph nodes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactKind;

/// A unique identifier for a port within a node.
///
/// Convention: `<node_id>.<port_name>`, e.g., `generation.out`.
pub type PortId = String;

/// The direction of a port: input (inbound) or output (outbound).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum PortDirection {
    /// An input port — receives artifacts from inbound edges.
    Input,
    /// An output port — emits artifacts to outbound edges.
    Output,
}

/// Specification of a single port on a node.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PortSpecEntry {
    /// The port name (unique within the node).
    pub name: String,
    /// The direction of this port.
    pub direction: PortDirection,
    /// The kind of artifact this port accepts (input) or emits (output).
    pub kind: ArtifactKind,
    /// Whether this port is required (input ports) or optional.
    #[serde(default)]
    pub required: bool,
}

/// The full port specification for a node.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct PortSpec {
    /// All input ports.
    pub inputs: Vec<PortSpecEntry>,
    /// All output ports.
    pub outputs: Vec<PortSpecEntry>,
}

impl PortSpec {
    /// Create a new port spec from input and output entries.
    #[must_use]
    pub const fn new(inputs: Vec<PortSpecEntry>, outputs: Vec<PortSpecEntry>) -> Self {
        Self { inputs, outputs }
    }

    /// Get the kind expected by an input port.
    #[must_use]
    pub fn input_kind(&self, name: &str) -> Option<ArtifactKind> {
        self.inputs
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.kind.clone())
    }

    /// Get the kind emitted by an output port.
    #[must_use]
    pub fn output_kind(&self, name: &str) -> Option<ArtifactKind> {
        self.outputs
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.kind.clone())
    }

    /// Check if an input port exists and is required.
    #[must_use]
    pub fn is_input_required(&self, name: &str) -> bool {
        self.inputs
            .iter()
            .find(|p| p.name == name)
            .is_some_and(|p| p.required)
    }

    /// Return all required input port names.
    #[must_use]
    pub fn required_inputs(&self) -> Vec<String> {
        self.inputs
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.clone())
            .collect()
    }

    /// Return all output port names.
    #[must_use]
    pub fn output_names(&self) -> Vec<String> {
        self.outputs.iter().map(|p| p.name.clone()).collect()
    }

    /// Return all input port names.
    #[must_use]
    pub fn input_names(&self) -> Vec<String> {
        self.inputs.iter().map(|p| p.name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_spec() -> PortSpec {
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

    #[test]
    fn test_input_kind() {
        let spec = test_spec();
        assert_eq!(spec.input_kind("in"), Some("Goal".to_string()));
        assert_eq!(spec.input_kind("nonexistent"), None);
    }

    #[test]
    fn test_required_inputs() {
        let spec = test_spec();
        let required = spec.required_inputs();
        assert!(required.contains(&"in".to_string()));
    }
}
