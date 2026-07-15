//! Port types — typed input/output ports for graph nodes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::artifact::ArtifactKind;

/// A lightweight port declaration used in YAML manifest and agent definitions.
///
/// Fields match the YAML schema (`port` + `kind`, with an optional
/// `required` for inputs). Converts to [`PortSpecEntry`] for use in the
/// node registry.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct PortDef {
    /// The port name used in graph edges (e.g. `"in"`, `"out"`, `"top"`).
    pub port: String,
    /// The artifact kind accepted/emitted on this port (e.g. `"Hypotheses"`).
    pub kind: ArtifactKind,
    /// Whether this input port is required. Only meaningful for inputs;
    /// outputs are always optional. Defaults to `true` for inputs when
    /// omitted, preserving backward compatibility. Set `required: false`
    /// to declare an optional secondary input that need not be wired for
    /// every activation.
    #[serde(default)]
    pub required: Option<bool>,
}

impl PortDef {
    /// Convert this port declaration to an input `PortSpecEntry`. The port is
    /// required unless `required: false` is explicitly set in the manifest.
    #[must_use]
    pub fn to_input_spec(&self) -> PortSpecEntry {
        PortSpecEntry {
            name: self.port.clone(),
            direction: PortDirection::Input,
            kind: self.kind.clone(),
            required: self.required.unwrap_or(true),
        }
    }

    /// Convert this port declaration to an output `PortSpecEntry` (optional by default).
    #[must_use]
    pub fn to_output_spec(&self) -> PortSpecEntry {
        PortSpecEntry {
            name: self.port.clone(),
            direction: PortDirection::Output,
            kind: self.kind.clone(),
            required: false,
        }
    }
}

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
        self.inputs.iter().find(|p| p.name == name).map(|p| p.kind.clone())
    }

    /// Get the kind emitted by an output port.
    #[must_use]
    pub fn output_kind(&self, name: &str) -> Option<ArtifactKind> {
        self.outputs.iter().find(|p| p.name == name).map(|p| p.kind.clone())
    }

    /// Check if an input port exists and is required.
    #[must_use]
    pub fn is_input_required(&self, name: &str) -> bool {
        self.inputs.iter().find(|p| p.name == name).is_some_and(|p| p.required)
    }

    /// Return all required input port names.
    #[must_use]
    pub fn required_inputs(&self) -> Vec<String> {
        self.inputs.iter().filter(|p| p.required).map(|p| p.name.clone()).collect()
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

    /// Build a `PortSpec` directly from `PortDef` slices (the common case for
    /// agents, control nodes, and manifests).
    #[must_use]
    pub fn from_defs(inputs: &[PortDef], outputs: &[PortDef]) -> Self {
        Self::new(
            inputs.iter().map(PortDef::to_input_spec).collect(),
            outputs.iter().map(PortDef::to_output_spec).collect(),
        )
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

    #[test]
    fn test_optional_input_via_manifest() {
        // Regression (F): a PortDef with `required: false` must produce an
        // optional input port spec (previously every input was forced required).
        let optional = PortDef {
            port: "graph".to_string(),
            kind: "ProximityGraph".to_string(),
            required: Some(false),
        };
        let entry = optional.to_input_spec();
        assert!(!entry.required);
        assert!(!PortSpec::new(vec![entry], vec![]).is_input_required("graph"));

        // And the default (None) is still required for backward compat.
        let default = PortDef { port: "in".to_string(), kind: "Goal".to_string(), required: None };
        assert!(default.to_input_spec().required);
    }
}
