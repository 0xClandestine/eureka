//! Agent LLM configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Global defaults for per-agent LLM configuration.
///
/// These values apply to every agent that does not have an entry in
/// `[agent.overrides]` in the TOML config. Override per-agent under
/// `[agent.overrides.<agent_id>]` in `eureka.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Sampling temperature (higher = more diverse output).
    pub temperature: f64,
    /// Maximum number of loop iterations before the agent is forcibly halted.
    pub max_iterations: u32,
}

impl AgentConfig {
    /// Return the per-agent override for `agent_id`, or `self` if none exists.
    #[must_use]
    pub fn resolve_for(global: &Self, overrides: &HashMap<String, Self>, agent_id: &str) -> Self {
        overrides
            .get(agent_id)
            .cloned()
            .unwrap_or_else(|| global.clone())
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_iterations: 10,
        }
    }
}
