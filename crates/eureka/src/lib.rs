//! # Eureka
//!
//! Graph-based AI co-scientist runtime. Every concern lives in its own module
//! and composes into `Session`, the single entry point for running a research
//! graph.
//!
//! ## Module map
//!
//! | Module | What it owns |
//! |---|---|
//! | [`graph`] | Pure topology: nodes, edges, ports, artifacts, validation |
//! | [`scheduler`] | Event-driven graph executor |
//! | [`manifest`] | YAML graph format — loads a file into a [`graph::GraphSpec`] |
//! | [`agents`] | LLM-backed nodes: `AgentDef`, `LlmClient`, `LlmAgentNode` |
//! | [`control`] | Subprocess-backed control nodes and the shared process runner |
//! | [`config`] | Typed, layered runtime configuration |
//! | [`session`] | Assembles all of the above into a single runnable session |

// The strict workspace lints deny unwrap/expect in production code. Tests,
// however, use unwrap/expect liberally for ergonomics; allow them in test
// builds so `cargo clippy --all-targets` stays green without sacrificing
// the deny policy for real code paths.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::items_after_statements,
        clippy::too_many_lines,
        clippy::option_if_let_else,
        clippy::needless_borrows_for_generic_args,
        clippy::field_reassign_with_default,
        clippy::manual_let_else,
        clippy::significant_drop_tightening,
        clippy::needless_raw_string_hashes,
        trivial_casts,
        unused_qualifications,
        dead_code,
        unused_variables,
        unused_mut
    )
)]

pub mod agents;
pub mod config;
pub mod control;
pub mod error;
pub mod graph;
pub mod manager;
pub mod manifest;
pub mod run;
pub mod scheduler;
pub mod session;
pub mod tracing;

pub use error::EngineError;
pub use manager::{CreateRunRequest, RunManager};
pub use run::{
    open_persistence, ActivationSnapshot, CheckpointReason, CheckpointStore, EventStore,
    FileRunStore, InMemoryRunPersistence, PendingInput, PersistenceError, Revision, RunCheckpoint,
    RunEnvironment, RunEvent, RunFilter, RunOutput, RunPersistence, RunRecord, RunRepository,
    RunStatus, RunStore, SqliteRunPersistence, DATABASE_SCHEMA_VERSION,
};
pub use session::Session;
