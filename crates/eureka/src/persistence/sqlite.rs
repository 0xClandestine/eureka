//! `SQLite`-backed run persistence implementation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::OptionalExtension;

use super::checkpoint::RunCheckpoint;
use super::record::{RunFilter, RunRecord};
use super::store::{
    CheckpointStore, EventStore, PersistenceError, Revision, RunEvent, RunRepository, RunStore,
};

/// Wrap any Display-able error as an I/O error for `PersistenceError`.
pub(super) fn io_err(error: impl std::fmt::Display) -> PersistenceError {
    PersistenceError::Io(std::io::Error::other(error.to_string()))
}

/// Return the current Unix timestamp in milliseconds.
pub(super) fn unix_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
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
                 );
                 CREATE TABLE IF NOT EXISTS eureka_events (
                   run_id TEXT NOT NULL,
                   sequence INTEGER NOT NULL,
                   timestamp_ms INTEGER NOT NULL,
                   event_json TEXT NOT NULL,
                   PRIMARY KEY (run_id, sequence)
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
            connection.execute("DELETE FROM eureka_events WHERE run_id = ?1", [&id_text])?;
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
#[async_trait]
impl EventStore for SqliteRunPersistence {
    async fn append_event(
        &self,
        run_id: uuid::Uuid,
        event: serde_json::Value,
    ) -> Result<RunEvent, PersistenceError> {
        let _guard = self.lock.lock().await;
        let id = run_id.to_string();
        self.blocking(move |connection| {
            let sequence: i64 = connection.query_row(
                "SELECT COALESCE(MAX(sequence) + 1, 0) FROM eureka_events WHERE run_id = ?1",
                [&id],
                |row| row.get(0),
            )?;
            let timestamp_ms = i64::try_from(unix_timestamp_ms()).unwrap_or(i64::MAX);
            let event_json = serde_json::to_string(&event)?;
            connection.execute(
                "INSERT INTO eureka_events (run_id, sequence, timestamp_ms, event_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, sequence, timestamp_ms, event_json],
            )?;
            Ok(RunEvent { run_id, sequence: u64::try_from(sequence).unwrap_or(0), timestamp_ms: u64::try_from(timestamp_ms).unwrap_or(0), event })
        }).await
    }

    async fn load_events(&self, run_id: uuid::Uuid) -> Result<Vec<RunEvent>, PersistenceError> {
        let id = run_id.to_string();
        self.blocking(move |connection| {
            let mut statement = connection.prepare(
                "SELECT sequence, timestamp_ms, event_json FROM eureka_events WHERE run_id = ?1 ORDER BY sequence",
            )?;
            let rows = statement.query_map([&id], |row| {
                let event_json: String = row.get(2)?;
                let event = serde_json::from_str(&event_json)
                    .map_err(|error| rusqlite::Error::FromSqlConversionFailure(
                        2, rusqlite::types::Type::Text, Box::new(error),
                    ))?;
                Ok(RunEvent { run_id, sequence: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0), timestamp_ms: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0), event })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(PersistenceError::from)
        }).await
    }

    async fn delete_events(&self, run_id: uuid::Uuid) -> Result<(), PersistenceError> {
        let id = run_id.to_string();
        self.blocking(move |connection| {
            connection.execute("DELETE FROM eureka_events WHERE run_id = ?1", [&id])?;
            Ok(())
        })
        .await
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
