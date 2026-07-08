use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, span, warn, Level};

use crate::config::Budget;
use crate::graph::artifact::Artifact;
use crate::graph::control::RunStats;
use crate::graph::edge::Edge;
use crate::graph::node::{BoxedNode, Emit, NodeCtx, NodeError, NodeUsage, PortMsg};
use crate::graph::spec::GraphSpec;

use super::error::SchedulerError;
use super::event::{SchedulerEvent, SchedulerSignal};

/// A ready activation: a node plus its joined inputs, about to run.
#[derive(Debug)]
struct Activation {
    /// The node ID to activate.
    node_id: String,
    /// The kind of the node being activated.
    node_kind: String,
    /// The round this activation belongs to.
    round: u32,
    /// The context for this activation.
    ctx: NodeCtx,
    /// The joined input messages (one per populated input port).
    inputs: Vec<PortMsg>,
}

/// The result of running an activation, sent back from a worker task to the
/// main scheduler loop.
struct ActivationResult {
    /// The node ID that ran.
    node_id: String,
    /// The kind of the node that ran.
    node_kind: String,
    /// The round this activation belonged to.
    round: u32,
    /// The outcome: emitted artifacts plus a usage report, or an error.
    result: Result<(Vec<Emit>, NodeUsage), NodeError>,
}

/// Per-task handles cloned into every spawned activation so worker tasks can
/// acquire permits, emit events, and report results without holding a borrow
/// on the scheduler.
#[derive(Clone)]
struct TaskHandles {
    /// Channel back to the main loop carrying activation results.
    results_tx: mpsc::UnboundedSender<ActivationResult>,
    /// Channel for scheduler observability events.
    event_tx: mpsc::Sender<SchedulerEvent>,
    /// Semaphore capping concurrent in-flight `process` calls.
    in_flight_sem: Arc<Semaphore>,
    /// Cancellation token for graceful shutdown.
    cancel: CancellationToken,
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
    /// # Errors
    ///
    /// Returns a `SchedulerError` if a referenced node is not found or an
    /// internal channel error occurs.
    #[allow(clippy::too_many_lines)]
    pub async fn run(
        &mut self,
        initial_artifacts: HashMap<String, Vec<Artifact>>,
    ) -> Result<RunStats, SchedulerError> {
        let start = std::time::Instant::now();

        let outbound: HashMap<String, Vec<Edge>> = {
            let mut map: HashMap<String, Vec<Edge>> = HashMap::new();
            for edge in &self.spec.edges {
                map.entry(edge.from_node.clone())
                    .or_default()
                    .push(edge.clone());
            }
            map
        };

        // Results channel: worker tasks send their outcome back here. Unbounded
        // so a large fan-out never blocks the workers. The JoinSet owns task
        // lifecycles and aborts in-flight tasks when dropped (e.g. on cancel).
        let (results_tx, mut results_rx) = mpsc::unbounded_channel::<ActivationResult>();
        let mut tasks: JoinSet<()> = JoinSet::new();
        let handles = TaskHandles {
            results_tx,
            event_tx: self.event_tx.clone(),
            in_flight_sem: Arc::clone(&self.in_flight_sem),
            cancel: self.cancel.clone(),
        };
        let mut pending: usize = 0;

        // Per-node input buffer keyed by (node_id, round). The scheduler joins
        // inputs: a node fires once when all its required input ports are
        // present for a given round. Single-input nodes fire immediately on
        // arrival, exactly as before.
        let mut input_buffer: HashMap<(String, u32), HashMap<String, Artifact>> = HashMap::new();

        for (source_id, artifacts) in &initial_artifacts {
            if self.nodes.get(source_id).is_some() {
                for artifact in artifacts {
                    let span = span!(Level::INFO, "inject", node = %source_id);
                    let _guard = span.enter();
                    // Delivering the goal artifact into the source node's first
                    // input port; this either fires immediately (single-input
                    // node) or buffers until the remaining required inputs arrive.
                    pending += self.deliver_input(
                        source_id,
                        // The first declared input port receives the goal.
                        self.nodes[source_id]
                            .ports()
                            .input_names()
                            .first()
                            .cloned()
                            .unwrap_or_default()
                            .as_str(),
                        artifact.clone(),
                        self.stats.rounds_completed,
                        &mut input_buffer,
                        &handles,
                        &mut tasks,
                    )?;
                }
            }
        }

        let mut budget_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        budget_ticker.tick().await;

        loop {
            if pending == 0 {
                break;
            }

            tokio::select! {
                () = self.cancel.cancelled() => {
                    info!("Run cancelled");
                    tasks.abort_all();
                    self.stats.elapsed_secs = start.elapsed().as_secs_f64();
                    return Ok(self.stats.clone());
                }

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
                        }
                        None => {}
                    }
                }

                maybe_result = results_rx.recv() => {
                    match maybe_result {
                        Some(result) => {
                            pending -= 1;
                            let node_id = result.node_id.clone();
                            let node_kind = result.node_kind.clone();
                            let round = result.round;

                            match result.result {
                                Ok((emits, usage)) => {
                                    // Aggregate resource usage into the run stats
                                    // so the cost/token budget backstops fire.
                                    self.stats.total_input_tokens += usage.input_tokens;
                                    self.stats.total_output_tokens += usage.output_tokens;
                                    self.stats.total_tokens += usage.total_tokens;
                                    self.stats.total_cost_usd += usage.cost_usd;

                                    let outputs: Vec<serde_json::Value> = emits
                                        .iter()
                                        .map(|e| serde_json::json!({
                                            "port": e.port,
                                            "kind": e.artifact.kind.clone(),
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

                                    for emit in emits {
                                        let (new_count, crossed_cycle) = self.route_emission(
                                            &node_id,
                                            &emit,
                                            &outbound,
                                            self.stats.rounds_completed,
                                            &mut input_buffer,
                                            &handles,
                                            &mut tasks,
                                        )?;
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
                                }
                            }
                        }
                        None => {
                            // results_tx dropped: all worker handles gone. The
                            // remaining `pending` work can never complete.
                            break;
                        }
                    }
                }

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

    /// Route a single emission along its outbound edges, buffering each
    /// delivered input and dispatching an activation when a target node's
    /// required inputs are all present.
    ///
    /// Returns `(enqueued_count, crossed_feedback_edge)`.
    fn route_emission(
        &self,
        from_node_id: &str,
        emit: &Emit,
        outbound: &HashMap<String, Vec<Edge>>,
        round: u32,
        input_buffer: &mut HashMap<(String, u32), HashMap<String, Artifact>>,
        handles: &TaskHandles,
        tasks: &mut JoinSet<()>,
    ) -> Result<(usize, bool), SchedulerError> {
        let Some(edges) = outbound.get(from_node_id) else {
            return Ok((0, false));
        };

        let mut enqueued = 0usize;
        let mut crossed_feedback = false;

        for edge in edges {
            if edge.from_port != emit.port {
                continue;
            }

            if edge.feedback {
                crossed_feedback = true;
            }

            // The round attributed to a feedback input is the *next* round,
            // since it crossed a cycle boundary.
            let target_round = if edge.feedback { round + 1 } else { round };

            enqueued += self.deliver_input(
                &edge.to_node,
                &edge.to_port,
                emit.artifact.clone(),
                target_round,
                input_buffer,
                handles,
                tasks,
            )?;
        }

        Ok((enqueued, crossed_feedback))
    }

    /// Buffer a single input for `(node_id, round)` and, if the node now has
    /// all required inputs present, drain the buffer into an `Activation` and
    /// spawn it as a worker task. Returns the number of activations dispatched
    /// (0 or 1).
    fn deliver_input(
        &self,
        node_id: &str,
        port: &str,
        artifact: Artifact,
        round: u32,
        input_buffer: &mut HashMap<(String, u32), HashMap<String, Artifact>>,
        handles: &TaskHandles,
        tasks: &mut JoinSet<()>,
    ) -> Result<usize, SchedulerError> {
        let bucket = input_buffer.entry((node_id.to_string(), round)).or_default();
        bucket.insert(port.to_string(), artifact);

        // Determine readiness: are all required input ports present?
        let Some(node) = self.nodes.get(node_id) else {
            return Err(SchedulerError::NodeNotFound(node_id.to_string()));
        };
        let ports = node.ports();
        let required: Vec<String> = ports.required_inputs();

        let ready = !required.is_empty()
            && required
                .iter()
                .all(|p| bucket.contains_key(p));

        if !ready {
            return Ok(0);
        }

        // Drain the buffer for this (node, round) into a joined input vec.
        let bucket = input_buffer
            .remove(&(node_id.to_string(), round))
            .unwrap_or_default();
        let mut inputs: Vec<PortMsg> = Vec::with_capacity(ports.inputs.len());
        // Emit required inputs first, then any optional inputs that arrived.
        for p in &ports.inputs {
            if let Some(art) = bucket.get(&p.name) {
                inputs.push(PortMsg {
                    port: p.name.clone(),
                    artifact: art.clone(),
                });
            }
        }

        let mut ctx = NodeCtx::new(
            node_id.to_string(),
            self.spec.node_kind(node_id).unwrap_or("unknown"),
            round,
            self.cancel.clone(),
        );
        ctx.event_tx = Some(self.event_tx.clone());

        let activation = Activation {
            node_id: node_id.to_string(),
            node_kind: ctx.node_kind.clone(),
            round,
            ctx,
            inputs,
        };
        spawn_activation(activation, node.clone(), handles, tasks);
        Ok(1)
    }
}

/// Spawn a single activation as a worker task. The task emits an
/// `ActivationStarted` event, acquires an in-flight permit (cancelling
/// promptly if the run is cancelled), runs the node, and sends the result
/// back to the main loop via `handles.results_tx`. Aborting the JoinSet
/// (on cancel or drop) cancels in-flight `process` calls.
fn spawn_activation(
    activation: Activation,
    node: BoxedNode,
    handles: &TaskHandles,
    tasks: &mut JoinSet<()>,
) {
        let node_id = activation.node_id.clone();
        let node_kind = activation.node_kind.clone();
        let round = activation.round;
        let event_tx = handles.event_tx.clone();
        let results_tx = handles.results_tx.clone();
        let sem = Arc::clone(&handles.in_flight_sem);
        let cancel = handles.cancel.clone();
        let ctx = activation.ctx;
        let inputs = activation.inputs;

        tasks.spawn(async move {
            let _ = event_tx
                .send(SchedulerEvent::ActivationStarted {
                    node_id: node_id.clone(),
                    node_kind: node_kind.clone(),
                    round,
                })
                .await;

            // Acquire a permit, bailing out promptly if cancelled while
            // waiting for a free slot.
            let permit = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                p = sem.acquire_owned() => match p {
                    Ok(p) => p,
                    Err(_) => return,
                }
            };

            let result = node.process(&ctx, inputs).await;
            drop(permit);

            let _ = results_tx.send(ActivationResult {
                node_id,
                node_kind,
                round,
                result,
            });
        });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use async_trait::async_trait;

    use crate::config::Budget;
    use crate::graph::artifact::Artifact;
    use crate::graph::node::{BoxedNode, Emit, Node, NodeCtx, NodeError, PortMsg};
    use crate::graph::port::{PortDirection, PortSpec, PortSpecEntry};
    use crate::graph::spec::{GraphNodeSpec, GraphSpec};

    use super::*;

    struct TestNode {
        ports: PortSpec,
        output: Option<Artifact>,
    }

    #[async_trait]
    impl Node for TestNode {
        fn ports(&self) -> PortSpec {
            self.ports.clone()
        }

        async fn process(
            &self,
            _ctx: &NodeCtx,
            _inputs: Vec<PortMsg>,
        ) -> Result<(Vec<Emit>, crate::graph::node::NodeUsage), NodeError> {
            let emits = if let Some(artifact) = &self.output {
                vec![Emit::new("out", artifact.clone())]
            } else {
                vec![]
            };
            Ok((emits, crate::graph::node::NodeUsage::default()))
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
        let stats = scheduler.run(HashMap::new()).await.unwrap();
        assert_eq!(stats.rounds_completed, 0);
    }

    #[tokio::test]
    async fn test_run_handles_large_fanout_without_deadlock() {
        // Regression (H3): a single source routing >256 new activations must
        // not block the single consumer loop on a bounded activation channel.
        const N: usize = 300;
        let goal = Artifact {
            kind: "Goal".to_string(),
            data: serde_json::json!({}),
        };
        let source = BoxedNode::new(TestNode {
            ports: PortSpec::new(
                vec![PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Goal".to_string(),
                    required: true,
                }],
                vec![PortSpecEntry {
                    name: "out".into(),
                    direction: PortDirection::Output,
                    kind: "Goal".to_string(),
                    required: false,
                }],
            ),
            output: Some(goal.clone()),
        });
        let sink = BoxedNode::new(TestNode {
            ports: PortSpec::new(
                vec![PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Goal".to_string(),
                    required: false,
                }],
                vec![],
            ),
            output: None,
        });

        let mut nodes = HashMap::new();
        nodes.insert("src".to_string(), source);
        for i in 0..N {
            let id = format!("sink{i}");
            nodes.insert(id, sink.clone());
        }

        let mut node_specs = vec![GraphNodeSpec {
            id: "src".into(),
            kind: "test.node".into(),
            config: serde_json::Value::Null,
            description: None,
        }];
        for i in 0..N {
            node_specs.push(GraphNodeSpec {
                id: format!("sink{i}"),
                kind: "test.node".into(),
                config: serde_json::Value::Null,
                description: None,
            });
        }
        let mut edges = vec![Edge::new("src", "out", "sink0", "in")];
        for i in 0..N {
            edges.push(Edge::new("src", "out", &format!("sink{i}"), "in"));
        }

        let spec = GraphSpec {
            name: Some("fanout".into()),
            description: None,
            nodes: node_specs,
            edges,
            metadata: serde_json::Value::Null,
        };
        let mut scheduler = Scheduler::new(spec, nodes, Budget::default(), 4);
        // Inject a goal into the source. If the activation channel were
        // bounded at 256, this would deadlock and time out.
        let initial = HashMap::from([("src".to_string(), vec![goal])]);
        let stats = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            scheduler.run(initial),
        )
        .await
        .expect("run must not deadlock on large fan-out")
        .unwrap();
        assert_eq!(stats.rounds_completed, 0);
    }

    #[tokio::test]
    async fn test_multi_input_node_receives_joined_inputs() {
        // Regression (A): a node with multiple required inputs must receive
        // ALL of them in a single process() call, not one call per input.
        use std::sync::Mutex;

        struct JoiningNode {
            received: Arc<Mutex<Vec<Vec<String>>>>,
            ports: PortSpec,
        }

        #[async_trait]
        impl Node for JoiningNode {
            fn ports(&self) -> PortSpec {
                self.ports.clone()
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, crate::graph::node::NodeUsage), NodeError> {
                let ports: Vec<String> = inputs.iter().map(|m| m.port.clone()).collect();
                self.received.lock().unwrap().push(ports);
                Ok((vec![], crate::graph::node::NodeUsage::default()))
            }
        }

        let received: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let join_ports = PortSpec::new(
            vec![
                PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Goal".to_string(),
                    required: true,
                },
                PortSpecEntry {
                    name: "context".into(),
                    direction: PortDirection::Input,
                    kind: "Insights".to_string(),
                    required: true,
                },
            ],
            vec![],
        );

        let mut nodes = HashMap::new();
        nodes.insert(
            "gen".to_string(),
            BoxedNode::new(JoiningNode {
                received: Arc::clone(&received),
                ports: join_ports,
            }),
        );
        nodes.insert(
            "goal_src".to_string(),
            BoxedNode::new(TestNode {
                ports: PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".to_string(),
                        required: true,
                    }],
                    vec![PortSpecEntry {
                        name: "out".into(),
                        direction: PortDirection::Output,
                        kind: "Goal".to_string(),
                        required: false,
                    }],
                ),
                output: Some(Artifact {
                    kind: "Goal".to_string(),
                    data: serde_json::json!({ "goal": "x" }),
                }),
            }),
        );
        nodes.insert(
            "ctx_src".to_string(),
            BoxedNode::new(TestNode {
                ports: PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".to_string(),
                        required: true,
                    }],
                    vec![PortSpecEntry {
                        name: "out".into(),
                        direction: PortDirection::Output,
                        kind: "Insights".to_string(),
                        required: false,
                    }],
                ),
                output: Some(Artifact {
                    kind: "Insights".to_string(),
                    data: serde_json::json!({ "insights": [] }),
                }),
            }),
        );

        let spec = GraphSpec {
            name: Some("join-test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "goal_src".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "ctx_src".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "gen".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![
                Edge::new("goal_src", "out", "gen", "in"),
                Edge::new("ctx_src", "out", "gen", "context"),
            ],
            metadata: serde_json::Value::Null,
        };
        let mut scheduler = Scheduler::new(spec, nodes, Budget::default(), 4);
        let goal = Artifact {
            kind: "Goal".to_string(),
            data: serde_json::json!({}),
        };
        let initial = HashMap::from([
            ("goal_src".to_string(), vec![goal.clone()]),
            ("ctx_src".to_string(), vec![goal]),
        ]);
        scheduler.run(initial).await.unwrap();

        let calls = received.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "joining node should be activated exactly once, got {calls:?}"
        );
        let mut ports = calls[0].clone();
        ports.sort();
        assert_eq!(ports, vec!["context".to_string(), "in".to_string()]);
    }

    #[tokio::test]
    async fn test_node_usage_aggregated_into_run_stats() {
        // Regression (D): a node that reports token usage must have it
        // accumulated into RunStats so the cost/token budget backstops fire.
        use crate::graph::node::NodeUsage;

        struct UsageNode {
            usage: NodeUsage,
            ports: PortSpec,
        }

        #[async_trait]
        impl Node for UsageNode {
            fn ports(&self) -> PortSpec {
                self.ports.clone()
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                _inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, NodeUsage), NodeError> {
                Ok((
                    vec![Emit::new(
                        "out",
                        Artifact {
                            kind: "Goal".to_string(),
                            data: serde_json::json!({}),
                        },
                    )],
                    self.usage,
                ))
            }
        }

        let usage = NodeUsage {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            cost_usd: 0.42,
        };
        let ports = PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: "Goal".to_string(),
                required: true,
            }],
            vec![PortSpecEntry {
                name: "out".into(),
                direction: PortDirection::Output,
                kind: "Goal".to_string(),
                required: false,
            }],
        );

        let mut nodes = HashMap::new();
        nodes.insert(
            "src".to_string(),
            BoxedNode::new(UsageNode {
                usage,
                ports: ports.clone(),
            }),
        );
        // A sink so the emit has somewhere to go and the run terminates.
        nodes.insert(
            "sink".to_string(),
            BoxedNode::new(TestNode {
                ports: PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".to_string(),
                        required: false,
                    }],
                    vec![],
                ),
                output: None,
            }),
        );

        let spec = GraphSpec {
            name: Some("usage-test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "src".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "sink".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![Edge::new("src", "out", "sink", "in")],
            metadata: serde_json::Value::Null,
        };
        let mut scheduler = Scheduler::new(spec, nodes, Budget::default(), 4);
        let stats = scheduler
            .run(HashMap::from([(
                "src".to_string(),
                vec![Artifact {
                    kind: "Goal".to_string(),
                    data: serde_json::json!({}),
                }],
            )]))
            .await
            .unwrap();

        assert_eq!(stats.total_input_tokens, 1000);
        assert_eq!(stats.total_output_tokens, 500);
        assert_eq!(stats.total_tokens, 1500);
        assert!((stats.total_cost_usd - 0.42).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_activations_run_concurrently_not_sequentially() {
        // Regression (B): with max_in_flight >= 2, two independent slow nodes
        // activated in the same round must run in parallel, so their combined
        // wall-clock is ~max(t1, t2) rather than t1 + t2.
        use std::time::{Duration, Instant};

        struct SlowNode {
            delay: Duration,
            ports: PortSpec,
        }

        #[async_trait]
        impl Node for SlowNode {
            fn ports(&self) -> PortSpec {
                self.ports.clone()
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, crate::graph::node::NodeUsage), NodeError> {
                tokio::time::sleep(self.delay).await;
                Ok((
                    inputs
                        .into_iter()
                        .map(|m| Emit::new("out", m.artifact))
                        .collect(),
                    crate::graph::node::NodeUsage::default(),
                ))
            }
        }

        let slow_ports = PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: "Goal".to_string(),
                required: true,
            }],
            vec![PortSpecEntry {
                name: "out".into(),
                direction: PortDirection::Output,
                kind: "Goal".to_string(),
                required: false,
            }],
        );
        let delay = Duration::from_millis(250);
        let mut nodes = HashMap::new();
        nodes.insert(
            "a".to_string(),
            BoxedNode::new(SlowNode {
                delay,
                ports: slow_ports.clone(),
            }),
        );
        nodes.insert(
            "b".to_string(),
            BoxedNode::new(SlowNode {
                delay,
                ports: slow_ports.clone(),
            }),
        );
        nodes.insert(
            "sink".to_string(),
            BoxedNode::new(TestNode {
                ports: PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".to_string(),
                        required: false,
                    }],
                    vec![],
                ),
                output: None,
            }),
        );

        let spec = GraphSpec {
            name: Some("concurrency-test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "a".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "b".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "sink".into(),
                    kind: "test.node".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![
                Edge::new("a", "out", "sink", "in"),
                Edge::new("b", "out", "sink", "in"),
            ],
            metadata: serde_json::Value::Null,
        };
        let mut scheduler = Scheduler::new(spec, nodes, Budget::default(), 4);
        let goal = Artifact {
            kind: "Goal".to_string(),
            data: serde_json::json!({}),
        };
        let initial = HashMap::from([
            ("a".to_string(), vec![goal.clone()]),
            ("b".to_string(), vec![goal]),
        ]);
        let start = Instant::now();
        scheduler.run(initial).await.unwrap();
        let elapsed = start.elapsed();

        // Two 250ms nodes in parallel should finish well under 2*250ms. Allow
        // generous headroom for CI scheduling. Sequential would be >= 500ms.
        assert!(
            elapsed < Duration::from_millis(450),
            "expected concurrent execution (~250ms) but took {elapsed:?}"
        );
    }
}
