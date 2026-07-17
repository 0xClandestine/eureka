//! # Config
//!
//! Typed, layered configuration for Eureka.
//!
//! [`DEFAULT_TOML`] is the **single source of truth** for every default value.
//! All `Default` implementations and `#[serde(default)]` fallbacks on
//! sub-structs have been intentionally removed — if a field is missing from the
//! merged figment result it means `DEFAULT_TOML` is incomplete, which is a bug
//! we want caught immediately rather than silently papered over.
//!
//! Configuration is loaded from: defaults → file → env → CLI.

pub mod agent;
pub mod budget;
pub mod provider;
pub mod rag;
pub mod scheduler;
pub mod tracing;

pub use agent::{AgentConfig, AgentConfigOverride};
pub use budget::{parse_duration, Budget, RunStats};
pub use provider::{Pricing, ProviderConfig, ProviderKind};
pub use rag::{EmbeddingProvider, RagConfig};
pub use scheduler::SchedulerConfig;
pub use tracing::TracingConfig;

use std::collections::HashMap;
use std::path::Path;

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default configuration as a TOML string — the **single source of truth** for
/// every default value in [`EurekaConfig`].
///
/// This is merged as the base layer in [`EurekaConfig::load`] before the
/// user's file and environment variables. Keeping defaults here means they
/// are written in the same format users write their own `eureka.toml`, so
/// there's no mystery about what the defaults are.
const DEFAULT_TOML: &str = r#"
graph = "example/coscientist.yml"

[provider]
kind = "openrouter"
generation_model = "deepseek/deepseek-v4-flash"

[scheduler]
max_in_flight = 8

[budget]
max_cost_usd = 25.0
max_tokens = 5_000_000
max_wallclock = "45m"
max_rounds = 12

[agent]
temperature = 0.7
max_iterations = 10

# Per-agent overrides (optional). Set different temperature/max_iterations/workers
# for specific agents by their ID. Falls back to [agent] globals when absent.
# workers > 1 creates independent replica instances (cross-product edge fan-out).

[tracing]
enabled = false
max_file_bytes = 0
include_artifacts = true
"#;

/// The top-level Eureka configuration.
///
/// The agents directory is always `<graph_dir>/agents/` and is not configurable
/// separately — it is co-located with the graph file.
///
/// All fields are required at the serde level; missing values are supplied by
/// the [`DEFAULT_TOML`] base layer in [`EurekaConfig::load`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EurekaConfig {
    /// Path to the graph YAML manifest file.
    pub graph: String,
    /// Provider configuration.
    pub provider: ProviderConfig,
    /// Scheduler configuration.
    pub scheduler: SchedulerConfig,
    /// Budget configuration.
    pub budget: Budget,
    /// Global agent LLM defaults (overridable per-agent via `agent_overrides`).
    pub agent: AgentConfig,
    /// Per-agent LLM configuration overrides keyed by agent ID.
    ///
    /// Set in `eureka.toml` under `[agent_overrides.<agent_id>]`.
    /// Falls back to `agent` (the global default) when an agent has no match.
    #[serde(default)]
    pub agent_overrides: HashMap<String, AgentConfigOverride>,
    /// Tracing configuration for durable `SQLite` event history.
    pub tracing: TracingConfig,
    /// Optional RAG configuration. Absent or `enabled = false` disables RAG.
    #[serde(default)]
    pub rag: Option<RagConfig>,
}

impl Default for EurekaConfig {
    /// Returns the default configuration by parsing [`DEFAULT_TOML`].
    ///
    /// `DEFAULT_TOML` is always a valid complete config, so this can never
    /// fail in practice. Panics (via `unreachable!`) only if `DEFAULT_TOML`
    /// itself is malformed, which is caught by tests.
    fn default() -> Self {
        Figment::new().merge(Toml::string(DEFAULT_TOML)).extract::<Self>().unwrap_or_else(|_| {
            unreachable!("DEFAULT_TOML must always produce a valid EurekaConfig")
        })
    }
}

impl EurekaConfig {
    /// Load configuration from layered sources using figment.
    ///
    /// Layers (later wins):
    /// 1. [`DEFAULT_TOML`] — bundled defaults
    /// 2. Config file at `path` (or `"eureka.toml"` in cwd if `path` is `None`)
    /// 3. Environment variables with `EUREKA_` prefix
    ///
    /// CLI overrides are not applied here — the caller should patch the returned
    /// struct as needed (e.g., `max_rounds` from `--max-rounds`).
    ///
    /// # Errors
    ///
    /// Returns a `ConfigError` if loading or merging fails.
    pub fn load(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut figment = Figment::new().merge(Toml::string(DEFAULT_TOML));

        // Layer 2: config file
        match config_path {
            Some(path) if path.exists() => {
                figment = figment.merge(Toml::file(path));
            }
            Some(path) if !path.exists() => {
                // Non-default path that doesn't exist is an error.
                return Err(ConfigError::FileError(format!(
                    "Config file not found: {}",
                    path.display()
                )));
            }
            _ => {
                // No explicit path — try the default.
                let default = Path::new("eureka.toml");
                if default.exists() {
                    figment = figment.merge(Toml::file(default));
                }
            }
        }

        // Layer 3: environment variables (EUREKA_GRAPH, EUREKA_PROVIDER_KIND, …)
        //
        // Figment's Env provider splits keys on `_` to create nested paths, so
        // `EUREKA_PROVIDER_KIND` becomes `provider.kind` and maps directly to
        // the config struct. This means a field name containing an underscore
        // (like `max_wallclock`) cannot be set with a single env var; use the
        // underscore-free alias `maxwallclock` instead, i.e.
        // `EUREKA_BUDGET_MAXWALLCLOCK`. See the field docs on `Budget`.
        figment = figment.merge(Env::prefixed("EUREKA_"));

        figment.extract().map_err(|e| ConfigError::ParseError(e.to_string()))
    }
}

/// Errors that can occur during configuration loading.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A parse error occurred.
    #[error("Parse error: {0}")]
    ParseError(String),

    /// A file could not be read.
    #[error("File error: {0}")]
    FileError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = EurekaConfig::default();
        assert_eq!(config.graph, "example/coscientist.yml");
        assert_eq!(config.provider.kind, ProviderKind::OpenRouter);
        assert_eq!(config.scheduler.max_in_flight, 8);
    }

    #[test]
    fn test_duration_parsing() {
        assert!((parse_duration("30s").unwrap() - 30.0).abs() < f64::EPSILON);
        assert!((parse_duration("45m").unwrap() - 2700.0).abs() < f64::EPSILON);
        assert!((parse_duration("2h").unwrap() - 7200.0).abs() < f64::EPSILON);
        assert!((parse_duration("30").unwrap() - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_from_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml_path = dir.path().join("graph.toml");
        std::fs::write(
            &toml_path,
            r#"
graph = "graphs/coscientist/graph.json"

[provider]
kind = "anthropic"
generation_model = "claude-sonnet-4-20250514"

[scheduler]
max_in_flight = 4

[budget]
max_cost_usd = 10.0
max_tokens = 1000000
max_wallclock = "30m"
max_rounds = 6
"#,
        )
        .unwrap();
        let config = EurekaConfig::load(Some(&toml_path)).unwrap();
        assert_eq!(config.provider.kind, ProviderKind::Anthropic);
        assert_eq!(config.scheduler.max_in_flight, 4);
        assert!((config.budget.max_cost_usd - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_budget_defaults() {
        let config = EurekaConfig::default();
        assert!((config.budget.max_wallclock - 2700.0).abs() < f64::EPSILON);
        assert_eq!(config.budget.max_rounds, 12);
    }

    #[test]
    fn test_figment_load_with_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml_path = dir.path().join("test.toml");
        std::fs::write(
            &toml_path,
            r#"
[provider]
kind = "anthropic"
generation_model = "claude-sonnet-4-20250514"

[scheduler]
max_in_flight = 4

[budget]
max_cost_usd = 10.0
max_tokens = 1000000
max_wallclock = "30m"
max_rounds = 6
"#,
        )
        .unwrap();

        let config = EurekaConfig::load(Some(&toml_path)).unwrap();
        assert_eq!(config.provider.kind, ProviderKind::Anthropic);
        assert_eq!(config.provider.generation_model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(config.scheduler.max_in_flight, 4);
        assert!((config.budget.max_cost_usd - 10.0).abs() < f64::EPSILON);
        assert_eq!(config.budget.max_rounds, 6);
    }

    #[test]
    fn test_figment_load_missing_file_returns_defaults() {
        // When no file path is given and no default eureka.toml exists,
        // figment falls back to the DEFAULT_TOML base layer.
        let config = EurekaConfig::load(None).unwrap();
        assert_eq!(config.graph, "example/coscientist.yml");
        assert_eq!(config.provider.kind, ProviderKind::OpenRouter);
    }

    #[test]
    fn test_figment_load_explicit_missing_path_is_error() {
        let result = EurekaConfig::load(Some(Path::new("/nonexistent/eureka.toml")));
        assert!(result.is_err(), "explicit missing path should error");
    }

    #[test]
    fn test_figment_load_partial_file_merges_with_defaults() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml_path = dir.path().join("partial.toml");
        std::fs::write(&toml_path, r#"graph = "custom/graph.yml""#).unwrap();

        let config = EurekaConfig::load(Some(&toml_path)).unwrap();
        assert_eq!(config.graph, "custom/graph.yml");
        // Everything else should still be defaults
        assert_eq!(config.provider.kind, ProviderKind::OpenRouter);
        assert_eq!(config.scheduler.max_in_flight, 8);
    }

    #[test]
    fn test_env_override_max_wallclock_via_alias() {
        // Regression (M6): `max_wallclock` contains an underscore, so figment's
        // `_`-split Env layer would nest it as `budget.max.wallclock` and fail
        // to set it. The serde alias `maxwallclock` (underscore-free) lets the
        // env var `EUREKA_BUDGET_MAXWALLCLOCK` map cleanly. Test the alias via
        // a direct figment layer that mimics the env-derived key path.
        // The env-derived key path is `budget.maxwallclock`. Confirm the alias
        // resolves by deserializing a JSON object with that shape directly.
        let json = serde_json::json!({
            "max_cost_usd": 25.0,
            "max_tokens": 5_000_000,
            "maxwallclock": 120_u64,
            "max_rounds": 12
        });
        let budget: Budget = serde_json::from_value(json).unwrap();
        assert!((budget.max_wallclock - 120.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_pricing_cost_splits_input_and_output() {
        // Gotcha #4: cost must use separate input/output rates, not a flat
        // per-model rate. 1M input @ $0.27 + 0.5M output @ $1.10 = $0.82.
        let pricing = Pricing { input_per_million: 0.27, output_per_million: 1.10 };
        let cost = pricing.cost(1_000_000, 500_000);
        assert!((cost - 0.82).abs() < 1e-9, "expected 0.82, got {cost}");
        assert!(pricing.is_configured());
        assert!(!Pricing::default().is_configured());
    }

    #[test]
    fn test_provider_config_pricing_from_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml_path = dir.path().join("p.toml");
        std::fs::write(
            &toml_path,
            r#"
[provider]
kind = "openrouter"
generation_model = "x"
[provider.pricing]
input_per_million = 0.27
output_per_million = 1.10
"#,
        )
        .unwrap();
        let config = EurekaConfig::load(Some(&toml_path)).unwrap();
        let pricing = config.provider.pricing.expect("pricing should parse");
        assert!((pricing.input_per_million - 0.27).abs() < f64::EPSILON);
        assert!((pricing.output_per_million - 1.10).abs() < f64::EPSILON);
    }
}
