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
