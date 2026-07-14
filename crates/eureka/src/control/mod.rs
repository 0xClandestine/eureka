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
//! - **Call**: the scheduler writes one JSON envelope to stdin. For a
//!   single-input node this is `{"port": "in", "artifact": {"kind": "...", "data": {...}}}`.
//!   For a multi-input node the envelope additionally carries an `inputs` array
//!   of `{port, artifact}` objects, one per populated input port, so scripts
//!   that need all inputs can read `envelope["inputs"]` while legacy scripts
//!   reading `envelope["artifact"]` keep working:
//!   ```json
//!   {"port": "in", "artifact": {"kind": "...", "data": {...}},
//!    "inputs": [{"port": "in", "artifact": {...}}, {"port": "context", "artifact": {...}}]}
//!   ```
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

/// Control node definition.
pub mod def;
/// Control error types.
pub mod error;
/// Subprocess-backed control node implementation.
pub mod node;
/// Shared subprocess runner.
pub mod process;

pub use def::ControlNodeDef;
pub use error::ControlError;
pub use node::ControlNode;
pub use process::run_subprocess;
