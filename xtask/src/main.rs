//! # Xtask — Build tasks for Eureka
//!
//! Graph validation in CI, documentation generation, and lint bundle.

use clap::{Parser, Subcommand};

/// Build tasks for the Eureka project.
#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Build automation for Eureka")]
struct Xtask {
    #[command(subcommand)]
    command: XtaskCommand,
}

#[derive(Debug, Subcommand)]
enum XtaskCommand {
    /// Validate all shipped graph specifications.
    ///
    /// Walks graphs/<name>/graph.json for each graph package.
    ValidateGraphs {
        /// Root directory containing graph packages.
        #[arg(default_value = "graphs")]
        graph_dir: String,
    },

    /// Generate documentation.
    Doc {
        /// Open in browser after building.
        #[arg(short, long)]
        open: bool,
    },

    /// Run the full lint bundle (fmt + clippy).
    Lint,
}

fn main() -> anyhow::Result<()> {
    let xtask = Xtask::parse();

    match xtask.command {
        XtaskCommand::ValidateGraphs { graph_dir } => {
            validate_graphs(&graph_dir)?;
        }
        XtaskCommand::Doc { open } => {
            build_docs(open)?;
        }
        XtaskCommand::Lint => {
            run_lint_bundle()?;
        }
    }

    Ok(())
}

/// Validate all graph packages found under `graph_dir`.
///
/// Each graph package is a subdirectory containing `graph.json`.
/// Uses a `PortRegistry` populated with all standard node kinds.
fn validate_graphs(graph_dir: &str) -> anyhow::Result<()> {
    let dir = std::path::Path::new(graph_dir);
    if !dir.exists() {
        anyhow::bail!("Graph directory '{}' does not exist", graph_dir);
    }

    let mut valid_count = 0;
    let mut invalid_count = 0;

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let package_dir = entry.path();
        if !package_dir.is_dir() {
            continue;
        }

        let graph_file = package_dir.join("graph.json");
        if !graph_file.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&graph_file)?;
        match eureka_graph::spec::GraphSpec::from_json(&content) {
            Ok(spec) => {
                let registry = build_port_registry();
                let result = eureka_graph::validate::validate_graph(&spec, &registry);
                if result.valid {
                    println!("  ✓  {}", graph_file.display());
                    valid_count += 1;
                } else {
                    println!("  ✗  {}:", graph_file.display());
                    for err in &result.errors {
                        println!("       {err}");
                    }
                    invalid_count += 1;
                }
            }
            Err(e) => {
                println!("  ✗  {} — parse error: {e}", graph_file.display());
                invalid_count += 1;
            }
        }
    }

    println!("\n{valid_count} valid, {invalid_count} invalid");
    if invalid_count > 0 {
        anyhow::bail!("{invalid_count} graph(s) failed validation");
    }
    Ok(())
}

/// Build a `PortRegistry` containing all standard node kinds.
///
/// Mirrors `build_standard_registry()` in `eureka-cli/src/commands/validate.rs`.
/// Keep these in sync.
fn build_port_registry() -> eureka_graph::validate::PortRegistry {
    use eureka_graph::port::{PortDirection, PortSpec, PortSpecEntry};

    let mut reg = eureka_graph::validate::PortRegistry::new();

    let input = |name: &str, kind: &str, required: bool| PortSpecEntry {
        name: name.into(),
        direction: PortDirection::Input,
        kind: kind.to_string(),
        required,
    };
    let output = |name: &str, kind: &str| PortSpecEntry {
        name: name.into(),
        direction: PortDirection::Output,
        kind: kind.to_string(),
        required: false,
    };

    reg.register(
        "generation",
        PortSpec::new(
            vec![
                input("in", "Goal", true),
                input("context", "Insights", false),
            ],
            vec![output("out", "Hypotheses")],
        ),
    );
    reg.register(
        "reflection",
        PortSpec::new(
            vec![input("in", "Hypotheses", true)],
            vec![output("out", "Reviews")],
        ),
    );
    reg.register(
        "evolution",
        PortSpec::new(
            vec![input("in", "Hypotheses", true)],
            vec![output("out", "Hypotheses")],
        ),
    );
    reg.register(
        "meta_review",
        PortSpec::new(
            vec![input("in", "Ranking", true)],
            vec![
                output("insights", "Insights"),
                output("overview", "Overview"),
            ],
        ),
    );
    reg.register(
        "control.ranking",
        PortSpec::new(
            vec![
                input("in", "Reviews", true),
                input("cycle", "Control", false),
            ],
            vec![output("top", "Hypotheses"), output("state", "Ranking")],
        ),
    );
    reg.register(
        "control.proximity",
        PortSpec::new(
            vec![input("in", "Hypotheses", true)],
            vec![output("unique", "Hypotheses")],
        ),
    );
    reg.register(
        "control.governor",
        PortSpec::new(
            vec![input("in", "Hypotheses", true)],
            vec![output("continue", "Control"), output("halt", "Control")],
        ),
    );
    reg.register(
        "control.merge",
        PortSpec::new(
            vec![
                input("in_a", "Hypotheses", false),
                input("in_b", "Hypotheses", false),
                input("in_c", "Hypotheses", false),
            ],
            vec![output("out", "Hypotheses"), output("state", "Ranking")],
        ),
    );
    reg.register(
        "control.router",
        PortSpec::new(
            vec![input("in", "Hypotheses", true)],
            vec![output("out", "Hypotheses")],
        ),
    );
    reg.register(
        "control.human_gate",
        PortSpec::new(
            vec![input("in", "Control", true)],
            vec![output("out", "Goal")],
        ),
    );

    reg
}

/// Build documentation.
fn build_docs(open: bool) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["doc", "--no-deps", "--workspace"]);
    if open {
        cmd.arg("--open");
    }
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("Documentation build failed");
    }
    Ok(())
}

/// Run fmt check + clippy.
fn run_lint_bundle() -> anyhow::Result<()> {
    println!("=== Lint Bundle ===");

    let steps: Vec<(&str, &[&str])> = vec![
        ("Format check", &["fmt", "--check"]),
        (
            "Clippy",
            &[
                "clippy",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
    ];

    for (name, args) in &steps {
        print!("  {name}... ");
        let status = std::process::Command::new("cargo").args(*args).status()?;
        if status.success() {
            println!("✓");
        } else {
            println!("✗");
            anyhow::bail!("{name} failed");
        }
    }

    println!("\nAll lint checks passed.");
    Ok(())
}
