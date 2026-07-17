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
