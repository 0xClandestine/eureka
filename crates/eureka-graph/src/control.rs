//! Control nodes — the pieces that make arbitrary topologies safe and expressive.
//!
//! These are built-in node kinds that handle orchestration, lifecycle, and
//! flow control, not LLM-based scientific work.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Control signals that flow through control nodes and feedback edges.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum ControlSignal {
    /// Continue processing (next round).
    Continue {
        /// The round number.
        round: u32,
    },
    /// Halt processing (termination).
    Halt {
        /// Reason for halting.
        reason: String,
    },
    /// Pause for human review.
    Pause,
}

impl std::fmt::Display for ControlSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue { round } => write!(f, "Continue(round={round})"),
            Self::Halt { reason } => write!(f, "Halt({reason})"),
            Self::Pause => write!(f, "Pause"),
        }
    }
}

/// A budget that governs how long a graph run can continue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum total cost in USD.
    pub max_cost_usd: f64,
    /// Maximum total tokens consumed.
    pub max_tokens: u64,
    /// Maximum wall-clock time in seconds.
    ///
    /// Supports deserialization from either a float (seconds) or a string
    /// with a suffix (`"30s"`, `"45m"`, `"2h"`).
    #[serde(alias = "max_wallclock", deserialize_with = "deserialize_wallclock")]
    pub max_wallclock_secs: f64,
    /// Maximum number of rounds (cycles). Acts as a hard backstop;
    /// the graph's governor plugin is the primary round controller.
    pub max_rounds: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_cost_usd: 25.0,
            max_tokens: 5_000_000,
            max_wallclock_secs: 2700.0, // 45 minutes
            max_rounds: 12,
        }
    }
}

/// Deserialize a wall-clock value that is either a float (seconds) or a
/// human-readable duration string (`"30s"`, `"45m"`, `"2h"`).
fn deserialize_wallclock<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct WallclockVisitor;

    impl<'de> de::Visitor<'de> for WallclockVisitor {
        type Value = f64;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a float (seconds) or a string with suffix (e.g. \"45m\", \"2h\", \"30s\")")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> {
            Ok(v)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> {
            Ok(v as f64)
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> {
            Ok(v as f64)
        }

        fn visit_str<E: de::Error>(self, s: &str) -> Result<f64, E> {
            parse_duration(s).ok_or_else(|| de::Error::custom(format!(
                "invalid duration string: '{s}'. Expected format: number + suffix (s/m/h), e.g. \"45m\""
            )))
        }
    }

    deserializer.deserialize_any(WallclockVisitor)
}

/// Parse a human-readable duration string (e.g., `"45m"`, `"2h"`, `"30s"`)
/// into seconds. Supports `s` (seconds), `m` (minutes), `h` (hours). A plain
/// number without a suffix is returned as-is.
#[must_use]
pub fn parse_duration(duration: &str) -> Option<f64> {
    let duration = duration.trim();
    if duration.ends_with('s') {
        duration[..duration.len() - 1].parse::<f64>().ok()
    } else if duration.ends_with('m') {
        duration[..duration.len() - 1]
            .parse::<f64>()
            .ok()
            .map(|v| v * 60.0)
    } else if duration.ends_with('h') {
        duration[..duration.len() - 1]
            .parse::<f64>()
            .ok()
            .map(|v| v * 3600.0)
    } else {
        duration.parse::<f64>().ok()
    }
}

/// Accumulated run statistics used by the governor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunStats {
    /// Total cost accumulated so far.
    pub total_cost_usd: f64,
    /// Total tokens consumed so far.
    pub total_tokens: u64,
    /// Wall-clock seconds elapsed.
    pub elapsed_secs: f64,
    /// Rounds completed.
    pub rounds_completed: u32,
}

impl RunStats {
    /// Check whether any budget limit has been exceeded.
    ///
    /// This is a hard backstop. The graph's governor plugin is the primary
    /// round controller; this only fires if cost/time/token limits are hit
    /// or if rounds exceed the configured hard cap.
    #[must_use]
    pub fn is_budget_exhausted(&self, budget: &Budget) -> Option<String> {
        if self.total_cost_usd >= budget.max_cost_usd {
            return Some(format!(
                "Cost budget exhausted: ${:.2} >= ${:.2}",
                self.total_cost_usd, budget.max_cost_usd
            ));
        }
        if self.total_tokens >= budget.max_tokens {
            return Some(format!(
                "Token budget exhausted: {} >= {}",
                self.total_tokens, budget.max_tokens
            ));
        }
        if self.elapsed_secs >= budget.max_wallclock_secs {
            return Some(format!(
                "Wall-clock budget exhausted: {:.0}s >= {:.0}s",
                self.elapsed_secs, budget.max_wallclock_secs
            ));
        }
        if self.rounds_completed >= budget.max_rounds {
            return Some(format!(
                "Round budget exhausted: {} >= {}",
                self.rounds_completed, budget.max_rounds
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_exhaustion_cost() {
        let budget = Budget {
            max_cost_usd: 10.0,
            ..Budget::default()
        };
        let stats = RunStats {
            total_cost_usd: 15.0,
            ..RunStats::default()
        };
        assert!(stats.is_budget_exhausted(&budget).is_some());
    }

    #[test]
    fn test_budget_not_exhausted() {
        let budget = Budget::default();
        let stats = RunStats::default();
        assert!(stats.is_budget_exhausted(&budget).is_none());
    }
}
