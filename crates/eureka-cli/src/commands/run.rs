//! `eureka run` command — execute a research session.

use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use eureka::config::{Budget, EurekaConfig};
use eureka::persistence::{open_persistence, RunPersistence};
use eureka::scheduler::SchedulerSignal;
use eureka::{RunManager, Session};
use tokio::sync::{broadcast, mpsc};

/// Arguments for the `run` command.
#[derive(Debug)]
pub struct RunArgs {
    /// Path to the config file.
    pub config_path: String,
    /// Research goal title.
    pub goal: String,
    /// Description of the research question.
    pub description: Option<String>,
    /// Domain of study.
    pub domain: String,
    /// Maximum number of rounds.
    pub max_rounds: Option<u32>,
    /// Output file path.
    pub output: Option<String>,
    /// Verbose output.
    pub verbose: bool,
    /// Port for the UI server (0 = disabled).
    pub port: u16,
}

/// Execute the `run` command.
///
/// # Errors
///
/// Returns an error if the session fails.
#[allow(clippy::too_many_lines)]
pub async fn execute(args: RunArgs) -> Result<()> {
    // Load config via figment layers: defaults → file → env
    let config = EurekaConfig::load(Some(Path::new(&args.config_path)))
        .with_context(|| format!("Failed to load config from '{}'", args.config_path))?;

    // Override max rounds if specified
    let config = if let Some(max_rounds) = args.max_rounds {
        EurekaConfig { budget: Budget { max_rounds, ..config.budget }, ..config }
    } else {
        config
    };

    // Capture budget snapshot before config is moved into Session.
    let budget_snapshot = crate::server::BudgetSnapshot {
        max_rounds: Some(config.budget.max_rounds),
        max_tokens: Some(config.budget.max_tokens),
        max_cost_usd: Some(config.budget.max_cost_usd),
        max_wallclock_secs: Some(config.budget.max_wallclock as u64),
    };

    // Generate session ID early so it can be shared with the DB and plugins.
    let session_id = uuid::Uuid::now_v7();

    // Compute a per-session SQLite database path. Control nodes (supervisor,
    // ranker, proximity) use EUREKA_DB_PATH to persist Elo ratings, context
    // memory, and the proximity graph across rounds. Co-locate it with the
    // graph manifest under a `.eureka/` subdirectory so a graph's run state
    // lives alongside the graph.
    let graph_path = Path::new(&config.graph);
    let graph_dir = graph_path.parent().unwrap_or_else(|| Path::new("."));
    // Canonicalize to an absolute path so EUREKA_DB_PATH is absolute when
    // passed to tool subprocesses that run with current_dir = graph_dir.
    // Without this, a relative db_path resolves incorrectly inside the subprocess.
    let graph_dir_abs =
        std::fs::canonicalize(graph_dir).unwrap_or_else(|_| graph_dir.to_path_buf());
    let sessions_dir = graph_dir_abs.join(".eureka").join("sessions");
    let db_path = sessions_dir.join(format!("{session_id}.sqlite"));
    if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
        tracing::warn!(
            error = %e,
            "Failed to create sessions directory '{}'; control-node persistence disabled",
            sessions_dir.display()
        );
    }
    let db_path = if sessions_dir.exists() { Some(db_path) } else { None };
    let persistence: Arc<dyn RunPersistence> = match db_path.clone() {
        Some(path) => open_persistence(path).await.context("Failed to open SQLite persistence")?,
        None => Arc::new(eureka::InMemoryRunPersistence::new()),
    };

    let manager =
        RunManager::new(config.clone(), Arc::clone(&persistence), Some(sessions_dir.clone()));

    let goal = serde_json::json!({
        "goal": args.goal,
        "description": args.description.clone().unwrap_or_default(),
        "domain": args.domain,
    });

    // Create the session — loads the manifest, validates, preps for run
    let mut session = Session::new(config, &session_id.to_string(), db_path)
        .context("Failed to create session")?;
    session.set_checkpoint_store(persistence.clone());
    session.set_event_store(persistence.clone());

    // Graceful shutdown: Ctrl+C / SIGTERM sends Pause so a checkpoint is saved
    // before the process exits, making the session resumable.
    let signal_sink: Arc<tokio::sync::Mutex<Option<mpsc::Sender<SchedulerSignal>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    session.set_scheduler_signal_sink(Arc::clone(&signal_sink));
    install_shutdown_handler(signal_sink);

    tracing::info!(
        goal = %args.goal,
        domain = %args.domain,
        session_id = %session_id,
        "Starting Eureka research session"
    );

    // Set up the broadcast channel for real-time observability
    let (event_tx, _) = broadcast::channel(256);
    session.set_event_broadcaster(event_tx.clone());

    // Start the UI server (skip if port = 0)
    let _server_handle;
    let _tracker_handle;
    if args.port > 0 {
        // Shared per-request counters for real-time cost/token observability.
        // Both LLM completions and embedding requests increment these immediately.
        let live_token_counter = Arc::new(AtomicU64::new(0));
        let live_input_counter = Arc::new(AtomicU64::new(0));
        let live_output_counter = Arc::new(AtomicU64::new(0));
        let live_cost_counter = Arc::new(Mutex::new(0.0_f64));
        session.set_live_counters(
            Arc::clone(&live_token_counter),
            Arc::clone(&live_input_counter),
            Arc::clone(&live_output_counter),
            Arc::clone(&live_cost_counter),
        );

        let live_state = Arc::new(tokio::sync::Mutex::new(crate::server::LiveState {
            goal: args.goal.clone(),
            run_id: Some(session_id.to_string()),
            budget: budget_snapshot,
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
            Some(session_id),
            Some(manager.clone()),
            Some(live_token_counter),
            Some(live_input_counter),
            Some(live_output_counter),
            Some(live_cost_counter),
        );
    } else {
        drop(event_tx);
        _tracker_handle = tokio::spawn(async {});
        _server_handle = tokio::spawn(async {});
    }

    let stats = session
        .run_with_store(goal, Some(persistence.as_ref()))
        .await
        .context("Failed to run session")?;

    tracing::info!(
        rounds = stats.rounds_completed,
        elapsed_secs = stats.elapsed_secs,
        "Research session completed"
    );

    if let Some(output_path) = args.output {
        let output_json =
            serde_json::to_string_pretty(&stats).context("Failed to serialize run stats")?;
        std::fs::write(&output_path, output_json)
            .with_context(|| format!("Failed to write output to {output_path}"))?;
        tracing::info!("Results written to {output_path}");
    }

    Ok(())
}

/// Spawn a task that listens for Ctrl+C and SIGTERM and sends `Pause` to the
/// scheduler so it saves a checkpoint before the process exits.
pub(crate) fn install_shutdown_handler(
    sink: Arc<tokio::sync::Mutex<Option<mpsc::Sender<SchedulerSignal>>>>,
) {
    // Ctrl+C
    let sink_ctrlc = Arc::clone(&sink);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("Ctrl+C received — saving checkpoint before exit…");
            if let Some(tx) = sink_ctrlc.lock().await.as_ref() {
                let _ = tx.send(SchedulerSignal::Pause).await;
            }
        }
    });

    // SIGTERM (Unix only)
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut stream) = signal(SignalKind::terminate()) {
            stream.recv().await;
            tracing::info!("SIGTERM received — saving checkpoint before exit…");
            if let Some(tx) = sink.lock().await.as_ref() {
                let _ = tx.send(SchedulerSignal::Pause).await;
            }
        }
    });
}
