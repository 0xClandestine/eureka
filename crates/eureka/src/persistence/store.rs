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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_default_is_zero() {
        assert_eq!(Revision::default(), Revision(0));
    }

    #[test]
    fn revision_ordering() {
        assert!(Revision(0) < Revision(1));
        assert!(Revision(1) > Revision(0));
        assert_eq!(Revision(3), Revision(3));
        assert!(Revision(-1) < Revision(0));
        assert!(Revision(i64::MAX) > Revision(i64::MIN));
    }

    #[test]
    fn revision_serde_is_transparent() {
        let json = serde_json::to_value(&Revision(42)).unwrap();
        assert_eq!(json, serde_json::json!(42));
        let back: Revision = serde_json::from_value(json).unwrap();
        assert_eq!(back, Revision(42));
    }

    #[test]
    fn revision_default_serde_as_zero() {
        let json = serde_json::to_string(&Revision::default()).unwrap();
        assert_eq!(json, "0");
    }

    #[test]
    fn persistence_error_not_found_displays_run_id() {
        let id = uuid::Uuid::now_v7();
        let err = PersistenceError::NotFound(id);
        let msg = err.to_string();
        assert!(msg.contains(&id.to_string()), "message should contain run id: {msg}");
    }

    #[test]
    fn persistence_error_already_exists_displays_run_id() {
        let id = uuid::Uuid::now_v7();
        let err = PersistenceError::AlreadyExists(id);
        let msg = err.to_string();
        assert!(msg.contains(&id.to_string()), "message should contain run id: {msg}");
    }

    #[test]
    fn persistence_error_revision_conflict_displays_all_fields() {
        let id = uuid::Uuid::now_v7();
        let err = PersistenceError::RevisionConflict {
            run_id: id,
            expected: Revision(2),
            actual: Revision(1),
        };
        let msg = err.to_string();
        assert!(msg.contains(&id.to_string()), "message should contain run id: {msg}");
        assert!(msg.contains("expected"), "message should mention expected: {msg}");
        assert!(msg.contains("2"), "message should show expected revision: {msg}");
    }

    #[test]
    fn persistence_error_input_conflict_displays_node_and_port() {
        let err = PersistenceError::InputConflict { node_id: "gen".into(), port: "goal".into() };
        let msg = err.to_string();
        assert!(msg.contains("gen"), "message should contain node_id: {msg}");
        assert!(msg.contains("goal"), "message should contain port: {msg}");
    }

    #[test]
    fn persistence_error_io_displays() {
        let err = PersistenceError::Io(std::io::Error::other("disk on fire"));
        let msg = err.to_string();
        assert!(msg.contains("disk on fire"), "message should wrap io error: {msg}");
    }

    #[test]
    fn persistence_error_serialization_displays() {
        let err = PersistenceError::Serialization("trailing comma".into());
        let msg = err.to_string();
        assert!(msg.contains("trailing comma"), "message should wrap: {msg}");
    }

    #[test]
    fn from_io_error_for_persistence_error() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let pe: PersistenceError = io.into();
        assert!(matches!(pe, PersistenceError::Io(_)));
        assert!(pe.to_string().contains("gone"));
    }

    #[test]
    fn from_serde_json_error_for_persistence_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("{invalid}").unwrap_err();
        let pe: PersistenceError = json_err.into();
        assert!(matches!(pe, PersistenceError::Serialization(_)));
    }

    #[test]
    fn run_event_serde_round_trip() {
        let id = uuid::Uuid::now_v7();
        let event = serde_json::json!({"type": "activationStarted", "nodeId": "gen", "nodeKind": "gen", "round": 1});
        let re = RunEvent {
            run_id: id,
            sequence: 5,
            timestamp_ms: 1_750_000_000_000,
            event: event.clone(),
        };
        let json = serde_json::to_string(&re).unwrap();
        let restored: RunEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.run_id, id);
        assert_eq!(restored.sequence, 5);
        assert_eq!(restored.timestamp_ms, 1_750_000_000_000);
        assert_eq!(restored.event, event);
    }

    #[test]
    fn run_event_deserializes_snake_case_fields() {
        let json = serde_json::json!({
            "run_id": "00000000-0000-0000-0000-000000000000",
            "sequence": 0,
            "timestamp_ms": 0,
            "event": {}
        });
        let re: RunEvent = serde_json::from_value(json).unwrap();
        assert_eq!(re.sequence, 0);
        assert_eq!(re.timestamp_ms, 0);
    }

    #[test]
    fn persistence_error_debug_does_not_panic() {
        let id = uuid::Uuid::now_v7();
        let err = PersistenceError::RevisionConflict {
            run_id: id,
            expected: Revision(1),
            actual: Revision(0),
        };
        let _ = format!("{err:?}");
    }

    #[test]
    fn from_rusqlite_error_wraps_into_io_variant() {
        let sq_err = rusqlite::Error::QueryReturnedNoRows;
        let pe: PersistenceError = sq_err.into();
        assert!(matches!(pe, PersistenceError::Io(_)));
        assert!(!pe.to_string().is_empty());
    }

    #[test]
    fn revision_max_value_serde_round_trip() {
        let json = serde_json::to_value(&Revision(i64::MAX)).unwrap();
        let back: Revision = serde_json::from_value(json).unwrap();
        assert_eq!(back, Revision(i64::MAX));
    }

    #[test]
    fn revision_negative_value_serde() {
        let json = serde_json::to_string(&Revision(-5)).unwrap();
        assert_eq!(json, "-5");
        let back: Revision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Revision(-5));
    }

    #[test]
    fn run_event_zero_sequence_and_timestamp() {
        let id = uuid::Uuid::now_v7();
        let re = RunEvent {
            run_id: id,
            sequence: 0,
            timestamp_ms: 0,
            event: serde_json::json!({"type": "runHalted", "reason": "budget", "totalRounds": 0}),
        };
        let json = serde_json::to_string(&re).unwrap();
        let restored: RunEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.sequence, 0);
        assert_eq!(restored.timestamp_ms, 0);
    }

    #[test]
    fn run_event_max_sequence_value() {
        let id = uuid::Uuid::now_v7();
        let re = RunEvent {
            run_id: id,
            sequence: u64::MAX,
            timestamp_ms: u64::MAX,
            event: serde_json::json!({}),
        };
        let json = serde_json::to_string(&re).unwrap();
        let restored: RunEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.sequence, u64::MAX);
        assert_eq!(restored.timestamp_ms, u64::MAX);
    }
}
