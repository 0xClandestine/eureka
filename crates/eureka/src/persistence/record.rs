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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::artifact::Artifact;

    #[test]
    fn run_record_new_sets_created_status() {
        let id = uuid::Uuid::now_v7();
        let goal = serde_json::json!({"goal": "find cure"});
        let record = RunRecord::new(id, "example/coscientist.yml", goal.clone());
        assert_eq!(record.id, id);
        assert_eq!(record.graph, "example/coscientist.yml");
        assert_eq!(record.goal, goal);
        assert_eq!(record.status, RunStatus::Created);
        assert!(record.error.is_none());
        assert!(record.stats.is_none());
    }

    #[test]
    fn run_status_serde_snake_case() {
        let cases = [
            (RunStatus::Created, "created"),
            (RunStatus::Running, "running"),
            (RunStatus::Paused, "paused"),
            (RunStatus::Completed, "completed"),
            (RunStatus::Failed, "failed"),
            (RunStatus::Cancelled, "cancelled"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json.as_str().unwrap(), expected, "variant {variant:?}");
            let back: RunStatus = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn run_status_deserialize_rejects_invalid() {
        let result: Result<RunStatus, _> = serde_json::from_str("\"unknown_status\"");
        assert!(result.is_err());
    }

    #[test]
    fn run_record_serialization_round_trip() {
        let id = uuid::Uuid::now_v7();
        let original = RunRecord {
            id,
            graph: "graph.yml".into(),
            goal: serde_json::json!({"x": 1}),
            status: RunStatus::Running,
            revision: crate::persistence::store::Revision(3),
            stats: Some(crate::config::RunStats {
                total_cost_usd: 1.5,
                total_tokens: 1000,
                total_input_tokens: 600,
                total_output_tokens: 400,
                elapsed_secs: 42.0,
                rounds_completed: 3,
            }),
            error: Some("oops".into()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: RunRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.status, original.status);
        assert_eq!(restored.error, original.error);
        assert_eq!(restored.stats, original.stats);
    }

    #[test]
    fn run_filter_default_has_no_constraints() {
        let filter = RunFilter::default();
        assert!(filter.status.is_none());
        assert!(filter.limit.is_none());
    }

    #[test]
    fn run_filter_with_status_and_limit() {
        let filter = RunFilter { status: Some(RunStatus::Failed), limit: Some(5) };
        assert_eq!(filter.status, Some(RunStatus::Failed));
        assert_eq!(filter.limit, Some(5));
    }

    #[test]
    fn run_filter_limit_zero() {
        let filter = RunFilter { status: None, limit: Some(0) };
        assert_eq!(filter.limit, Some(0));
    }

    #[test]
    fn run_output_serialization_round_trip() {
        let output = RunOutput {
            node_id: "generation".into(),
            port: "hypotheses".into(),
            round: 2,
            artifact: Artifact {
                kind: "Hypothesis".into(),
                data: serde_json::json!({"text": "X"}),
            },
        };
        let json = serde_json::to_string(&output).unwrap();
        let restored: RunOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, output);
    }

    #[test]
    fn run_output_equality() {
        let a = RunOutput {
            node_id: "n1".into(),
            port: "p1".into(),
            round: 0,
            artifact: Artifact { kind: "K".into(), data: serde_json::json!({}) },
        };
        let b = RunOutput {
            node_id: "n1".into(),
            port: "p1".into(),
            round: 0,
            artifact: Artifact { kind: "K".into(), data: serde_json::json!({}) },
        };
        assert_eq!(a, b);
    }

    #[test]
    fn run_record_revision_default_is_zero() {
        let id = uuid::Uuid::now_v7();
        let record = RunRecord::new(id, "g.yml", serde_json::json!({}));
        assert_eq!(record.revision.0, 0);
    }

    #[test]
    fn run_record_deserialize_missing_revision_defaults_to_zero() {
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "graph": "g.yml",
            "goal": {},
            "status": "created"
        });
        let record: RunRecord = serde_json::from_value(json).unwrap();
        assert_eq!(record.revision.0, 0);
    }

    #[test]
    fn run_record_deserialize_missing_stats_and_error_defaults_to_none() {
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "graph": "g.yml",
            "goal": {},
            "status": "completed"
        });
        let record: RunRecord = serde_json::from_value(json).unwrap();
        assert!(record.stats.is_none());
        assert!(record.error.is_none());
    }

    #[test]
    fn run_record_revision_must_not_be_negative_in_practice() {
        let record = RunRecord::new(uuid::Uuid::now_v7(), "g.yml", serde_json::json!({}));
        assert!(record.revision.0 >= 0);
    }
}
