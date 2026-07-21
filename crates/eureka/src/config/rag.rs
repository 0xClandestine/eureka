//! RAG (Retrieval-Augmented Generation) configuration.

use serde::{Deserialize, Serialize};

use super::tracing::default_true;

/// Supported embedding providers for RAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddingProvider {
    /// `OpenAI` embeddings (`OPENAI_API_KEY`).
    #[serde(rename = "openai")]
    OpenAI,
    /// Cohere embeddings (`COHERE_API_KEY`).
    #[serde(rename = "cohere")]
    Cohere,
    /// Ollama local embeddings (`OLLAMA_BASE_URL`, required).
    #[serde(rename = "ollama")]
    Ollama,
    /// `VoyageAI` embeddings — dedicated embedding API (`VOYAGE_API_KEY`).
    /// Model-specific dimension defaults apply; `embedding_ndims` overrides.
    #[serde(rename = "voyageai")]
    VoyageAI,
    /// Gemini embeddings (`GEMINI_API_KEY`).
    /// Dimensions are inferred from the model name automatically.
    #[serde(rename = "gemini")]
    Gemini,
    /// Together `AI` embeddings — large OSS model catalogue (`TOGETHER_API_KEY`).
    /// Set `embedding_ndims` to match the chosen model's output dimensions.
    #[serde(rename = "together")]
    Together,
    /// Llamafile local embeddings (`LLAMAFILE_API_BASE_URL`).
    /// Set `embedding_ndims` if the model does not advertise its dimensions.
    #[serde(rename = "llamafile")]
    Llamafile,
    /// `OpenRouter` embeddings (`OPENROUTER_API_KEY`).
    /// `embedding_ndims` is optional; omitting it lets the model decide.
    #[serde(rename = "openrouter")]
    OpenRouter,
}

/// Per-agent RAG configuration, declared under each agent's `rag:` key in the YAML manifest.
///
/// Overrides the global `[rag]` settings for one specific agent.
/// When absent, global `RagConfig` values apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRagConfig {
    /// If `false`, this agent neither receives RAG context nor has its outputs indexed.
    /// Defaults to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Override the global `top_k` for this agent's dynamic context query.
    /// When absent, the global `RagConfig::top_k` applies.
    #[serde(default)]
    pub top_k: Option<usize>,
    /// Dot-notation path to extract index text from the artifact payload.
    ///
    /// Supports `field[*].subfield` to iterate arrays, e.g.:
    /// - `"hypotheses[*].statement"` — extracts `statement` from every element
    /// - `"review.hypothesis.statement"` — navigates nested fields
    ///
    /// When absent, the entire artifact data is serialised as a single JSON
    /// string (legacy behaviour).
    #[serde(default)]
    pub index_path: Option<String>,
    /// Template for the text stored in the vector index (and injected as RAG context).
    ///
    /// `{{text}}` is replaced with the extracted text.
    /// Example: `"Prior hypothesis: {{text}}"`.
    /// When absent, the raw extracted text is stored verbatim.
    #[serde(default)]
    pub inject_format: Option<String>,
}

/// Configuration for Retrieval-Augmented Generation.
///
/// Add a `[rag]` section to `eureka.toml` to enable. Requires an embedding
/// API key (or a running Ollama server for local embeddings).
///
/// **Example:**
/// ```toml
/// [rag]
/// enabled = true
/// embedding_provider = "openai"
/// embedding_model = "text-embedding-3-small"
/// top_k = 5
/// session_scoped = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    /// Enable or disable RAG for this run.
    pub enabled: bool,
    /// Embedding provider to use.
    pub embedding_provider: EmbeddingProvider,
    /// Embedding model name (provider-specific).
    pub embedding_model: String,
    /// Number of top documents retrieved per agent turn.
    pub top_k: usize,
    /// Artifact kinds to index. Empty means all kinds are indexed.
    #[serde(default)]
    pub index_kinds: Vec<String>,
    /// Agent IDs to attach dynamic context to. Empty means all agents.
    #[serde(default)]
    pub agent_ids: Vec<String>,
    /// Restrict retrieval to the current session only.
    #[serde(default = "default_true")]
    pub session_scoped: bool,
    /// Embedding output dimensions (required for Ollama; ignored otherwise).
    #[serde(default)]
    pub embedding_ndims: Option<usize>,
    /// Cost per million tokens for the embedding model (USD).
    ///
    /// When set, embedding token usage is estimated (chars/4) and accumulated
    /// into the per-request live cost counter. Omit for providers that do not
    /// charge for embeddings (e.g. local Ollama).
    #[serde(default)]
    pub embedding_cost_per_million_tokens: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_provider_serde_round_trip() {
        let providers = [
            EmbeddingProvider::OpenAI,
            EmbeddingProvider::Cohere,
            EmbeddingProvider::Ollama,
            EmbeddingProvider::VoyageAI,
            EmbeddingProvider::Gemini,
            EmbeddingProvider::Together,
            EmbeddingProvider::Llamafile,
            EmbeddingProvider::OpenRouter,
        ];
        for provider in providers {
            let json = serde_json::to_value(&provider).unwrap();
            let back: EmbeddingProvider = serde_json::from_value(json).unwrap();
            assert_eq!(back, provider);
        }
    }

    #[test]
    fn node_rag_config_default_enabled() {
        let json = serde_json::json!({});
        let cfg: NodeRagConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.top_k.is_none());
        assert!(cfg.index_path.is_none());
        assert!(cfg.inject_format.is_none());
    }

    #[test]
    fn node_rag_config_full_deserialization() {
        let json = serde_json::json!({
            "enabled": false,
            "top_k": 15,
            "index_path": "data.items[*].text",
            "inject_format": "Context: {{text}}"
        });
        let cfg: NodeRagConfig = serde_json::from_value(json).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.top_k, Some(15));
        assert_eq!(cfg.index_path.as_deref(), Some("data.items[*].text"));
        assert_eq!(cfg.inject_format.as_deref(), Some("Context: {{text}}"));
    }

    #[test]
    fn rag_config_defaults() {
        let json = serde_json::json!({
            "enabled": true,
            "embedding_provider": "openai",
            "embedding_model": "text-embedding-3-small",
            "top_k": 5
        });
        let cfg: RagConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.embedding_provider, EmbeddingProvider::OpenAI);
        assert_eq!(cfg.embedding_model, "text-embedding-3-small");
        assert_eq!(cfg.top_k, 5);
        assert!(cfg.index_kinds.is_empty());
        assert!(cfg.agent_ids.is_empty());
        assert!(cfg.session_scoped);
        assert!(cfg.embedding_ndims.is_none());
        assert!(cfg.embedding_cost_per_million_tokens.is_none());
    }

    #[test]
    fn rag_config_index_kinds_and_agent_ids() {
        let json = serde_json::json!({
            "enabled": true,
            "embedding_provider": "cohere",
            "embedding_model": "embed-v3",
            "top_k": 3,
            "index_kinds": ["Hypothesis", "Review"],
            "agent_ids": ["generation", "reflection"],
            "session_scoped": false,
            "embedding_ndims": 1024,
            "embedding_cost_per_million_tokens": 0.1
        });
        let cfg: RagConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.index_kinds, vec!["Hypothesis", "Review"]);
        assert_eq!(cfg.agent_ids, vec!["generation", "reflection"]);
        assert!(!cfg.session_scoped);
        assert_eq!(cfg.embedding_ndims, Some(1024));
        assert_eq!(cfg.embedding_cost_per_million_tokens, Some(0.1));
    }

    #[test]
    fn node_rag_config_serde_round_trip() {
        let original = NodeRagConfig {
            enabled: false,
            top_k: Some(20),
            index_path: Some("items[*].text".into()),
            inject_format: Some("Prior: {{text}}".into()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: NodeRagConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.enabled, original.enabled);
        assert_eq!(restored.top_k, original.top_k);
        assert_eq!(restored.index_path, original.index_path);
        assert_eq!(restored.inject_format, original.inject_format);
    }

    #[test]
    fn embedding_provider_deserialize_rejects_invalid() {
        let result: Result<EmbeddingProvider, _> = serde_json::from_str("\"invalid_provider\"");
        assert!(result.is_err());
    }

    #[test]
    fn node_rag_config_top_k_zero() {
        let json = serde_json::json!({ "top_k": 0 });
        let cfg: NodeRagConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.top_k, Some(0));
    }
}
