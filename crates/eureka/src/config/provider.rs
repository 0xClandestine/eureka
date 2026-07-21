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
        let input_rate = self.input_per_million.max(0.0);
        let output_rate = self.output_per_million.max(0.0);
        (output_tokens as f64 / 1_000_000.0)
            .mul_add(output_rate, (input_tokens as f64 / 1_000_000.0) * input_rate)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_provider_kinds_display_and_round_trip() {
        let kinds = [
            ProviderKind::Anthropic,
            ProviderKind::OpenAI,
            ProviderKind::OpenRouter,
            ProviderKind::Gemini,
            ProviderKind::Groq,
            ProviderKind::Mistral,
            ProviderKind::Cohere,
            ProviderKind::DeepSeek,
            ProviderKind::Perplexity,
            ProviderKind::Together,
            ProviderKind::XAI,
            ProviderKind::Ollama,
        ];
        for kind in kinds {
            let display = kind.to_string();
            assert!(!display.is_empty(), "display empty for {kind:?}");
            let json = serde_json::to_value(&kind).unwrap();
            let back: ProviderKind = serde_json::from_value(json).unwrap();
            assert_eq!(back, kind, "round-trip failed for {kind:?}");
        }
    }

    #[test]
    fn pricing_cost_zero_tokens_is_zero() {
        let pricing = Pricing { input_per_million: 1.0, output_per_million: 2.0 };
        assert_eq!(pricing.cost(0, 0), 0.0);
    }

    #[test]
    fn pricing_cost_input_only() {
        let pricing = Pricing { input_per_million: 1.0, output_per_million: 0.0 };
        assert!((pricing.cost(1_000_000, 0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pricing_cost_output_only() {
        let pricing = Pricing { input_per_million: 0.0, output_per_million: 1.0 };
        assert!((pricing.cost(0, 1_000_000) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pricing_is_configured_with_non_zero_input() {
        let p = Pricing { input_per_million: 0.01, output_per_million: 0.0 };
        assert!(p.is_configured());
    }

    #[test]
    fn pricing_is_configured_with_non_zero_output() {
        let p = Pricing { input_per_million: 0.0, output_per_million: 0.01 };
        assert!(p.is_configured());
    }

    #[test]
    fn pricing_not_configured_with_all_zeros() {
        assert!(!Pricing::default().is_configured());
    }

    #[test]
    fn pricing_cost_fractional_tokens() {
        let pricing = Pricing { input_per_million: 500.0, output_per_million: 500.0 };
        let cost = pricing.cost(500, 500);
        assert!(cost > 0.0);
        assert!(cost < 1.0);
    }

    #[test]
    fn provider_config_agent_models_default_to_empty() {
        let json = serde_json::json!({ "kind": "openai" });
        let config: ProviderConfig = serde_json::from_value(json).unwrap();
        assert!(config.agent_models.is_empty());
        assert!(config.generation_model.is_none());
        assert!(config.pricing.is_none());
    }

    #[test]
    fn provider_config_with_agent_models_deserializes() {
        let json = serde_json::json!({
            "kind": "openai",
            "generation_model": "gpt-4o",
            "agent_models": { "reflection": "gpt-4o-mini" }
        });
        let config: ProviderConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.agent_models.get("reflection"), Some(&"gpt-4o-mini".to_string()));
    }

    #[test]
    fn pricing_negative_input_rate_is_not_configured() {
        let p = Pricing { input_per_million: -1.0, output_per_million: 0.0 };
        assert!(!p.is_configured());
    }

    #[test]
    fn pricing_negative_output_rate_is_not_configured() {
        let p = Pricing { input_per_million: 0.0, output_per_million: -0.5 };
        assert!(!p.is_configured());
    }

    #[test]
    fn pricing_negative_rates_are_clamped_to_zero() {
        let p = Pricing { input_per_million: -1.0, output_per_million: -2.0 };
        let cost = p.cost(1_000_000, 1_000_000);
        assert!((cost - 0.0).abs() < 1e-9);
    }

    #[test]
    fn pricing_cost_with_max_u64_tokens_does_not_overflow() {
        let p = Pricing { input_per_million: 1.0, output_per_million: 1.0 };
        let cost = p.cost(u64::MAX, u64::MAX);
        assert!(cost.is_finite());
    }
}
