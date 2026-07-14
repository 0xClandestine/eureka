//! Durable run record types.

use serde::{Deserialize, Serialize};

use crate::config::RunStats;

/// Lifecycle state persisted for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// A record has been created but execution has not started.
    Created,
    /// Execution is currently in progress.
    Running,
    /// Execution was paused by an external signal.
    Paused,
    /// Execution completed normally.
    Completed,
    /// Execution failed.
    Failed,
    /// Execution was cancelled.
    Cancelled,
}

/// Durable metadata for one graph execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Stable run identifier.
    pub id: uuid::Uuid,
    /// Graph manifest path used by the run.
    pub graph: String,
    /// Initial goal supplied to the run.
    pub goal: serde_json::Value,
    /// Current lifecycle status.
    pub status: RunStatus,
    /// Revision used for optimistic concurrency control.
    #[serde(default)]
    pub revision: super::store::Revision,
    /// Most recent scheduler statistics, when available.
    pub stats: Option<RunStats>,
    /// Human-readable failure or cancellation reason.
    pub error: Option<String>,
}

impl RunRecord {
    /// Create a new record in the [`RunStatus::Created`] state.
    #[must_use]
    pub fn new(id: uuid::Uuid, graph: impl Into<String>, goal: serde_json::Value) -> Self {
        Self {
            id,
            graph: graph.into(),
            goal,
            status: RunStatus::Created,
            revision: super::store::Revision::default(),
            stats: None,
            error: None,
        }
    }
}

/// Filter used when listing persisted runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFilter {
    /// Restrict results to one lifecycle status.
    pub status: Option<RunStatus>,
    /// Maximum number of records to return.
    pub limit: Option<usize>,
}

/// An artifact emitted by a terminal/sink node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutput {
    /// Node that emitted the artifact.
    pub node_id: String,
    /// Output port that emitted the artifact.
    pub port: String,
    /// Scheduler round in which it was emitted.
    pub round: u32,
    /// Terminal artifact.
    pub artifact: crate::graph::artifact::Artifact,
}
