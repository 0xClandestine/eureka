//! LLM provider configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
/// budget backstop.
///
/// Most providers charge different rates for input (prompt) vs output
/// (completion) tokens; set both for an accurate estimate. As a convenience,
/// a single flat `cost_per_million_tokens` rate on [`ProviderConfig`] sets
/// both to the same value when `pricing` is absent.
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
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (output_tokens as f64 / 1_000_000.0).mul_add(
            self.output_per_million,
            (input_tokens as f64 / 1_000_000.0) * self.input_per_million,
        )
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
    pub agent_models: HashMap<String, String>,
    /// Per-input/per-output pricing (USD per million tokens) for the cost
    /// budget backstop. Most providers charge different rates for input
    /// (prompt) vs output (completion) tokens, so set both fields. When
    /// `None`, cost is not tracked and only the token budget is enforced.
    #[serde(default)]
    pub pricing: Option<Pricing>,
}
