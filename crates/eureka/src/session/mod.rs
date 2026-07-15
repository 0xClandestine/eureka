//! Session management — orchestrates the full lifecycle of a Eureka run.
//!
//! A session loads the single YAML manifest (`config.graph`) once, derives
//! both the graph topology and the node implementations from it — no separate
//! node registry needed.

mod builder;
mod runner;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent::LlmClient;
use crate::config::{EurekaConfig, RunStats};
use crate::error::EngineError;
use crate::graph::node::BoxedNode;
use crate::graph::port::PortSpec;
use crate::graph::spec::{GraphError, GraphSpec};
use crate::graph::validate::{validate_graph, PortRegistry};
use crate::manifest::GraphManifest;
use crate::persistence::{CheckpointStore, EventStore, RunCheckpoint};
use crate::scheduler::{SchedulerEvent, SchedulerSignal};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

/// A session represents a single Eureka research run.
pub struct Session {
    /// The configuration for this session.
    pub(super) config: EurekaConfig,
    /// The graph specification being executed (derived from manifest).
    pub(super) spec: GraphSpec,
    /// The parsed manifest — used for node construction during `run()`.
    pub(super) manifest: GraphManifest,
    /// The graph directory (parent of config.graph).
    pub(super) graph_dir: PathBuf,
    /// The session ID.
    #[allow(clippy::struct_field_names)]
    pub(super) session_id: uuid::Uuid,
    /// Optional path to the session database (passed to control nodes).
    pub(super) db_path: Option<PathBuf>,
    /// Cached LLM clients, keyed by model ID.
    pub(super) client_cache: HashMap<String, Arc<dyn LlmClient>>,
    /// Default model ID for agents without a per-agent override.
    pub(super) default_model: String,
    /// The final run statistics.
    pub(super) stats: Option<RunStats>,
    /// Broadcast sender for scheduler events (for UI/observability).
    pub(super) event_broadcaster: Option<broadcast::Sender<SchedulerEvent>>,
    /// Node overrides for testing — keyed by node ID.
    pub(super) node_overrides: HashMap<String, BoxedNode>,
    /// Optional durable scheduler checkpoint backend.
    pub(super) checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    /// Optional durable scheduler event backend.
    pub(super) event_store: Option<Arc<dyn EventStore>>,
    /// Signal sender for the active scheduler, when a run is executing.
    pub(super) scheduler_signal: Option<mpsc::Sender<SchedulerSignal>>,
    /// Optional shared sink used by a run manager to observe the active sender.
    pub(super) scheduler_signal_sink:
        Option<Arc<tokio::sync::Mutex<Option<mpsc::Sender<SchedulerSignal>>>>>,
    /// Optional RAG vector index for query-time retrieval (attached to agents).
    pub(super) rag_index: Option<crate::rag::RagIndexHandle>,
    /// Optional RAG indexer for post-activation document embedding.
    pub(super) rag_indexer: Option<Arc<crate::rag::RagIndexer>>,
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
                    .map(ToString::to_string)
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
        if let Ok(client) = builder::build_client(&config, &default_model, None, Vec::new()) {
            client_cache.insert(default_model.clone(), client);
        }

        let session_id = uuid::Uuid::parse_str(session_id)
            .map_err(|e| EngineError::Run(format!("invalid session UUID '{session_id}': {e}")))?;

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
            event_store: None,
            scheduler_signal: None,
            scheduler_signal_sink: None,
            rag_index: None,
            rag_indexer: None,
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
                    .map(ToString::to_string)
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
            event_store: None,
            scheduler_signal: None,
            rag_index: None,
            rag_indexer: None,
            scheduler_signal_sink: None,
        })
    }

    /// Attach a broadcast sender so the UI server receives every `SchedulerEvent`.
    pub fn set_event_broadcaster(&mut self, tx: broadcast::Sender<SchedulerEvent>) {
        self.event_broadcaster = Some(tx);
    }

    /// Load the latest durable checkpoint for this session.
    ///
    /// # Errors
    /// Returns `EngineError::Store` on persistence failures.
    pub async fn latest_checkpoint(&self) -> Result<Option<RunCheckpoint>, EngineError> {
        let Some(store) = &self.checkpoint_store else {
            return Ok(None);
        };
        store
            .load_checkpoint(self.session_id)
            .await
            .map_err(|error| EngineError::Store(error.to_string()))
    }

    /// Configure durable scheduler checkpoints for this session.
    pub fn set_checkpoint_store(&mut self, store: Arc<dyn CheckpointStore>) {
        self.checkpoint_store = Some(store);
    }

    /// Attach durable scheduler event storage.
    pub fn set_event_store(&mut self, store: Arc<dyn EventStore>) {
        self.event_store = Some(store);
    }

    /// Install a shared sink for application run managers.
    pub fn set_scheduler_signal_sink(
        &mut self,
        sink: Arc<tokio::sync::Mutex<Option<mpsc::Sender<SchedulerSignal>>>>,
    ) {
        self.scheduler_signal_sink = Some(sink);
    }

    /// Validate an artifact kind against a manifest input port.
    ///
    /// # Errors
    /// Returns `EngineError::Run` if the node ID is unknown or the port spec
    /// cannot be found.
    pub fn validate_input(&self, node_id: &str, port: &str, kind: &str) -> Result<(), EngineError> {
        let registry = build_port_registry(&self.manifest);
        let Some(node_spec) = self.spec.nodes.iter().find(|node| node.id == node_id) else {
            return Err(EngineError::Run(format!("unknown node '{node_id}'")));
        };
        let Some(spec) = registry.get(&node_spec.kind) else {
            return Err(EngineError::Run(format!(
                "unknown node kind '{}'",
                node_spec.kind
            )));
        };
        let Some(expected) = spec.input_kind(port) else {
            return Err(EngineError::Run(format!(
                "unknown input port '{node_id}.{port}'"
            )));
        };
        if expected != kind {
            return Err(EngineError::Graph(GraphError::PortKindMismatch(format!(
                "{node_id}.{port} expects {expected}, received {kind}"
            ))));
        }
        Ok(())
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
    ///
    /// # Errors
    /// Returns `EngineError::Scheduler` on runtime errors (budget exceeded,
    /// node failure), or `EngineError::Store` on persistence failures.
    pub async fn run_with_store(
        &mut self,
        goal: serde_json::Value,
        store: Option<&dyn crate::persistence::RunStore>,
    ) -> Result<RunStats, EngineError> {
        self.run_internal(goal, store, None).await
    }

    /// Resume this session from a previously persisted scheduler checkpoint.
    ///
    /// The checkpoint must have been created for the same graph and runtime
    /// configuration. The scheduler validates both identities before dispatch.
    ///
    /// # Errors
    /// Same as [`run_with_store`](Self::run_with_store).
    pub async fn resume_with_store(
        &mut self,
        goal: serde_json::Value,
        store: Option<&dyn crate::persistence::RunStore>,
        checkpoint: RunCheckpoint,
    ) -> Result<RunStats, EngineError> {
        self.run_internal(goal, store, Some(checkpoint)).await
    }
}

/// Build a [`PortRegistry`] from the agents and control nodes declared in a
/// [`GraphManifest`]. Used for graph validation.
fn build_port_registry(manifest: &GraphManifest) -> PortRegistry {
    let mut reg = PortRegistry::new();
    for agent in &manifest.agents {
        reg.register(
            agent.id.clone(),
            PortSpec::from_defs(&agent.inputs, &agent.outputs),
        );
    }
    for ctrl in &manifest.control {
        reg.register(
            ctrl.kind.clone(),
            PortSpec::from_defs(&ctrl.inputs, &ctrl.outputs),
        );
    }
    reg
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    #[tokio::test]
    async fn test_build_agent_node_rejects_submit_tool_name() {
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
        let result = session.build_agent_node(&agent_spec).await;
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
            artifact: crate::graph::artifact::Artifact {
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
            artifact: crate::graph::artifact::Artifact {
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
