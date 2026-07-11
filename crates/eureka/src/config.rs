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

use std::path::Path;

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/// Deserialize a wall-clock value that is either a float (seconds) or a
/// human-readable duration string (`"30s"`, `"45m"`, `"2h"`).
fn deserialize_wallclock<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct WallclockVisitor;

    impl de::Visitor<'_> for WallclockVisitor {
        type Value = f64;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a float (seconds) or a string with suffix (e.g. \"45m\", \"2h\", \"30s\")")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> {
            Ok(v)
        }

        #[allow(clippy::cast_precision_loss)]
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> {
            Ok(v as f64)
        }

        #[allow(clippy::cast_precision_loss)]
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> {
            Ok(v as f64)
        }

        fn visit_str<E: de::Error>(self, s: &str) -> Result<f64, E> {
            parse_duration(s).ok_or_else(|| {
                de::Error::custom(format!(
                    "invalid duration string: '{s}'. Expected format: number + suffix (s/m/h), e.g. \"45m\""
                ))
            })
        }
    }

    deserializer.deserialize_any(WallclockVisitor)
}

/// Parse a human-readable duration string (e.g., `"45m"`, `"2h"`, `"30s"`)
/// into seconds. Supports `s` (seconds), `m` (minutes), `h` (hours). A plain
/// number without a suffix is returned as-is.
#[must_use]
#[allow(clippy::option_if_let_else)]
pub fn parse_duration(duration: &str) -> Option<f64> {
    let duration = duration.trim();
    if let Some(s) = duration.strip_suffix('s') {
        s.parse::<f64>().ok()
    } else if let Some(s) = duration.strip_suffix('m') {
        s.parse::<f64>().ok().map(|v| v * 60.0)
    } else if let Some(s) = duration.strip_suffix('h') {
        s.parse::<f64>().ok().map(|v| v * 3600.0)
    } else {
        duration.parse::<f64>().ok()
    }
}

/// Budget that governs how long a graph run can continue.
///
/// The `max_wallclock` field accepts both a float (seconds) and a
/// human-readable string (`"30s"`, `"45m"`, `"2h"`). The legacy name
/// `max_wallclock_secs` is also accepted as an alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum total cost in USD.
    pub max_cost_usd: f64,
    /// Maximum total tokens consumed.
    pub max_tokens: u64,
    /// Maximum wall-clock time in seconds.
    ///
    /// Accepts a float (seconds) or a human-readable string (`"30s"`,
    /// `"45m"`, `"2h"`) in config files. The legacy name `max_wallclock_secs`
    /// is also accepted. Via environment variables, figment splits keys on
    /// `_`, so set this with `EUREKA_BUDGET_MAXWALLCLOCK` (no inner
    /// underscore) or `EUREKA_BUDGET_MAXWALLCLOCKSECS`.
    #[serde(
        alias = "max_wallclock_secs",
        alias = "maxwallclock",
        alias = "maxwallclocksecs",
        deserialize_with = "deserialize_wallclock"
    )]
    pub max_wallclock: f64,
    /// Maximum number of rounds (cycles). Acts as a hard backstop;
    /// the graph's governor plugin is the primary round controller.
    pub max_rounds: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_cost_usd: 25.0,
            max_tokens: 5_000_000,
            max_wallclock: 2700.0, // 45 minutes
            max_rounds: 12,
        }
    }
}

// ---------------------------------------------------------------------------
// Run statistics
// ---------------------------------------------------------------------------

/// Accumulated run statistics, aggregated by the scheduler from each node's
/// [`NodeUsage`](crate::graph::node::NodeUsage) report and checked against a
/// [`Budget`] each cycle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStats {
    /// Total cost accumulated so far (best-effort; `0.0` if no pricing is
    /// configured — see [`ProviderConfig::pricing`]).
    pub total_cost_usd: f64,
    /// Total tokens consumed so far (input + output, or the provider's
    /// aggregate when it does not split them).
    pub total_tokens: u64,
    /// Total prompt/input tokens consumed so far.
    pub total_input_tokens: u64,
    /// Total completion/output tokens consumed so far.
    pub total_output_tokens: u64,
    /// Wall-clock seconds elapsed.
    pub elapsed_secs: f64,
    /// Rounds completed.
    pub rounds_completed: u32,
}

impl RunStats {
    /// Check whether any budget limit has been exceeded.
    ///
    /// This is a hard backstop. The graph's governor node is the primary
    /// round controller; this only fires if cost/time/token limits are hit
    /// or if rounds exceed the configured hard cap.
    #[must_use]
    pub fn is_budget_exhausted(&self, budget: &Budget) -> Option<String> {
        if self.total_cost_usd >= budget.max_cost_usd {
            return Some(format!(
                "Cost budget exhausted: ${:.2} >= ${:.2}",
                self.total_cost_usd, budget.max_cost_usd
            ));
        }
        if self.total_tokens >= budget.max_tokens {
            return Some(format!(
                "Token budget exhausted: {} >= {}",
                self.total_tokens, budget.max_tokens
            ));
        }
        if self.elapsed_secs >= budget.max_wallclock {
            return Some(format!(
                "Wall-clock budget exhausted: {:.0}s >= {:.0}s",
                self.elapsed_secs, budget.max_wallclock
            ));
        }
        if self.rounds_completed >= budget.max_rounds {
            return Some(format!(
                "Round budget exhausted: {} >= {}",
                self.rounds_completed, budget.max_rounds
            ));
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Control signals
// ---------------------------------------------------------------------------

/// Control signals that flow through control nodes and feedback edges.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ControlSignal {
    /// Continue processing (next round).
    Continue {
        /// The round number.
        round: u32,
    },
    /// Halt processing (termination).
    Halt {
        /// Reason for halting.
        reason: String,
    },
    /// Pause for human review.
    Pause,
}

impl std::fmt::Display for ControlSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue { round } => write!(f, "Continue(round={round})"),
            Self::Halt { reason } => write!(f, "Halt({reason})"),
            Self::Pause => write!(f, "Pause"),
        }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// Global defaults for per-agent LLM configuration.
///
/// These values are used when an agent's YAML spec does not explicitly set
/// `temperature` or `max_iterations`. Override globally via `[agent]` in
/// `eureka.toml` or per-agent via the agent's `config:` block in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Sampling temperature (higher = more diverse output).
    pub temperature: f64,
    /// Maximum number of loop iterations before the agent is forcibly halted.
    pub max_iterations: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_iterations: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Supported LLM provider kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderKind {
    /// Anthropic (Claude) provider.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// `OpenAI` provider.
    #[serde(rename = "openai")]
    OpenAI,
    /// `OpenRouter` — aggregator with access to hundreds of models.
    /// Uses `OPENROUTER_API_KEY`. Model IDs take the form `org/model`.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Google Gemini provider.
    #[serde(rename = "gemini")]
    Gemini,
    /// Groq inference provider (fast open-source models).
    #[serde(rename = "groq")]
    Groq,
    /// Mistral AI provider.
    #[serde(rename = "mistral")]
    Mistral,
    /// Cohere provider.
    #[serde(rename = "cohere")]
    Cohere,
    /// `DeepSeek` provider.
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// Perplexity AI provider.
    #[serde(rename = "perplexity")]
    Perplexity,
    /// Together AI provider.
    #[serde(rename = "together")]
    Together,
    /// xAI (Grok) provider.
    #[serde(rename = "xai")]
    XAI,
    /// Ollama — local models. Uses `OLLAMA_API_BASE_URL` (default: `http://localhost:11434`).
    #[serde(rename = "ollama")]
    Ollama,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic => write!(f, "anthropic"),
            Self::OpenAI => write!(f, "openai"),
            Self::OpenRouter => write!(f, "openrouter"),
            Self::Gemini => write!(f, "gemini"),
            Self::Groq => write!(f, "groq"),
            Self::Mistral => write!(f, "mistral"),
            Self::Cohere => write!(f, "cohere"),
            Self::DeepSeek => write!(f, "deepseek"),
            Self::Perplexity => write!(f, "perplexity"),
            Self::Together => write!(f, "together"),
            Self::XAI => write!(f, "xai"),
            Self::Ollama => write!(f, "ollama"),
        }
    }
}

/// Per-model token pricing (USD per million tokens) used by the cost
/// budget backstop. Most providers charge different rates for input
/// (prompt) vs output (completion) tokens; set both for an accurate
/// estimate. As a convenience, a single flat `cost_per_million_tokens`
/// rate on [`ProviderConfig`] sets both to the same value when `pricing`
/// is absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pricing {
    /// USD per million input (prompt) tokens.
    #[serde(default)]
    pub input_per_million: f64,
    /// USD per million output (completion) tokens.
    #[serde(default)]
    pub output_per_million: f64,
}

impl Pricing {
    /// Compute the USD cost for the given token counts.
    #[must_use]
    pub fn cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 / 1_000_000.0) * self.input_per_million
            + (output_tokens as f64 / 1_000_000.0) * self.output_per_million
    }

    /// Whether any non-zero rate is configured.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.input_per_million > 0.0 || self.output_per_million > 0.0
    }
}

/// Configuration for a model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// The provider kind.
    pub kind: ProviderKind,
    /// Default model ID used for all agents unless overridden by `agent_models`.
    pub generation_model: Option<String>,
    /// Per-agent model overrides. Keys are agent names (e.g. `"reflection"`);
    /// values are model IDs. Falls back to `generation_model` when absent.
    #[serde(default)]
    pub agent_models: std::collections::HashMap<String, String>,
    /// Per-input/per-output pricing (USD per million tokens) for the cost
    /// budget backstop. Most providers charge different rates for input
    /// (prompt) vs output (completion) tokens, so set both fields. When
    /// `None`, cost is not tracked and only the token budget is enforced.
    #[serde(default)]
    pub pricing: Option<Pricing>,
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// Scheduler configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// Maximum number of in-flight LLM calls.
    pub max_in_flight: usize,
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

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

[tracing]
enabled = false
max_file_bytes = 0
include_artifacts = true
"#;

/// Configuration for durable JSONL tracing of scheduler events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    /// Write scheduler events to `.eureka/sessions/{session_id}.traces.jsonl`.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum file size in bytes before rotation (0 = no rotation).
    /// Reserved for future use.
    #[serde(default)]
    pub max_file_bytes: u64,
    /// Whether to include artifact payloads in `ActivationCompleted` outputs.
    #[serde(default = "default_true")]
    pub include_artifacts: bool,
}

fn default_true() -> bool {
    true
}

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
    /// Global agent LLM defaults (overridable per-agent in the manifest).
    pub agent: AgentConfig,
    /// Tracing configuration for durable JSONL event logs.
    pub tracing: TracingConfig,
}

impl Default for EurekaConfig {
    /// Returns the default configuration by parsing [`DEFAULT_TOML`].
    ///
    /// `DEFAULT_TOML` is always a valid complete config, so this can never
    /// fail in practice. Panics (via `unreachable!`) only if `DEFAULT_TOML`
    /// itself is malformed, which is caught by tests.
    fn default() -> Self {
        Figment::new()
            .merge(Toml::string(DEFAULT_TOML))
            .extract::<Self>()
            .unwrap_or_else(|_| {
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

        figment
            .extract()
            .map_err(|e| ConfigError::ParseError(e.to_string()))
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
        assert_eq!(
            config.provider.generation_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
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
        let pricing = Pricing {
            input_per_million: 0.27,
            output_per_million: 1.10,
        };
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
