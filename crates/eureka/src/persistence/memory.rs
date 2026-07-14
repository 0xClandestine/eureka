//! In-memory and file-system-backed persistence implementations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use super::checkpoint::RunCheckpoint;
use super::record::{RunFilter, RunRecord};
use super::sqlite::io_err;
use super::store::{
    CheckpointStore, EventStore, PersistenceError, Revision, RunEvent, RunRepository, RunStore,
};

// ---------------------------------------------------------------------------
// InMemoryRunPersistence
// ---------------------------------------------------------------------------

/// In-memory run repository for testing and single-process use.
#[derive(Debug, Clone, Default)]
pub struct InMemoryRunPersistence {
    /// In-memory run records.
    runs: Arc<tokio::sync::RwLock<HashMap<uuid::Uuid, RunRecord>>>,
    /// In-memory checkpoint records.
    checkpoints: Arc<tokio::sync::RwLock<HashMap<uuid::Uuid, RunCheckpoint>>>,
    /// In-memory scheduler event history.
    events: Arc<tokio::sync::RwLock<HashMap<uuid::Uuid, Vec<RunEvent>>>>,
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
        let mut runs = self.runs.write().await;
        if !runs.contains_key(&id) {
            return Err(PersistenceError::NotFound(id));
        }
        runs.remove(&id);
        drop(runs);
        self.checkpoints.write().await.remove(&id);
        self.events.write().await.remove(&id);
        Ok(())
    }
}

#[async_trait]
impl EventStore for InMemoryRunPersistence {
    async fn append_event(
        &self,
        run_id: uuid::Uuid,
        event: serde_json::Value,
    ) -> Result<RunEvent, PersistenceError> {
        let mut events = self.events.write().await;
        let sequence = events.get(&run_id).map_or(0, |items| items.len() as u64);
        let entry = RunEvent {
            run_id,
            sequence,
            timestamp_ms: super::sqlite::unix_timestamp_ms(),
            event,
        };
        events.entry(run_id).or_default().push(entry.clone());
        drop(events);
        Ok(entry)
    }

    async fn load_events(&self, run_id: uuid::Uuid) -> Result<Vec<RunEvent>, PersistenceError> {
        Ok(self
            .events
            .read()
            .await
            .get(&run_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn delete_events(&self, run_id: uuid::Uuid) -> Result<(), PersistenceError> {
        self.events.write().await.remove(&run_id);
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

// ---------------------------------------------------------------------------
// FileRunStore
// ---------------------------------------------------------------------------

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
    use crate::persistence::record::{RunRecord, RunStatus};
    use crate::persistence::store::{CheckpointStore, RunRepository};

    #[test]
    fn subprocess_environment_contains_shared_database_contract() {
        use crate::persistence::environment::{RunEnvironment, DATABASE_SCHEMA_VERSION};
        use std::path::PathBuf;

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
        use crate::persistence::checkpoint::RunCheckpoint;

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
        use crate::persistence::checkpoint::RunCheckpoint;

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
