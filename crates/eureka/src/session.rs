//! Session management — orchestrates the full lifecycle of a Eureka run.
//!
//! A session loads the single YAML manifest (`config.graph`) once, derives
//! both the graph topology and the node implementations from it — no separate
//! node registry needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::agents::def::{AgentConfig, AgentDef, AgentPort, ToolDef};
use crate::agents::{LlmAgentNode, LlmClient, RigClient};
use crate::config::{EurekaConfig, ProviderKind, RunStats};
use crate::graph::artifact::Artifact;
use crate::graph::node::BoxedNode;
use crate::graph::port::{PortDef, PortDirection, PortSpec, PortSpecEntry};
use crate::graph::spec::{GraphError, GraphNodeSpec, GraphSpec};
use crate::graph::validate::{validate_graph, PortRegistry};
use crate::manifest::{AgentSpec, ControlSpec, GraphManifest};
use crate::run::{CheckpointStore, RunEnvironment, RunRecord, RunStatus, RunStore};
use crate::scheduler::{Scheduler, SchedulerError, SchedulerEvent, SchedulerSignal};
use anyhow::Context;
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::providers::{
    anthropic, cohere, deepseek, gemini, groq, mistral, ollama, openai, openrouter, perplexity,
    together, xai,
};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::control::node::{ControlNode, ControlNodeDef};
use crate::error::EngineError;
use crate::tracing::JsonlTraceWriter;

/// A session represents a single Eureka research run.
pub struct Session {
    /// The configuration for this session.
    config: EurekaConfig,
    /// The graph specification being executed (derived from manifest).
    spec: GraphSpec,
    /// The parsed manifest — used for node construction during `run()`.
    manifest: GraphManifest,
    /// The graph directory (parent of config.graph).
    graph_dir: PathBuf,
    /// The session ID.
    #[allow(clippy::struct_field_names)]
    session_id: uuid::Uuid,
    /// Optional path to the session database (passed to control nodes).
    db_path: Option<PathBuf>,
    /// Cached LLM clients, keyed by model ID.
    client_cache: HashMap<String, Arc<dyn LlmClient>>,
    /// Default model ID for agents without a per-agent override.
    default_model: String,
    /// The final run statistics.
    stats: Option<RunStats>,
    /// Broadcast sender for scheduler events (for UI/observability).
    event_broadcaster: Option<broadcast::Sender<SchedulerEvent>>,
    /// Node overrides for testing — keyed by node ID.
    node_overrides: HashMap<String, BoxedNode>,
    /// Optional durable scheduler checkpoint backend.
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    /// Signal sender for the active scheduler, when a run is executing.
    scheduler_signal: Option<mpsc::Sender<SchedulerSignal>>,
}

impl Session {
    /// Create a new session from a config.
    ///
    /// Loads the YAML manifest from `config.graph`, validates the graph
    /// topology, and prepares everything needed to construct nodes during
    /// `run()`.
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if the manifest cannot be loaded, the graph
    /// is invalid, or an LLM client cannot be constructed.
    pub fn new(
        config: EurekaConfig,
        session_id: &str,
        db_path: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        let graph_path = Path::new(&config.graph);
        let manifest = GraphManifest::load(graph_path)?;

        let spec = manifest.to_graph_spec();

        // Build a PortRegistry from the manifest for validation.
        let port_registry = build_port_registry(&manifest);

        // Validate the graph topology.
        let result = validate_graph(&spec, &port_registry);
        if !result.valid {
            for err in &result.errors {
                error!("Graph validation error: {err}");
            }
            return Err(EngineError::Graph(GraphError::ParseError(format!(
                "Graph validation failed with {} errors: {}",
                result.errors.len(),
                result
                    .errors
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))));
        }

        info!(
            graph_name = ?spec.name,
            nodes = spec.nodes.len(),
            edges = spec.edges.len(),
            "Graph specification validated successfully"
        );

        let graph_dir = graph_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let default_model = config
            .provider
            .generation_model
            .as_deref()
            .unwrap_or("deepseek/deepseek-v4-flash")
            .to_string();

        // Pre-warm the default LLM client.
        let mut client_cache: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        if let Ok(client) = build_llm_client(&config, &default_model) {
            client_cache.insert(default_model.clone(), client);
        }

        let session_id: uuid::Uuid = uuid::Uuid::parse_str(session_id).unwrap_or_else(|_| {
            warn!("Invalid session UUID '{session_id}', generating new one");
            uuid::Uuid::now_v7()
        });

        Ok(Self {
            config,
            spec,
            manifest,
            graph_dir,
            session_id,
            db_path,
            client_cache,
            default_model,
            stats: None,
            event_broadcaster: None,
            node_overrides: HashMap::new(),
            checkpoint_store: None,
            scheduler_signal: None,
        })
    }

    /// Create a session with pre-built nodes and spec (for testing).
    ///
    /// The `nodes` map must contain an entry for every node in `spec`.
    /// Port validation is performed based on each node's `ports()` declaration.
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if validation fails or a node is missing.
    pub fn from_parts(
        config: EurekaConfig,
        spec: GraphSpec,
        nodes: HashMap<String, BoxedNode>,
        session_id: uuid::Uuid,
    ) -> Result<Self, EngineError> {
        // Build a PortRegistry from the actual nodes.
        let mut port_registry = PortRegistry::new();
        for (node_id, node) in &nodes {
            let port_spec = node.ports();
            port_registry.register(node_id.clone(), port_spec);
        }

        // Also register by kind for nodes that are referenced by kind in validation.
        for node_spec in &spec.nodes {
            if let Some(node) = nodes.get(&node_spec.id) {
                let port_spec = node.ports();
                port_registry.register(node_spec.kind.clone(), port_spec);
            }
        }

        // Validate topology.
        let result = validate_graph(&spec, &port_registry);
        if !result.valid {
            for err in &result.errors {
                error!("Graph validation error: {err}");
            }
            return Err(EngineError::Graph(GraphError::ParseError(format!(
                "Graph validation failed with {} errors: {}",
                result.errors.len(),
                result
                    .errors
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))));
        }

        // Verify every node in the spec has a corresponding node in the map.
        for node_spec in &spec.nodes {
            if !nodes.contains_key(&node_spec.id) {
                return Err(EngineError::NodeCreation(format!(
                    "Missing node '{}' in provided node map",
                    node_spec.id
                )));
            }
        }

        info!(
            graph_name = ?spec.name,
            nodes = spec.nodes.len(),
            edges = spec.edges.len(),
            "Graph specification validated successfully (from_parts)"
        );

        // Build a minimal empty manifest for the nodes-based path.
        let manifest = GraphManifest {
            name: spec.name.clone(),
            description: spec.description.clone(),
            agents: vec![],
            control: vec![],
            edges: spec.edges.clone(),
            metadata: spec.metadata.clone(),
        };

        Ok(Self {
            config,
            spec,
            manifest,
            graph_dir: PathBuf::from("."),
            session_id,
            db_path: None,
            client_cache: HashMap::new(),
            default_model: String::new(),
            stats: None,
            event_broadcaster: None,
            node_overrides: nodes,
            checkpoint_store: None,
            scheduler_signal: None,
        })
    }

    /// Attach a broadcast sender so the UI server receives every `SchedulerEvent`.
    pub fn set_event_broadcaster(&mut self, tx: broadcast::Sender<SchedulerEvent>) {
        self.event_broadcaster = Some(tx);
    }

    /// Configure durable scheduler checkpoints for this session.
    pub fn set_checkpoint_store(&mut self, store: Arc<dyn CheckpointStore>) {
        self.checkpoint_store = Some(store);
    }

    /// Return a signal sender for the active scheduler, if any.
    #[must_use]
    pub fn scheduler_signal(&self) -> Option<mpsc::Sender<SchedulerSignal>> {
        self.scheduler_signal.clone()
    }

    /// Get the session ID.
    #[must_use]
    pub const fn session_id(&self) -> uuid::Uuid {
        self.session_id
    }

    /// Get the run statistics (if the session has been run).
    #[must_use]
    pub const fn stats(&self) -> Option<&RunStats> {
        self.stats.as_ref()
    }

    /// Get the graph spec.
    #[must_use]
    pub const fn spec(&self) -> &GraphSpec {
        &self.spec
    }

    /// Run the session with the given initial goal as a JSON value.
    ///
    /// The value is injected as a `"Goal"` artifact into every source node
    /// (nodes whose required input ports accept `"Goal"`-kind artifacts).
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if node construction or scheduling fails.
    pub async fn run(&mut self, goal: serde_json::Value) -> Result<RunStats, EngineError> {
        self.run_with_store(goal, None).await
    }

    /// Run the session while persisting lifecycle records to `store`.
    ///
    /// The store is updated before execution, after successful completion, and
    /// when a pause or node failure is observed. This gives API callers a
    /// durable record even when execution returns an error.
    pub async fn run_with_store(
        &mut self,
        goal: serde_json::Value,
        store: Option<&dyn RunStore>,
    ) -> Result<RunStats, EngineError> {
        info!(
            session_id = %self.session_id,
            goal = %goal.get("goal").and_then(|v| v.as_str()).unwrap_or("(no goal)"),
            "Starting Eureka session"
        );

        let mut record = RunRecord::new(self.session_id, self.config.graph.clone(), goal.clone());
        record.status = RunStatus::Running;
        if let Some(store) = store {
            store
                .save(record.clone())
                .await
                .map_err(|e| EngineError::Store(e.to_string()))?;
        }

        // Construct all nodes from the spec — either from overrides or from manifest.
        // Clone the node specs to avoid holding an immutable borrow on self while
        // construct_node needs &mut self (for the LLM client cache).
        let node_specs = self.spec.nodes.clone();
        let mut nodes: HashMap<String, BoxedNode> = HashMap::new();
        for node_spec in &node_specs {
            let node_result = if let Some(override_node) = self.node_overrides.get(&node_spec.id) {
                Ok(override_node.clone())
            } else {
                self.construct_node(node_spec)
            };
            let node = match node_result {
                Ok(node) => node,
                Err(error) => {
                    record.status = RunStatus::Failed;
                    record.error = Some(error.to_string());
                    if let Some(store) = store {
                        store
                            .save(record)
                            .await
                            .map_err(|e| EngineError::Store(e.to_string()))?;
                    }
                    return Err(error);
                }
            };
            info!(
                "Constructed node '{}' (kind: {})",
                node_spec.id, node_spec.kind
            );
            nodes.insert(node_spec.id.clone(), node);
        }

        // Create the scheduler
        let max_in_flight = self.config.scheduler.max_in_flight;
        let budget = self.config.budget.clone();
        let scheduler = Scheduler::new(self.spec.clone(), nodes, budget, max_in_flight);
        let mut scheduler = if let Some(store) = &self.checkpoint_store {
            scheduler.with_checkpoint_store(
                Arc::clone(store),
                self.session_id,
                stable_hash(&self.spec),
                stable_hash(&self.config),
            )
        } else {
            scheduler
        };
        self.scheduler_signal = Some(scheduler.signal_sender());

        let mut events = scheduler.event_receiver();

        // Optional durable trace writer (JSONL file co-located with the session DB).
        let trace_writer = if self.config.tracing.enabled {
            let sessions_dir = self.graph_dir.join(".eureka").join("sessions");
            let provider = self.config.provider.kind.to_string();
            let model = self
                .config
                .provider
                .generation_model
                .clone()
                .unwrap_or_default();
            JsonlTraceWriter::open(
                &sessions_dir,
                &self.session_id,
                &self.config.tracing,
                &self.config.graph,
                &provider,
                &model,
                &self.config.budget,
                self.config.scheduler.max_in_flight,
            )
        } else {
            None
        };

        let trace_writer: Arc<Mutex<Option<JsonlTraceWriter>>> = Arc::new(Mutex::new(trace_writer));

        let broadcaster = self.event_broadcaster.clone();
        let tw = Arc::clone(&trace_writer);
        let event_handle = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let Some(tx) = &broadcaster {
                    let _ = tx.send(event.clone());
                }
                match &event {
                    SchedulerEvent::ActivationStarted {
                        node_id,
                        node_kind,
                        round,
                    } => {
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
                    SchedulerEvent::ActivationFailed {
                        node_id,
                        node_kind,
                        round,
                        error,
                    } => {
                        error!(%node_id, %node_kind, round, %error, "Node activation failed");
                    }
                    SchedulerEvent::CycleCompleted { round } => {
                        info!(round, "Cycle completed");
                    }
                    SchedulerEvent::ToolCalled {
                        node_id,
                        tool,
                        args_summary,
                        ..
                    } => {
                        info!(%node_id, %tool, %args_summary, "Tool called");
                    }
                    SchedulerEvent::RunHalted {
                        reason,
                        total_rounds,
                    } => {
                        info!(%reason, total_rounds, "Run halted");
                    }
                    SchedulerEvent::RunPaused { round } => {
                        info!(round, "Run paused");
                    }
                }

                // Write to the durable trace file (best-effort).
                if let Ok(mut guard) = tw.lock() {
                    if let Some(ref mut w) = *guard {
                        w.write_event(&event);
                    }
                }
            }
        });

        // Inject the goal artifact into every source node
        let source_ids = self.spec.source_node_ids();
        let mut initial_artifacts = HashMap::new();
        for source_id in &source_ids {
            let artifact = Artifact {
                kind: "Goal".to_string(),
                data: goal.clone(),
            };
            initial_artifacts.insert(source_id.clone(), vec![artifact]);
        }

        let stats = match scheduler.run(initial_artifacts).await {
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
                    _ => {
                        record.error = Some(error.to_string());
                        RunStatus::Cancelled
                    }
                };
                if let SchedulerError::Paused(stats) = &error {
                    record.stats = Some(stats.clone());
                }
                if let Some(store) = store {
                    store
                        .save(record)
                        .await
                        .map_err(|e| EngineError::Store(e.to_string()))?;
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
            store
                .save(record)
                .await
                .map_err(|e| EngineError::Store(e.to_string()))?;
        }
        event_handle.abort();
        self.scheduler_signal = None;

        // Close the trace writer with final stats (best-effort).
        if let Ok(mut guard) = trace_writer.lock() {
            if let Some(writer) = guard.take() {
                if let Err(e) = writer.close(&stats) {
                    warn!(
                        error = %e,
                        "Failed to close trace file"
                    );
                }
            }
        }

        info!(
            session_id = %self.session_id,
            rounds = stats.rounds_completed,
            elapsed_secs = stats.elapsed_secs,
            "Session completed"
        );

        Ok(stats)
    }

    /// Construct a single node from a `GraphNodeSpec` using manifest data.
    fn construct_node(&mut self, spec: &GraphNodeSpec) -> Result<BoxedNode, EngineError> {
        // Find agent or control spec by node ID (clone out to drop immutable borrow before
        // calling build methods which need &mut self for the LLM client cache).
        let agent_spec = self
            .manifest
            .agents
            .iter()
            .find(|a| a.id == spec.id)
            .cloned();
        let ctrl_spec = self
            .manifest
            .control
            .iter()
            .find(|c| c.id == spec.id)
            .cloned();
        let node_config = spec.config.clone();

        if let Some(agent) = agent_spec {
            return self.build_agent_node(&agent);
        }

        if let Some(ctrl) = ctrl_spec {
            return Ok(self.build_control_node(&ctrl, &node_config));
        }

        Err(EngineError::UnknownNodeKind(format!(
            "Node '{}' (kind: '{}') not found in manifest agents or control nodes. \
             Every node in the graph must have a matching entry in the YAML manifest.",
            spec.id, spec.kind
        )))
    }

    /// Build an LLM agent node from an `AgentSpec`.
    fn build_agent_node(&mut self, agent_spec: &AgentSpec) -> Result<BoxedNode, EngineError> {
        let prompt_content = agent_spec.prompt.read().map_err(|e| {
            EngineError::NodeCreation(format!(
                "Failed to read prompt '{}': {e}",
                agent_spec.prompt.as_str()
            ))
        })?;

        let agent_def = AgentDef {
            name: agent_spec.id.clone(),
            description: agent_spec.description.clone(),
            preamble: prompt_content,
            inputs: agent_spec
                .inputs
                .iter()
                .map(|p| AgentPort {
                    kind: p.kind.clone(),
                    port: p.port.clone(),
                    required: p.required,
                })
                .collect(),
            outputs: agent_spec
                .outputs
                .iter()
                .map(|p| AgentPort {
                    kind: p.kind.clone(),
                    port: p.port.clone(),
                    required: p.required,
                })
                .collect(),
            config: AgentConfig {
                temperature: agent_spec
                    .config
                    .get("temperature")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(self.config.agent.temperature),
                max_iterations: u32::try_from(
                    agent_spec
                        .config
                        .get("max_iterations")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_else(|| u64::from(self.config.agent.max_iterations)),
                )
                .unwrap_or(self.config.agent.max_iterations),
            },
            output_schema: agent_spec.output_schema.clone(),
            tools: agent_spec
                .tools
                .iter()
                .map(|t| ToolDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    command: t.command.clone(),
                    args_schema: t.args_schema.clone(),
                    timeout_secs: t.timeout_secs,
                })
                .collect(),
        };

        // Reject tool names that shadow the terminal `submit` tool — the
        // agent would never be able to terminate.
        for tool in &agent_def.tools {
            if tool.name.eq_ignore_ascii_case("submit") {
                return Err(EngineError::NodeCreation(format!(
                    "Agent '{}' declares a tool named 'submit', which is reserved 
                     for the agent's terminal output tool. Rename the tool.",
                    agent_spec.id
                )));
            }
        }

        let model_id = self
            .config
            .provider
            .agent_models
            .get(&agent_spec.id)
            .map_or(self.default_model.as_str(), String::as_str)
            .to_string();

        let client_arc = self.get_or_create_client(&model_id).map_err(|e| {
            EngineError::NodeCreation(format!(
                "Failed to build LLM client for model '{model_id}' (agent '{}'): {e}",
                agent_spec.id
            ))
        })?;

        Ok(BoxedNode::new(LlmAgentNode::with_environment(
            Arc::new(agent_def),
            client_arc,
            self.graph_dir.clone(),
            RunEnvironment::new(self.session_id.to_string(), self.db_path.clone()),
        )))
    }

    /// Build a control plugin node from a `ControlSpec`.
    ///
    /// The command is used verbatim: the manifest's path resolver
    /// (`ControlSpec::resolve_paths`) already rewrites relative `command[0]`
    /// to an absolute path when the resolved binary exists on disk; for bare
    /// interpreter names like `python3` it leaves them alone so the OS PATH
    /// lookup is used. The control node spawns with `current_dir = graph_dir`,
    /// so relative script arguments (e.g. `control/ranker.py`) resolve against
    /// the graph directory automatically. Rewriting `command[0]` here would
    /// turn bare `python3` into `<graph_dir>/python3`, which does not exist.
    fn build_control_node(
        &self,
        ctrl_spec: &ControlSpec,
        node_config: &serde_json::Value,
    ) -> BoxedNode {
        let def = ControlNodeDef {
            name: format!("{}.{}", ctrl_spec.kind, ctrl_spec.id),
            work_dir: self.graph_dir.clone(),
            command: ctrl_spec.command.clone(),
            inputs: ctrl_spec
                .inputs
                .iter()
                .map(|p| PortDef {
                    port: p.port.clone(),
                    kind: p.kind.clone(),
                    required: p.required,
                })
                .collect(),
            outputs: ctrl_spec
                .outputs
                .iter()
                .map(|p| PortDef {
                    port: p.port.clone(),
                    kind: p.kind.clone(),
                    required: p.required,
                })
                .collect(),
            timeout_secs: ctrl_spec.timeout_secs,
        };

        BoxedNode::new(ControlNode::with_environment(
            def,
            RunEnvironment::new(self.session_id.to_string(), self.db_path.clone()),
            node_config.clone(),
        ))
    }

    /// Get or create an LLM client for the given model ID.
    fn get_or_create_client(
        &mut self,
        model_id: &str,
    ) -> Result<Arc<dyn LlmClient>, anyhow::Error> {
        if let Some(client) = self.client_cache.get(model_id) {
            return Ok(Arc::clone(client));
        }

        let client = build_llm_client(&self.config, model_id)?;
        self.client_cache
            .insert(model_id.to_string(), Arc::clone(&client));
        Ok(client)
    }
}

/// Compute a stable content hash for checkpoint identity validation.
fn stable_hash<T: serde::Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hash: u64 = 14695981039346656037;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Build a `PortRegistry` from the agents and control nodes declared in a
/// `GraphManifest`. Used for graph validation.
fn build_port_registry(manifest: &GraphManifest) -> PortRegistry {
    let mut reg = PortRegistry::new();

    for agent in &manifest.agents {
        let inputs = agent
            .inputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Input,
                kind: p.kind.clone(),
                required: p.required.unwrap_or(true),
            })
            .collect();

        let outputs = agent
            .outputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Output,
                kind: p.kind.clone(),
                required: false,
            })
            .collect();

        // Agents are keyed by their ID (the kind string in GraphNodeSpec).
        reg.register(agent.id.clone(), PortSpec::new(inputs, outputs));
    }

    for ctrl in &manifest.control {
        let inputs = ctrl
            .inputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Input,
                kind: p.kind.clone(),
                required: p.required.unwrap_or(true),
            })
            .collect();

        let outputs = ctrl
            .outputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Output,
                kind: p.kind.clone(),
                required: false,
            })
            .collect();

        // Control nodes are keyed by their kind (e.g., "elo-ranker").
        reg.register(ctrl.kind.clone(), PortSpec::new(inputs, outputs));
    }

    reg
}

/// Build a type-erased `LlmClient` from the provider configuration.
fn build_llm_client(
    config: &EurekaConfig,
    model_id: &str,
) -> Result<Arc<dyn LlmClient>, anyhow::Error> {
    match config.provider.kind {
        ProviderKind::Anthropic => {
            let client = anthropic::Client::from_env()
                .context("ANTHROPIC_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::OpenAI => {
            let client = openai::Client::from_env()
                .context("OPENAI_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::OpenRouter => {
            let client = openrouter::Client::from_env()
                .context("OPENROUTER_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::Gemini => {
            let client = gemini::Client::from_env()
                .context("GEMINI_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::Groq => {
            let client =
                groq::Client::from_env().context("GROQ_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::Mistral => {
            let client = mistral::Client::from_env()
                .context("MISTRAL_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::Cohere => {
            let client = cohere::Client::from_env()
                .context("COHERE_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::DeepSeek => {
            let client = deepseek::Client::from_env()
                .context("DEEPSEEK_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::Perplexity => {
            let client = perplexity::Client::from_env()
                .context("PERPLEXITY_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::Together => {
            let client = together::Client::from_env()
                .context("TOGETHER_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::XAI => {
            let client =
                xai::Client::from_env().context("XAI_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
        ProviderKind::Ollama => {
            let client = ollama::Client::from_env()
                .context("Failed to initialise Ollama client (check OLLAMA_API_BASE_URL)")?;
            Ok(Arc::new(RigClient::new(
                client.completion_model(model_id),
                config.provider.pricing.clone(),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::edge::Edge;
    use crate::graph::node::{Emit, Node, NodeCtx, NodeError, PortMsg};
    use crate::graph::port::{PortDirection, PortSpecEntry};
    use crate::graph::spec::GraphNodeSpec;
    use async_trait::async_trait;

    struct SourceNode;

    #[async_trait]
    impl Node for SourceNode {
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
                    kind: "Hypotheses".to_string(),
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

    struct SinkNode;

    #[async_trait]
    impl Node for SinkNode {
        fn ports(&self) -> PortSpec {
            PortSpec::new(
                vec![PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Hypotheses".to_string(),
                    required: true,
                }],
                vec![PortSpecEntry {
                    name: "out".into(),
                    direction: PortDirection::Output,
                    kind: "Overview".to_string(),
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

    #[test]
    fn test_build_agent_node_rejects_submit_tool_name() {
        // Regression (M2): a tool named 'submit' would shadow the terminal
        // submit tool and the agent could never terminate.
        let dir = tempfile::TempDir::new().unwrap();
        let graph_path = dir.path().join("graph.yml");
        std::fs::create_dir_all(dir.path().join("prompts")).unwrap();
        std::fs::write(dir.path().join("prompts/g.md"), "p").unwrap();
        std::fs::write(
            &graph_path,
            r#"
name: t
agents:
  - id: gen
    prompt: prompts/g.md
    inputs: [{ port: in, kind: Goal }]
    outputs: [{ port: out, kind: TestOut }]
    output_schema: { type: object }
    tools:
      - name: submit
        description: bad
        command: [echo, hi]
        args_schema: { type: object }
edges: []
"#,
        )
        .unwrap();

        let mut config = EurekaConfig::default();
        config.graph = graph_path.to_string_lossy().to_string();

        let mut session =
            Session::new(config, "00000000-0000-0000-0000-000000000000", None).unwrap();
        let agent_spec = session.manifest.agents.first().unwrap().clone();
        let result = session.build_agent_node(&agent_spec);
        let err = match result {
            Ok(_) => panic!("build_agent_node must reject a tool named 'submit'"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("reserved"),
            "error should explain 'submit' is reserved: {msg}"
        );
    }

    #[test]
    fn test_build_control_node_preserves_bare_interpreter() {
        // Regression: `build_control_node` must NOT rewrite a bare relative
        // `command[0]` (e.g. "python3") to `graph_dir/python3`. The control
        // node spawns with `current_dir = graph_dir`, so PATH lookup handles
        // bare interpreters and relative script args resolve against work_dir.
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return; // python3 not installed; cannot exercise spawn path
        }
        let dir = tempfile::TempDir::new().unwrap();
        let graph_path = dir.path().join("graph.yml");
        std::fs::write(
            &graph_path,
            r#"
name: ctrl-test
control:
  - id: echo
    kind: echo-ctrl
    command: [python3, -c, "print('hi')"]
    inputs: [{ port: in, kind: Goal }]
    outputs: [{ port: out, kind: TestOut }]
edges: []
"#,
        )
        .unwrap();

        let mut config = EurekaConfig::default();
        config.graph = graph_path.to_string_lossy().to_string();

        let session = Session::new(config, "00000000-0000-0000-0000-000000000000", None)
            .expect("session should construct");

        // Use a command that reads stdin and emits a valid emit envelope, so a
        // successful spawn yields `Ok`. With the bug, `command[0]` became
        // `graph_dir/python3` (NotFound) and this would fail.
        let ctrl_spec = session.manifest.control.first().unwrap();
        let mut ctrl_spec = ctrl_spec.clone();
        ctrl_spec.command = vec![
            "python3".to_string(),
            "-c".to_string(),
            "import sys,json; sys.stdin.read(); print(json.dumps({\"port\":\"out\",\"artifact\":{\"kind\":\"TestOut\",\"data\":{}}}))".to_string(),
        ];
        let boxed = session.build_control_node(&ctrl_spec, &serde_json::Value::Null);

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("echo", "echo-ctrl", 0, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact {
                kind: "Goal".to_string(),
                data: serde_json::json!({}),
            },
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(boxed.process(&ctx, vec![msg]));
        assert!(
            result.is_ok(),
            "control node should spawn successfully with bare `python3`; got: {:?}",
            result.err()
        );
        let (emits, _) = result.unwrap();
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].port, "out");
        assert_eq!(emits[0].artifact.kind, "TestOut");
    }

    #[test]
    fn test_control_node_receives_db_path_env() {
        // Regression (E): when a db_path is provided, the control node must
        // expose it to the subprocess as EUREKA_DB_PATH so the Python control
        // nodes can persist state across rounds.
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let graph_path = dir.path().join("graph.yml");
        std::fs::write(
            &graph_path,
            r#"
name: db-test
control:
  - id: probe
    kind: probe-ctrl
    command: [python3, -c, "import os,json; print(json.dumps({\"port\":\"out\",\"artifact\":{\"kind\":\"TestOut\",\"data\":{\"db\":os.environ.get('EUREKA_DB_PATH','')}}}))"]
    inputs: [{ port: in, kind: Goal }]
    outputs: [{ port: out, kind: TestOut }]
edges: []
"#,
        )
        .unwrap();

        let mut config = EurekaConfig::default();
        config.graph = graph_path.to_string_lossy().to_string();

        let db_path = dir.path().join("session.sqlite");
        let mut session = Session::new(
            config,
            "00000000-0000-0000-0000-000000000000",
            Some(db_path.clone()),
        )
        .expect("session should construct");

        let ctrl_spec = session.manifest.control.first().unwrap().clone();
        let boxed = session.build_control_node(&ctrl_spec, &serde_json::Value::Null);
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("probe", "probe-ctrl", 0, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact {
                kind: "Goal".to_string(),
                data: serde_json::json!({}),
            },
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (emits, _) = rt
            .block_on(boxed.process(&ctx, vec![msg]))
            .expect("spawn ok");
        assert_eq!(emits.len(), 1);
        let db = emits[0].artifact.data["db"].as_str().unwrap_or("");
        assert_eq!(db, db_path.to_string_lossy());
    }

    #[test]
    fn test_session_from_parts_validation() {
        let config = EurekaConfig::default();
        let mut nodes = HashMap::new();
        nodes.insert("source".to_string(), BoxedNode::new(SourceNode));
        nodes.insert("sink".to_string(), BoxedNode::new(SinkNode));

        let spec = GraphSpec {
            name: Some("test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "source".into(),
                    kind: "source".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "sink".into(),
                    kind: "sink".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![Edge::new("source", "out", "sink", "in")],
            metadata: serde_json::Value::Null,
        };

        let session = Session::from_parts(config, spec, nodes, uuid::Uuid::nil());
        assert!(
            session.is_ok(),
            "from_parts should succeed: {:?}",
            session.err()
        );
    }

    #[test]
    fn test_session_from_parts_missing_node() {
        let config = EurekaConfig::default();
        let mut nodes = HashMap::new();
        nodes.insert("source".to_string(), BoxedNode::new(SourceNode));

        let spec = GraphSpec {
            name: Some("test".into()),
            description: None,
            nodes: vec![
                GraphNodeSpec {
                    id: "source".into(),
                    kind: "source".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
                GraphNodeSpec {
                    id: "ghost".into(),
                    kind: "missing".into(),
                    config: serde_json::Value::Null,
                    description: None,
                },
            ],
            edges: vec![],
            metadata: serde_json::Value::Null,
        };

        let result = Session::from_parts(config, spec, nodes, uuid::Uuid::nil());
        assert!(
            result.is_err(),
            "from_parts should fail when nodes are missing"
        );
    }
}
