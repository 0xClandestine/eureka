//! Error types for the agents crate.

use thiserror::Error;

/// Errors that can occur during agent execution.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The LLM returned output that could not be parsed as valid JSON.
    #[error("Extraction failed: {0}")]
    ExtractionFailed(String),

    /// The LLM provider returned an error.
    #[error("Provider error: {0}")]
    Provider(String),

    /// A serialization error occurred.
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
