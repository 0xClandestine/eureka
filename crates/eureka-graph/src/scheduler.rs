//! Scheduler — walks the graph, activates nodes when inputs arrive,
//! enforces budget and cycles, and manages the run lifecycle.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{info, span, warn, Level};

use crate::artifact::Artifact;
use crate::control::{Budget, RunStats};
use crate::edge::Edge;
use crate::node::{BoxedNode, Emit, NodeCtx, PortMsg};
use crate::spec::GraphSpec;

/// A pending activation ready to be dispatched to a node.
#[derive(Debug)]
struct Activation {
    /// The node ID to activate.
    node_id: String,
    /// The context for this activation.
    ctx: NodeCtx,
    /// The input message.
    msg: PortMsg,
}

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
    /// The run was halted.
    RunHalted {
        /// The reason the run was halted.
        reason: String,
        /// The total number of rounds executed before halting.
        total_rounds: u32,
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

/// The event-driven scheduler that walks the graph.
pub struct Scheduler {
    /// The graph specification being executed.
    spec: GraphSpec,
    /// Node registry: maps node ID to boxed node.
    nodes: HashMap<String, BoxedNode>,
    /// Budget for the run.
    budget: Budget,
    /// Current run statistics.
    stats: RunStats,
    /// Semaphore to cap in-flight LLM calls.
    in_flight_sem: Arc<Semaphore>,
    /// Cancellation token for graceful shutdown.
    cancel: CancellationToken,
    /// Sender for scheduler signals (exposed to callers via `signal_sender()`).
    signal_tx: mpsc::Sender<SchedulerSignal>,
    /// Receiver for scheduler signals.
    signal_rx: Option<mpsc::Receiver<SchedulerSignal>>,
    /// Channel to broadcast scheduler events.
    event_tx: mpsc::Sender<SchedulerEvent>,
    /// Event receiver for external consumers.
    event_rx: Option<mpsc::Receiver<SchedulerEvent>>,
}

impl Scheduler {
    /// Create a new scheduler with the given configuration.
    #[must_use]
    pub fn new(
        spec: GraphSpec,
        nodes: HashMap<String, BoxedNode>,
        budget: Budget,
        max_in_flight: usize,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(1024);
        let (signal_tx, signal_rx) = mpsc::channel(16);

        Self {
            spec,
            nodes,
            budget,
            stats: RunStats::default(),
            in_flight_sem: Arc::new(Semaphore::new(max_in_flight)),
            cancel: CancellationToken::new(),
            signal_tx,
            signal_rx: Some(signal_rx),
            event_tx,
            event_rx: Some(event_rx),
        }
    }

    /// Get a sender for scheduler signals.
    #[must_use]
    pub fn signal_sender(&self) -> mpsc::Sender<SchedulerSignal> {
        self.signal_tx.clone()
    }

    /// Get a receiver for scheduler events.
    #[must_use]
    pub fn event_receiver(&mut self) -> mpsc::Receiver<SchedulerEvent> {
        self.event_rx.take().unwrap_or_else(|| {
            let (tx, rx) = mpsc::channel(1024);
            // Drop the old tx — events will go nowhere if replaced
            drop(tx);
            rx
        })
    }

    /// Get a cancellation token for this run.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Run the graph starting from the given initial artifacts.
    ///
    /// The scheduler:
    /// 1. Injects initial artifacts onto source nodes' input ports.
    /// 2. Enters the activation loop: waits for ready activations and dispatches them.
    /// 3. Routes emitted artifacts along edges.
    /// 4. Enforces budget and stop conditions.
    /// 5. Returns the final run stats on completion.
    ///
    /// # Errors
    ///
    /// Returns a `SchedulerError` if a referenced node is not found in the registry
    /// or an internal channel error occurs.
    pub async fn run(
        &mut self,
        initial_artifacts: HashMap<String, Vec<Artifact>>,
    ) -> Result<RunStats, SchedulerError> {
        let start = std::time::Instant::now();

        // Build outbound routing table
        let outbound: HashMap<String, Vec<Edge>> = {
            let mut map: HashMap<String, Vec<Edge>> = HashMap::new();
            for edge in &self.spec.edges {
                map.entry(edge.from_node.clone())
                    .or_default()
                    .push(edge.clone());
            }
            map
        };

        // Channel for ready activations. `pending` tracks enqueued + in-flight
        // work so we can detect quiescence without dropping the sender.
        let (activation_tx, mut activation_rx) = mpsc::channel::<Activation>(256);
        let mut pending: usize = 0;

        // Inject initial artifacts onto source nodes
        for (source_id, artifacts) in &initial_artifacts {
            if let Some(node) = self.nodes.get(source_id) {
                let ports = node.ports();
                for artifact in artifacts {
                    let span = span!(Level::INFO, "inject", node = %source_id);
                    let _guard = span.enter();

                    // Determine which input port to use
                    let port_name = if let Some(input_name) = ports.input_names().first() {
                        input_name.clone()
                    } else {
                        continue;
                    };

                    let mut ctx = NodeCtx::new(
                        source_id.clone(),
                        self.spec.node_kind(source_id).unwrap_or("unknown"),
                        self.stats.rounds_completed,
                        self.cancel.clone(),
                    );
                    ctx.event_tx = Some(self.event_tx.clone());

                    let msg = PortMsg {
                        port: port_name,
                        artifact: artifact.clone(),
                    };

                    let act = Activation {
                        node_id: source_id.clone(),
                        ctx,
                        msg,
                    };

                    if activation_tx.send(act).await.is_err() {
                        return Err(SchedulerError::Internal("Activation channel closed".into()));
                    }
                    pending += 1;
                }
            }
        }

        // Budget ticker — fires every 100ms independently of activation throughput.
        // Using an interval (not sleep-in-select) so the timer is not reset on
        // every loop iteration; it fires even when activations complete instantly.
        let mut budget_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        budget_ticker.tick().await; // consume the immediate first tick

        // Main activation loop — exits when pending == 0 (quiescent).
        loop {
            if pending == 0 {
                break;
            }

            tokio::select! {
                // Check for cancellation
                () = self.cancel.cancelled() => {
                    info!("Run cancelled");
                    self.stats.elapsed_secs = start.elapsed().as_secs_f64();
                    return Ok(self.stats.clone());
                }

                // Check for external signals
                maybe_signal = async {
                    if let Some(rx) = &mut self.signal_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match maybe_signal {
                        Some(SchedulerSignal::Cancel) => {
                            info!("Run cancelled via signal");
                            self.cancel.cancel();
                        }
                        Some(SchedulerSignal::Pause) => {
                            info!("Run paused");
                            // In a full implementation, we'd block here until resume
                        }
                        None => {}
                    }
                }

                // Process ready activations
                maybe_act = activation_rx.recv() => {
                    match maybe_act {
                        Some(activation) => {
                            pending -= 1;

                            let node_id = activation.node_id.clone();
                            let node_kind = activation.ctx.node_kind.clone();
                            let round = activation.ctx.round;

                            // Emit event
                            let _ = self.event_tx.send(SchedulerEvent::ActivationStarted {
                                node_id: node_id.clone(),
                                node_kind: node_kind.clone(),
                                round,
                            }).await;

                            // Acquire a semaphore permit (limiting concurrency)
                            let permit = self.in_flight_sem.clone().acquire_owned().await
                                .map_err(|_| SchedulerError::Internal("Semaphore closed".into()))?;

                            let node = self.nodes.get(&node_id).ok_or_else(|| {
                                SchedulerError::NodeNotFound(node_id.clone())
                            })?;

                            let ctx = activation.ctx;
                            let msg = activation.msg;

                            // Process the node (this is where the LLM call happens)
                            let result = node.process(&ctx, msg).await;

                            // Release permit
                            drop(permit);

                            match result {
                                Ok(emits) => {
                                    let outputs: Vec<serde_json::Value> = emits
                                        .iter()
                                        .map(|e| serde_json::json!({
                                            "port": e.port,
                                            "kind": e.artifact.kind.to_string(),
                                            "data": e.artifact.data,
                                        }))
                                        .collect();
                                    let _ = self.event_tx.send(SchedulerEvent::ActivationCompleted {
                                        node_id: node_id.clone(),
                                        node_kind: node_kind.clone(),
                                        round,
                                        emit_count: emits.len(),
                                        outputs,
                                    }).await;

                                    // Route emissions along edges
                                    for emit in emits {
                                        let (new_count, crossed_cycle) = self.route_emission(
                                            &node_id,
                                            &emit,
                                            &outbound,
                                            &activation_tx,
                                        ).await?;
                                        pending += new_count;
                                        if crossed_cycle {
                                            self.stats.rounds_completed += 1;
                                            let _ = self.event_tx.send(SchedulerEvent::CycleCompleted {
                                                round: self.stats.rounds_completed,
                                            }).await;
                                        }
                                    }
                                }
                                Err(err) => {
                                    warn!(%node_id, error = %err, "Node activation failed");
                                    let _ = self.event_tx.send(SchedulerEvent::ActivationFailed {
                                        node_id,
                                        node_kind,
                                        round,
                                        error: err.to_string(),
                                    }).await;
                                    // Degrade gracefully — don't abort the run
                                }
                            }
                        }
                        None => {
                            // Channel closed — shouldn't happen since activation_tx is alive
                            break;
                        }
                    }
                }

                // Budget check fires every 100ms regardless of activation rate.
                _ = budget_ticker.tick() => {
                    self.stats.elapsed_secs = start.elapsed().as_secs_f64();
                    if let Some(reason) = self.stats.is_budget_exhausted(&self.budget) {
                        info!(%reason, "Budget exhausted, halting run");
                        let _ = self.event_tx.send(SchedulerEvent::RunHalted {
                            reason: reason.clone(),
                            total_rounds: self.stats.rounds_completed,
                        }).await;
                        self.cancel.cancel();
                    }
                }
            }
        }

        self.stats.elapsed_secs = start.elapsed().as_secs_f64();
        info!(
            rounds = self.stats.rounds_completed,
            elapsed_secs = self.stats.elapsed_secs,
            "Run completed"
        );

        Ok(self.stats.clone())
    }

    /// Route a single emission along its outbound edges.
    ///
    /// Returns `(enqueued_count, crossed_feedback_edge)`.
    async fn route_emission(
        &self,
        from_node_id: &str,
        emit: &Emit,
        outbound: &HashMap<String, Vec<Edge>>,
        activation_tx: &mpsc::Sender<Activation>,
    ) -> Result<(usize, bool), SchedulerError> {
        let edges = match outbound.get(from_node_id) {
            Some(edges) => edges,
            None => return Ok((0, false)), // No outbound edges — terminal emission
        };

        let mut enqueued = 0usize;
        let mut crossed_feedback = false;

        for edge in edges {
            // Check if the port matches
            if edge.from_port != emit.port {
                continue;
            }

            let target_id = &edge.to_node;
            let target_port = &edge.to_port;

            if edge.feedback {
                crossed_feedback = true;
            }

            let target_kind = self
                .spec
                .node_kind(target_id)
                .unwrap_or("unknown")
                .to_string();

            let mut ctx = NodeCtx::new(
                target_id.clone(),
                target_kind,
                self.stats.rounds_completed,
                self.cancel.clone(),
            );
            ctx.event_tx = Some(self.event_tx.clone());

            let msg = PortMsg {
                port: target_port.clone(),
                artifact: emit.artifact.clone(),
            };

            let act = Activation {
                node_id: target_id.clone(),
                ctx,
                msg,
            };

            if activation_tx.send(act).await.is_err() {
                return Err(SchedulerError::Internal("Activation channel closed".into()));
            }
            enqueued += 1;
        }

        Ok((enqueued, crossed_feedback))
    }
}

/// Errors that can occur during scheduling.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// A node was not found in the registry.
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    /// An internal scheduler error occurred.
    #[error("Internal scheduler error: {0}")]
    Internal(String),

    /// The run was cancelled.
    #[error("Run cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Node;
    use crate::node::NodeError;
    use crate::port::{PortDirection, PortSpec, PortSpecEntry};
    use crate::spec::GraphNodeSpec;
    use async_trait::async_trait;

    struct TestNode {
        ports: PortSpec,
        output: Option<Artifact>,
    }

    #[async_trait]
    impl Node for TestNode {
        fn ports(&self) -> PortSpec {
            self.ports.clone()
        }

        async fn process(&self, _ctx: &NodeCtx, _msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
            if let Some(artifact) = &self.output {
                Ok(vec![Emit::new("out", artifact.clone())])
            } else {
                Ok(vec![])
            }
        }
    }

    #[tokio::test]
    async fn test_scheduler_creation() {
        let spec = GraphSpec {
            name: Some("test".into()),
            description: None,
            nodes: vec![GraphNodeSpec {
                id: "source".into(),
                kind: "test.node".into(),
                config: serde_json::Value::Null,
                description: None,
            }],
            edges: vec![],
            metadata: serde_json::Value::Null,
        };

        let mut nodes = HashMap::new();
        nodes.insert(
            "source".into(),
            BoxedNode::new(TestNode {
                ports: PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".to_string(),
                        required: true,
                    }],
                    vec![],
                ),
                output: None,
            }),
        );

        let scheduler = Scheduler::new(spec, nodes, Budget::default(), 4);
        let cancel = scheduler.cancellation_token();
        assert!(!cancel.is_cancelled());
    }

    #[tokio::test]
    async fn test_signal_sender_is_usable() {
        let spec = GraphSpec {
            name: None,
            description: None,
            nodes: vec![],
            edges: vec![],
            metadata: serde_json::Value::Null,
        };
        let scheduler = Scheduler::new(spec, HashMap::new(), Budget::default(), 4);
        let tx = scheduler.signal_sender();
        // Sending to the channel should not fail (receiver is still alive)
        assert!(tx.try_send(SchedulerSignal::Cancel).is_ok());
    }

    #[tokio::test]
    async fn test_run_terminates_with_no_work() {
        let spec = GraphSpec {
            name: None,
            description: None,
            nodes: vec![],
            edges: vec![],
            metadata: serde_json::Value::Null,
        };
        let mut scheduler = Scheduler::new(spec, HashMap::new(), Budget::default(), 4);
        // No initial artifacts → pending = 0 → should terminate immediately
        let stats = scheduler.run(HashMap::new()).await.unwrap();
        assert_eq!(stats.rounds_completed, 0);
    }
}
