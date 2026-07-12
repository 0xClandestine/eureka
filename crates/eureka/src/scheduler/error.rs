use thiserror::Error;

use crate::config::RunStats;

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

    /// The run was paused and can be resumed by the caller.
    #[error("Run paused after round {0:?}")]
    Paused(RunStats),

    /// A durable checkpoint does not match the scheduler identity.
    #[error("checkpoint graph or configuration identity does not match the scheduler")]
    CheckpointMismatch,

    /// A node activation failed and the run was aborted.
    #[error("Node '{node_id}' failed in round {round}: {error}")]
    NodeFailed {
        /// The failed node ID.
        node_id: String,
        /// The round containing the failed activation.
        round: u32,
        /// The node error text.
        error: String,
    },
}
