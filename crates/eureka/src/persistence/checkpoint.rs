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
    /// Returns `PersistenceError::AlreadyExists` if an input already exists for the
    /// same (`node_id`, port) pair.
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
        self.pending_inputs.push(PendingInput {
            node_id,
            round: self.round,
            port,
            artifact,
        });
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

