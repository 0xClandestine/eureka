//! `eureka list` command — list available agents and graph specs.

use anyhow::Result;

/// Execute the `list` command.
///
/// # Errors
///
/// This command never returns an error.
pub fn execute() -> Result<()> {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║                 Eureka — Agent Registry                 ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    println!("── Scientific Agents ──");
    println!();
    print_agent(
        "generation",
        "Drafts novel hypotheses from scratch or grounded in literature",
    );
    print_agent(
        "reflection",
        "Multi-axis critique (Novelty, Correctness, Feasibility, Safety)",
    );
    print_agent(
        "ranking",
        "Pairwise scientific debate and Elo tournament ranking",
    );
    print_agent(
        "evolution",
        "Mutates and crossbreeds top hypotheses into improved offspring",
    );
    print_agent(
        "proximity",
        "Builds similarity graph, clusters hypotheses, selects diverse frontier",
    );
    print_agent(
        "meta_review",
        "Synthesizes tournament insights, feeds back to all agents, produces overview",
    );
    println!();

    println!("── Control Nodes ──");
    println!();
    print_agent(
        "supervisor",
        "Dynamic orchestrator: computes statistics, tracks convergence, manages context memory",
    );
    print_agent(
        "elo-ranker",
        "Elo tournament with similarity-based matchmaking from the proximity graph",
    );
    print_agent(
        "control.router",
        "Conditional dispatch based on artifact content/variant",
    );
    print_agent("control.merge", "Fan-in join over multiple inbound edges");
    print_agent(
        "control.broadcast",
        "Fan-out policy for distributing artifacts to multiple consumers",
    );
    println!();

    println!("── Available Tools ──");
    println!();
    print_tool("search_literature", "Searches arXiv for grounding literature");
    print_tool("read_paper", "Fetches full text of arXiv papers as Markdown");
    println!();

    println!("── Shipped Graph Specs ──");
    println!();
    println!("  coscientist/coscientist.yml  — Google's co-scientist topology (default)");
    println!();

    Ok(())
}

/// Print a formatted agent entry.
fn print_agent(name: &str, description: &str) {
    println!("  {name:25}  {description}");
}

/// Print a formatted tool entry.
fn print_tool(name: &str, description: &str) {
    println!("  {name:25}  {description}");
}
