//! Durable run records and storage.
//!
//! A [`RunRecord`] captures the lifecycle metadata for a Eureka execution.
//! The scheduler remains responsible for executing graph work; this module
//! provides a small persistence boundary so callers can inspect runs after a
//! process exits and build resumable APIs without coupling them to a database.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
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
            stats: None,
            error: None,
        }
    }
}

/// Persistence interface for durable run records.
#[async_trait]
pub trait RunStore: Send + Sync {
    /// Insert or replace a run record.
    async fn save(&self, record: RunRecord) -> std::io::Result<()>;
    /// Load a run record by ID, if it exists.
    async fn get(&self, id: uuid::Uuid) -> std::io::Result<Option<RunRecord>>;
    /// Delete a run record.
    async fn delete(&self, id: uuid::Uuid) -> std::io::Result<()>;
}

/// JSON-file-backed [`RunStore`].
#[derive(Debug, Clone)]
pub struct FileRunStore {
    directory: Arc<PathBuf>,
}

impl FileRunStore {
    /// Create a file store rooted at `directory`.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: Arc::new(directory.into()),
        }
    }

    /// Return the directory containing run records.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.directory.as_path()
    }

    fn path_for(&self, id: uuid::Uuid) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
}

#[async_trait]
impl RunStore for FileRunStore {
    async fn save(&self, record: RunRecord) -> std::io::Result<()> {
        let directory = self.directory.clone();
        let path = self.path_for(record.id);
        let temp_path = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|error| std::io::Error::other(error.to_string()))?;

        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(directory.as_path())?;
            std::fs::write(&temp_path, bytes)?;
            std::fs::rename(temp_path, path)
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    async fn get(&self, id: uuid::Uuid) -> std::io::Result<Option<RunRecord>> {
        let path = self.path_for(id);
        tokio::task::spawn_blocking(move || match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| std::io::Error::other(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    async fn delete(&self, id: uuid::Uuid) -> std::io::Result<()> {
        let path = self.path_for(id);
        tokio::task::spawn_blocking(move || match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_store_round_trips_records_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(directory.path());
        let id = uuid::Uuid::now_v7();
        let record = RunRecord::new(id, "graph.yml", serde_json::json!({"goal": "test"}));

        store.save(record.clone()).await.unwrap();
        let loaded = store.get(id).await.unwrap().unwrap();
        assert_eq!(loaded.id, record.id);
        assert_eq!(loaded.status, record.status);
        assert_eq!(loaded.goal, record.goal);
        store.delete(id).await.unwrap();
        assert!(store.get(id).await.unwrap().is_none());
    }
}
