//! Durable run records and storage.
//!
//! A [`RunRecord`] captures the lifecycle metadata for a Eureka execution.
//! The scheduler remains responsible for executing graph work; this module
//! provides a small persistence boundary so callers can inspect runs after a
//! process exits and build resumable APIs without coupling them to a database.

use std::collections::HashMap;
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
    /// Unique session identifier.
    session_id: String,
    /// Optional path to the per-run `SQLite` database.
    db_path: Option<PathBuf>,
    /// Expected database schema version for compatibility checks.
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
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::config::RunStats;
use crate::graph::artifact::Artifact;
use crate::graph::node::PortMsg;

/// Monotonically increasing revision for optimistic concurrency control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub i64);

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

/// Wrap any Display-able error as an I/O error for `PersistenceError`.
fn io_err(error: impl std::fmt::Display) -> PersistenceError {
    PersistenceError::Io(std::io::Error::other(error.to_string()))
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
    pub artifact: Artifact,
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
    pub artifact: Artifact,
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
        artifact: Artifact,
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

/// A persistence backend that supports run metadata and checkpoints.
///
/// This is the canonical storage boundary for lifecycle management. The
/// lower-level [`RunRepository`] API remains available for callers that need
/// optimistic-concurrency details directly.
pub trait RunPersistence: RunStore + CheckpointStore {}

impl<T: RunStore + CheckpointStore> RunPersistence for T {}

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
    pub revision: Revision,
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
            revision: Revision::default(),
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

/// Open the preferred SQLite persistence backend, falling back to files.
///
/// The fallback keeps local runs usable on systems where SQLite cannot create
/// or lock a database. Callers receive one capability object regardless of the
/// selected backend.
pub async fn open_persistence(
    sqlite_path: Option<PathBuf>,
    fallback_directory: impl Into<PathBuf>,
) -> Arc<dyn RunPersistence> {
    let fallback_directory = fallback_directory.into();
    if let Some(path) = sqlite_path {
        match SqliteRunPersistence::open(path).await {
            Ok(store) => return Arc::new(store),
            Err(error) => {
                tracing::warn!(%error, "SQLite persistence unavailable; using file store")
            }
        }
    }
    Arc::new(FileRunStore::new(fallback_directory))
}

/// SQLite-backed run persistence.
#[derive(Debug, Clone)]
pub struct SqliteRunPersistence {
    /// Path to the `SQLite` database file.
    path: Arc<PathBuf>,
    /// Mutex serializing concurrent database access.
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl SqliteRunPersistence {
    /// Open or create a `SQLite` database and initialize runtime tables.
    ///
    /// # Errors
    /// Returns `PersistenceError::Io` on filesystem errors.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, PersistenceError> {
        let persistence = Self {
            path: Arc::new(path.into()),
            lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        let path = persistence.path.clone();
        tokio::task::spawn_blocking(move || {
            let connection = rusqlite::Connection::open(path.as_path())?;
            connection.execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS eureka_runs (
                   id TEXT PRIMARY KEY,
                   record_json TEXT NOT NULL,
                   revision INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS eureka_checkpoints (
                   run_id TEXT PRIMARY KEY,
                   checkpoint_json TEXT NOT NULL,
                   revision INTEGER NOT NULL
                 );",
            )?;
            Ok::<(), PersistenceError>(())
        })
        .await
        .map_err(io_err)??;
        Ok(persistence)
    }

    /// Return the database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Wrap an async operation in a blocking database task.
    async fn blocking<T, F>(&self, operation: F) -> Result<T, PersistenceError>
    where
        T: Send + 'static,
        F: FnOnce(rusqlite::Connection) -> Result<T, PersistenceError> + Send + 'static,
    {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let connection = rusqlite::Connection::open(path.as_path())?;
            connection.pragma_update(None, "foreign_keys", "ON")?;
            operation(connection)
        })
        .await
        .map_err(io_err)?
    }
}

#[async_trait]
impl RunRepository for SqliteRunPersistence {
    async fn create(&self, mut record: RunRecord) -> Result<RunRecord, PersistenceError> {
        let _guard = self.lock.lock().await;
        record.revision = Revision::default();
        let id = record.id.to_string();
        self.blocking(move |connection| {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM eureka_runs WHERE id = ?1)",
                [&id],
                |row| row.get(0),
            )?;
            if exists {
                return Err(PersistenceError::AlreadyExists(record.id));
            }
            let json = serde_json::to_string(&record)?;
            connection.execute(
                "INSERT INTO eureka_runs (id, record_json, revision) VALUES (?1, ?2, 0)",
                [&id, &json],
            )?;
            Ok(record)
        })
        .await
    }

    async fn get_versioned(&self, id: uuid::Uuid) -> Result<Option<RunRecord>, PersistenceError> {
        let id_text = id.to_string();
        self.blocking(move |connection| {
            let result: Result<String, rusqlite::Error> = connection.query_row(
                "SELECT record_json FROM eureka_runs WHERE id = ?1",
                [&id_text],
                |row| row.get(0),
            );
            match result {
                Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(io_err(error)),
            }
        })
        .await
    }

    async fn save_if_revision(
        &self,
        mut record: RunRecord,
        expected: Revision,
    ) -> Result<RunRecord, PersistenceError> {
        let _guard = self.lock.lock().await;
        let id = record.id;
        let id_text = id.to_string();
        self.blocking(move |connection| {
            let actual: Option<i64> = connection
                .query_row(
                    "SELECT revision FROM eureka_runs WHERE id = ?1",
                    [&id_text],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(actual) = actual else {
                return Err(PersistenceError::NotFound(id));
            };
            let actual = Revision(actual);
            if actual != expected {
                return Err(PersistenceError::RevisionConflict {
                    run_id: id,
                    expected,
                    actual,
                });
            }
            record.revision = Revision(expected.0 + 1);
            let updated = serde_json::to_string(&record)?;
            connection.execute(
                "UPDATE eureka_runs SET record_json = ?2, revision = ?3 WHERE id = ?1",
                rusqlite::params![id_text, updated, record.revision.0],
            )?;
            Ok(record)
        })
        .await
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>, PersistenceError> {
        self.blocking(move |connection| {
            let mut statement =
                connection.prepare("SELECT record_json FROM eureka_runs ORDER BY id")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut records = Vec::new();
            for row in rows {
                let json = row.map_err(io_err)?;
                let record: RunRecord = serde_json::from_str(&json)?;
                if filter.status.is_none_or(|status| record.status == status) {
                    records.push(record);
                }
                if filter.limit.is_some_and(|limit| records.len() >= limit) {
                    break;
                }
            }
            Ok(records)
        })
        .await
    }

    async fn delete_versioned(&self, id: uuid::Uuid) -> Result<(), PersistenceError> {
        let _guard = self.lock.lock().await;
        let id_text = id.to_string();
        self.blocking(move |connection| {
            connection
                .execute(
                    "DELETE FROM eureka_checkpoints WHERE run_id = ?1",
                    [&id_text],
                )
                .ok();
            connection.execute("DELETE FROM eureka_runs WHERE id = ?1", [&id_text])?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl RunStore for SqliteRunPersistence {
    async fn save(&self, record: RunRecord) -> std::io::Result<()> {
        let current = self
            .get_versioned(record.id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let result = match current {
            Some(current) => self
                .save_if_revision(record, current.revision)
                .await
                .map(|_| ()),
            None => self.create(record).await.map(|_| ()),
        };
        result.map_err(|error| std::io::Error::other(error.to_string()))
    }

    async fn get(&self, id: uuid::Uuid) -> std::io::Result<Option<RunRecord>> {
        self.get_versioned(id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    async fn delete(&self, id: uuid::Uuid) -> std::io::Result<()> {
        self.delete_versioned(id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    async fn list(&self) -> std::io::Result<Vec<RunRecord>> {
        RunRepository::list(self, RunFilter::default())
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[async_trait]
impl CheckpointStore for SqliteRunPersistence {
    async fn save_checkpoint(
        &self,
        mut checkpoint: RunCheckpoint,
        expected: Option<Revision>,
    ) -> Result<RunCheckpoint, PersistenceError> {
        let _guard = self.lock.lock().await;
        let id = checkpoint.run_id;
        let id_text = id.to_string();
        self.blocking(move |connection| {
            let current: Option<(String, i64)> = connection
                .query_row(
                    "SELECT checkpoint_json, revision FROM eureka_checkpoints WHERE run_id = ?1",
                    [&id_text],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let actual = current
                .as_ref()
                .map_or_else(Revision::default, |item| Revision(item.1));
            if let Some(expected) = expected {
                if actual != expected {
                    return Err(PersistenceError::RevisionConflict {
                        run_id: id,
                        expected,
                        actual,
                    });
                }
            }
            checkpoint.revision = Revision(actual.0 + 1);
            let json = serde_json::to_string(&checkpoint)?;
            connection.execute(
                "INSERT INTO eureka_checkpoints (run_id, checkpoint_json, revision) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(run_id) DO UPDATE \
                 SET checkpoint_json = excluded.checkpoint_json, \
                     revision = excluded.revision",
                rusqlite::params![id_text, json, checkpoint.revision.0],
            )?;
            Ok(checkpoint)
        })
        .await
    }

    async fn load_checkpoint(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<RunCheckpoint>, PersistenceError> {
        let id_text = run_id.to_string();
        self.blocking(move |connection| {
            let result: Result<String, rusqlite::Error> = connection.query_row(
                "SELECT checkpoint_json FROM eureka_checkpoints WHERE run_id = ?1",
                [&id_text],
                |row| row.get(0),
            );
            match result {
                Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(io_err(error)),
            }
        })
        .await
    }

    async fn delete_checkpoint(&self, run_id: uuid::Uuid) -> Result<(), PersistenceError> {
        let id_text = run_id.to_string();
        self.blocking(move |connection| {
            connection.execute(
                "DELETE FROM eureka_checkpoints WHERE run_id = ?1",
                [&id_text],
            )?;
            Ok(())
        })
        .await
    }
}

/// In-memory run repository for testing and single-process use.
#[derive(Debug, Clone, Default)]
pub struct InMemoryRunPersistence {
    /// In-memory run records.
    runs: Arc<tokio::sync::RwLock<HashMap<uuid::Uuid, RunRecord>>>,
    /// In-memory checkpoint records.
    checkpoints: Arc<tokio::sync::RwLock<HashMap<uuid::Uuid, RunCheckpoint>>>,
}

impl InMemoryRunPersistence {
    /// Create an empty in-memory persistence backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RunRepository for InMemoryRunPersistence {
    async fn create(&self, mut record: RunRecord) -> Result<RunRecord, PersistenceError> {
        let mut runs = self.runs.write().await;
        if runs.contains_key(&record.id) {
            return Err(PersistenceError::AlreadyExists(record.id));
        }
        record.revision = Revision::default();
        runs.insert(record.id, record.clone());
        drop(runs);
        Ok(record)
    }

    async fn get_versioned(&self, id: uuid::Uuid) -> Result<Option<RunRecord>, PersistenceError> {
        Ok(self.runs.read().await.get(&id).cloned())
    }

    async fn save_if_revision(
        &self,
        mut record: RunRecord,
        expected: Revision,
    ) -> Result<RunRecord, PersistenceError> {
        let mut runs = self.runs.write().await;
        let current = runs
            .get(&record.id)
            .ok_or(PersistenceError::NotFound(record.id))?;
        if current.revision != expected {
            return Err(PersistenceError::RevisionConflict {
                run_id: record.id,
                expected,
                actual: current.revision,
            });
        }
        record.revision = Revision(expected.0.saturating_add(1));
        runs.insert(record.id, record.clone());
        drop(runs);
        Ok(record)
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>, PersistenceError> {
        let mut records: Vec<_> = self
            .runs
            .read()
            .await
            .values()
            .filter(|record| filter.status.is_none_or(|status| record.status == status))
            .cloned()
            .collect();
        records.sort_by_key(|record| record.id);
        if let Some(limit) = filter.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    async fn delete_versioned(&self, id: uuid::Uuid) -> Result<(), PersistenceError> {
        self.runs.write().await.remove(&id);
        self.checkpoints.write().await.remove(&id);
        Ok(())
    }
}

#[async_trait]
impl RunStore for InMemoryRunPersistence {
    async fn save(&self, record: RunRecord) -> std::io::Result<()> {
        let current = self
            .get_versioned(record.id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        match current {
            Some(current) => self
                .save_if_revision(record, current.revision)
                .await
                .map(|_| ())
                .map_err(|error| std::io::Error::other(error.to_string())),
            None => self
                .create(record)
                .await
                .map(|_| ())
                .map_err(|error| std::io::Error::other(error.to_string())),
        }
    }

    async fn get(&self, id: uuid::Uuid) -> std::io::Result<Option<RunRecord>> {
        self.get_versioned(id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    async fn delete(&self, id: uuid::Uuid) -> std::io::Result<()> {
        self.delete_versioned(id)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    async fn list(&self) -> std::io::Result<Vec<RunRecord>> {
        RunRepository::list(self, RunFilter::default())
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[async_trait]
impl CheckpointStore for InMemoryRunPersistence {
    async fn save_checkpoint(
        &self,
        mut checkpoint: RunCheckpoint,
        expected: Option<Revision>,
    ) -> Result<RunCheckpoint, PersistenceError> {
        let mut checkpoints = self.checkpoints.write().await;
        let current = checkpoints.get(&checkpoint.run_id);
        if let Some(expected) = expected {
            let actual = current.map_or_else(Revision::default, |item| item.revision);
            if actual != expected {
                return Err(PersistenceError::RevisionConflict {
                    run_id: checkpoint.run_id,
                    expected,
                    actual,
                });
            }
        }
        checkpoint.revision =
            current.map_or_else(|| Revision(1), |item| Revision(item.revision.0 + 1));
        checkpoints.insert(checkpoint.run_id, checkpoint.clone());
        drop(checkpoints);
        Ok(checkpoint)
    }

    async fn load_checkpoint(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<RunCheckpoint>, PersistenceError> {
        Ok(self.checkpoints.read().await.get(&run_id).cloned())
    }

    async fn delete_checkpoint(&self, run_id: uuid::Uuid) -> Result<(), PersistenceError> {
        self.checkpoints.write().await.remove(&run_id);
        Ok(())
    }
}

/// JSON-file-backed [`RunStore`] and checkpoint store.
#[derive(Debug, Clone)]
/// File-system-backed run store.
pub struct FileRunStore {
    /// Base directory for run record files.
    directory: Arc<PathBuf>,
    /// Mutex serializing concurrent filesystem access.
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl FileRunStore {
    /// Create a file store rooted at `directory`.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: Arc::new(directory.into()),
            lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Return the directory containing run records.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.directory.as_path()
    }

    /// Build the filesystem path for a run record.
    fn path_for(&self, id: uuid::Uuid) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }

    /// Build the filesystem path for a checkpoint record.
    fn checkpoint_path_for(&self, id: uuid::Uuid) -> PathBuf {
        self.directory.join(format!("{id}.checkpoint.json"))
    }

    /// Read a single run record from the filesystem.
    async fn read_record(&self, id: uuid::Uuid) -> std::io::Result<Option<RunRecord>> {
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

    /// Write a single run record to the filesystem.
    async fn write_record(&self, record: &RunRecord) -> std::io::Result<()> {
        let directory = self.directory.clone();
        let path = self.path_for(record.id);
        let temp_path = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(directory.as_path())?;
            std::fs::write(&temp_path, bytes)?;
            std::fs::rename(temp_path, path)
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    /// List all run records from the filesystem.
    async fn list_records(&self) -> std::io::Result<Vec<RunRecord>> {
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            let entries = match std::fs::read_dir(directory.as_path()) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            };
            let mut records = Vec::new();
            for entry in entries {
                let path = entry?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json")
                    || path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(".checkpoint.json"))
                {
                    continue;
                }
                let bytes = std::fs::read(path)?;
                let record = serde_json::from_slice(&bytes)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                records.push(record);
            }
            Ok(records)
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    /// Delete a single run record from the filesystem.
    async fn delete_record(&self, id: uuid::Uuid) -> std::io::Result<()> {
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

#[async_trait]
impl RunRepository for FileRunStore {
    async fn create(&self, mut record: RunRecord) -> Result<RunRecord, PersistenceError> {
        let _guard = self.lock.lock().await;
        if self
            .read_record(record.id)
            .await
            .map_err(PersistenceError::from)?
            .is_some()
        {
            return Err(PersistenceError::AlreadyExists(record.id));
        }
        record.revision = Revision::default();
        self.write_record(&record)
            .await
            .map_err(PersistenceError::from)?;
        Ok(record)
    }

    async fn get_versioned(&self, id: uuid::Uuid) -> Result<Option<RunRecord>, PersistenceError> {
        self.read_record(id).await.map_err(PersistenceError::from)
    }

    async fn save_if_revision(
        &self,
        mut record: RunRecord,
        expected: Revision,
    ) -> Result<RunRecord, PersistenceError> {
        let _guard = self.lock.lock().await;
        let current = self
            .read_record(record.id)
            .await
            .map_err(PersistenceError::from)?
            .ok_or(PersistenceError::NotFound(record.id))?;
        if current.revision != expected {
            return Err(PersistenceError::RevisionConflict {
                run_id: record.id,
                expected,
                actual: current.revision,
            });
        }
        record.revision = Revision(expected.0.saturating_add(1));
        self.write_record(&record)
            .await
            .map_err(PersistenceError::from)?;
        Ok(record)
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>, PersistenceError> {
        let _guard = self.lock.lock().await;
        let mut records = self.list_records().await.map_err(PersistenceError::from)?;
        if let Some(status) = filter.status {
            records.retain(|record| record.status == status);
        }
        records.sort_by_key(|record| record.id);
        if let Some(limit) = filter.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    async fn delete_versioned(&self, id: uuid::Uuid) -> Result<(), PersistenceError> {
        let _guard = self.lock.lock().await;
        self.delete_record(id).await.map_err(PersistenceError::from)
    }
}

#[async_trait]
impl CheckpointStore for FileRunStore {
    async fn save_checkpoint(
        &self,
        mut checkpoint: RunCheckpoint,
        expected: Option<Revision>,
    ) -> Result<RunCheckpoint, PersistenceError> {
        let _guard = self.lock.lock().await;
        let path = self.checkpoint_path_for(checkpoint.run_id);
        let current = tokio::task::spawn_blocking({
            let path = path.clone();
            move || match std::fs::read(path) {
                Ok(bytes) => serde_json::from_slice::<RunCheckpoint>(&bytes)
                    .map(Some)
                    .map_err(|error| std::io::Error::other(error.to_string())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(io_err)??;
        if let Some(expected) = expected {
            let actual = current
                .as_ref()
                .map_or_else(Revision::default, |item| item.revision);
            if actual != expected {
                return Err(PersistenceError::RevisionConflict {
                    run_id: checkpoint.run_id,
                    expected,
                    actual,
                });
            }
        }
        checkpoint.revision = current.map_or(Revision(1), |item| Revision(item.revision.0 + 1));
        let directory = self.directory.clone();
        let bytes = serde_json::to_vec_pretty(&checkpoint)?;
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(directory.as_path())?;
            let temp = path.with_extension("checkpoint.json.tmp");
            std::fs::write(&temp, bytes)?;
            std::fs::rename(temp, path)
        })
        .await
        .map_err(io_err)??;
        Ok(checkpoint)
    }

    async fn load_checkpoint(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<RunCheckpoint>, PersistenceError> {
        let _guard = self.lock.lock().await;
        let path = self.checkpoint_path_for(run_id);
        tokio::task::spawn_blocking(move || match std::fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_err(error)),
        })
        .await
        .map_err(io_err)?
    }

    async fn delete_checkpoint(&self, run_id: uuid::Uuid) -> Result<(), PersistenceError> {
        let _guard = self.lock.lock().await;
        let path = self.checkpoint_path_for(run_id);
        tokio::task::spawn_blocking(move || match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_err(error)),
        })
        .await
        .map_err(io_err)?
    }
}

#[async_trait]
impl RunStore for FileRunStore {
    async fn save(&self, record: RunRecord) -> std::io::Result<()> {
        let _guard = self.lock.lock().await;
        self.write_record(&record).await
    }

    async fn get(&self, id: uuid::Uuid) -> std::io::Result<Option<RunRecord>> {
        let _guard = self.lock.lock().await;
        self.read_record(id).await
    }

    async fn delete(&self, id: uuid::Uuid) -> std::io::Result<()> {
        let _guard = self.lock.lock().await;
        self.delete_record(id).await
    }

    async fn list(&self) -> std::io::Result<Vec<RunRecord>> {
        let _guard = self.lock.lock().await;
        self.list_records().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subprocess_environment_contains_shared_database_contract() {
        let environment = RunEnvironment::new("run-1", Some(PathBuf::from("run.sqlite")));
        let values = environment.subprocess_env("ranker", 3, "{}");
        let values: HashMap<_, _> = values.into_iter().collect();
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

    #[tokio::test]
    async fn versioned_file_store_rejects_stale_updates_and_lists_runs() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(directory.path());
        let id = uuid::Uuid::now_v7();
        let created = RunRepository::create(
            &store,
            RunRecord::new(id, "graph.yml", serde_json::json!({})),
        )
        .await
        .unwrap();
        let mut updated = created.clone();
        updated.status = RunStatus::Running;
        let updated = RunRepository::save_if_revision(&store, updated, created.revision)
            .await
            .unwrap();
        let mut stale = created;
        stale.status = RunStatus::Failed;
        let error = RunRepository::save_if_revision(&store, stale, Revision(0))
            .await
            .unwrap_err();
        assert!(matches!(error, PersistenceError::RevisionConflict { .. }));
        let records = RunRepository::list(&store, RunFilter::default())
            .await
            .unwrap();
        assert_eq!(records, vec![updated]);
    }

    #[tokio::test]
    async fn file_store_round_trips_checkpoints_with_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(directory.path());
        let run_id = uuid::Uuid::now_v7();
        let checkpoint = RunCheckpoint::new(run_id, "graph-v1".into(), "config-v1".into());
        let saved = CheckpointStore::save_checkpoint(&store, checkpoint, None)
            .await
            .unwrap();
        assert_eq!(saved.revision, Revision(1));
        let loaded = CheckpointStore::load_checkpoint(&store, run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, saved);

        let mut next = saved.clone();
        next.round = 2;
        let next = CheckpointStore::save_checkpoint(&store, next, Some(saved.revision))
            .await
            .unwrap();
        assert_eq!(next.revision, Revision(2));
        let mut stale = next.clone();
        stale.round = 1;
        let error = CheckpointStore::save_checkpoint(&store, stale, Some(saved.revision))
            .await
            .unwrap_err();
        assert!(matches!(error, PersistenceError::RevisionConflict { .. }));
    }

    #[tokio::test]
    async fn in_memory_backend_matches_versioned_contract() {
        let store = InMemoryRunPersistence::new();
        let id = uuid::Uuid::now_v7();
        let record = RunRepository::create(
            &store,
            RunRecord::new(id, "graph.yml", serde_json::json!({})),
        )
        .await
        .unwrap();
        let checkpoint = CheckpointStore::save_checkpoint(
            &store,
            RunCheckpoint::new(id, "graph".into(), "config".into()),
            None,
        )
        .await
        .unwrap();
        assert_eq!(checkpoint.revision, Revision(1));
        assert_eq!(
            RunRepository::get_versioned(&store, id).await.unwrap(),
            Some(record)
        );
    }
}
