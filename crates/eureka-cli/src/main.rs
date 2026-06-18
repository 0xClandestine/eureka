//! # Eureka CLI
//!
//! The `eureka` binary — command-line interface for the AI co-scientist.
//! Uses `clap` for command parsing and `tracing-subscriber` for observability.

pub mod commands;
pub mod server;

use clap::{Parser, Subcommand};

/// Eureka — a graph-based AI co-scientist built in Rust.
#[derive(Debug, Parser)]
#[command(name = "eureka", version, about, author)]
#[command(propagate_version = true)]
struct Cli {
    /// Config file path.
    #[arg(short, long, default_value = "eureka.toml")]
    config: String,

    /// Subcommand to execute.
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a research session.
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

        /// Output file for the final overview (JSON).
        #[arg(short, long)]
        output: Option<String>,

        /// Verbose output.
        #[arg(short, long)]
        verbose: bool,

        /// Port for the real-time UI server (0 to disable).
        #[arg(long, default_value_t = 7773)]
        port: u16,
    },

    /// Evaluate two graph specifications on the same goal.
    Eval {
        /// First graph spec file.
        graph_a: String,

        /// Second graph spec file.
        graph_b: String,

        /// The research goal.
        goal: String,

        /// Output file for comparison results.
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Validate a graph specification file.
    Validate {
        /// Path to the graph spec file.
        graph: String,
    },

    /// List available agents and graph specs.
    List,
}

/// Main entry point for the CLI.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .pretty()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            goal,
            description,
            domain,
            max_rounds,
            output,
            verbose,
            port,
        } => {
            commands::run::execute(commands::run::RunArgs {
                config_path: cli.config,
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
        Commands::Eval {
            graph_a,
            graph_b,
            goal,
            output,
        } => {
            commands::eval::execute(commands::eval::EvalArgs {
                config_path: cli.config,
                graph_a,
                graph_b,
                goal,
                output,
            })
            .await?;
        }
        Commands::Validate { graph } => {
            commands::validate::execute(graph).await?;
        }
        Commands::List => {
            commands::list::execute().await?;
        }
    }

    Ok(())
}
