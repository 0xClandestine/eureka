//! `eureka run` command — execute a research session.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use eureka::config::{Budget, EurekaConfig};
use eureka::Session;
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
    // Load config via figment layers: defaults → file → env
    let config = EurekaConfig::load(Some(Path::new(&args.config_path)))
        .with_context(|| format!("Failed to load config from '{}'", args.config_path))?;

    // Override max rounds if specified
    let config = if let Some(max_rounds) = args.max_rounds {
        EurekaConfig {
            budget: Budget {
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

    // Create the session — loads the manifest, validates, preps for run
    let mut session =
        Session::new(config, &session_id.to_string(), None).context("Failed to create session")?;

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
