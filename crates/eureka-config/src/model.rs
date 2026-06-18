//! Model types for Eureka configuration.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Supported LLM provider kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderKind {
    /// Anthropic (Claude) provider.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// OpenAI provider.
    #[serde(rename = "openai")]
    OpenAI,
    /// OpenRouter — aggregator with access to hundreds of models.
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
    /// DeepSeek provider.
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
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::OpenRouter,
            generation_model: Some("deepseek/deepseek-v4-flash".into()),
            agent_models: std::collections::HashMap::new(),
        }
    }
}

/// Scheduler configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// Maximum number of in-flight LLM calls.
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight: usize,
}

const fn default_max_in_flight() -> usize {
    8
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_in_flight: default_max_in_flight(),
        }
    }
}

/// Budget configuration for a run.
///
/// This acts as a hard safety backstop. The graph's governor plugin is the
/// primary round controller — set `max_rounds` in the governor node's `config`
/// block in `graph.json`. The values here only fire if cost/time/token limits
/// are exceeded, or as a last-resort round cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Maximum total cost in USD.
    #[serde(default = "default_max_cost")]
    pub max_cost_usd: f64,
    /// Maximum total tokens consumed.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    /// Maximum wall-clock time (human-readable duration).
    #[serde(default = "default_max_wallclock")]
    pub max_wallclock: String,
    /// Hard round cap. The governor plugin in graph.json is the primary
    /// controller; this only fires if the plugin fails to halt.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
}

const fn default_max_cost() -> f64 {
    25.0
}

const fn default_max_tokens() -> u64 {
    5_000_000
}

fn default_max_wallclock() -> String {
    "45m".into()
}

const fn default_max_rounds() -> u32 {
    100
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_cost_usd: default_max_cost(),
            max_tokens: default_max_tokens(),
            max_wallclock: default_max_wallclock(),
            max_rounds: default_max_rounds(),
        }
    }
}

/// The top-level Eureka configuration, loaded from layered sources.
///
/// The agents directory is always `<graph_dir>/agents/` and is not configurable
/// separately — it is co-located with the graph file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EurekaConfig {
    /// Path to the graph specification JSON file.
    /// The `agents/` directory is expected to be a sibling of this file.
    #[serde(default = "default_graph_path")]
    pub graph: String,
    /// Provider configuration.
    #[serde(default)]
    pub provider: ProviderConfig,
    /// Scheduler configuration.
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    /// Budget configuration.
    #[serde(default)]
    pub budget: BudgetConfig,
}

fn default_graph_path() -> String {
    "graphs/coscientist/graph.json".into()
}

impl Default for EurekaConfig {
    fn default() -> Self {
        Self {
            graph: default_graph_path(),
            provider: ProviderConfig::default(),
            scheduler: SchedulerConfig::default(),
            budget: BudgetConfig::default(),
        }
    }
}

impl EurekaConfig {
    /// Load configuration from layered sources using figment.
    ///
    /// Order: defaults → file → env → CLI overrides.
    ///
    /// # Errors
    ///
    /// Returns a `ConfigError` if loading or merging fails.
    pub fn load() -> Result<Self, ConfigError> {
        // In a full implementation, this uses figment with TOML file + env + CLI
        // For now, return defaults (which is fine for development)
        Ok(Self::default())
    }

    /// Load configuration from a TOML string.
    ///
    /// # Errors
    ///
    /// Returns a `ConfigError` if the TOML is invalid.
    pub fn from_toml(toml_str: &str) -> Result<Self, ConfigError> {
        toml::from_str(toml_str).map_err(|e| ConfigError::ParseError(e.to_string()))
    }

    /// Convert to a `Budget` for the scheduler.
    #[must_use]
    pub fn to_graph_budget(&self) -> eureka_graph::control::Budget {
        let max_wallclock_secs = parse_duration(&self.budget.max_wallclock).unwrap_or(2700.0);
        eureka_graph::control::Budget {
            max_cost_usd: self.budget.max_cost_usd,
            max_tokens: self.budget.max_tokens,
            max_wallclock_secs,
            max_rounds: self.budget.max_rounds,
        }
    }
}

/// Parse a human-readable duration string (e.g., "45m", "2h", "30s") to seconds.
#[must_use]
fn parse_duration(duration: &str) -> Option<f64> {
    let duration = duration.trim();
    if duration.ends_with('s') {
        duration[..duration.len() - 1].parse::<f64>().ok()
    } else if duration.ends_with('m') {
        duration[..duration.len() - 1]
            .parse::<f64>()
            .ok()
            .map(|v| v * 60.0)
    } else if duration.ends_with('h') {
        duration[..duration.len() - 1]
            .parse::<f64>()
            .ok()
            .map(|v| v * 3600.0)
    } else {
        duration.parse::<f64>().ok()
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

    /// A required field is missing.
    #[error("Missing config: {0}")]
    Missing(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = EurekaConfig::default();
        assert_eq!(config.graph, "graphs/coscientist/graph.json");
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
        let toml_str = r#"
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
"#;
        let config = EurekaConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.provider.kind, ProviderKind::Anthropic);
        assert_eq!(config.scheduler.max_in_flight, 4);
        assert!((config.budget.max_cost_usd - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_budget_conversion() {
        let config = EurekaConfig::default();
        let budget = config.to_graph_budget();
        assert!((budget.max_wallclock_secs - 2700.0).abs() < f64::EPSILON);
        assert_eq!(budget.max_rounds, 100);
    }
}
