//! High-level run lifecycle management for application integrations.
//!
//! [`RunManager`] coordinates session construction, background execution,
//! durable lifecycle records, checkpoints, pause/resume, cancellation, and
//! human artifact injection. It intentionally keeps graph routing in the
//! scheduler rather than introducing a shared mutable workflow context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use crate::config::EurekaConfig;
use crate::graph::artifact::Artifact;
use crate::persistence::{CheckpointReason, RunCheckpoint, RunPersistence, RunRecord, RunStatus};
use crate::scheduler::SchedulerSignal;
use crate::session::Session;
use crate::EngineError;

/// A request to start a new graph run.
#[derive(Debug, Clone)]
pub struct CreateRunRequest {
    /// Initial goal artifact payload.
    pub goal: serde_json::Value,
}

/// Type alias for the map of active run signal channels.
type ActiveRunMap = HashMap<uuid::Uuid, Arc<Mutex<Option<mpsc::Sender<SchedulerSignal>>>>>;

/// High-level lifecycle service for application and HTTP integrations.
#[derive(Clone)]
pub struct RunManager {
    /// Runtime configuration.
    config: EurekaConfig,
    /// Durable run and checkpoint persistence.
    persistence: Arc<dyn RunPersistence>,
    /// Optional database directory for SQLite-backed sessions.
    db_directory: Option<PathBuf>,
    /// Map of active run signal channels, keyed by run UUID.
    #[allow(clippy::type_complexity)]
    active: Arc<Mutex<ActiveRunMap>>,
}

impl RunManager {
    /// Create a manager with shared lifecycle/checkpoint persistence.
    #[must_use]
    pub fn new(
        config: EurekaConfig,
        persistence: Arc<dyn RunPersistence>,
        db_directory: Option<PathBuf>,
    ) -> Self {
        Self {
            config,
            persistence,
            db_directory,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the run record, if it exists.
    ///
    /// # Errors
    /// Returns `EngineError::Store` on persistence failures.
    pub async fn get_run(&self, run_id: uuid::Uuid) -> Result<Option<RunRecord>, EngineError> {
        self.persistence
            .get(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// List all persisted run records.
    ///
    /// # Errors
    /// Returns `EngineError::Store` on persistence failures.
    pub async fn list_runs(&self) -> Result<Vec<RunRecord>, EngineError> {
        self.persistence
            .list()
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// Load the latest durable checkpoint for a run.
    ///
    /// # Errors
    /// Returns `EngineError::Store` on persistence failures.
    pub async fn get_checkpoint(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<RunCheckpoint>, EngineError> {
        self.persistence
            .load_checkpoint(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// Load the durable scheduler event history for a run.
    ///
    /// # Errors
    /// Returns `EngineError::Store` on persistence failures.
    pub async fn get_events(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Vec<crate::persistence::RunEvent>, EngineError> {
        self.persistence
            .load_events(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// Start a new run in the background and return its ID immediately.
    ///
    /// # Errors
    /// Returns `EngineError::Store` on persistence failures.
    pub async fn create_run(&self, request: CreateRunRequest) -> Result<uuid::Uuid, EngineError> {
        let run_id = uuid::Uuid::now_v7();
        let record = RunRecord::new(run_id, self.config.graph.clone(), request.goal.clone());
        self.persistence
            .save(record)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))?;
        self.spawn_run(run_id, request.goal, None).await?;
        Ok(run_id)
    }

    /// Request a pause on an active run.
    ///
    /// # Errors
    /// Returns `EngineError::Run` if the run is not active or not ready.
    pub async fn pause_run(&self, run_id: uuid::Uuid) -> Result<(), EngineError> {
        self.send_signal(run_id, SchedulerSignal::Pause).await
    }

    /// Request cancellation on an active run.
    ///
    /// # Errors
    /// Returns `EngineError::Run` if the run is not active or not ready.
    pub async fn cancel_run(&self, run_id: uuid::Uuid) -> Result<(), EngineError> {
        self.send_signal(run_id, SchedulerSignal::Cancel).await
    }

    /// Resume a paused run from its latest durable checkpoint.
    ///
    /// # Errors
    /// Returns `EngineError::Run` if the run is not found, not paused, or has no checkpoint.
    pub async fn resume_run(&self, run_id: uuid::Uuid) -> Result<(), EngineError> {
        let record = self
            .get_run(run_id)
            .await?
            .ok_or_else(|| EngineError::Run(format!("run {run_id} not found")))?;
        if record.status != RunStatus::Paused {
            return Err(EngineError::Run(format!(
                "run {run_id} is not paused (status {:?})",
                record.status
            )));
        }
        let checkpoint = self
            .persistence
            .load_checkpoint(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))?
            .ok_or_else(|| EngineError::Run(format!("run {run_id} has no checkpoint")))?;
        self.spawn_run(run_id, record.goal, Some(checkpoint)).await
    }

    /// Inject one typed artifact into a paused run's checkpoint.
    ///
    /// # Errors
    /// Returns `EngineError::Run` if the run is not active or `EngineError::Store` on
    /// persistence failures.
    pub async fn submit_input(
        &self,
        run_id: uuid::Uuid,
        node_id: impl Into<String>,
        port: impl Into<String>,
        artifact: Artifact,
    ) -> Result<RunCheckpoint, EngineError> {
        let node_id = node_id.into();
        let port = port.into();
        let record = self
            .get_run(run_id)
            .await?
            .ok_or_else(|| EngineError::Run(format!("run {run_id} not found")))?;
        if record.status != RunStatus::Paused {
            return Err(EngineError::Run(format!(
                "run {run_id} must be paused before input injection"
            )));
        }

        let validation_session = Session::new(self.config.clone(), &run_id.to_string(), None)?;
        validation_session.validate_input(&node_id, &port, &artifact.kind)?;

        let mut checkpoint = self
            .persistence
            .load_checkpoint(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))?
            .ok_or_else(|| EngineError::Run(format!("run {run_id} has no checkpoint")))?;
        let expected = checkpoint.revision;
        checkpoint
            .inject_input(node_id, port, artifact)
            .map_err(|error| EngineError::Store(error.to_string()))?;
        checkpoint.reason = CheckpointReason::InputAccepted;
        self.persistence
            .save_checkpoint(checkpoint, Some(expected))
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// Send a signal to an active run's scheduler.
    async fn send_signal(
        &self,
        run_id: uuid::Uuid,
        signal: SchedulerSignal,
    ) -> Result<(), EngineError> {
        let active = self.active.lock().await;
        let Some(slot) = active.get(&run_id) else {
            return Err(EngineError::Run(format!("run {run_id} is not active")));
        };
        let sender = slot
            .lock()
            .await
            .clone()
            .ok_or_else(|| EngineError::Run(format!("run {run_id} scheduler is not ready")))?;
        drop(active);
        sender
            .send(signal)
            .await
            .map_err(|error| EngineError::Run(format!("failed to signal run {run_id}: {error}")))
    }

    /// Spawn a new run in the background and register it in the active map.
    async fn spawn_run(
        &self,
        run_id: uuid::Uuid,
        goal: serde_json::Value,
        checkpoint: Option<RunCheckpoint>,
    ) -> Result<(), EngineError> {
        let slot = Arc::new(Mutex::new(None));
        {
            let mut active = self.active.lock().await;
            if active.contains_key(&run_id) {
                return Err(EngineError::Run(format!("run {run_id} is already active")));
            }
            active.insert(run_id, Arc::clone(&slot));
        }

        let config = self.config.clone();
        let persistence = Arc::clone(&self.persistence);
        let db_path = self
            .db_directory
            .as_ref()
            .map(|directory| directory.join(format!("{run_id}.sqlite")));
        let active = Arc::clone(&self.active);
        tokio::spawn(async move {
            let result = async {
                let mut session = Session::new(config, &run_id.to_string(), db_path)?;
                session.set_checkpoint_store(persistence.clone());
                session.set_event_store(persistence.clone());
                session.set_scheduler_signal_sink(Arc::clone(&slot));
                if let Some(checkpoint) = checkpoint {
                    session
                        .resume_with_store(goal, Some(persistence.as_ref()), checkpoint)
                        .await
                } else {
                    session
                        .run_with_store(goal, Some(persistence.as_ref()))
                        .await
                }
            }
            .await;
            if let Err(error) = result {
                tracing::error!(run_id = %run_id, error = %error, "managed run exited with an error");
            }
            *slot.lock().await = None;
            active.lock().await.remove(&run_id);
        });
        Ok(())
    }
}
