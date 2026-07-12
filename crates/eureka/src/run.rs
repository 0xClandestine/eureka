//! Durable run records and storage.
//!
//! A [`RunRecord`] captures the lifecycle metadata for a Eureka execution.
//! The scheduler remains responsible for executing graph work; this module
//! provides a small persistence boundary so callers can inspect runs after a
//! process exits and build resumable APIs without coupling them to a database.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Current version of the runtime database environment contract.
///
/// This is deliberately independent from the plugin schemas stored in the
/// per-run database. It lets subprocesses reject an environment they do not
/// understand without requiring Eureka to own every plugin migration.
pub const DATABASE_SCHEMA_VERSION: u32 = 1;

/// Run-scoped database and identity information shared with executable nodes.
///
/// The runtime keeps this as a capability descriptor rather than handing graph
/// nodes a raw database connection. Control nodes and agent tools can use the
/// descriptor to open the same per-run database, while the Rust scheduler keeps
/// ownership of its own persistence transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnvironment {
    session_id: String,
    db_path: Option<PathBuf>,
    database_schema_version: u32,
}

impl RunEnvironment {
    /// Create a run environment using the current database contract version.
    #[must_use]
    pub fn new(session_id: impl Into<String>, db_path: Option<PathBuf>) -> Self {
        Self {
            session_id: session_id.into(),
            db_path,
            database_schema_version: DATABASE_SCHEMA_VERSION,
        }
    }

    /// Return the stable session/run identifier.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Return the optional path to the per-run database.
    #[must_use]
    pub const fn db_path(&self) -> Option<&PathBuf> {
        self.db_path.as_ref()
    }

    /// Return the runtime database contract version.
    #[must_use]
    pub const fn database_schema_version(&self) -> u32 {
        self.database_schema_version
    }

    /// Build the environment passed to one executable node invocation.
    ///
    /// The returned values are owned because subprocess execution can outlive
    /// the caller's stack frame. `EUREKA_DB_NAMESPACE` identifies the node's
    /// plugin namespace; it does not grant access to runtime-owned tables.
    #[must_use]
    pub fn subprocess_env(
        &self,
        node_id: &str,
        round: u32,
        config_json: &str,
    ) -> Vec<(String, String)> {
        let mut env = vec![
            ("EUREKA_SESSION_ID".to_string(), self.session_id.clone()),
            ("EUREKA_NODE_ID".to_string(), node_id.to_string()),
            ("EUREKA_ROUND".to_string(), round.to_string()),
            ("EUREKA_CONFIG".to_string(), config_json.to_string()),
            (
                "EUREKA_DB_SCHEMA_VERSION".to_string(),
                self.database_schema_version.to_string(),
            ),
            ("EUREKA_DB_NAMESPACE".to_string(), node_id.to_string()),
        ];
        if let Some(path) = &self.db_path {
            env.push(("EUREKA_DB_PATH".to_string(), path.display().to_string()));
        }
        env
    }
}

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

    #[test]
    fn subprocess_environment_contains_shared_database_contract() {
        let environment = RunEnvironment::new("run-1", Some(PathBuf::from("run.sqlite")));
        let values = environment.subprocess_env("ranker", 3, "{}");
        let values: std::collections::HashMap<_, _> = values.into_iter().collect();
        assert_eq!(values.get("EUREKA_SESSION_ID"), Some(&"run-1".to_string()));
        assert_eq!(values.get("EUREKA_NODE_ID"), Some(&"ranker".to_string()));
        assert_eq!(values.get("EUREKA_ROUND"), Some(&"3".to_string()));
        assert_eq!(
            values.get("EUREKA_DB_PATH"),
            Some(&"run.sqlite".to_string())
        );
        assert_eq!(
            values.get("EUREKA_DB_SCHEMA_VERSION"),
            Some(&DATABASE_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            values.get("EUREKA_DB_NAMESPACE"),
            Some(&"ranker".to_string())
        );
    }

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
