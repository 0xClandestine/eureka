//! Budget and run-statistics types.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Wallclock deserializer
// ---------------------------------------------------------------------------

/// Deserialize a wall-clock value that is either a float (seconds) or a
/// human-readable duration string (`"30s"`, `"45m"`, `"2h"`).
pub(super) fn deserialize_wallclock<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct WallclockVisitor;

    impl de::Visitor<'_> for WallclockVisitor {
        type Value = f64;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a float (seconds) or a string with suffix (e.g. \"45m\", \"2h\", \"30s\")")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> {
            Ok(v)
        }

        #[allow(clippy::cast_precision_loss)]
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> {
            Ok(v as f64)
        }

        #[allow(clippy::cast_precision_loss)]
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> {
            Ok(v as f64)
        }

        fn visit_str<E: de::Error>(self, s: &str) -> Result<f64, E> {
            parse_duration(s).ok_or_else(|| {
                de::Error::custom(format!(
                    "invalid duration string: '{s}'. Expected format: number + suffix (s/m/h), e.g. \"45m\""
                ))
            })
        }
    }

    deserializer.deserialize_any(WallclockVisitor)
}

/// Parse a human-readable duration string (e.g., `"45m"`, `"2h"`, `"30s"`)
/// into seconds. Supports `s` (seconds), `m` (minutes), `h` (hours). A plain
/// number without a suffix is returned as-is.
#[must_use]
#[allow(clippy::option_if_let_else)]
pub fn parse_duration(duration: &str) -> Option<f64> {
    let duration = duration.trim();
    if let Some(s) = duration.strip_suffix('s') {
        s.parse::<f64>().ok()
    } else if let Some(s) = duration.strip_suffix('m') {
        s.parse::<f64>().ok().map(|v| v * 60.0)
    } else if let Some(s) = duration.strip_suffix('h') {
        s.parse::<f64>().ok().map(|v| v * 3600.0)
    } else {
        duration.parse::<f64>().ok()
    }
}

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/// Budget that governs how long a graph run can continue.
///
/// The `max_wallclock` field accepts both a float (seconds) and a
/// human-readable string (`"30s"`, `"45m"`, `"2h"`). The legacy name
/// `max_wallclock_secs` is also accepted as an alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum total cost in USD.
    pub max_cost_usd: f64,
    /// Maximum total tokens consumed.
    pub max_tokens: u64,
    /// Maximum wall-clock time in seconds.
    ///
    /// Accepts a float (seconds) or a human-readable string (`"30s"`,
    /// `"45m"`, `"2h"`) in config files. The legacy name `max_wallclock_secs`
    /// is also accepted. Via environment variables, figment splits keys on
    /// `_`, so set this with `EUREKA_BUDGET_MAXWALLCLOCK` (no inner
    /// underscore) or `EUREKA_BUDGET_MAXWALLCLOCKSECS`.
    #[serde(
        alias = "max_wallclock_secs",
        alias = "maxwallclock",
        alias = "maxwallclocksecs",
        deserialize_with = "deserialize_wallclock"
    )]
    pub max_wallclock: f64,
    /// Maximum number of rounds (cycles). Acts as a hard backstop;
    /// the graph's governor plugin is the primary round controller.
    pub max_rounds: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_cost_usd: 25.0,
            max_tokens: 5_000_000,
            max_wallclock: 2700.0, // 45 minutes
            max_rounds: 12,
        }
    }
}

// ---------------------------------------------------------------------------
// Run statistics
// ---------------------------------------------------------------------------

/// Accumulated run statistics, aggregated by the scheduler from each node's
/// [`NodeUsage`](crate::graph::node::NodeUsage) report and checked against a
/// [`Budget`] each cycle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStats {
    /// Total cost accumulated so far (best-effort; `0.0` if no pricing is
    /// configured — see [`ProviderConfig::pricing`](crate::config::ProviderConfig::pricing)).
    pub total_cost_usd: f64,
    /// Total tokens consumed so far (input + output, or the provider's
    /// aggregate when it does not split them).
    pub total_tokens: u64,
    /// Total prompt/input tokens consumed so far.
    pub total_input_tokens: u64,
    /// Total completion/output tokens consumed so far.
    pub total_output_tokens: u64,
    /// Wall-clock seconds elapsed.
    pub elapsed_secs: f64,
    /// Rounds completed.
    pub rounds_completed: u32,
}

impl RunStats {
    /// Check whether any budget limit has been exceeded.
    ///
    /// This is a hard backstop. The graph's governor node is the primary
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
        if self.elapsed_secs >= budget.max_wallclock {
            return Some(format!(
                "Wall-clock budget exhausted: {:.0}s >= {:.0}s",
                self.elapsed_secs, budget.max_wallclock
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
        let budget = Budget { max_cost_usd: 10.0, ..Budget::default() };
        let stats = RunStats { total_cost_usd: 15.0, ..RunStats::default() };
        assert!(stats.is_budget_exhausted(&budget).is_some());
    }

    #[test]
    fn test_budget_exhaustion_tokens() {
        let budget = Budget { max_tokens: 1000, ..Budget::default() };
        let stats = RunStats { total_tokens: 1500, ..RunStats::default() };
        assert!(stats.is_budget_exhausted(&budget).is_some());
    }

    #[test]
    fn test_budget_not_exhausted() {
        let budget = Budget::default();
        let stats = RunStats::default();
        assert!(stats.is_budget_exhausted(&budget).is_none());
    }
}
