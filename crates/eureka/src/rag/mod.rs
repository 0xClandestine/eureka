//! RAG (Retrieval-Augmented Generation) integration.
//!
//! Wires `rig-sqlite` vector search into the agent loop via
//! [`rig_core::agent::AgentBuilder::dynamic_context`].
//!
//! ## Usage
//!
//! 1. Call [`init::register_sqlite_vec`] once at process startup (before any
//!    `SQLite` connection is opened).
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
pub mod handle;
pub mod indexer;
pub mod init;

pub use embedding::build_rag_components;
pub use handle::RagIndexHandle;
pub use indexer::{RagDocument, RagIndexer};
