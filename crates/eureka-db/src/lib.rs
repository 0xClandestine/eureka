//! # Eureka DB
//!
//! SQLite-backed session database for Eureka research runs.
//!
//! Every run is a [`SessionDb`] stored at
//! `~/.eureka/sessions/<session-id>/session.db`. The database records
//! every artifact emitted by the graph so the full scientific history of
//! the session is available for inspection, resume, and export.

pub mod db;
pub mod error;

pub use db::SessionDb;
pub use error::DbError;
