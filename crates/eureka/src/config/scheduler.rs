//! Scheduler configuration.

use serde::{Deserialize, Serialize};

/// Scheduler configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// Maximum number of in-flight LLM calls.
    pub max_in_flight: usize,
}
