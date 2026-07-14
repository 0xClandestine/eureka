//! RAG (Retrieval-Augmented Generation) integration.
//!
//! Wires `rig-sqlite` vector search into the agent loop via
//! [`rig_core::agent::AgentBuilder::dynamic_context`].
//!
//! ## Usage
//!
//! 1. Call [`init::register_sqlite_vec`] once at process startup (before any
//!    SQLite connection is opened).
//! 2. Add a `[rag]` section to `eureka.toml` (see [`crate::config::RagConfig`]).
//! 3. The session wires everything automatically — no per-agent code required.
//!
//! ## Architecture
//!
//! `SqliteVectorIndex<E, T>` is generic over the embedding model type `E`,
//! which is not dyn-compatible. [`RagIndexHandle`] erases `E` by storing the
//! index as `Arc<dyn VectorStoreIndexDyn + Send + Sync>` and forwarding calls,
//! allowing it to be cheaply cloned and passed to `dynamic_context`.

pub mod embedding;
pub mod indexer;
pub mod init;

use std::sync::Arc;

use rig_core::vector_store::request::{Filter, VectorSearchRequest};
use rig_core::vector_store::{TopNResults, VectorStoreError, VectorStoreIndexDyn};
use rig_core::wasm_compat::WasmBoxedFuture;

pub use embedding::build_rag_components;
pub use indexer::{RagDocument, RagIndexer};

/// Type-erased, cheaply-cloneable wrapper around a [`VectorStoreIndexDyn`].
///
/// `SqliteVectorIndex<E, T>` cannot be stored in `RigClient<M>` without adding
/// a second generic parameter. `RagIndexHandle` erases the embedding model type
/// `E` so `RigClient<M>` stays single-generic while still supporting
/// `AgentBuilder::dynamic_context`.
///
/// [`VectorStoreIndexDyn`] is implemented directly (not via the blanket impl
/// for `VectorStoreIndex`) to allow the delegation pattern used here.
#[derive(Clone)]
pub struct RagIndexHandle(pub Arc<dyn VectorStoreIndexDyn + Send + Sync + 'static>);

impl VectorStoreIndexDyn for RagIndexHandle {
    fn top_n<'a>(
        &'a self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> WasmBoxedFuture<'a, TopNResults> {
        self.0.top_n(req)
    }

    fn top_n_ids<'a>(
        &'a self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> WasmBoxedFuture<'a, Result<Vec<(f64, String)>, VectorStoreError>> {
        self.0.top_n_ids(req)
    }
}
