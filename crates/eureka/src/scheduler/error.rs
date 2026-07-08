use thiserror::Error;

/// Errors that can occur during scheduling.
#[derive(Debug, Error)]
pub enum SchedulerError {
    /// A node was not found in the registry.
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    /// An internal scheduler error occurred.
    #[error("Internal scheduler error: {0}")]
    Internal(String),

    /// The run was cancelled.
    #[error("Run cancelled")]
    Cancelled,
}
