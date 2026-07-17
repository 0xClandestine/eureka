//! RAG document type and indexer.
//!
//! [`RagDocument`] defines the schema stored in the `sqlite-vec` vector store.
//! [`RagIndexer`] receives artifact emissions from the scheduler and embeds
//! them asynchronously in a best-effort, non-blocking manner.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rig_core::Embed;
use rig_sqlite::{Column, ColumnValue, SqliteVectorStoreTable};
use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::graph::artifact::Artifact;

// ---------------------------------------------------------------------------
// Document type
// ---------------------------------------------------------------------------

/// A single embeddable document stored in the `sqlite-vec` vector store.
///
/// The `#[embed]` attribute on `text` tells `rig-core` which field to embed.
/// `metadata` carries provenance (node ID, round, artifact kind, session ID).
#[derive(Clone, Debug, Deserialize, Embed, Serialize)]
pub struct RagDocument {
    /// Unique document identifier (`<node_id>:r<round>:<chunk_index>`).
    pub id: String,
    /// Embeddable text chunk from the artifact payload.
    #[embed]
    pub text: String,
    /// JSON provenance metadata.
    pub metadata: serde_json::Value,
}

impl SqliteVectorStoreTable for RagDocument {
    fn name() -> &'static str {
        "eureka_rag_documents"
    }

    fn schema() -> Vec<Column> {
        vec![
            Column::new("id", "TEXT PRIMARY KEY"),
            Column::new("text", "TEXT"),
            Column::new("metadata", "JSON"),
        ]
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn column_values(&self) -> Vec<(&'static str, Box<dyn ColumnValue>)> {
        vec![
            ("id", Box::new(self.id.clone())),
            ("text", Box::new(self.text.clone())),
            ("metadata", Box::new(self.metadata.clone())),
        ]
    }
}

// ---------------------------------------------------------------------------
// Indexer
// ---------------------------------------------------------------------------

/// Type alias for the boxed async embed-and-insert closure.
pub(crate) type EmbedInsertFn = Arc<
    dyn Fn(Vec<RagDocument>) -> Pin<Box<dyn Future<Output = Result<(), EngineError>> + Send>>
        + Send
        + Sync,
>;

/// Indexes artifact emissions into the `sqlite-vec` vector store.
///
/// Invocations are detached (`tokio::spawn`) by the scheduler so vector
/// embedding never blocks graph execution. All failures are logged as warnings.
pub struct RagIndexer {
    /// Type-erased closure that embeds and inserts a batch of documents.
    embed_and_insert: EmbedInsertFn,
    /// Artifact kinds to index. Empty means all kinds.
    index_kinds: HashSet<String>,
    /// Session ID embedded in document metadata for session-scoped retrieval.
    session_id: String,
    /// Shared atomic counter for live embedding token tracking. Incremented
    /// per-request so `/api/state` reflects up-to-date usage.
    live_tokens: Option<Arc<AtomicU64>>,
    /// Shared mutex for live embedding cost tracking in USD.
    live_cost: Option<Arc<Mutex<f64>>>,
    /// Cost per million tokens for the embedding model. Used to compute the
    /// `live_cost` increment from estimated token counts.
    embedding_cost_per_million_tokens: Option<f64>,
}

impl RagIndexer {
    /// Create a new indexer.
    ///
    /// `embed_and_insert` is a closure that takes a `Vec<RagDocument>`,
    /// generates embeddings, and inserts them into the store. It is called
    /// from a detached `tokio::spawn` task so it must be `Send + 'static`.
    pub fn new(
        embed_and_insert: EmbedInsertFn,
        index_kinds: HashSet<String>,
        session_id: String,
    ) -> Self {
        Self {
            embed_and_insert,
            index_kinds,
            session_id,
            live_tokens: None,
            live_cost: None,
            embedding_cost_per_million_tokens: None,
        }
    }

    /// Attach live per-request token and cost counters for real-time
    /// observability. Call once before the indexer is used by the scheduler.
    #[must_use]
    pub fn with_live_counters(
        mut self,
        tokens: Arc<AtomicU64>,
        cost: Arc<Mutex<f64>>,
        cost_per_million: Option<f64>,
    ) -> Self {
        self.live_tokens = Some(tokens);
        self.live_cost = Some(cost);
        self.embedding_cost_per_million_tokens = cost_per_million;
        self
    }

    /// Chunk `artifact`, build [`RagDocument`]s, and insert them into the
    /// store via the embedded closure.
    ///
    /// Returns early with `Ok(())` when the artifact kind is not in the
    /// configured allow-list (unless the list is empty, meaning all kinds).
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` if embedding or insertion fails.
    pub async fn index_artifact(
        &self,
        node_id: &str,
        round: u32,
        artifact: &Artifact,
    ) -> Result<(), EngineError> {
        if !self.index_kinds.is_empty() && !self.index_kinds.contains(&artifact.kind) {
            return Ok(());
        }

        let chunks = chunk_artifact(&artifact.kind, &artifact.data);
        if chunks.is_empty() {
            return Ok(());
        }

        let docs: Vec<RagDocument> = chunks
            .into_iter()
            .enumerate()
            .map(|(i, text)| RagDocument {
                id: format!("{node_id}:r{round}:{i}"),
                text,
                metadata: serde_json::json!({
                    "node_id":    node_id,
                    "round":      round,
                    "kind":       &artifact.kind,
                    "session_id": &self.session_id,
                }),
            })
            .collect();

        // Estimate embedding tokens and accumulate into live counters before
        // the async embed call so the UI updates without waiting.
        let estimated_tokens: u64 = docs.iter().map(|d| ((d.text.len() as u64) + 3) / 4).sum();
        if let Some(ref t) = self.live_tokens {
            t.fetch_add(estimated_tokens, Ordering::Relaxed);
        }
        if let (Some(ref c), Some(rate)) = (&self.live_cost, self.embedding_cost_per_million_tokens)
        {
            if let Ok(mut guard) = c.lock() {
                *guard += estimated_tokens as f64 * rate / 1_000_000.0;
            }
        }

        (self.embed_and_insert)(docs).await
    }
}

/// Split an artifact payload into individually embeddable chunks.
///
/// Array-valued artifacts (Hypotheses, Reviews, etc.) become one chunk per
/// element. All other artifacts are serialized as a single chunk.
fn chunk_artifact(kind: &str, data: &serde_json::Value) -> Vec<String> {
    let _ = kind; // reserved for per-kind strategy overrides
    if let serde_json::Value::Array(items) = data {
        items
            .iter()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        let s = serde_json::to_string(data).unwrap_or_default();
        if s.is_empty() {
            vec![]
        } else {
            vec![s]
        }
    }
}
