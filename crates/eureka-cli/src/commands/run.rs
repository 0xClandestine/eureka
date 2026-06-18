//! `eureka run` command — execute a research session.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use eureka_config::model::EurekaConfig;
use eureka_db::SessionDb;
use eureka_engine::registry_builder;
use eureka_engine::session::Session;
use tokio::sync::broadcast;

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
pub async fn execute(args: RunArgs) -> Result<()> {
    // Load config
    let config = if Path::new(&args.config_path).exists() {
        let config_str = std::fs::read_to_string(&args.config_path)
            .with_context(|| format!("Failed to read config file: {}", args.config_path))?;
        EurekaConfig::from_toml(&config_str)?
    } else {
        tracing::warn!(
            "Config file '{}' not found, using defaults",
            args.config_path
        );
        EurekaConfig::default()
    };

    // Override max rounds if specified
    let config = if let Some(max_rounds) = args.max_rounds {
        EurekaConfig {
            budget: eureka_config::model::BudgetConfig {
                max_rounds,
                ..config.budget
            },
            ..config
        }
    } else {
        config
    };

    // Generate session ID early so it can be shared with the DB and plugins.
    let session_id = uuid::Uuid::now_v7();

    let goal = serde_json::json!({
        "goal": args.goal,
        "description": args.description.clone().unwrap_or_default(),
        "domain": args.domain,
    });

    // Create session DB. Failure is non-fatal — the session still runs without persistence.
    let graph_id = Path::new(&config.graph)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let goal_json = serde_json::to_string(&goal).unwrap_or_else(|_| "{}".to_string());
    let db: Option<Arc<SessionDb>> =
        match SessionDb::create(&session_id.to_string(), &graph_id, &goal_json) {
            Ok(db) => {
                tracing::info!(
                    session_id = %session_id,
                    path = %db.path().display(),
                    "Session database created"
                );
                Some(Arc::new(db))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to create session database; events will not be persisted"
                );
                None
            }
        };
    let db_path: Option<PathBuf> = db.as_ref().map(|d| d.path().clone());

    // Build the node registry — loads all agents and plugins from the graph directory
    let registry = registry_builder::build_registry(&config, &session_id.to_string(), db_path)
        .context("Failed to build node registry")?;

    tracing::info!(
        goal = %args.goal,
        domain = %args.domain,
        session_id = %session_id,
        "Starting Eureka research session"
    );

    // Create the session with the pre-generated session ID
    let mut session =
        Session::new_with_id(config, registry, session_id).context("Failed to create session")?;

    if let Some(db) = db {
        session.set_db(db);
    }

    // Set up the broadcast channel for real-time observability
    let (event_tx, _) = broadcast::channel(256);
    session.set_event_broadcaster(event_tx.clone());

    // Start the UI server (skip if port = 0)
    let _server_handle;
    let _tracker_handle;
    if args.port > 0 {
        let live_state = Arc::new(tokio::sync::Mutex::new(crate::server::LiveState {
            goal: args.goal.clone(),
            ..Default::default()
        }));
        _tracker_handle =
            crate::server::track_live_state(event_tx.subscribe(), Arc::clone(&live_state));
        _server_handle =
            crate::server::start_server(session.spec().clone(), event_tx, live_state, args.port);
    } else {
        drop(event_tx);
        _tracker_handle = tokio::spawn(async {});
        _server_handle = tokio::spawn(async {});
    }

    let stats = session.run(goal).await.context("Failed to run session")?;

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
