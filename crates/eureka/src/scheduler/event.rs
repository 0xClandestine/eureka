/// Events emitted by the scheduler for observability.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SchedulerEvent {
    /// A node activation started.
    ActivationStarted {
        /// The ID of the node being activated.
        node_id: String,
        /// The kind (type) of the node being activated.
        node_kind: String,
        /// The scheduler round in which this activation started.
        round: u32,
    },
    /// A node activation completed.
    ActivationCompleted {
        /// The ID of the node that completed activation.
        node_id: String,
        /// The kind (type) of the node that completed activation.
        node_kind: String,
        /// The scheduler round in which this activation completed.
        round: u32,
        /// The number of messages emitted by this activation.
        emit_count: usize,
        /// Serialized inputs: `[{ "port": "in", "kind": "Goal", "data": {...} }]`.
        #[serde(default)]
        inputs: Vec<serde_json::Value>,
        /// Serialized outputs: `[{ "port": "out", "kind": "Hypotheses", "data": {...} }]`.
        #[serde(default)]
        outputs: Vec<serde_json::Value>,
        /// Resource usage for this activation (tokens, cost).
        #[serde(default)]
        usage: crate::graph::node::NodeUsage,
    },
    /// A node activation was retried after a transient failure.
    ActivationRetried {
        /// The ID of the node being retried.
        node_id: String,
        /// The kind (type) of the node being retried.
        node_kind: String,
        /// The scheduler round of the activation.
        round: u32,
        /// The attempt number (1 = first retry, 2 = second retry, …).
        attempt: u32,
        /// The error that triggered the retry.
        error: String,
    },
    /// A node activation failed.
    ActivationFailed {
        /// The ID of the node whose activation failed.
        node_id: String,
        /// The kind (type) of the node whose activation failed.
        node_kind: String,
        /// The scheduler round in which the failure occurred.
        round: u32,
        /// A description of the error that caused the failure.
        error: String,
    },
    /// A cycle was completed.
    CycleCompleted {
        /// The round number of the completed cycle.
        round: u32,
    },
    /// An agent node called a tool (e.g. a literature search or paper fetch).
    ToolCalled {
        /// The ID of the node that invoked the tool.
        node_id: String,
        /// The kind of the node.
        node_kind: String,
        /// The scheduler round.
        round: u32,
        /// The name of the tool that was called.
        tool: String,
        /// A human-readable one-line summary of the arguments.
        args_summary: String,
        /// Full argument object passed to the tool.
        #[serde(default)]
        args: serde_json::Value,
    },
    /// A tool call completed with a result.
    ToolCompleted {
        /// The ID of the node that invoked the tool.
        node_id: String,
        /// The kind of the node.
        node_kind: String,
        /// The scheduler round.
        round: u32,
        /// The name of the tool that completed.
        tool: String,
        /// First 500 characters of the tool output.
        result_preview: String,
        /// Whether the subprocess exited successfully.
        success: bool,
    },
    /// The run was halted by a budget or graph control decision.
    RunHalted {
        /// The reason the run was halted.
        reason: String,
        /// The total number of rounds executed before halting.
        total_rounds: u32,
    },
    /// The run paused after an external pause signal.
    RunPaused {
        /// The round at which the run paused.
        round: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the type tag uses camelCase (from `rename_all` on the enum),
    /// while fields within struct variants keep their original snake_case names.
    #[test]
    fn tool_called_type_tag_is_camel_case_fields_are_snake_case() {
        let ev = SchedulerEvent::ToolCalled {
            node_id: "plan".into(),
            node_kind: "plan".into(),
            round: 0,
            tool: "search_literature".into(),
            args_summary: "R1CS SHA-256".into(),
            args: serde_json::json!({"query": "R1CS SHA-256"}),
        };
        let v = serde_json::to_value(&ev).unwrap();
        // Variant name is renamed to camelCase
        assert_eq!(v["type"], "toolCalled");
        // Fields within struct variants keep snake_case
        assert_eq!(v["node_id"], "plan");
        assert_eq!(v["tool"], "search_literature");
        assert_eq!(v["args_summary"], "R1CS SHA-256");
    }

    #[test]
    fn tool_completed_has_result_preview_and_success() {
        let ev = SchedulerEvent::ToolCompleted {
            node_id: "plan".into(),
            node_kind: "plan".into(),
            round: 0,
            tool: "search_literature".into(),
            result_preview: r#"{"papers":[{"title":"Test"}]}"#.into(),
            success: true,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "toolCompleted");
        assert_eq!(v["result_preview"], r#"{"papers":[{"title":"Test"}]}"#);
        assert_eq!(v["success"], true);
    }

    #[test]
    fn activation_started_type_tag_is_camel_case() {
        let ev = SchedulerEvent::ActivationStarted {
            node_id: "generation".into(),
            node_kind: "generation".into(),
            round: 1,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "activationStarted");
        assert_eq!(v["node_id"], "generation");
        assert_eq!(v["round"], 1);
    }

    #[test]
    fn activation_completed_usage_is_snake_case() {
        let ev = SchedulerEvent::ActivationCompleted {
            node_id: "reflection".into(),
            node_kind: "reflection".into(),
            round: 2,
            emit_count: 1,
            inputs: vec![],
            outputs: vec![],
            usage: crate::graph::node::NodeUsage {
                total_tokens: 1000,
                input_tokens: 600,
                output_tokens: 400,
                cost_usd: 0.001,
            },
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "activationCompleted");
        assert_eq!(v["usage"]["total_tokens"], 1000);
        assert_eq!(v["usage"]["cost_usd"], 0.001);
        assert_eq!(v["emit_count"], 1);
    }

    #[test]
    fn run_halted_type_tag_is_camel_case() {
        let ev = SchedulerEvent::RunHalted { reason: "max_rounds".into(), total_rounds: 5 };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "runHalted");
        assert_eq!(v["total_rounds"], 5);
    }
}

/// Signals sent to the scheduler from the outside.
#[derive(Debug)]
pub enum SchedulerSignal {
    /// Cancel the run gracefully.
    Cancel,
    /// Pause processing at the next scheduler boundary.
    Pause,
    /// Resume processing after a pause.
    Resume,
}
