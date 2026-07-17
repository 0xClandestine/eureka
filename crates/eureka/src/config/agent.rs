//! Agent LLM configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Global defaults for per-agent LLM configuration.
///
/// These values apply to every agent that does not have an entry in
/// `[agent_overrides]` in the TOML config. Override per-agent under
/// `[agent_overrides.<agent_id>]` in `eureka.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Sampling temperature (higher = more diverse output).
    pub temperature: f64,
    /// Maximum number of loop iterations before the agent is forcibly halted.
    pub max_iterations: u32,
    /// Number of concurrent replica instances per node activation.
    ///
    /// When `workers > 1` the scheduler expands the node into `workers`
    /// independent replicas (e.g. `generation.r0`, `generation.r1`). Each runs
    /// concurrently within the `max_in_flight` limit. Edges fan out via
    /// cross-product: each upstream replica feeds each downstream replica.
    #[serde(default = "default_workers")]
    pub workers: u32,
}

/// Default worker count for agents without an explicit override.
const fn default_workers() -> u32 {
    1
}

/// Partial per-agent override.
///
/// Only the fields explicitly set in `[agent_overrides.<id>]` differ from the
/// global `[agent]` defaults. Unset fields (deserialize as `None`) fall back
/// to the global value at resolve time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfigOverride {
    /// Override sampling temperature. Falls back to `agent.temperature`.
    pub temperature: Option<f64>,
    /// Override max iterations. Falls back to `agent.max_iterations`.
    pub max_iterations: Option<u32>,
    /// Override worker count. Falls back to `agent.workers`.
    pub workers: Option<u32>,
}

impl AgentConfig {
    /// Merge the per-agent override (if any) onto `global` and return the
    /// resolved config for `agent_id`. Unset override fields use the global
    /// value, so a TOML entry of `[agent_overrides.generation] workers = 5`
    /// preserves the global `temperature` and `max_iterations`.
    #[must_use]
    pub fn resolve_for(
        global: &Self,
        overrides: &HashMap<String, AgentConfigOverride>,
        agent_id: &str,
    ) -> Self {
        overrides.get(agent_id).map_or_else(
            || global.clone(),
            |ov| Self {
                temperature: ov.temperature.unwrap_or(global.temperature),
                max_iterations: ov.max_iterations.unwrap_or(global.max_iterations),
                workers: ov.workers.unwrap_or(global.workers),
            },
        )
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self { temperature: 0.7, max_iterations: 10, workers: 1 }
    }
}
