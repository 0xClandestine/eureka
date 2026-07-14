//! Tracing / event-history configuration.

use serde::{Deserialize, Serialize};

/// Serde default helper: returns `true`.
pub(super) const fn default_true() -> bool {
    true
}

/// Configuration for durable scheduler event history in `SQLite`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    /// Persist scheduler events to the `SQLite` event history.
    #[serde(default)]
    pub enabled: bool,
    /// Retained for configuration compatibility; `SQLite` has no file rotation.
    #[serde(default)]
    pub max_file_bytes: u64,
    /// Whether to include artifact payloads in activation-completed events.
    #[serde(default = "default_true")]
    pub include_artifacts: bool,
}
