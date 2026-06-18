//! # Eureka Config
//!
//! Typed, layered configuration for Eureka using figment/serde.
//! Configuration is loaded from: defaults → file → env → CLI.

pub mod model;

pub use model::*;
