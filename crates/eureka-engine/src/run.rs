//! Run orchestration — managing the lifecycle of a batch research run.

use eureka_config::model::EurekaConfig;
use eureka_graph::control::RunStats;

use crate::error::EngineError;
use crate::registry::NodeRegistry;
use crate::session::Session;

/// Configuration for a batch research run.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The research goal as a JSON value.
    pub goal: serde_json::Value,
    /// Optional overrides for the config.
    pub config_overrides: Option<EurekaConfig>,
}

/// The result of a single research run.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// The session ID.
    pub session_id: uuid::Uuid,
    /// The run statistics.
    pub stats: RunStats,
}

/// Execute a single research run with the given configuration and registry.
///
/// # Errors
///
/// Returns an `EngineError` if the session cannot start or run.
pub async fn run_single(
    config: EurekaConfig,
    registry: NodeRegistry,
    goal: serde_json::Value,
) -> Result<RunResult, EngineError> {
    let mut session = Session::new(config, registry)?;
    let stats = session.run(goal).await?;

    Ok(RunResult {
        session_id: session.session_id(),
        stats,
    })
}

/// Execute multiple research runs (for A/B comparison or batch scenarios).
///
/// # Errors
///
/// Returns an `EngineError` if any session fails.
pub async fn run_batch(
    base_config: EurekaConfig,
    registry: NodeRegistry,
    runs: Vec<RunConfig>,
) -> Result<Vec<RunResult>, EngineError> {
    let mut results = Vec::with_capacity(runs.len());

    for run_config in runs {
        let config = run_config
            .config_overrides
            .unwrap_or_else(|| base_config.clone());
        let result = run_single(config, registry.clone(), run_config.goal).await?;
        results.push(result);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_config_creation() {
        let config = RunConfig {
            goal: serde_json::json!({ "goal": "Test", "domain": "test" }),
            config_overrides: None,
        };
        assert_eq!(
            config.goal.get("goal").and_then(|v| v.as_str()),
            Some("Test")
        );
    }
}
