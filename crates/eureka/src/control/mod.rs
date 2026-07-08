//! # Control
//!
//! Subprocess-backed control nodes and the shared process runner.
//!
//! Control nodes are external programs (Python, shell, or any executable)
//! that act as graph nodes. They communicate with the scheduler via a simple
//! JSON envelope protocol on stdin/stdout.
//!
//! ## Invocation protocol
//!
//! - **Call**: the scheduler writes one JSON envelope to stdin:
//!   `{"port": "in", "artifact": {"kind": "...", "data": {...}}}`
//! - **Emit**: the subprocess writes zero or more envelopes to stdout, one
//!   per line: `{"port": "out", "artifact": {"kind": "...", "data": {...}}}`
//!
//! ## Environment variables
//!
//! | Variable | Value |
//! |---|---|
//! | `EUREKA_SESSION_ID` | Session UUID |
//! | `EUREKA_NODE_ID` | Node ID from the manifest |
//! | `EUREKA_ROUND` | Current round number |
//! | `EUREKA_CONFIG` | JSON-encoded node `config` from the manifest |
//! | `EUREKA_DB_PATH` | Path to the session SQLite database (if available) |

pub mod error;
pub mod node;
pub mod process;

pub use error::ControlError;
pub use node::{ControlNode, ControlNodeDef};
pub use process::run_subprocess;
