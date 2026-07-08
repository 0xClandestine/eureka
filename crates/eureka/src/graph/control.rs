//! Control nodes — the pieces that make arbitrary topologies safe and expressive.
//!
//! These are built-in node kinds that handle orchestration, lifecycle, and
//! flow control, not LLM-based scientific work.
//!
//! Runtime accounting types ([`Budget`], [`RunStats`], [`ControlSignal`])
//! used to live here but now live in [`crate::config`], so the `graph` module
//! stays free of configuration concerns and its dependency direction remains
//! one-way (everything depends on `graph`; `graph` depends on nothing).

#[cfg(test)]
mod tests {
    use crate::config::{Budget, RunStats};

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
    fn test_budget_exhaustion_tokens() {
        let budget = Budget {
            max_tokens: 1000,
            ..Budget::default()
        };
        let stats = RunStats {
            total_tokens: 1500,
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
