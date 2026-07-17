//! `eureka resume` — resume a failed or paused session without the daemon.
//!
//! Reads persisted run state from the session database, loads any available
//! checkpoint, and re-runs the scheduler from where it left off.  When no
//! checkpoint was saved (e.g. the session failed before completing its first
//! round) the session is restarted from the beginning with the same goal.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use eureka::config::EurekaConfig;
use eureka::persistence::{open_persistence, RunPersistence};
use eureka::scheduler::SchedulerSignal;
use eureka::{RunManager, Session};
use tokio::sync::{broadcast, mpsc};

/// Arguments for the `resume` command.
#[derive(Debug)]
pub struct ResumeArgs {
    /// Session UUID to resume.
    pub session_id: String,
    /// Path to the config file (used to locate the graph and session DB).
    pub config_path: String,
    /// Port for the observability UI server (0 = disabled).
    pub port: u16,
}

/// Execute the `resume` command.
///
/// # Errors
///
/// Returns an error if the session database cannot be opened, the run record
/// is missing, or the session fails.
pub async fn execute(args: ResumeArgs) -> Result<()> {
    let session_uuid = uuid::Uuid::parse_str(&args.session_id)
        .with_context(|| format!("'{}' is not a valid session UUID", args.session_id))?;

    let config = EurekaConfig::load(Some(Path::new(&args.config_path)))
        .with_context(|| format!("Failed to load config from '{}'", args.config_path))?;

    // Derive the session database path from config.graph, mirroring run.rs.
    let graph_path = Path::new(&config.graph);
    let graph_dir = graph_path.parent().unwrap_or_else(|| Path::new("."));
    let graph_dir_abs =
        std::fs::canonicalize(graph_dir).unwrap_or_else(|_| graph_dir.to_path_buf());
    let sessions_dir = graph_dir_abs.join(".eureka").join("sessions");
    let db_path = sessions_dir.join(format!("{session_uuid}.sqlite"));

    if !db_path.exists() {
        anyhow::bail!(
            "No session database found for '{}' at '{}'.\n\
             Use `eureka run` to start a new session.",
            args.session_id,
            db_path.display()
        );
    }

    let persistence: Arc<dyn RunPersistence> =
        open_persistence(db_path.clone()).await.context("Failed to open session database")?;

    // Load the stored run record to recover the original goal.
    let record = persistence
        .get(session_uuid)
        .await
        .context("Failed to read run record from database")?
        .with_context(|| {
            format!("Session '{}' has no run record in the database", args.session_id)
        })?;

    let goal = record.goal.clone();

    tracing::info!(
        session_id = %session_uuid,
        status = ?record.status,
        graph = %record.graph,
        "Resuming session"
    );

    // Load the latest checkpoint, if any.
    let checkpoint = persistence
        .load_checkpoint(session_uuid)
        .await
        .context("Failed to load checkpoint from database")?;

    let manager =
        RunManager::new(config.clone(), Arc::clone(&persistence), Some(sessions_dir.clone()));

    let mut session =
        Session::new(config.clone(), &session_uuid.to_string(), Some(db_path.clone()))
            .context("Failed to create session")?;
    session.set_checkpoint_store(persistence.clone());
    session.set_event_store(persistence.clone());

    // Graceful shutdown: Ctrl+C / SIGTERM sends Pause so a checkpoint is saved
    // before exit, keeping the session resumable.
    let signal_sink: Arc<tokio::sync::Mutex<Option<mpsc::Sender<SchedulerSignal>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    session.set_scheduler_signal_sink(Arc::clone(&signal_sink));
    super::run::install_shutdown_handler(signal_sink);

    // Set up the broadcast channel for real-time observability.
    let (event_tx, _) = broadcast::channel(256);
    session.set_event_broadcaster(event_tx.clone());

    let _server_handle;
    let _tracker_handle;
    if args.port > 0 {
        let live_state = Arc::new(tokio::sync::Mutex::new(crate::server::LiveState {
            goal: goal["goal"].as_str().unwrap_or("").to_string(),
            ..Default::default()
        }));
        _tracker_handle =
            crate::server::track_live_state(event_tx.subscribe(), Arc::clone(&live_state));
        _server_handle = crate::server::start_server_with_manager(
            session.spec().clone(),
            event_tx,
            live_state,
            args.port,
            Some(Arc::clone(&persistence)),
            Some(session_uuid),
            Some(manager.clone()),
            None,
            None,
            None,
            None,
        );
    } else {
        drop(event_tx);
        _tracker_handle = tokio::spawn(async {});
        _server_handle = tokio::spawn(async {});
    }

    let stats = if let Some(cp) = checkpoint {
        tracing::info!(round = cp.round, "Resuming from checkpoint at round {}", cp.round);
        session
            .resume_with_store(goal, Some(persistence.as_ref()), cp)
            .await
            .context("Session resume failed")?
    } else {
        tracing::info!("No checkpoint found — restarting from round 0 with original goal");
        session
            .run_with_store(goal, Some(persistence.as_ref()))
            .await
            .context("Session run failed")?
    };

    tracing::info!(
        rounds = stats.rounds_completed,
        elapsed_secs = stats.elapsed_secs,
        "Session completed"
    );

    Ok(())
}
