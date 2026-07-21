//! RAG document type and indexer.
//!
//! [`RagDocument`] defines the schema stored in the `sqlite-vec` vector store.
//! [`RagIndexer`] receives artifact emissions from the scheduler and embeds
//! them asynchronously in a best-effort, non-blocking manner.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rig_core::Embed;
use rig_sqlite::{Column, ColumnValue, SqliteVectorStoreTable};
use serde::{Deserialize, Serialize};

use crate::config::NodeRagConfig;
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
    /// Per-node RAG configuration keyed by node ID.
    node_configs: HashMap<String, NodeRagConfig>,
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
    /// `node_configs` provides per-node extraction and formatting overrides.
    pub fn new(
        embed_and_insert: EmbedInsertFn,
        index_kinds: HashSet<String>,
        session_id: String,
        node_configs: HashMap<String, NodeRagConfig>,
    ) -> Self {
        Self {
            embed_and_insert,
            index_kinds,
            session_id,
            node_configs,
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
    #[allow(clippy::cast_precision_loss)]
    pub async fn index_artifact(
        &self,
        node_id: &str,
        round: u32,
        artifact: &Artifact,
    ) -> Result<(), EngineError> {
        if !self.index_kinds.is_empty() && !self.index_kinds.contains(&artifact.kind) {
            return Ok(());
        }

        // Per-node config: check enabled flag first, then extract and format.
        let node_cfg = self.node_configs.get(node_id);
        if node_cfg.is_some_and(|c| !c.enabled) {
            return Ok(());
        }

        let raw_texts = if let Some(path) = node_cfg.and_then(|c| c.index_path.as_deref()) {
            extract_texts(path, &artifact.data)
        } else {
            chunk_artifact(&artifact.kind, &artifact.data)
        };
        if raw_texts.is_empty() {
            return Ok(());
        }

        // Apply inject_format template when configured; otherwise use raw text.
        let chunks: Vec<String> =
            if let Some(fmt) = node_cfg.and_then(|c| c.inject_format.as_deref()) {
                raw_texts.iter().map(|t| fmt.replace("{{text}}", t)).collect()
            } else {
                raw_texts
            };

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
        let estimated_tokens: u64 = docs.iter().map(|d| (d.text.len() as u64).div_ceil(4)).sum();
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

/// Split an artifact payload into individually embeddable JSON strings.
///
/// Array-valued artifacts become one chunk per element. All other artifacts
/// are serialised as a single chunk. Used as the fallback when no
/// `index_path` is configured for the emitting node.
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

/// Extract leaf text values from `data` following a dot-notation path.
///
/// Supports `field[*].subfield` to fan out over arrays, e.g.:
/// - `"hypotheses[*].statement"` → one string per hypothesis statement
/// - `"review.hypothesis.statement"` → single nested field
///
/// String leaves are returned as-is; non-string, non-null leaves are
/// JSON-serialised. Null leaves and empty strings are skipped.
fn extract_texts(path: &str, data: &serde_json::Value) -> Vec<String> {
    let mut current = vec![data.clone()];

    for segment in path.split('.') {
        let (field, is_wildcard) =
            segment.strip_suffix("[*]").map_or((segment, false), |f| (f, true));

        let mut next = Vec::new();
        for val in current {
            let child = if field.is_empty() {
                val
            } else {
                val.get(field).cloned().unwrap_or(serde_json::Value::Null)
            };
            if is_wildcard {
                if let serde_json::Value::Array(items) = child {
                    next.extend(items);
                }
            } else {
                next.push(child);
            }
        }
        current = next;
    }

    current
        .into_iter()
        .filter_map(|v| match v {
            serde_json::Value::String(s) if !s.is_empty() => Some(s),
            serde_json::Value::String(_) | serde_json::Value::Null => None,
            other => {
                let s = serde_json::to_string(&other).unwrap_or_default();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            }
        })
        .collect()
}
