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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_not_found_displays_id() {
        let err = SchedulerError::NodeNotFound("ghost".into());
        assert_eq!(err.to_string(), "Node not found: ghost");
    }

    #[test]
    fn internal_displays_message() {
        let err = SchedulerError::Internal("ratelimit exceeded".into());
        assert_eq!(err.to_string(), "Internal scheduler error: ratelimit exceeded");
    }

    #[test]
    fn cancelled_displays() {
        let err = SchedulerError::Cancelled;
        assert_eq!(err.to_string(), "Run cancelled");
    }

    #[test]
    fn paused_displays_stats() {
        let stats = RunStats { rounds_completed: 5, ..RunStats::default() };
        let err = SchedulerError::Paused(stats);
        let msg = err.to_string();
        assert!(msg.contains("Run paused"), "should mention pause: {msg}");
        assert!(msg.contains("5"), "should contain round count: {msg}");
    }

    #[test]
    fn checkpoint_mismatch_displays() {
        let err = SchedulerError::CheckpointMismatch;
        assert_eq!(
            err.to_string(),
            "checkpoint graph or configuration identity does not match the scheduler"
        );
    }

    #[test]
    fn node_failed_displays_all_fields() {
        let err = SchedulerError::NodeFailed {
            node_id: "gen".into(),
            round: 3,
            error: "LLM timeout".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("gen"), "should contain node_id: {msg}");
        assert!(msg.contains("3"), "should contain round: {msg}");
        assert!(msg.contains("LLM timeout"), "should contain error: {msg}");
    }

    #[test]
    fn paused_preserves_stats_for_caller() {
        let stats = RunStats {
            total_cost_usd: 2.5,
            total_tokens: 5000,
            elapsed_secs: 120.0,
            rounds_completed: 2,
            ..RunStats::default()
        };
        match SchedulerError::Paused(stats.clone()) {
            SchedulerError::Paused(s) => assert_eq!(s, stats),
            _ => panic!("expected Paused variant"),
        }
    }

    #[test]
    fn all_variants_debug_and_display_non_empty() {
        let variants: &[SchedulerError] = &[
            SchedulerError::NodeNotFound("x".into()),
            SchedulerError::Internal("x".into()),
            SchedulerError::Cancelled,
            SchedulerError::Paused(RunStats::default()),
            SchedulerError::CheckpointMismatch,
            SchedulerError::NodeFailed { node_id: "x".into(), round: 0, error: "x".into() },
        ];
        for v in variants {
            assert!(!v.to_string().is_empty(), "display empty for {v:?}");
            let _ = format!("{v:?}");
        }
    }
}
