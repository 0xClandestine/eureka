//! `eureka eval` command — A/B compare two graph specifications.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use eureka_config::model::EurekaConfig;
use eureka_engine::registry::NodeRegistry;
use eureka_engine::session::Session;
use eureka_graph::spec::GraphSpec;

/// Arguments for the `eval` command.
#[derive(Debug)]
pub struct EvalArgs {
    /// Path to the config file.
    pub config_path: String,
    /// First graph spec file.
    pub graph_a: String,
    /// Second graph spec file.
    pub graph_b: String,
    /// The research goal.
    pub goal: String,
    /// Output file for comparison results.
    pub output: Option<String>,
}

/// Execute the `eval` command.
///
/// # Errors
///
/// Returns an error if either session fails.
pub async fn execute(args: EvalArgs) -> Result<()> {
    // Load config
    let config = EurekaConfig::default();

    // Load graph specs
    let spec_a = load_graph_spec(&args.graph_a)?;
    let spec_b = load_graph_spec(&args.graph_b)?;

    // Build registry
    let registry = build_eval_registry();

    let goal = serde_json::json!({ "goal": args.goal, "domain": "general" });

    tracing::info!("Running GraphSpec A: {}", args.graph_a);
    let mut session_a = Session::with_spec(config.clone(), spec_a, registry.clone())?;
    let stats_a = session_a.run(goal.clone()).await?;

    tracing::info!("Running GraphSpec B: {}", args.graph_b);
    let mut session_b = Session::with_spec(config, spec_b, registry)?;
    let stats_b = session_b.run(goal).await?;

    // Compare results
    tracing::info!("=== Comparison Results ===");
    tracing::info!(
        "Graph A: {} rounds, {:.2}s",
        stats_a.rounds_completed,
        stats_a.elapsed_secs
    );
    tracing::info!(
        "Graph B: {} rounds, {:.2}s",
        stats_b.rounds_completed,
        stats_b.elapsed_secs
    );

    // Write output
    if let Some(output_path) = args.output {
        let comparison = serde_json::json!({
            "graph_a": args.graph_a,
            "graph_b": args.graph_b,
            "stats_a": {
                "rounds": stats_a.rounds_completed,
                "elapsed_secs": stats_a.elapsed_secs,
                "total_cost_usd": stats_a.total_cost_usd,
                "total_tokens": stats_a.total_tokens,
            },
            "stats_b": {
                "rounds": stats_b.rounds_completed,
                "elapsed_secs": stats_b.elapsed_secs,
                "total_cost_usd": stats_b.total_cost_usd,
                "total_tokens": stats_b.total_tokens,
            },
        });
        let json_str = serde_json::to_string_pretty(&comparison)?;
        std::fs::write(&output_path, json_str)
            .with_context(|| format!("Failed to write output to {}", output_path))?;
        tracing::info!("Comparison written to {}", output_path);
    }

    Ok(())
}

/// Load a graph spec from a file path.
fn load_graph_spec(path: &str) -> Result<GraphSpec> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read graph spec: {path}"))?;

    if path.ends_with(".json") {
        GraphSpec::from_json(&content)
            .with_context(|| format!("Failed to parse JSON graph spec: {path}"))
    } else {
        GraphSpec::from_toml(&content)
            .with_context(|| format!("Failed to parse TOML graph spec: {path}"))
    }
}

/// Build a minimal registry for evaluation.
fn build_eval_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();

    let ports = eureka_graph::port::PortSpec::new(
        vec![eureka_graph::port::PortSpecEntry {
            name: "in".into(),
            direction: eureka_graph::port::PortDirection::Input,
            kind: "Goal".to_string(),
            required: true,
        }],
        vec![eureka_graph::port::PortSpecEntry {
            name: "out".into(),
            direction: eureka_graph::port::PortDirection::Output,
            kind: "Overview".to_string(),
            required: false,
        }],
    );
    registry.register(
        "test.node",
        Arc::new(|_spec| Ok(eureka_graph::node::BoxedNode::new(TestNode))),
        ports,
    );

    registry
}

struct TestNode;

#[async_trait]
impl eureka_graph::node::Node for TestNode {
    fn ports(&self) -> eureka_graph::port::PortSpec {
        eureka_graph::port::PortSpec::new(
            vec![eureka_graph::port::PortSpecEntry {
                name: "in".into(),
                direction: eureka_graph::port::PortDirection::Input,
                kind: "Goal".to_string(),
                required: true,
            }],
            vec![eureka_graph::port::PortSpecEntry {
                name: "out".into(),
                direction: eureka_graph::port::PortDirection::Output,
                kind: "Overview".to_string(),
                required: false,
            }],
        )
    }

    async fn process(
        &self,
        _ctx: &eureka_graph::node::NodeCtx,
        _msg: eureka_graph::node::PortMsg,
    ) -> std::result::Result<Vec<eureka_graph::node::Emit>, eureka_graph::node::NodeError> {
        let artifact = eureka_graph::artifact::Artifact {
            kind: "Overview".to_string(),
            data: serde_json::json!({ "summary": "Evaluation complete." }),
        };
        Ok(vec![eureka_graph::node::Emit::new("out", artifact)])
    }
}
