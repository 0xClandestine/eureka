//! Scheduler checkpoint types for durable recovery.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::RunStats;
use crate::graph::node::PortMsg;

use super::record::RunOutput;
use super::store::{PersistenceError, Revision};

/// Why a scheduler checkpoint was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointReason {
    /// Checkpoint written after a completed scheduler round.
    RoundCompleted,
    /// Checkpoint written after a requested pause.
    Pause,
    /// Checkpoint written after cancellation.
    Cancellation,
    /// Checkpoint written before terminal completion.
    Completion,
    /// Checkpoint written after accepting external input.
    InputAccepted,
}

/// An input artifact waiting for a node activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInput {
    /// Target node ID.
    pub node_id: String,
    /// Scheduler round containing the input.
    pub round: u32,
    /// Target input port.
    pub port: String,
    /// Artifact waiting at the port.
    pub artifact: crate::graph::artifact::Artifact,
}

/// A ready activation captured at a scheduler boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationSnapshot {
    /// Node ID to activate.
    pub node_id: String,
    /// Scheduler round containing the activation.
    pub round: u32,
    /// Joined input messages.
    pub inputs: Vec<PortMsg>,
}

/// Serializable scheduler state used for durable recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCheckpoint {
    /// Run this checkpoint belongs to.
    pub run_id: uuid::Uuid,
    /// Monotonic checkpoint revision.
    pub revision: Revision,
    /// Hash of the graph manifest/topology used to create the checkpoint.
    pub graph_hash: String,
    /// Hash of runtime configuration used to create the checkpoint.
    pub config_hash: String,
    /// Current synchronized scheduler round.
    pub round: u32,
    /// Outstanding activation count by round.
    pub round_pending: HashMap<u32, usize>,
    /// Artifacts buffered for not-yet-ready nodes.
    pub pending_inputs: Vec<PendingInput>,
    /// Activations ready to dispatch at the checkpoint boundary.
    pub ready_activations: Vec<ActivationSnapshot>,
    /// Statistics accumulated through this boundary.
    pub stats: RunStats,
    /// Terminal artifacts emitted through this boundary.
    #[serde(default)]
    pub outputs: Vec<RunOutput>,
    /// Reason this checkpoint was written.
    pub reason: CheckpointReason,
}

impl RunCheckpoint {
    /// Add a human or external artifact to a target port in this checkpoint.
    ///
    /// The graph/session layer performs node and port validation before calling
    /// this method. Replacing an existing artifact on the same port is
    /// rejected so an input cannot be silently lost.
    ///
    /// # Errors
    /// Returns `PersistenceError::InputConflict` if an input already exists for the
    /// same (`node_id`, port) pair at the current round.
    pub fn inject_input(
        &mut self,
        node_id: impl Into<String>,
        port: impl Into<String>,
        artifact: crate::graph::artifact::Artifact,
    ) -> Result<(), PersistenceError> {
        let node_id = node_id.into();
        let port = port.into();
        if self.pending_inputs.iter().any(|input| {
            input.node_id == node_id && input.port == port && input.round == self.round
        }) {
            return Err(PersistenceError::InputConflict { node_id, port });
        }
        self.pending_inputs.push(PendingInput { node_id, round: self.round, port, artifact });
        Ok(())
    }

    /// Create an empty checkpoint at revision zero.
    #[must_use]
    pub fn new(run_id: uuid::Uuid, graph_hash: String, config_hash: String) -> Self {
        Self {
            run_id,
            revision: Revision::default(),
            graph_hash,
            config_hash,
            round: 0,
            round_pending: HashMap::new(),
            pending_inputs: Vec::new(),
            ready_activations: Vec::new(),
            stats: RunStats::default(),
            outputs: Vec::new(),
            reason: CheckpointReason::RoundCompleted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::artifact::Artifact;

    #[test]
    fn checkpoint_reason_serde_snake_case() {
        let cases = [
            (CheckpointReason::RoundCompleted, "round_completed"),
            (CheckpointReason::Pause, "pause"),
            (CheckpointReason::Cancellation, "cancellation"),
            (CheckpointReason::Completion, "completion"),
            (CheckpointReason::InputAccepted, "input_accepted"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json.as_str().unwrap(), expected, "variant {variant:?}");
            let back: CheckpointReason = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn checkpoint_new_has_revision_zero_and_default_round() {
        let id = uuid::Uuid::now_v7();
        let cp = RunCheckpoint::new(id, "gh".into(), "ch".into());
        assert_eq!(cp.run_id, id);
        assert_eq!(cp.graph_hash, "gh");
        assert_eq!(cp.config_hash, "ch");
        assert_eq!(cp.revision.0, 0);
        assert_eq!(cp.round, 0);
        assert_eq!(cp.reason, CheckpointReason::RoundCompleted);
        assert!(cp.pending_inputs.is_empty());
        assert!(cp.ready_activations.is_empty());
        assert!(cp.outputs.is_empty());
        assert_eq!(cp.stats, RunStats::default());
    }

    #[test]
    fn inject_input_adds_pending_input() {
        let id = uuid::Uuid::now_v7();
        let mut cp = RunCheckpoint::new(id, "gh".into(), "ch".into());
        let artifact = Artifact { kind: "Goal".into(), data: serde_json::json!({"x": 1}) };
        cp.inject_input("gen", "goal", artifact.clone()).unwrap();
        assert_eq!(cp.pending_inputs.len(), 1);
        assert_eq!(cp.pending_inputs[0].node_id, "gen");
        assert_eq!(cp.pending_inputs[0].port, "goal");
        assert_eq!(cp.pending_inputs[0].round, 0);
        assert_eq!(cp.pending_inputs[0].artifact, artifact);
    }

    #[test]
    fn inject_input_rejects_duplicate_same_port_same_round() {
        let id = uuid::Uuid::now_v7();
        let mut cp = RunCheckpoint::new(id, "gh".into(), "ch".into());
        let artifact = Artifact { kind: "Goal".into(), data: serde_json::json!({}) };
        cp.inject_input("gen", "goal", artifact.clone()).unwrap();
        let err = cp.inject_input("gen", "goal", artifact).unwrap_err();
        assert!(matches!(err, PersistenceError::InputConflict { .. }));
    }

    #[test]
    fn inject_input_allows_same_port_different_round() {
        let id = uuid::Uuid::now_v7();
        let mut cp = RunCheckpoint::new(id, "gh".into(), "ch".into());
        let a1 = Artifact { kind: "Goal".into(), data: serde_json::json!({}) };
        cp.inject_input("gen", "goal", a1.clone()).unwrap();
        cp.round = 1;
        cp.inject_input("gen", "goal", a1).unwrap();
        assert_eq!(cp.pending_inputs.len(), 2);
    }

    #[test]
    fn inject_input_allows_different_port_same_round() {
        let id = uuid::Uuid::now_v7();
        let mut cp = RunCheckpoint::new(id, "gh".into(), "ch".into());
        let a = Artifact { kind: "Goal".into(), data: serde_json::json!({}) };
        cp.inject_input("gen", "port_a", a.clone()).unwrap();
        cp.inject_input("gen", "port_b", a).unwrap();
        assert_eq!(cp.pending_inputs.len(), 2);
    }

    #[test]
    fn checkpoint_serialization_round_trip() {
        let id = uuid::Uuid::now_v7();
        let original = RunCheckpoint {
            run_id: id,
            revision: Revision(5),
            graph_hash: "abc".into(),
            config_hash: "def".into(),
            round: 3,
            round_pending: [(1, 2usize), (2, 1)].into_iter().collect(),
            pending_inputs: vec![PendingInput {
                node_id: "n".into(),
                round: 3,
                port: "p".into(),
                artifact: Artifact { kind: "K".into(), data: serde_json::json!({}) },
            }],
            ready_activations: vec![ActivationSnapshot {
                node_id: "n".into(),
                round: 3,
                inputs: vec![],
            }],
            stats: RunStats { rounds_completed: 3, ..RunStats::default() },
            outputs: vec![],
            reason: CheckpointReason::Pause,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: RunCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.run_id, original.run_id);
        assert_eq!(restored.revision, original.revision);
        assert_eq!(restored.graph_hash, original.graph_hash);
        assert_eq!(restored.round, original.round);
        assert_eq!(restored.reason, original.reason);
        assert_eq!(restored.pending_inputs, original.pending_inputs);
        assert_eq!(restored.ready_activations, original.ready_activations);
        assert_eq!(restored.stats, original.stats);
    }

    #[test]
    fn pending_input_serde_round_trip() {
        let input = PendingInput {
            node_id: "node".into(),
            round: 2,
            port: "in".into(),
            artifact: Artifact { kind: "Hypothesis".into(), data: serde_json::json!({"x": 1}) },
        };
        let json = serde_json::to_string(&input).unwrap();
        let back: PendingInput = serde_json::from_str(&json).unwrap();
        assert_eq!(back, input);
    }

    #[test]
    fn activation_snapshot_serde_round_trip() {
        use crate::graph::node::PortMsg;
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact { kind: "Goal".into(), data: serde_json::json!({}) },
        };
        let snap = ActivationSnapshot { node_id: "gen".into(), round: 1, inputs: vec![msg] };
        let json = serde_json::to_string(&snap).unwrap();
        let back: ActivationSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.node_id, snap.node_id);
        assert_eq!(back.round, snap.round);
        assert_eq!(back.inputs.len(), 1);
        assert_eq!(back.inputs[0], snap.inputs[0]);
    }

    #[test]
    fn checkpoint_outputs_field_defaults_to_empty_vec_on_deserialize() {
        let json = serde_json::json!({
            "run_id": "00000000-0000-0000-0000-000000000000",
            "revision": 0,
            "graph_hash": "gh",
            "config_hash": "ch",
            "round": 0,
            "round_pending": {},
            "pending_inputs": [],
            "ready_activations": [],
            "stats": {
                "total_cost_usd": 0.0,
                "total_tokens": 0,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "elapsed_secs": 0.0,
                "rounds_completed": 0
            },
            "reason": "pause"
        });
        let cp: RunCheckpoint = serde_json::from_value(json).unwrap();
        assert!(cp.outputs.is_empty());
    }

    #[test]
    fn checkpoint_reason_deserialize_rejects_invalid() {
        let result: Result<CheckpointReason, _> = serde_json::from_str("\"unknown_reason\"");
        assert!(result.is_err());
    }

    #[test]
    fn checkpoint_deserialize_missing_round_pending_fails() {
        let json = serde_json::json!({
            "run_id": "00000000-0000-0000-0000-000000000000",
            "revision": 0,
            "graph_hash": "gh",
            "config_hash": "ch",
            "round": 0,
            "pending_inputs": [],
            "ready_activations": [],
            "stats": {
                "total_cost_usd": 0.0,
                "total_tokens": 0,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "elapsed_secs": 0.0,
                "rounds_completed": 0
            },
            "reason": "round_completed"
        });
        let result: Result<RunCheckpoint, _> = serde_json::from_value(json);
        assert!(result.is_err(), "missing required field round_pending should fail");
    }

    #[test]
    fn pending_input_equality_is_deterministic() {
        let a = PendingInput {
            node_id: "n".into(),
            round: 1,
            port: "p".into(),
            artifact: crate::graph::artifact::Artifact {
                kind: "K".into(),
                data: serde_json::json!({}),
            },
        };
        let b = PendingInput {
            node_id: "n".into(),
            round: 1,
            port: "p".into(),
            artifact: crate::graph::artifact::Artifact {
                kind: "K".into(),
                data: serde_json::json!({}),
            },
        };
        assert_eq!(a, b);
    }

    #[test]
    fn inject_input_different_node_same_port_no_conflict() {
        let id = uuid::Uuid::now_v7();
        let mut cp = RunCheckpoint::new(id, "gh".into(), "ch".into());
        let a =
            crate::graph::artifact::Artifact { kind: "Goal".into(), data: serde_json::json!({}) };
        cp.inject_input("gen", "goal", a.clone()).unwrap();
        cp.inject_input("ref", "goal", a).unwrap();
        assert_eq!(cp.pending_inputs.len(), 2);
    }
}
