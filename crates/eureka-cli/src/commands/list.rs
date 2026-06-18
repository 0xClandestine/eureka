//! `eureka list` command — list available agents and graph specs.

use anyhow::Result;

/// Execute the `list` command.
///
/// # Errors
///
/// This command never returns an error.
pub async fn execute() -> Result<()> {
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
        "Deduplicates and preserves a diverse hypothesis frontier via embeddings",
    );
    print_agent(
        "meta_review",
        "Synthesizes recurring patterns and produces the final ResearchOverview",
    );
    println!();

    println!("── Control Nodes ──");
    println!();
    print_agent(
        "control.governor",
        "Cycle gate: enforces budget (cost/tokens/time/rounds) and convergence detection",
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
    print_agent(
        "control.human_gate",
        "Optional human-in-the-loop pause point for seed injection / feedback",
    );
    println!();

    println!("── Available Tools ──");
    println!();
    print_tool("web_search", "Searches the web for grounding information");
    print_tool("fetch", "Fetches and extracts content from URLs");
    println!();

    println!("── Shipped Graph Specs ──");
    println!();
    println!("  graphs/coscientist/graph.json  — Google's co-scientist topology (default)");
    println!("  graphs/debate/graph.json       — Debate-only ensemble topology");
    println!("  graphs/jury/graph.json         — Multi-ranker jury topology");
    println!();

    println!("── Graph-as-Eval ──");
    println!();
    println!("  eureka eval <graph_a> <graph_b> <goal>");
    println!("  A/B test two graph topologies on the same goal + seed.");
    println!();

    Ok(())
}

fn print_agent(name: &str, description: &str) {
    println!("  {name:25}  {description}");
}

fn print_tool(name: &str, description: &str) {
    println!("  {name:25}  {description}");
}
