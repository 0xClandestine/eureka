//! Scheduler configuration.

use serde::{Deserialize, Serialize};

/// Scheduler configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// Maximum number of in-flight LLM calls.
    pub max_in_flight: usize,
    /// Maximum number of times to retry a failed activation before aborting
    /// the run. A value of `3` means up to 3 re-attempts after the initial
    /// failure (4 total attempts). Set to `0` to disable retries.
    pub max_retries: u32,
    /// Base delay in milliseconds between retry attempts. Each successive
    /// attempt waits `retry_backoff_ms * 2^(attempt-1)`, capped at 30 000 ms.
    pub retry_backoff_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_config_deserialization() {
        let json = serde_json::json!({
            "max_in_flight": 8,
            "max_retries": 3,
            "retry_backoff_ms": 1000
        });
        let config: SchedulerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.max_in_flight, 8);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.retry_backoff_ms, 1000);
    }

    #[test]
    fn scheduler_config_zero_retries_disables_retry() {
        let json = serde_json::json!({
            "max_in_flight": 1,
            "max_retries": 0,
            "retry_backoff_ms": 500
        });
        let config: SchedulerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.max_retries, 0);
    }

    #[test]
    fn scheduler_config_serde_round_trip() {
        let original =
            SchedulerConfig { max_in_flight: 16, max_retries: 5, retry_backoff_ms: 2000 };
        let json = serde_json::to_string(&original).unwrap();
        let restored: SchedulerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.max_in_flight, original.max_in_flight);
        assert_eq!(restored.max_retries, original.max_retries);
        assert_eq!(restored.retry_backoff_ms, original.retry_backoff_ms);
    }

    #[test]
    fn scheduler_config_max_in_flight_zero() {
        let json = serde_json::json!({
            "max_in_flight": 0,
            "max_retries": 0,
            "retry_backoff_ms": 0
        });
        let config: SchedulerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.max_in_flight, 0);
    }
}
