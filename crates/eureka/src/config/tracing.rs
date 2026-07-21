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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracing_config_defaults_disabled_with_zero_max_file_bytes() {
        let json = serde_json::json!({});
        let cfg: TracingConfig = serde_json::from_value(json).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.max_file_bytes, 0);
        assert!(cfg.include_artifacts);
    }

    #[test]
    fn tracing_config_enabled_with_include_artifacts_false() {
        let json = serde_json::json!({
            "enabled": true,
            "max_file_bytes": 1048576,
            "include_artifacts": false
        });
        let cfg: TracingConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.max_file_bytes, 1_048_576);
        assert!(!cfg.include_artifacts);
    }

    #[test]
    fn tracing_config_include_artifacts_defaults_to_true() {
        let json = serde_json::json!({
            "enabled": true,
            "max_file_bytes": 0
        });
        let cfg: TracingConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.include_artifacts);
    }

    #[test]
    fn default_true_returns_true() {
        assert!(default_true());
    }

    #[test]
    fn tracing_config_serde_round_trip() {
        let original =
            TracingConfig { enabled: true, max_file_bytes: 5_000_000, include_artifacts: false };
        let json = serde_json::to_string(&original).unwrap();
        let restored: TracingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.enabled, original.enabled);
        assert_eq!(restored.max_file_bytes, original.max_file_bytes);
        assert_eq!(restored.include_artifacts, original.include_artifacts);
    }
}
