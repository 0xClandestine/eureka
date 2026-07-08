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
