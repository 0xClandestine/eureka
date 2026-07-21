//! Error types for the engine layer.

use thiserror::Error;

/// Errors that can occur during engine operations.
#[derive(Debug, Error)]
pub enum EngineError {
    /// A graph specification error occurred.
    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::spec::GraphError),
    /// A configuration error occurred.
    #[error("Config error: {0}")]
    Config(#[from] crate::config::ConfigError),
    /// A scheduler error occurred.
    #[error("Scheduler error: {0}")]
    Scheduler(String),
    /// A node could not be created from the spec.
    #[error("Node creation error: {0}")]
    NodeCreation(String),
    /// An unknown node kind was encountered.
    #[error("Unknown node kind: {0}")]
    UnknownNodeKind(String),
    /// The run could not be started.
    #[error("Run error: {0}")]
    Run(String),
    /// A store error occurred.
    #[error("Store error: {0}")]
    Store(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_error_displays_with_prefix() {
        let err =
            EngineError::Graph(crate::graph::spec::GraphError::ParseError("bad graph".into()));
        assert_eq!(err.to_string(), "Graph error: bad graph");
    }

    #[test]
    fn config_error_displays_with_prefix() {
        let err = EngineError::Config(crate::config::ConfigError::ParseError("bad toml".into()));
        assert_eq!(err.to_string(), "Config error: Parse error: bad toml");
    }

    #[test]
    fn scheduler_error_displays() {
        let err = EngineError::Scheduler("timeout".into());
        assert_eq!(err.to_string(), "Scheduler error: timeout");
    }

    #[test]
    fn node_creation_error_displays() {
        let err = EngineError::NodeCreation("missing model".into());
        assert_eq!(err.to_string(), "Node creation error: missing model");
    }

    #[test]
    fn unknown_node_kind_displays() {
        let err = EngineError::UnknownNodeKind("foo".into());
        assert_eq!(err.to_string(), "Unknown node kind: foo");
    }

    #[test]
    fn run_error_displays() {
        let err = EngineError::Run("session died".into());
        assert_eq!(err.to_string(), "Run error: session died");
    }

    #[test]
    fn store_error_displays() {
        let err = EngineError::Store("disk full".into());
        assert_eq!(err.to_string(), "Store error: disk full");
    }

    #[test]
    fn from_graph_error_via_question_mark() {
        let ge = crate::graph::spec::GraphError::ParseError("oops".into());
        let engine_err: EngineError = ge.into();
        assert!(matches!(engine_err, EngineError::Graph(_)));
    }

    #[test]
    fn from_config_error_via_question_mark() {
        let ce = crate::config::ConfigError::FileError("missing".into());
        let engine_err: EngineError = ce.into();
        assert!(matches!(engine_err, EngineError::Config(_)));
    }

    #[test]
    fn debug_format_does_not_panic() {
        let err = EngineError::Run("test".into());
        let _ = format!("{err:?}");
    }

    #[test]
    fn all_variants_produce_non_empty_display() {
        let variants: &[EngineError] = &[
            EngineError::Scheduler("a".into()),
            EngineError::NodeCreation("a".into()),
            EngineError::UnknownNodeKind("a".into()),
            EngineError::Run("a".into()),
            EngineError::Store("a".into()),
        ];
        for v in variants {
            assert!(!v.to_string().is_empty(), "variant {v:?} had empty display");
        }
    }
}
