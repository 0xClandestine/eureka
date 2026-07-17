//! Scheduler execution loop for [`super::Session`].
//!
//! Contains the private `run_internal` implementation and its helper
//! utilities. Public entry points (`run`, `run_with_store`, `resume_with_store`)
//! live in `mod.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::EngineError;
use crate::graph::artifact::Artifact;
use crate::graph::edge::Edge;
use crate::graph::node::BoxedNode;
use crate::graph::spec::{GraphNodeSpec, GraphSpec};
use crate::persistence::{RunRecord, RunStatus, RunStore};
use crate::scheduler::{Scheduler, SchedulerError, SchedulerEvent};
use tracing::{error, info, warn};

impl super::Session {
    /// Internal entry point that assembles the graph and drives the scheduler.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn run_internal(
        &mut self,
        goal: serde_json::Value,
        store: Option<&dyn RunStore>,
        checkpoint: Option<crate::persistence::RunCheckpoint>,
    ) -> Result<crate::config::RunStats, EngineError> {
        info!(
            session_id = %self.session_id,
            goal = %goal.get("goal").and_then(|v| v.as_str()).unwrap_or("(no goal)"),
            "Starting Eureka session"
        );

        // Lazily initialize RAG on first run when enabled.
        if self.rag_index.is_none() {
            if let Some(rag_cfg) = self.config.rag.as_ref().filter(|r| r.enabled) {
                if let Some(ref db_path) = self.db_path {
                    match crate::rag::build_rag_components(
                        rag_cfg,
                        db_path,
                        &self.session_id.to_string(),
                        self.live_tokens.clone(),
                        self.live_cost.clone(),
                    )
                    .await
                    {
                        Ok((handle, indexer)) => {
                            self.rag_index = Some(handle);
                            self.rag_indexer = Some(indexer);
                        }
                        Err(e) => warn!("RAG initialization failed (running without RAG): {e}"),
                    }
                } else {
                    warn!("RAG requires a db_path but none was configured; running without RAG");
                }
            }
        }

        let mut record = RunRecord::new(self.session_id, self.config.graph.clone(), goal.clone());
        record.status = RunStatus::Running;
        if let Some(store) = store {
            store.save(record.clone()).await.map_err(|e| EngineError::Store(e.to_string()))?;
        }

        // Construct all nodes from the spec — either from overrides or from manifest.
        // Clone the node specs to avoid holding an immutable borrow on self while
        // construct_node needs &mut self (for the LLM client cache).
        let node_specs = self.spec.nodes.clone();
        let mut base_nodes: HashMap<String, BoxedNode> = HashMap::new();
        for node_spec in &node_specs {
            #[allow(clippy::option_if_let_else)]
            let node_result = if let Some(override_node) = self.node_overrides.get(&node_spec.id) {
                Ok(override_node.clone())
            } else {
                self.construct_node(node_spec).await
            };
            let node = match node_result {
                Ok(node) => node,
                Err(error) => {
                    record.status = RunStatus::Failed;
                    record.error = Some(error.to_string());
                    if let Some(store) = store {
                        store.save(record).await.map_err(|e| EngineError::Store(e.to_string()))?;
                    }
                    return Err(error);
                }
            };
            info!("Constructed node '{}' (kind: {})", node_spec.id, node_spec.kind);
            base_nodes.insert(node_spec.id.clone(), node);
        }

        // Expand replicas: for each node, look up the workers count from the
        // runtime TOML config (not the YAML manifest). Edges fan out to all
        // replicas via cross-product.
        let mut expanded_nodes: HashMap<String, BoxedNode> = HashMap::new();
        let mut expanded_node_specs: Vec<GraphNodeSpec> = Vec::new();
        let mut worker_counts: HashMap<String, usize> = HashMap::new();
        for node_spec in &node_specs {
            let agent_cfg = crate::config::AgentConfig::resolve_for(
                &self.config.agent,
                &self.config.agent_overrides,
                &node_spec.id,
            );
            let w = agent_cfg.workers.max(1) as usize;
            worker_counts.insert(node_spec.id.clone(), w);
            let base_node = base_nodes.get(&node_spec.id).cloned().ok_or_else(|| {
                EngineError::Graph(crate::graph::spec::GraphError::ParseError(format!(
                    "node '{}' not found in base_nodes map",
                    node_spec.id
                )))
            })?;
            if w == 1 {
                expanded_nodes.insert(node_spec.id.clone(), base_node);
                expanded_node_specs.push(node_spec.clone());
            } else {
                for i in 0..w {
                    let replica_id = format!("{}.r{}", node_spec.id, i);
                    expanded_nodes.insert(replica_id.clone(), base_node.clone());
                    expanded_node_specs.push(GraphNodeSpec {
                        id: replica_id.clone(),
                        kind: node_spec.kind.clone(),
                        config: node_spec.config.clone(),
                        description: node_spec.description.clone(),
                    });
                    info!(
                        "Replica '{}' of node '{}' (kind: {}, workers: {w})",
                        replica_id, node_spec.id, node_spec.kind
                    );
                }
            }
        }

        // Expand edges: cross-product of from-replicas × to-replicas.
        let mut expanded_edges: Vec<Edge> = Vec::new();
        for edge in &self.spec.edges {
            let ra = worker_counts.get(&edge.from_node).copied().unwrap_or(1);
            let rb = worker_counts.get(&edge.to_node).copied().unwrap_or(1);
            for a in 0..ra {
                for b in 0..rb {
                    let mut e = edge.clone();
                    if ra > 1 {
                        e.from_node = format!("{}.r{}", edge.from_node, a);
                    }
                    if rb > 1 {
                        e.to_node = format!("{}.r{}", edge.to_node, b);
                    }
                    expanded_edges.push(e);
                }
            }
        }

        let expanded_spec = GraphSpec {
            name: self.spec.name.clone(),
            description: self.spec.description.clone(),
            nodes: expanded_node_specs,
            edges: expanded_edges,
            frontend: self.spec.frontend.clone(),
            metadata: self.spec.metadata.clone(),
        };

        // Create the scheduler
        let max_in_flight = self.config.scheduler.max_in_flight;
        let budget = self.config.budget.clone();
        let mut scheduler =
            Scheduler::new(expanded_spec.clone(), expanded_nodes, budget, max_in_flight);
        let live_tokens = self.live_tokens.clone();
        let live_input_tokens = self.live_input_tokens.clone();
        let live_output_tokens = self.live_output_tokens.clone();
        let live_cost = self.live_cost.clone();
        if let (Some(lt), Some(li), Some(lo), Some(lc)) =
            (&live_tokens, &live_input_tokens, &live_output_tokens, &live_cost)
        {
            scheduler = scheduler.with_live_counters(
                Arc::clone(lt),
                Arc::clone(li),
                Arc::clone(lo),
                Arc::clone(lc),
            );
        }
        if let Some(indexer) = &self.rag_indexer {
            scheduler = scheduler.with_rag_indexer(Arc::clone(indexer));
        }
        let mut scheduler = if let Some(store) = &self.checkpoint_store {
            scheduler.with_checkpoint_store(
                Arc::clone(store),
                self.session_id,
                stable_hash(&expanded_spec),
                stable_hash(&self.config),
            )
        } else {
            scheduler
        };
        self.scheduler_signal = Some(scheduler.signal_sender());
        if let Some(sink) = &self.scheduler_signal_sink {
            *sink.lock().await = self.scheduler_signal.clone();
        }

        let mut events = scheduler.event_receiver();

        // Durable event history is stored in the same persistence backend as
        // run metadata and checkpoints. This replaces the former JSONL trace.
        let event_store = self.config.tracing.enabled.then(|| self.event_store.clone()).flatten();
        let broadcaster = self.event_broadcaster.clone();
        let event_store_for_task = event_store.clone();
        let include_artifacts = self.config.tracing.include_artifacts;
        let event_session_id = self.session_id;
        let event_handle = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let Some(tx) = &broadcaster {
                    let _ = tx.send(event.clone());
                }
                match &event {
                    SchedulerEvent::ActivationStarted { node_id, node_kind, round } => {
                        info!(%node_id, %node_kind, round, "Node activation started");
                    }
                    SchedulerEvent::ActivationCompleted {
                        node_id,
                        node_kind,
                        round,
                        emit_count,
                        ..
                    } => {
                        info!(%node_id, %node_kind, round, emit_count, "Node activation completed");
                    }
                    SchedulerEvent::ActivationFailed { node_id, node_kind, round, error } => {
                        error!(%node_id, %node_kind, round, %error, "Node activation failed");
                    }
                    SchedulerEvent::CycleCompleted { round } => {
                        info!(round, "Cycle completed");
                    }
                    SchedulerEvent::ToolCalled { node_id, tool, args_summary, .. } => {
                        info!(%node_id, %tool, %args_summary, "Tool called");
                    }
                    SchedulerEvent::RunHalted { reason, total_rounds } => {
                        info!(%reason, total_rounds, "Run halted");
                    }
                    SchedulerEvent::RunPaused { round } => {
                        info!(round, "Run paused");
                    }
                }

                // Append to the durable database event history. Event
                // persistence is best-effort and must not stop graph work.
                if let Some(store) = &event_store_for_task {
                    let event = if include_artifacts {
                        event.clone()
                    } else {
                        strip_event_artifacts(&event)
                    };
                    if let Err(error) = store
                        .append_event(
                            event_session_id,
                            serde_json::to_value(&event).unwrap_or_default(),
                        )
                        .await
                    {
                        warn!(%error, "Failed to persist scheduler event");
                    }
                }
            }
        });

        // Inject the goal artifact into every source node
        let source_ids = expanded_spec.source_node_ids();
        let mut initial_artifacts = HashMap::new();
        for source_id in &source_ids {
            let artifact = Artifact { kind: "Goal".to_string(), data: goal.clone() };
            initial_artifacts.insert(source_id.clone(), vec![artifact]);
        }

        let stats = match if let Some(checkpoint) = checkpoint {
            scheduler.run_from_checkpoint(checkpoint).await
        } else {
            scheduler.run(initial_artifacts).await
        } {
            Ok(stats) => stats,
            Err(error) => {
                record.status = match &error {
                    SchedulerError::Paused(stats) => {
                        record.stats = Some(stats.clone());
                        RunStatus::Paused
                    }
                    SchedulerError::NodeFailed { error, .. } => {
                        record.error = Some(error.clone());
                        RunStatus::Failed
                    }
                    SchedulerError::Cancelled => {
                        record.error = Some(error.to_string());
                        RunStatus::Cancelled
                    }
                    _ => {
                        record.error = Some(error.to_string());
                        RunStatus::Failed
                    }
                };
                if let SchedulerError::Paused(stats) = &error {
                    record.stats = Some(stats.clone());
                }
                if let Some(store) = store {
                    store.save(record).await.map_err(|e| EngineError::Store(e.to_string()))?;
                }
                return Err(EngineError::Scheduler(error.to_string()));
            }
        };

        self.stats = Some(stats.clone());
        record.status = if stats.is_budget_exhausted(&self.config.budget).is_some() {
            RunStatus::Cancelled
        } else {
            RunStatus::Completed
        };
        record.stats = Some(stats.clone());
        if let Some(store) = store {
            store.save(record).await.map_err(|e| EngineError::Store(e.to_string()))?;
        }
        // The scheduler has finished producing events. Drop its event sender,
        // then drain the observer so all queued events reach SQLite before the
        // session returns.
        drop(scheduler);
        let _ = event_handle.await;
        self.scheduler_signal = None;
        if let Some(sink) = &self.scheduler_signal_sink {
            *sink.lock().await = None;
        }

        info!(
            session_id = %self.session_id,
            rounds = stats.rounds_completed,
            elapsed_secs = stats.elapsed_secs,
            "Session completed"
        );

        Ok(stats)
    }
}

/// Remove potentially large artifact payloads from a completed event.
fn strip_event_artifacts(event: &SchedulerEvent) -> SchedulerEvent {
    match event {
        SchedulerEvent::ActivationCompleted {
            node_id,
            node_kind,
            round,
            emit_count,
            usage,
            ..
        } => SchedulerEvent::ActivationCompleted {
            node_id: node_id.clone(),
            node_kind: node_kind.clone(),
            round: *round,
            emit_count: *emit_count,
            outputs: Vec::new(),
            usage: *usage,
        },
        other => other.clone(),
    }
}

/// Compute a stable content hash for checkpoint identity validation.
fn stable_hash<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            let mut hash: u64 = 14_695_981_039_346_656_037;
            for byte in bytes {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(1_099_511_628_211);
            }
            format!("{hash:016x}")
        }
        Err(e) => {
            error!("stable_hash: serialization failed — checkpoint identity will not match: {e}");
            format!("error:{e}")
        }
    }
}
