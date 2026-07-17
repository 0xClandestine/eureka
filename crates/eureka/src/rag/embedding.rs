//! Embedding model construction for RAG.
//!
//! [`build_rag_components`] is the single entry point. It reads the
//! [`RagConfig`], constructs the concrete embedding model (erasing its type),
//! opens a `tokio-rusqlite` connection to the per-run `SQLite` database, and
//! returns a type-erased [`super::RagIndexHandle`] (for querying) plus an
//! [`super::indexer::RagIndexer`] (for inserting).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use rig_core::client::{EmbeddingsClient, ProviderClient};
use rig_core::embeddings::{EmbeddingModel, EmbeddingsBuilder};
use rig_core::providers::{
    cohere, gemini, llamafile, ollama, openai, openrouter, together, voyageai,
};
use rig_core::vector_store::InsertDocuments;
use rig_sqlite::SqliteVectorStore;

use crate::config::{EmbeddingProvider, RagConfig};
use crate::error::EngineError;

use super::indexer::{RagDocument, RagIndexer};
use super::RagIndexHandle;

/// Build a [`RagIndexHandle`] (query) and [`RagIndexer`] (insert) from the
/// supplied configuration and database path.
///
/// All generics over the embedding model are erased inside this function —
/// callers only see the type-erased handle and indexer.
///
/// # Errors
///
/// Returns `EngineError::Store` if the database cannot be opened or the
/// vector store cannot be initialised.
pub async fn build_rag_components(
    cfg: &RagConfig,
    db_path: &Path,
    session_id: &str,
    live_tokens: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    live_cost: Option<std::sync::Arc<std::sync::Mutex<f64>>>,
) -> Result<(RagIndexHandle, Arc<RagIndexer>), EngineError> {
    let cost_per_million = cfg.embedding_cost_per_million_tokens;
    match cfg.embedding_provider {
        EmbeddingProvider::OpenAI => {
            let client = openai::Client::from_env()
                .map_err(|e| EngineError::Store(format!("OpenAI client init failed: {e}")))?;
            let model = client.embedding_model(&cfg.embedding_model);
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
        EmbeddingProvider::Cohere => {
            let client = cohere::Client::from_env()
                .map_err(|e| EngineError::Store(format!("Cohere client init failed: {e}")))?;
            // "search_document" is the appropriate input_type for indexing.
            let model = client.embedding_model(&cfg.embedding_model, "search_document");
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
        EmbeddingProvider::Ollama => {
            let ndims = cfg.embedding_ndims.ok_or_else(|| {
                EngineError::Store(
                    "Ollama embedding provider requires `embedding_ndims` in [rag] config"
                        .to_string(),
                )
            })?;
            let client = ollama::Client::from_env()
                .map_err(|e| EngineError::Store(format!("Ollama client init failed: {e}")))?;
            let model = client.embedding_model_with_ndims(&cfg.embedding_model, ndims);
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
        EmbeddingProvider::VoyageAI => {
            let client = voyageai::Client::from_env()
                .map_err(|e| EngineError::Store(format!("VoyageAI client init failed: {e}")))?;
            let model = cfg.embedding_ndims.map_or_else(
                || client.embedding_model(&cfg.embedding_model),
                |n| client.embedding_model_with_ndims(&cfg.embedding_model, n),
            );
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
        EmbeddingProvider::Gemini => {
            let client = gemini::Client::from_env()
                .map_err(|e| EngineError::Store(format!("Gemini client init failed: {e}")))?;
            // Gemini infers ndims from the model name automatically.
            let model = client.embedding_model(&cfg.embedding_model);
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
        EmbeddingProvider::Together => {
            let client = together::Client::from_env()
                .map_err(|e| EngineError::Store(format!("Together client init failed: {e}")))?;
            let model = cfg.embedding_ndims.map_or_else(
                || client.embedding_model(&cfg.embedding_model),
                |n| client.embedding_model_with_ndims(&cfg.embedding_model, n),
            );
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
        EmbeddingProvider::Llamafile => {
            let client = llamafile::Client::from_env()
                .map_err(|e| EngineError::Store(format!("Llamafile client init failed: {e}")))?;
            let model = cfg.embedding_ndims.map_or_else(
                || client.embedding_model(&cfg.embedding_model),
                |n| client.embedding_model_with_ndims(&cfg.embedding_model, n),
            );
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
        EmbeddingProvider::OpenRouter => {
            let client = openrouter::Client::from_env()
                .map_err(|e| EngineError::Store(format!("OpenRouter client init failed: {e}")))?;
            let model = cfg.embedding_ndims.map_or_else(
                || client.embedding_model(&cfg.embedding_model),
                |n| client.embedding_model_with_ndims(&cfg.embedding_model, n),
            );
            build_with_model(
                model,
                cfg,
                db_path,
                session_id,
                live_tokens,
                live_cost,
                cost_per_million,
            )
            .await
        }
    }
}

/// Inner generic builder — erases `E` at the call site boundary.
async fn build_with_model<E>(
    model: E,
    cfg: &RagConfig,
    db_path: &Path,
    session_id: &str,
    live_tokens: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    live_cost: Option<std::sync::Arc<std::sync::Mutex<f64>>>,
    embedding_cost_per_million_tokens: Option<f64>,
) -> Result<(RagIndexHandle, Arc<RagIndexer>), EngineError>
where
    E: EmbeddingModel + Clone + Send + Sync + 'static,
{
    let conn = tokio_rusqlite::Connection::open(db_path)
        .await
        .map_err(|e| EngineError::Store(format!("RAG DB open failed: {e}")))?;

    let store = SqliteVectorStore::<E, RagDocument>::new(conn, &model)
        .await
        .map_err(|e| EngineError::Store(format!("RAG vector store init failed: {e}")))?;

    // Clone the store so the insert path and the query path share the same
    // underlying tokio-rusqlite connection handle.
    let store_for_insert = store.clone();
    let index = store.index(model.clone());

    // Type-erase the index into our Arc<dyn VectorStoreIndexDyn> wrapper.
    let handle = RagIndexHandle(Arc::new(index));

    // Build the type-erased embed-and-insert closure for RagIndexer.
    let store_arc = Arc::new(store_for_insert);
    let embed_and_insert: super::indexer::EmbedInsertFn =
        Arc::new(move |docs: Vec<RagDocument>| {
            let store = Arc::clone(&store_arc);
            let model = model.clone();
            Box::pin(async move {
                let embeddings = EmbeddingsBuilder::new(model)
                    .documents(docs)
                    .map_err(|e| EngineError::Store(format!("RAG embed error: {e}")))?
                    .build()
                    .await
                    .map_err(|e| EngineError::Store(format!("RAG embed build error: {e}")))?;
                store
                    .insert_documents(embeddings)
                    .await
                    .map_err(|e| EngineError::Store(format!("RAG insert error: {e}")))
            })
        });

    let mut indexer = RagIndexer::new(
        embed_and_insert,
        cfg.index_kinds.iter().cloned().collect::<HashSet<_>>(),
        session_id.to_string(),
    );
    if let (Some(lt), Some(lc)) = (&live_tokens, &live_cost) {
        indexer = indexer.with_live_counters(
            Arc::clone(lt),
            Arc::clone(lc),
            embedding_cost_per_million_tokens,
        );
    }
    let indexer = Arc::new(indexer);

    Ok((handle, indexer))
}
