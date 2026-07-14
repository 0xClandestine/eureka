//! `RagIndexHandle` — type-erased, cheaply-cloneable vector store wrapper.

use std::sync::Arc;

use rig_core::vector_store::request::{Filter, VectorSearchRequest};
use rig_core::vector_store::{TopNResults, VectorStoreError, VectorStoreIndexDyn};
use rig_core::wasm_compat::WasmBoxedFuture;

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
    fn top_n(
        &self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> WasmBoxedFuture<'_, TopNResults> {
        self.0.top_n(req)
    }

    fn top_n_ids(
        &self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> WasmBoxedFuture<'_, Result<Vec<(f64, String)>, VectorStoreError>> {
        self.0.top_n_ids(req)
    }
}
