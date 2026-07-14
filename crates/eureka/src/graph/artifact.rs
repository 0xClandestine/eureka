//! Artifact types — the typed envelope for data flowing through the graph.
//!
//! An artifact is a `(kind, data)` pair where `kind` is a plain string used
//! for port type-matching and `data` is opaque JSON interpreted by each node.
//! This keeps `eureka-graph` free of any domain knowledge: the kind strings
//! are defined by agent JSON files and control-node configurations, not by
//! any Rust enum here.

use serde::{Deserialize, Serialize, ser::Error as _};

/// The kind of artifact — a plain string tag used for port type-matching.
///
/// Values are defined by agent JSON files (e.g. `"Hypotheses"`, `"Reviews"`)
/// and control node configurations. The graph engine treats them as opaque;
/// it only checks that the kind on an output port matches the kind on the
/// connected input port.
pub type ArtifactKind = String;

/// An artifact flowing through the graph: a kind tag plus JSON data.
///
/// The `kind` field enables build-time port validation — every edge's
/// producer kind must match the consumer port's expected kind, proven by
/// the validator before any model call is made. The `data` field is opaque
/// JSON that each node deserializes according to its own schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The kind of this artifact, used for port type-checking.
    pub kind: ArtifactKind,
    /// The artifact payload as JSON.
    pub data: serde_json::Value,
}

impl Artifact {
    /// Construct an artifact by serializing a typed value.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if the value cannot be serialized.
    pub fn new<T: Serialize>(
        kind: impl Into<String>,
        value: &T,
    ) -> Result<Self, serde_json::Error> {
        let kind = kind.into();
        if kind.is_empty() {
            return Err(serde_json::Error::custom("artifact kind must not be empty"));
        }
        Ok(Self {
            kind,
            data: serde_json::to_value(value)?,
        })
    }

    /// Deserialize the artifact's payload into a typed value.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if the data cannot be deserialized.
    pub fn deserialize_as<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.data.clone())
    }
}

impl std::fmt::Display for Artifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Artifact({})", self.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_artifact_roundtrip() {
        let data = json!({ "goal": "Test Goal" });
        let artifact = Artifact {
            kind: "Goal".to_string(),
            data: data.clone(),
        };
        assert_eq!(artifact.kind, "Goal");
        assert_eq!(artifact.data, data);
    }

    #[test]
    fn test_artifact_new_and_deserialize() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Payload {
            count: u32,
        }
        let p = Payload { count: 42 };
        let artifact = Artifact::new("Hypotheses", &p).unwrap();
        assert_eq!(artifact.kind, "Hypotheses");
        let decoded: Payload = artifact.deserialize_as().unwrap();
        assert_eq!(decoded.count, 42);
    }

    #[test]
    fn test_kind_is_string() {
        let kind: ArtifactKind = "Overview".to_string();
        assert_eq!(kind, "Overview");
    }
}
