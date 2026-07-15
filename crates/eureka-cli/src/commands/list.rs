//! `eureka list` command — introspect the configured graph manifest.
//!
//! Rather than advertising a hardcoded list of agents (which could drift from
//! what the shipped graph actually declares), this loads the graph manifest
//! referenced by the effective `EurekaConfig` and lists its declared agents,
//! control nodes, and tools.

use std::path::Path;

use anyhow::{Context, Result};
use eureka::config::EurekaConfig;
use eureka::manifest::GraphManifest;

/// Arguments for the `list` command.
#[derive(Debug, Default)]
pub struct ListArgs {
    /// Config file path.
    pub config_path: String,
}

/// Execute the `list` command.
///
/// Loads the graph manifest referenced by the config (defaults to
/// `eureka.toml` in the cwd) and prints a summary of its declared agents,
/// control nodes, and tools.
///
/// # Errors
///
/// Returns an error if the config or manifest cannot be loaded.
pub fn execute(args: ListArgs) -> Result<()> {
    let config_path =
        if args.config_path.is_empty() { "eureka.toml".to_string() } else { args.config_path };
    let config = EurekaConfig::load(Some(Path::new(&config_path)))
        .with_context(|| format!("Failed to load config from '{config_path}'"))?;

    let manifest = GraphManifest::load(Path::new(&config.graph))
        .with_context(|| format!("Failed to load graph manifest from '{}'", config.graph))?;

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║                 Eureka — Graph Manifest                 ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
    println!("  graph:  {}", manifest.name.as_deref().unwrap_or("(unnamed)"));
    println!("  path:   {}", config.graph);
    println!(
        "  nodes:  {} ({} agents, {} control)",
        manifest.agents.len() + manifest.control.len(),
        manifest.agents.len(),
        manifest.control.len()
    );
    println!("  edges:  {}", manifest.edges.len());
    println!();

    if !manifest.agents.is_empty() {
        println!("── Agents ──");
        println!();
        for agent in &manifest.agents {
            let desc = agent
                .description
                .as_deref()
                .map(|s| s.lines().next().unwrap_or("").to_string())
                .unwrap_or_default();
            print_entry(&agent.id, &desc);
        }
        println!();
    }

    if !manifest.control.is_empty() {
        println!("── Control Nodes ──");
        println!();
        for ctrl in &manifest.control {
            let desc = ctrl
                .description
                .as_deref()
                .map(|s| s.lines().next().unwrap_or("").to_string())
                .unwrap_or_default();
            print_entry(&ctrl.kind, &desc);
        }
        println!();
    }

    // Collect declared tool names across agents.
    let mut tools: Vec<(String, String)> = Vec::new();
    for agent in &manifest.agents {
        for tool in &agent.tools {
            if !tools.iter().any(|(n, _)| n == &tool.name) {
                tools.push((tool.name.clone(), tool.description.clone()));
            }
        }
    }
    if !tools.is_empty() {
        println!("── Tools ──");
        println!();
        for (name, desc) in &tools {
            print_entry(name, desc);
        }
        println!();
    }

    Ok(())
}

/// Print a formatted name/description entry.
fn print_entry(name: &str, description: &str) {
    println!("  {name:25}  {description}");
}
