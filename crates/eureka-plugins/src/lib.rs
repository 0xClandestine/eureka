//! # Eureka Plugins
//!
//! Subprocess-based plugin system for Eureka. A plugin is an external program
//! (Python, shell, or any executable) that can act as a **control node** in a
//! graph and/or a **tool** available to agents.
//!
//! ## Discovery
//!
//! [`PluginRegistry::discover`] scans two directories:
//!
//! 1. `~/.eureka/plugins/` — user-global (lower priority)
//! 2. `<graph_dir>/plugins/` — graph-local (higher priority; overrides global)
//!
//! Each subdirectory with a `plugin.json` is loaded as a plugin.
//!
//! ## Node invocation protocol
//!
//! When the scheduler activates a [`ControlPluginNode`]:
//!
//! - A JSON **call envelope** is written to the subprocess stdin:
//!   `{"port": "in", "artifact": {"kind": "...", "data": {...}}}`
//! - The subprocess writes zero or more **emit envelopes** to stdout,
//!   one per line: `{"port": "out", "artifact": {"kind": "...", "data": {...}}}`
//!
//! ## Environment variables
//!
//! Every subprocess receives:
//!
//! | Variable | Value |
//! |---|---|
//! | `EUREKA_SESSION_ID` | Session UUID |
//! | `EUREKA_NODE_ID` | Node ID from `graph.json` |
//! | `EUREKA_ROUND` | Current round number |
//! | `EUREKA_CONFIG` | JSON-encoded node `config` from `graph.json` |
//! | `EUREKA_DB_PATH` | Path to the session SQLite database (if available) |

pub mod error;
pub mod manifest;
pub mod node;
pub mod registry;

pub use error::PluginError;
pub use manifest::PluginManifest;
pub use node::ControlPluginNode;
pub use registry::{PluginEntry, PluginRegistry};
