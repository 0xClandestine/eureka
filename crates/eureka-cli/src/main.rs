//! # Eureka CLI
//!
//! The `eureka` binary — command-line interface for the AI co-scientist.
//! Uses `clap` for command parsing and `tracing-subscriber` for observability.

// Tests use unwrap/expect liberally; allow them in test builds so the
// workspace deny policy still applies to real CLI code paths.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod commands;
pub mod server;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Eureka — a graph-based AI co-scientist built in Rust.
#[derive(Debug, Parser)]
#[command(name = "eureka", version, about, author)]
#[command(propagate_version = true)]
struct Cli {
    /// Config file path (for daemon subcommands that load config).
    #[arg(short, long, global = true)]
    config: Option<String>,

    /// Data directory for daemon state (default: ~/.eureka).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Subcommand to execute.
    #[command(subcommand)]
    command: Commands,
}

/// Available subcommands for the Eureka CLI.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the background daemon (or daemon subcommands).
    #[command(subcommand)]
    Daemon(DaemonCommands),

    /// Start a new research run via the daemon.
    Start {
        /// The research goal (title).
        goal: String,

        /// Description of the research question.
        #[arg(short, long)]
        description: Option<String>,

        /// Domain of study.
        #[arg(short = 'D', long, default_value = "general")]
        domain: String,

        /// Maximum number of rounds.
        #[arg(short = 'r', long)]
        max_rounds: Option<u32>,
    },

    /// Inspect and control running or completed sessions.
    #[command(subcommand)]
    Session(SessionCommands),

    /// Run a research session directly (no daemon required).
    Run {
        /// The research goal (title).
        goal: String,

        /// Description of the research question.
        #[arg(short, long)]
        description: Option<String>,

        /// Domain of study.
        #[arg(short = 'D', long, default_value = "general")]
        domain: String,

        /// Maximum number of rounds.
        #[arg(short = 'r', long)]
        max_rounds: Option<u32>,

        /// Output file path for run stats JSON.
        #[arg(short, long)]
        output: Option<String>,

        /// Verbose output.
        #[arg(short, long)]
        verbose: bool,

        /// Port for the observability UI server (0 = disabled).
        #[arg(long, default_value_t = 7773)]
        port: u16,
    },

    /// Resume a failed or paused session (no daemon required).
    Resume {
        /// Session UUID to resume.
        id: String,

        /// Port for the observability UI server (0 = disabled).
        #[arg(long, default_value_t = 7773)]
        port: u16,
    },

    /// Pause a running session (daemon must be running).
    Pause {
        /// Session UUID to pause.
        id: String,
    },

    /// Cancel a running or paused session (daemon must be running).
    Cancel {
        /// Session UUID to cancel.
        id: String,
    },

    /// Validate a graph specification file.
    Validate {
        /// Path to the graph spec file.
        graph: String,
    },

    /// List available agents and graph specs.
    List,
}

/// Daemon subcommands.
#[derive(Debug, Subcommand)]
enum DaemonCommands {
    /// Start the background daemon (foreground; use & or service manager).
    Start {
        /// Port for the daemon HTTP server.
        #[arg(long, default_value_t = 7773)]
        port: u16,
    },
    /// Stop the running daemon.
    Stop,
    /// Check if the daemon is running.
    Status,
}

/// Session subcommands.
#[derive(Debug, Subcommand)]
enum SessionCommands {
    /// List all sessions managed by the daemon.
    List,
    /// Show detailed status of a session.
    Status {
        /// Session ID (UUID).
        id: String,
    },
    /// Show outputs from a completed or paused session.
    Output {
        /// Session ID (UUID).
        id: String,
        /// Optional node ID filter (show only outputs from this node).
        #[arg(long)]
        node: Option<String>,
        /// Output as raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Wait for a session to complete.
    Wait {
        /// Session ID (UUID).
        id: String,
        /// Maximum time to wait in seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Pause a running session.
    Pause {
        /// Session ID (UUID).
        id: String,
    },
    /// Resume a paused session.
    Resume {
        /// Session ID (UUID).
        id: String,
    },
    /// Cancel a running or paused session.
    Cancel {
        /// Session ID (UUID).
        id: String,
    },
    /// Inject an artifact into a paused session.
    Inject {
        /// Session ID (UUID).
        id: String,
        /// Target node ID.
        node: String,
        /// Target input port name.
        port: String,
        /// Artifact data as JSON.
        data: String,
    },
}

/// Main entry point.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Register sqlite-vec extension before any SQLite connection is opened.
    eureka::rag::init::register_sqlite_vec();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .pretty()
        .init();

    let cli = Cli::parse();

    // Resolve default data directory
    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);

    match cli.command {
        Commands::Daemon(cmd) => match cmd {
            DaemonCommands::Start { port } => {
                commands::daemon::execute_start(port, cli.config, &data_dir).await?;
            }
            DaemonCommands::Stop => {
                commands::daemon::execute_stop(&data_dir)?;
            }
            DaemonCommands::Status => {
                commands::daemon::execute_status(&data_dir)?;
            }
        },
        Commands::Start { goal, description, domain, max_rounds } => {
            commands::session::execute_start(
                &data_dir,
                &goal,
                description.as_deref(),
                &domain,
                max_rounds,
            )
            .await?;
        }
        Commands::Session(cmd) => match cmd {
            SessionCommands::List => {
                commands::session::execute_list(&data_dir).await?;
            }
            SessionCommands::Status { id } => {
                commands::session::execute_status(&data_dir, &id).await?;
            }
            SessionCommands::Output { id, node, json } => {
                commands::session::execute_output(&data_dir, &id, node.as_deref(), json).await?;
            }
            SessionCommands::Wait { id, timeout } => {
                commands::session::execute_wait(&data_dir, &id, timeout).await?;
            }
            SessionCommands::Pause { id } => {
                commands::session::execute_pause(&data_dir, &id).await?;
            }
            SessionCommands::Resume { id } => {
                commands::session::execute_resume(&data_dir, &id).await?;
            }
            SessionCommands::Cancel { id } => {
                commands::session::execute_cancel(&data_dir, &id).await?;
            }
            SessionCommands::Inject { id, node, port, data } => {
                commands::session::execute_inject(&data_dir, &id, &node, &port, &data).await?;
            }
        },
        Commands::Run { goal, description, domain, max_rounds, output, verbose, port } => {
            commands::run::execute(commands::run::RunArgs {
                config_path: cli.config.unwrap_or_else(|| "eureka.toml".to_string()),
                goal,
                description,
                domain,
                max_rounds,
                output,
                verbose,
                port,
            })
            .await?;
        }
        Commands::Resume { id, port } => {
            commands::resume::execute(commands::resume::ResumeArgs {
                session_id: id,
                config_path: cli.config.unwrap_or_else(|| "eureka.toml".to_string()),
                port,
            })
            .await?;
        }
        Commands::Pause { id } => {
            commands::session::execute_pause(&data_dir, &id).await?;
        }
        Commands::Cancel { id } => {
            commands::session::execute_cancel(&data_dir, &id).await?;
        }
        Commands::Validate { graph } => {
            commands::validate::execute(&graph)?;
        }
        Commands::List => {
            commands::list::execute(commands::list::ListArgs {
                config_path: cli.config.unwrap_or_else(|| "eureka.toml".to_string()),
            })?;
        }
    }

    Ok(())
}

/// Get the default daemon data directory (~/.eureka).
fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").ok().map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".eureka")
}
