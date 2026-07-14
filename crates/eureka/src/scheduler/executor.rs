use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, span, warn, Level};

use crate::config::{Budget, RunStats};
use crate::graph::artifact::Artifact;
use crate::graph::edge::Edge;
use crate::graph::node::{BoxedNode, Emit, NodeCtx, NodeError, NodeUsage, PortMsg};
use crate::graph::spec::GraphSpec;
use crate::persistence::{
    ActivationSnapshot, CheckpointReason, CheckpointStore, PendingInput, Revision, RunCheckpoint,
    RunOutput,
};
use crate::rag::RagIndexer;

use super::error::SchedulerError;
use super::event::{SchedulerEvent, SchedulerSignal};

/// A ready activation: a node plus its joined inputs, about to run.
#[derive(Debug)]
struct Activation {
    /// Stable identifier used to reconstruct work at a checkpoint boundary.
    id: u64,
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
    /// Stable identifier of the completed activation.
    id: u64,
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

/// Identity metadata required to validate checkpoints.
#[derive(Clone)]
struct CheckpointIdentity {
    /// Run UUID this identity belongs to.
    run_id: uuid::Uuid,
    /// Hash of the graph manifest topology.
    graph_hash: String,
    /// Hash of the runtime configuration.
    config_hash: String,
    /// Optional checkpoint revision for optimistic concurrency control.
    revision: Option<Revision>,
}

/// Total outstanding activations across all rounds.
fn total_pending(round_pending: &HashMap<u32, usize>) -> usize {
    round_pending.values().copied().sum()
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
    /// Optional durable checkpoint backend.
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    /// Identity metadata required to validate checkpoints.
    checkpoint_identity: Option<CheckpointIdentity>,
    /// Optional RAG indexer — spawns embedding tasks after each activation.
    rag_indexer: Option<Arc<RagIndexer>>,
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
            checkpoint_store: None,
            checkpoint_identity: None,
            rag_indexer: None,
        }
    }

    /// Attach a RAG indexer that spawns best-effort embedding tasks after
    /// each successful node activation.
    #[must_use]
    pub fn with_rag_indexer(mut self, indexer: Arc<RagIndexer>) -> Self {
        self.rag_indexer = Some(indexer);
        self
    }

    /// Attach a durable checkpoint backend and run identity.
    #[must_use]
    pub fn with_checkpoint_store(
        mut self,
        store: Arc<dyn CheckpointStore>,
        run_id: uuid::Uuid,
        graph_hash: impl Into<String>,
        config_hash: impl Into<String>,
    ) -> Self {
        self.checkpoint_store = Some(store);
        self.checkpoint_identity = Some(CheckpointIdentity {
            run_id,
            graph_hash: graph_hash.into(),
            config_hash: config_hash.into(),
            revision: None,
        });
        self
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
        self.run_until_signal(initial_artifacts, None).await
    }

    /// Resume graph execution from a previously persisted checkpoint.
    ///
    /// # Errors
    /// Returns `SchedulerError::CheckpointInvalid` if the checkpoint does not match the
    /// current graph/config, or `SchedulerError::SchedulerStopped` if the scheduler was
    /// cancelled.
    pub async fn run_from_checkpoint(
        &mut self,
        checkpoint: RunCheckpoint,
    ) -> Result<RunStats, SchedulerError> {
        let Some(identity) = &mut self.checkpoint_identity else {
            return Err(SchedulerError::CheckpointMismatch);
        };
        if identity.run_id != checkpoint.run_id
            || identity.graph_hash != checkpoint.graph_hash
            || identity.config_hash != checkpoint.config_hash
        {
            return Err(SchedulerError::CheckpointMismatch);
        }
        identity.revision = Some(checkpoint.revision);
        self.run_until_signal(HashMap::new(), Some(checkpoint))
            .await
    }

    /// Run the graph and return `Ok` only when it drains normally.
    ///
    /// A pause signal returns [`SchedulerError::Paused`] so the caller can
    /// persist the run and resume it later with a fresh scheduler instance.
    #[allow(clippy::too_many_lines)]
    async fn run_until_signal(
        &mut self,
        initial_artifacts: HashMap<String, Vec<Artifact>>,
        checkpoint: Option<RunCheckpoint>,
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

        // Per-node input buffer keyed by (node_id, round). The scheduler joins
        // inputs: a node fires once when all its required input ports are
        // present for a given round. Single-input nodes fire immediately on
        // arrival, exactly as before.
        let mut input_buffer: HashMap<(String, u32), HashMap<String, Artifact>> = HashMap::new();
        let mut ready_activations: Vec<ActivationSnapshot> = Vec::new();
        let mut in_flight: HashMap<u64, ActivationSnapshot> = HashMap::new();
        let mut next_activation_id: u64 = 0;
        let mut outputs: Vec<RunOutput> = checkpoint
            .as_ref()
            .map_or_else(Vec::new, |item| item.outputs.clone());

        // Synchronized round model: a round is one forward wave starting from
        // source injections and feedback inputs. Forward edges stay in the
        // same round; feedback edges deliver to `round + 1`. We track
        // outstanding activations per round so a round is "complete" only when
        // its pending count hits zero — at which point we advance to the next
        // round that has buffered work and emit a single `CycleCompleted`.
        // `rounds_completed` thus counts fully drained cycles, not arbitrary
        // feedback crossings.
        let mut current_round: u32 = checkpoint.as_ref().map_or(0, |item| item.round);
        let mut round_pending: HashMap<u32, usize> = checkpoint
            .as_ref()
            .map_or_else(HashMap::new, |item| item.round_pending.clone());
        if let Some(item) = &checkpoint {
            self.stats = item.stats.clone();
            for pending in &item.pending_inputs {
                input_buffer
                    .entry((pending.node_id.clone(), pending.round))
                    .or_default()
                    .insert(pending.port.clone(), pending.artifact.clone());
            }
            ready_activations = item.ready_activations.clone();
        }
        // A checkpoint may contain a partially joined input bucket. Re-submit
        // its buffered ports through the normal delivery path so an externally
        // injected human artifact can complete the join and dispatch the node.
        if checkpoint.is_some() {
            let restored_inputs: Vec<_> = input_buffer
                .drain()
                .flat_map(|((node_id, round), ports)| {
                    ports
                        .into_iter()
                        .map(move |(port, artifact)| (node_id.clone(), round, port, artifact))
                })
                .collect();
            for (node_id, round, port, artifact) in restored_inputs {
                let dispatched = self.deliver_input(
                    &node_id,
                    &port,
                    artifact,
                    round,
                    &mut input_buffer,
                    &mut in_flight,
                    &mut next_activation_id,
                    &handles,
                    &mut tasks,
                )?;
                if dispatched > 0 {
                    *round_pending.entry(round).or_insert(0) += dispatched;
                }
            }
        }

        for (source_id, artifacts) in &initial_artifacts {
            if self.nodes.contains_key(source_id) {
                for artifact in artifacts {
                    let span = span!(Level::INFO, "inject", node = %source_id);
                    let _guard = span.enter();
                    // Delivering the goal artifact into the source node's first
                    // input port; this either fires immediately (single-input
                    // node) or buffers until the remaining required inputs arrive.
                    let dispatched = self.deliver_input(
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
                        current_round,
                        &mut input_buffer,
                        &mut in_flight,
                        &mut next_activation_id,
                        &handles,
                        &mut tasks,
                    )?;
                    if dispatched > 0 {
                        *round_pending.entry(current_round).or_insert(0) += dispatched;
                    }
                }
            }
        }

        for snapshot in ready_activations {
            let Some(node) = self.nodes.get(&snapshot.node_id).cloned() else {
                return Err(SchedulerError::NodeNotFound(snapshot.node_id));
            };
            let node_kind = self
                .spec
                .node_kind(&snapshot.node_id)
                .unwrap_or("unknown")
                .to_string();
            let id = next_activation_id;
            next_activation_id = next_activation_id.saturating_add(1);
            in_flight.insert(id, snapshot.clone());
            let mut ctx = NodeCtx::new(
                snapshot.node_id.clone(),
                node_kind.clone(),
                snapshot.round,
                self.cancel.clone(),
            );
            ctx.event_tx = Some(self.event_tx.clone());
            spawn_activation(
                Activation {
                    id,
                    node_id: snapshot.node_id,
                    node_kind,
                    round: snapshot.round,
                    ctx,
                    inputs: snapshot.inputs,
                },
                node,
                &handles,
                &mut tasks,
            );
        }

        let mut budget_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        budget_ticker.tick().await;

        loop {
            if total_pending(&round_pending) == 0 {
                break;
            }

            tokio::select! {
                () = self.cancel.cancelled() => {
                    info!("Run cancelled");
                    tasks.abort_all();
                    self.stats.elapsed_secs = start.elapsed().as_secs_f64();
                    self.persist_checkpoint(
                        current_round,
                        &round_pending,
                        &input_buffer,
                        &in_flight,
                        &outputs,
                        CheckpointReason::Cancellation,
                    ).await?;
                    return Err(SchedulerError::Cancelled);
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
                            tasks.abort_all();
                            self.stats.elapsed_secs = start.elapsed().as_secs_f64();
                            self.persist_checkpoint(
                                current_round,
                                &round_pending,
                                &input_buffer,
                                &in_flight,
                                &outputs,
                                CheckpointReason::Cancellation,
                            ).await?;
                            return Err(SchedulerError::Cancelled);
                        }
                        Some(SchedulerSignal::Pause) => {
                            info!(round = current_round, "Run paused");
                            let _ = self
                                .event_tx
                                .send(SchedulerEvent::RunPaused { round: current_round })
                                .await;
                            tasks.abort_all();
                            self.stats.elapsed_secs = start.elapsed().as_secs_f64();
                            self.persist_checkpoint(
                                current_round,
                                &round_pending,
                                &input_buffer,
                                &in_flight,
                                &outputs,
                                CheckpointReason::Pause,
                            ).await?;
                            return Err(SchedulerError::Paused(self.stats.clone()));
                        }
                        Some(SchedulerSignal::Resume) => {
                            info!(round = current_round, "Run resume requested");
                        }
                        None => {}
                    }
                }

                maybe_result = results_rx.recv() => {
                    match maybe_result {
                        Some(result) => {
                            let node_id = result.node_id.clone();
                            let node_kind = result.node_kind.clone();
                            let round = result.round;
                            in_flight.remove(&result.id);

                            // Decrement this activation's round pending count.
                            if let Some(count) = round_pending.get_mut(&round) {
                                *count = count.saturating_sub(1);
                                if *count == 0 {
                                    round_pending.remove(&round);
                                }
                            }

                            match result.result {
                                Ok((emits, usage)) => {
                                    // Aggregate resource usage into the run stats
                                    // so the cost/token budget backstops fire.
                                    self.stats.total_input_tokens += usage.input_tokens;
                                    self.stats.total_output_tokens += usage.output_tokens;
                                    self.stats.total_tokens += usage.total_tokens;
                                    self.stats.total_cost_usd += usage.cost_usd;

                                    let event_outputs: Vec<serde_json::Value> = emits
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
                                        outputs: event_outputs,
                                    }).await;

                                    // Best-effort RAG indexing: spawn a
                                    // background task for each emission.
                                    if let Some(indexer) = &self.rag_indexer {
                                        for emit in &emits {
                                            let idx = Arc::clone(indexer);
                                            let art = emit.artifact.clone();
                                            let nid = node_id.clone();
                                            let rnd = round;
                                            tokio::spawn(async move {
                                                if let Err(e) =
                                                    idx.index_artifact(&nid, rnd, &art).await
                                                {
                                                    warn!("rag index failed: {e}");
                                                }
                                            });
                                        }
                                    }

                                    for emit in emits {
                                        if outbound.get(&node_id).is_none_or(|edges| !edges.iter().any(|edge| edge.from_port == emit.port)) {
                                            outputs.push(RunOutput {
                                                node_id: node_id.clone(),
                                                port: emit.port.clone(),
                                                round,
                                                artifact: emit.artifact.clone(),
                                            });
                                        }
                                        let (forward, feedback) = self.route_emission(
                                            &node_id,
                                            &emit,
                                            &outbound,
                                            round,
                                            &mut input_buffer,
                                            &mut in_flight,
                                            &mut next_activation_id,
                                            &handles,
                                            &mut tasks,
                                        )?;
                                        if forward > 0 {
                                            *round_pending.entry(round).or_insert(0) += forward;
                                        }
                                        if feedback > 0 {
                                            *round_pending.entry(round + 1).or_insert(0) += feedback;
                                        }
                                    }
                                }
                                Err(err) => {
                                    warn!(%node_id, error = %err, "Node activation failed");
                                    let error_text = err.to_string();
                                    let _ = self.event_tx.send(SchedulerEvent::ActivationFailed {
                                        node_id: node_id.clone(),
                                        node_kind: node_kind.clone(),
                                        round,
                                        error: error_text.clone(),
                                    }).await;
                                    self.cancel.cancel();
                                    tasks.abort_all();
                                    self.stats.elapsed_secs = start.elapsed().as_secs_f64();
                                    return Err(SchedulerError::NodeFailed {
                                        node_id: node_id.clone(),
                                        round,
                                        error: error_text,
                                    });
                                }
                            }

                            // Round advancement: when the current round has
                            // fully drained and a future round has buffered
                            // work, advance and emit a single CycleCompleted.
                            // This makes `rounds_completed` count complete
                            // cycles rather than per-emit feedback crossings.
                            let mut advanced_round = false;
                            while total_pending(&round_pending) > 0
                                && !round_pending.contains_key(&current_round)
                            {
                                current_round += 1;
                                self.stats.rounds_completed = current_round;
                                advanced_round = true;
                                let _ = self.event_tx.send(SchedulerEvent::CycleCompleted {
                                    round: current_round,
                                }).await;
                            }
                            if advanced_round {
                                self.persist_checkpoint(
                                    current_round,
                                    &round_pending,
                                    &input_buffer,
                                    &in_flight,
                                    &outputs,
                                    CheckpointReason::RoundCompleted,
                                ).await?;
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
        self.persist_checkpoint(
            current_round,
            &round_pending,
            &input_buffer,
            &in_flight,
            &outputs,
            CheckpointReason::Completion,
        )
        .await?;
        info!(
            rounds = self.stats.rounds_completed,
            elapsed_secs = self.stats.elapsed_secs,
            "Run completed"
        );

        Ok(self.stats.clone())
    }

    /// Persist a checkpoint through the store.
    async fn persist_checkpoint(
        &mut self,
        round: u32,
        round_pending: &HashMap<u32, usize>,
        input_buffer: &HashMap<(String, u32), HashMap<String, Artifact>>,
        in_flight: &HashMap<u64, ActivationSnapshot>,
        outputs: &[RunOutput],
        reason: CheckpointReason,
    ) -> Result<(), SchedulerError> {
        let (Some(store), Some(identity)) = (
            self.checkpoint_store.clone(),
            self.checkpoint_identity.as_mut(),
        ) else {
            return Ok(());
        };
        let pending_inputs = input_buffer
            .iter()
            .flat_map(|((node_id, round), ports)| {
                ports.iter().map(|(port, artifact)| PendingInput {
                    node_id: node_id.clone(),
                    round: *round,
                    port: port.clone(),
                    artifact: artifact.clone(),
                })
            })
            .collect();
        let checkpoint = RunCheckpoint {
            run_id: identity.run_id,
            revision: identity.revision.unwrap_or_default(),
            graph_hash: identity.graph_hash.clone(),
            config_hash: identity.config_hash.clone(),
            round,
            round_pending: round_pending.clone(),
            pending_inputs,
            ready_activations: in_flight.values().cloned().collect(),
            stats: self.stats.clone(),
            outputs: outputs.to_vec(),
            reason,
        };
        let saved = store
            .save_checkpoint(checkpoint, identity.revision)
            .await
            .map_err(|error| SchedulerError::Internal(error.to_string()))?;
        identity.revision = Some(saved.revision);
        Ok(())
    }

    /// Route a single emission along its outbound edges, buffering each
    /// delivered input and dispatching an activation when a target node's
    /// required inputs are all present.
    ///
    /// `round` is the round the *source* activation belonged to. Forward edges
    /// deliver to the same round; feedback edges deliver to `round + 1` (the
    /// next cycle). Returns `(forward, feedback)` activation counts.
    #[allow(clippy::too_many_arguments)]
    fn route_emission(
        &self,
        from_node_id: &str,
        emit: &Emit,
        outbound: &HashMap<String, Vec<Edge>>,
        round: u32,
        input_buffer: &mut HashMap<(String, u32), HashMap<String, Artifact>>,
        in_flight: &mut HashMap<u64, ActivationSnapshot>,
        next_activation_id: &mut u64,
        handles: &TaskHandles,
        tasks: &mut JoinSet<()>,
    ) -> Result<(usize, usize), SchedulerError> {
        let Some(edges) = outbound.get(from_node_id) else {
            return Ok((0, 0));
        };

        let mut forward = 0usize;
        let mut feedback = 0usize;

        for edge in edges {
            if edge.from_port != emit.port {
                continue;
            }

            // Forward edges stay in the same round; feedback edges cross a
            // cycle boundary and belong to the next round.
            let target_round = if edge.feedback { round + 1 } else { round };

            let dispatched = self.deliver_input(
                &edge.to_node,
                &edge.to_port,
                emit.artifact.clone(),
                target_round,
                input_buffer,
                in_flight,
                next_activation_id,
                handles,
                tasks,
            )?;
            if edge.feedback {
                feedback += dispatched;
            } else {
                forward += dispatched;
            }
        }

        Ok((forward, feedback))
    }

    /// Buffer a single input for `(node_id, round)` and, if the node now has
    /// all required inputs present, drain the buffer into an `Activation` and
    /// spawn it as a worker task. Returns the number of activations dispatched
    /// (0 or 1).
    #[allow(clippy::too_many_arguments)]
    fn deliver_input(
        &self,
        node_id: &str,
        port: &str,
        artifact: Artifact,
        round: u32,
        input_buffer: &mut HashMap<(String, u32), HashMap<String, Artifact>>,
        in_flight: &mut HashMap<u64, ActivationSnapshot>,
        next_activation_id: &mut u64,
        handles: &TaskHandles,
        tasks: &mut JoinSet<()>,
    ) -> Result<usize, SchedulerError> {
        let bucket = input_buffer
            .entry((node_id.to_string(), round))
            .or_default();
        bucket.insert(port.to_string(), artifact);

        // Determine readiness: are all required input ports present?
        let Some(node) = self.nodes.get(node_id) else {
            return Err(SchedulerError::NodeNotFound(node_id.to_string()));
        };
        let ports = node.ports();
        let required: Vec<String> = ports.required_inputs();

        let ready = !required.is_empty() && required.iter().all(|p| bucket.contains_key(p));

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

        let id = *next_activation_id;
        *next_activation_id = next_activation_id.saturating_add(1);
        in_flight.insert(
            id,
            ActivationSnapshot {
                node_id: node_id.to_string(),
                round,
                inputs: inputs.clone(),
            },
        );
        let activation = Activation {
            id,
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
/// back to the main loop via `handles.results_tx`. Aborting the `JoinSet`
/// (on cancel or drop) cancels in-flight `process` calls.
fn spawn_activation(
    activation: Activation,
    node: BoxedNode,
    handles: &TaskHandles,
    tasks: &mut JoinSet<()>,
) {
    let activation_id = activation.id;
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
            id: activation_id,
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
    use crate::graph::node::{BoxedNode, Emit, Node, NodeCtx, NodeError, NodeUsage, PortMsg};
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
    async fn test_node_failure_aborts_run_and_reports_node() {
        struct FailingNode;

        #[async_trait]
        impl Node for FailingNode {
            fn ports(&self) -> PortSpec {
                PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".into(),
                        required: true,
                    }],
                    vec![],
                )
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                _inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, NodeUsage), NodeError> {
                Err(NodeError::Internal("boom".into()))
            }
        }

        let spec = GraphSpec {
            name: Some("failure".into()),
            description: None,
            nodes: vec![GraphNodeSpec {
                id: "failing".into(),
                kind: "test.node".into(),
                config: serde_json::Value::Null,
                description: None,
            }],
            edges: vec![],
            metadata: serde_json::Value::Null,
        };
        let mut nodes = HashMap::new();
        nodes.insert("failing".into(), BoxedNode::new(FailingNode));
        let mut scheduler = Scheduler::new(spec, nodes, Budget::default(), 1);
        let result = scheduler
            .run(HashMap::from([(
                "failing".into(),
                vec![Artifact {
                    kind: "Goal".into(),
                    data: serde_json::json!({}),
                }],
            )]))
            .await;
        assert!(matches!(
            result,
            Err(SchedulerError::NodeFailed { node_id, .. }) if node_id == "failing"
        ));
    }

    #[tokio::test]
    async fn test_pause_signal_returns_resumable_error() {
        struct SlowPauseNode;

        #[async_trait]
        impl Node for SlowPauseNode {
            fn ports(&self) -> PortSpec {
                PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".into(),
                        required: true,
                    }],
                    vec![],
                )
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                _inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, NodeUsage), NodeError> {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok((vec![], NodeUsage::default()))
            }
        }

        let spec = GraphSpec {
            name: Some("pause".into()),
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
        nodes.insert("source".into(), BoxedNode::new(SlowPauseNode));
        let mut scheduler = Scheduler::new(spec, nodes, Budget::default(), 1);
        let signal_tx = scheduler.signal_sender();
        signal_tx.send(SchedulerSignal::Pause).await.unwrap();
        let result = scheduler
            .run(HashMap::from([(
                "source".into(),
                vec![Artifact {
                    kind: "Goal".into(),
                    data: serde_json::json!({}),
                }],
            )]))
            .await;
        assert!(matches!(result, Err(SchedulerError::Paused(_))));
    }

    #[tokio::test]
    async fn test_pause_checkpoint_can_restore_and_finish() {
        struct SlowNode;

        #[async_trait]
        impl Node for SlowNode {
            fn ports(&self) -> PortSpec {
                PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".into(),
                        required: true,
                    }],
                    vec![],
                )
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                _inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, NodeUsage), NodeError> {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                Ok((vec![], NodeUsage::default()))
            }
        }

        let run_id = uuid::Uuid::now_v7();
        let spec = GraphSpec {
            name: Some("checkpoint".into()),
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
        nodes.insert("source".into(), BoxedNode::new(SlowNode));
        let store = Arc::new(crate::persistence::InMemoryRunPersistence::new());
        let checkpoint_store: Arc<dyn CheckpointStore> = store.clone();
        let mut scheduler = Scheduler::new(spec.clone(), nodes, Budget::default(), 1)
            .with_checkpoint_store(checkpoint_store, run_id, "graph", "config");
        let signal_tx = scheduler.signal_sender();
        signal_tx.send(SchedulerSignal::Pause).await.unwrap();
        let input = HashMap::from([(
            "source".into(),
            vec![Artifact {
                kind: "Goal".into(),
                data: serde_json::json!({"value": 1}),
            }],
        )]);
        assert!(matches!(
            scheduler.run(input).await,
            Err(SchedulerError::Paused(_))
        ));
        let checkpoint = CheckpointStore::load_checkpoint(&*store, run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.round_pending.get(&0), Some(&1));

        let mut restored_nodes = HashMap::new();
        restored_nodes.insert("source".into(), BoxedNode::new(SlowNode));
        let checkpoint_store: Arc<dyn CheckpointStore> = store.clone();
        let mut restored = Scheduler::new(spec, restored_nodes, Budget::default(), 1)
            .with_checkpoint_store(checkpoint_store, run_id, "graph", "config");
        let stats = restored.run_from_checkpoint(checkpoint).await.unwrap();
        assert_eq!(stats.rounds_completed, 0);
        assert!(CheckpointStore::load_checkpoint(&*store, run_id)
            .await
            .unwrap()
            .is_some());
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
        let stats = tokio::time::timeout(std::time::Duration::from_secs(5), scheduler.run(initial))
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

    #[tokio::test]
    async fn test_round_model_counts_synchronized_cycles() {
        // Regression (gotcha #1): `rounds_completed` must count fully drained
        // cycles, not per-emit feedback crossings. A supervisor node emitting
        // on a single feedback edge that fans out to multiple consumers must
        // increment the round by exactly 1 when the cycle completes — and a
        // node emitting on multiple feedback edges in one activation must not
        // double-count.
        use std::sync::Mutex;

        // A supervisor that emits on two feedback edges at once (continue → a,
        // continue → b) and also a halt signal after N rounds.
        struct SupNode {
            rounds_left: Mutex<u32>,
        }

        #[async_trait]
        impl Node for SupNode {
            fn ports(&self) -> PortSpec {
                PortSpec::new(
                    vec![PortSpecEntry {
                        name: "in".into(),
                        direction: PortDirection::Input,
                        kind: "Goal".to_string(),
                        required: true,
                    }],
                    vec![
                        PortSpecEntry {
                            name: "continue".into(),
                            direction: PortDirection::Output,
                            kind: "Goal".to_string(),
                            required: false,
                        },
                        PortSpecEntry {
                            name: "halt".into(),
                            direction: PortDirection::Output,
                            kind: "Halt".to_string(),
                            required: false,
                        },
                    ],
                )
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                _inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, crate::graph::node::NodeUsage), NodeError> {
                let mut n = self.rounds_left.lock().unwrap();
                if *n == 0 {
                    Ok((
                        vec![Emit::new(
                            "halt",
                            Artifact {
                                kind: "Halt".to_string(),
                                data: serde_json::json!({}),
                            },
                        )],
                        crate::graph::node::NodeUsage::default(),
                    ))
                } else {
                    *n -= 1;
                    // Emit on the feedback continue port (fans out to two
                    // consumers via two feedback edges).
                    Ok((
                        vec![Emit::new(
                            "continue",
                            Artifact {
                                kind: "Goal".to_string(),
                                data: serde_json::json!({}),
                            },
                        )],
                        crate::graph::node::NodeUsage::default(),
                    ))
                }
            }
        }

        struct EchoNode;
        #[async_trait]
        impl Node for EchoNode {
            fn ports(&self) -> PortSpec {
                PortSpec::new(
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
                )
            }

            async fn process(
                &self,
                _ctx: &NodeCtx,
                inputs: Vec<PortMsg>,
            ) -> Result<(Vec<Emit>, crate::graph::node::NodeUsage), NodeError> {
                Ok((
                    inputs
                        .into_iter()
                        .map(|m| Emit::new("out", m.artifact))
                        .collect(),
                    crate::graph::node::NodeUsage::default(),
                ))
            }
        }

        // sup → echo_a (forward), echo_a → sup (feedback)
        // sup → echo_b (forward), echo_b → sup (feedback)
        // sup emits `continue` which fans out to BOTH echo_a and echo_b via
        // feedback edges. The old model would increment rounds_completed once
        // per feedback *emit* that crossed — here one emit fans out to two
        // feedback edges, which must still count as ONE cycle.
        let mut nodes = HashMap::new();
        nodes.insert(
            "sup".to_string(),
            BoxedNode::new(SupNode {
                rounds_left: Mutex::new(2),
            }),
        );
        nodes.insert("echo_a".to_string(), BoxedNode::new(EchoNode));
        nodes.insert("echo_b".to_string(), BoxedNode::new(EchoNode));

        let spec = GraphSpec {
            name: Some("round-test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "sup".into(),
                    kind: "t".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "echo_a".into(),
                    kind: "t".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "echo_b".into(),
                    kind: "t".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![
                Edge::new("sup", "continue", "echo_a", "in").feedback(),
                Edge::new("sup", "continue", "echo_b", "in").feedback(),
                Edge::new("echo_a", "out", "sup", "in").feedback(),
                Edge::new("echo_b", "out", "sup", "in").feedback(),
            ],
            metadata: serde_json::Value::Null,
        };

        let mut scheduler = Scheduler::new(spec, nodes, Budget::default(), 4);
        let goal = Artifact {
            kind: "Goal".to_string(),
            data: serde_json::json!({}),
        };
        let stats = scheduler
            .run(HashMap::from([("sup".to_string(), vec![goal])]))
            .await
            .unwrap();

        // The supervisor ran 3 times (initial + 2 continues) then halted. With
        // the synchronized model, each fully-drained cycle is one round. The
        // exact count depends on how cycles align, but it must be small and
        // stable (<= 4), and crucially NOT inflated by the 2-way feedback
        // fan-out (the old per-emit model would have produced more).
        assert!(
            stats.rounds_completed <= 4,
            "rounds_completed should count synchronized cycles, not per-emit \
             feedback crossings; got {}",
            stats.rounds_completed
        );
        assert!(stats.rounds_completed >= 1);
    }
}
