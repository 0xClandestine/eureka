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
use crate::run::{
    CheckpointReason, CheckpointStore, RunCheckpoint, RunRecord, RunStatus, RunStore,
};
use crate::scheduler::SchedulerSignal;
use crate::session::Session;
use crate::EngineError;

/// A request to start a new graph run.
#[derive(Debug, Clone)]
pub struct CreateRunRequest {
    /// Initial goal artifact payload.
    pub goal: serde_json::Value,
}

/// High-level lifecycle service for application and HTTP integrations.
#[derive(Clone)]
pub struct RunManager {
    config: EurekaConfig,
    run_store: Arc<dyn RunStore>,
    checkpoint_store: Arc<dyn CheckpointStore>,
    db_directory: Option<PathBuf>,
    active: Arc<Mutex<HashMap<uuid::Uuid, Arc<Mutex<Option<mpsc::Sender<SchedulerSignal>>>>>>>,
}

impl RunManager {
    /// Create a manager with shared lifecycle/checkpoint persistence.
    #[must_use]
    pub fn new(
        config: EurekaConfig,
        run_store: Arc<dyn RunStore>,
        checkpoint_store: Arc<dyn CheckpointStore>,
        db_directory: Option<PathBuf>,
    ) -> Self {
        Self {
            config,
            run_store,
            checkpoint_store,
            db_directory,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the run record, if it exists.
    pub async fn get_run(&self, run_id: uuid::Uuid) -> Result<Option<RunRecord>, EngineError> {
        self.run_store
            .get(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// List all persisted run records.
    pub async fn list_runs(&self) -> Result<Vec<RunRecord>, EngineError> {
        self.run_store
            .list()
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// Load the latest durable checkpoint for a run.
    pub async fn get_checkpoint(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<RunCheckpoint>, EngineError> {
        self.checkpoint_store
            .load_checkpoint(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// Start a new run in the background and return its ID immediately.
    pub async fn create_run(&self, request: CreateRunRequest) -> Result<uuid::Uuid, EngineError> {
        let run_id = uuid::Uuid::now_v7();
        let record = RunRecord::new(run_id, self.config.graph.clone(), request.goal.clone());
        self.run_store
            .save(record)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))?;
        self.spawn_run(run_id, request.goal, None).await?;
        Ok(run_id)
    }

    /// Request a pause on an active run.
    pub async fn pause_run(&self, run_id: uuid::Uuid) -> Result<(), EngineError> {
        self.send_signal(run_id, SchedulerSignal::Pause).await
    }

    /// Request cancellation on an active run.
    pub async fn cancel_run(&self, run_id: uuid::Uuid) -> Result<(), EngineError> {
        self.send_signal(run_id, SchedulerSignal::Cancel).await
    }

    /// Resume a paused run from its latest durable checkpoint.
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
            .checkpoint_store
            .load_checkpoint(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))?
            .ok_or_else(|| EngineError::Run(format!("run {run_id} has no checkpoint")))?;
        self.spawn_run(run_id, record.goal, Some(checkpoint)).await
    }

    /// Inject one typed artifact into a paused run's checkpoint.
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
            .checkpoint_store
            .load_checkpoint(run_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))?
            .ok_or_else(|| EngineError::Run(format!("run {run_id} has no checkpoint")))?;
        let expected = checkpoint.revision;
        checkpoint
            .inject_input(node_id, port, artifact)
            .map_err(|error| EngineError::Store(error.to_string()))?;
        checkpoint.reason = CheckpointReason::InputAccepted;
        self.checkpoint_store
            .save_checkpoint(checkpoint, Some(expected))
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

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
        sender
            .send(signal)
            .await
            .map_err(|error| EngineError::Run(format!("failed to signal run {run_id}: {error}")))
    }

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
        let run_store = Arc::clone(&self.run_store);
        let checkpoint_store = Arc::clone(&self.checkpoint_store);
        let db_path = self
            .db_directory
            .as_ref()
            .map(|directory| directory.join(format!("{run_id}.sqlite")));
        let active = Arc::clone(&self.active);
        tokio::spawn(async move {
            let result = async {
                let mut session = Session::new(config, &run_id.to_string(), db_path)?;
                session.set_checkpoint_store(checkpoint_store);
                session.set_scheduler_signal_sink(Arc::clone(&slot));
                if let Some(checkpoint) = checkpoint {
                    session
                        .resume_with_store(goal, Some(run_store.as_ref()), checkpoint)
                        .await
                } else {
                    session.run_with_store(goal, Some(run_store.as_ref())).await
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
