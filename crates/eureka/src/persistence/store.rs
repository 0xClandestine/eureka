//! Persistence trait definitions and error types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::checkpoint::RunCheckpoint;
use super::record::{RunFilter, RunRecord};

/// Monotonically increasing revision for optimistic concurrency control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub i64);

/// A durable scheduler event stored for a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    /// Run that produced the event.
    pub run_id: uuid::Uuid,
    /// Monotonically increasing event sequence within the run.
    pub sequence: u64,
    /// Unix timestamp in milliseconds when the event was stored.
    pub timestamp_ms: u64,
    /// Serialized scheduler event payload.
    pub event: serde_json::Value,
}

/// Errors returned by the versioned persistence APIs.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// The requested run does not exist.
    #[error("run {0} was not found")]
    NotFound(uuid::Uuid),
    /// A run already exists with the requested identifier.
    #[error("run {0} already exists")]
    AlreadyExists(uuid::Uuid),
    /// An optimistic-concurrency revision did not match.
    #[error("revision conflict for run {run_id}: expected {expected:?}, actual {actual:?}")]
    RevisionConflict {
        /// The run whose revision conflicted.
        run_id: uuid::Uuid,
        /// The revision supplied by the caller.
        expected: Revision,
        /// The revision currently stored.
        actual: Revision,
    },
    /// The persistence backend could not complete an operation.
    #[error("persistence I/O error: {0}")]
    Io(#[source] std::io::Error),
    /// A persisted record was malformed.
    #[error("invalid persisted run record: {0}")]
    Serialization(String),
    /// An external artifact would overwrite an existing buffered input.
    #[error("input already exists for node '{node_id}' port '{port}'")]
    InputConflict {
        /// Target node ID.
        node_id: String,
        /// Target input port.
        port: String,
    },
}

impl From<std::io::Error> for PersistenceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for PersistenceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Io(std::io::Error::other(error.to_string()))
    }
}

impl From<serde_json::Error> for PersistenceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

/// Persistence interface for the latest durable scheduler checkpoint.
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    /// Save a checkpoint, optionally requiring the current revision to match.
    async fn save_checkpoint(
        &self,
        checkpoint: RunCheckpoint,
        expected: Option<Revision>,
    ) -> Result<RunCheckpoint, PersistenceError>;
    /// Load the latest complete checkpoint for a run.
    async fn load_checkpoint(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<RunCheckpoint>, PersistenceError>;
    /// Delete the checkpoint for a run.
    async fn delete_checkpoint(&self, run_id: uuid::Uuid) -> Result<(), PersistenceError>;
}

/// Persistence interface for the append-only scheduler event history.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append one serialized scheduler event and assign its sequence number.
    async fn append_event(
        &self,
        run_id: uuid::Uuid,
        event: serde_json::Value,
    ) -> Result<RunEvent, PersistenceError>;
    /// Load events in execution order.
    async fn load_events(&self, run_id: uuid::Uuid) -> Result<Vec<RunEvent>, PersistenceError>;
    /// Delete all events for a run.
    async fn delete_events(&self, run_id: uuid::Uuid) -> Result<(), PersistenceError>;
}

/// A persistence backend that supports run metadata, checkpoints, and events.
///
/// This is the canonical storage boundary for lifecycle management. The
/// lower-level [`RunRepository`] API remains available for callers that need
/// optimistic-concurrency details directly.
pub trait RunPersistence: RunStore + CheckpointStore + EventStore {}

impl<T: RunStore + CheckpointStore + EventStore> RunPersistence for T {}

/// Versioned persistence interface used by run managers and recovery code.
#[async_trait]
pub trait RunRepository: Send + Sync {
    /// Insert a new run at revision zero.
    async fn create(&self, record: RunRecord) -> Result<RunRecord, PersistenceError>;
    /// Load a run by ID.
    async fn get_versioned(&self, id: uuid::Uuid) -> Result<Option<RunRecord>, PersistenceError>;
    /// Replace a run only when its current revision equals `expected`.
    async fn save_if_revision(
        &self,
        record: RunRecord,
        expected: Revision,
    ) -> Result<RunRecord, PersistenceError>;
    /// List records matching a filter.
    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>, PersistenceError>;
    /// Delete a run.
    async fn delete_versioned(&self, id: uuid::Uuid) -> Result<(), PersistenceError>;
}

/// Compatibility persistence interface for existing callers.
#[async_trait]
pub trait RunStore: Send + Sync {
    /// Insert or replace a run record.
    async fn save(&self, record: RunRecord) -> std::io::Result<()>;
    /// Load a run record by ID, if it exists.
    async fn get(&self, id: uuid::Uuid) -> std::io::Result<Option<RunRecord>>;
    /// Delete a run record.
    async fn delete(&self, id: uuid::Uuid) -> std::io::Result<()>;
    /// List all run records.
    async fn list(&self) -> std::io::Result<Vec<RunRecord>>;
}
