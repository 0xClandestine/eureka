//! Durable run records and storage.
//!
//! A [`RunRecord`] captures the lifecycle metadata for a Eureka execution.
//! The scheduler remains responsible for executing graph work; this module
//! provides a small persistence boundary so callers can inspect runs after a
//! process exits and build resumable APIs without coupling them to a database.

pub mod checkpoint;
pub mod environment;
pub mod memory;
pub mod record;
pub mod sqlite;
pub mod store;

pub use checkpoint::{ActivationSnapshot, CheckpointReason, PendingInput, RunCheckpoint};
pub use environment::{RunEnvironment, DATABASE_SCHEMA_VERSION};
pub use memory::{FileRunStore, InMemoryRunPersistence};
pub use record::{RunFilter, RunOutput, RunRecord, RunStatus};
pub use sqlite::SqliteRunPersistence;
pub use store::{
    CheckpointStore, EventStore, PersistenceError, Revision, RunEvent, RunPersistence,
    RunRepository, RunStore,
};

use std::path::PathBuf;
use std::sync::Arc;

/// Open the `SQLite` persistence backend.
///
/// # Errors
/// Returns a persistence error if the database cannot be opened or initialized.
pub async fn open_persistence(
    sqlite_path: impl Into<PathBuf>,
) -> Result<Arc<dyn RunPersistence>, PersistenceError> {
    Ok(Arc::new(SqliteRunPersistence::open(sqlite_path).await?))
}
