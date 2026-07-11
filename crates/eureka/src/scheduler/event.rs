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
        /// Serialized outputs: `[{ "port": "out", "kind": "Hypotheses", "data": {...} }]`.
        #[serde(default)]
        outputs: Vec<serde_json::Value>,
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

/// Signals sent to the scheduler from the outside.
#[derive(Debug)]
pub enum SchedulerSignal {
    /// Cancel the run gracefully.
    Cancel,
    /// Pause processing.
    Pause,
}
