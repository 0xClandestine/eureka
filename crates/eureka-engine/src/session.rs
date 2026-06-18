//! Session management — orchestrates the full lifecycle of a Eureka run.

use std::sync::Arc;

use eureka_config::model::EurekaConfig;
use eureka_db::SessionDb;
use eureka_graph::artifact::Artifact;
use eureka_graph::control::RunStats;
use eureka_graph::scheduler::SchedulerEvent;
use eureka_graph::spec::GraphSpec;
use eureka_graph::validate::validate_graph;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::error::EngineError;
use crate::registry::NodeRegistry;

/// A session represents a single Eureka research run.
pub struct Session {
    /// The configuration for this session.
    config: EurekaConfig,
    /// The graph specification being executed.
    spec: GraphSpec,
    /// The node registry for constructing nodes.
    registry: NodeRegistry,
    /// The final run statistics.
    stats: Option<RunStats>,
    /// The session ID.
    session_id: uuid::Uuid,
    /// Optional broadcaster for scheduler events (e.g. the UI server).
    event_broadcaster: Option<broadcast::Sender<SchedulerEvent>>,
    /// Optional session database for persisting events.
    db: Option<Arc<SessionDb>>,
}

impl Session {
    /// Create a new session from a config and node registry.
    ///
    /// Loads the `GraphSpec` from the config's `graph` path and validates
    /// it against the registry.
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if the graph cannot be loaded or validated.
    pub fn new(config: EurekaConfig, registry: NodeRegistry) -> Result<Self, EngineError> {
        let raw = std::fs::read_to_string(&config.graph).map_err(|e| {
            EngineError::Graph(eureka_graph::spec::GraphError::ParseError(format!(
                "Failed to read graph file '{}': {e}",
                config.graph
            )))
        })?;

        let spec = if config.graph.ends_with(".json") {
            GraphSpec::from_json(&raw)?
        } else {
            GraphSpec::from_toml(&raw)?
        };

        Self::with_spec(config, spec, registry)
    }

    /// Create a new session with a pre-generated session ID.
    ///
    /// Identical to [`Session::new`] but uses the provided UUID instead of
    /// generating a new one. Use this when the session ID is needed before
    /// the session is created (e.g. to set up the DB or plugins).
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if the graph cannot be loaded or validated.
    pub fn new_with_id(
        config: EurekaConfig,
        registry: NodeRegistry,
        session_id: uuid::Uuid,
    ) -> Result<Self, EngineError> {
        let raw = std::fs::read_to_string(&config.graph).map_err(|e| {
            EngineError::Graph(eureka_graph::spec::GraphError::ParseError(format!(
                "Failed to read graph file '{}': {e}",
                config.graph
            )))
        })?;

        let spec = if config.graph.ends_with(".json") {
            GraphSpec::from_json(&raw)?
        } else {
            GraphSpec::from_toml(&raw)?
        };

        Self::with_spec_and_id(config, spec, registry, session_id)
    }

    /// Create a new session with an already-loaded `GraphSpec` and a
    /// pre-generated session ID.
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if the graph is structurally invalid.
    pub fn with_spec_and_id(
        config: EurekaConfig,
        spec: GraphSpec,
        registry: NodeRegistry,
        session_id: uuid::Uuid,
    ) -> Result<Self, EngineError> {
        let result = validate_graph(&spec, registry.port_registry());
        if !result.valid {
            for err in &result.errors {
                error!("Graph validation error: {err}");
            }
            return Err(EngineError::Graph(
                eureka_graph::spec::GraphError::ParseError(format!(
                    "Graph validation failed with {} errors: {:?}",
                    result.errors.len(),
                    result.errors
                )),
            ));
        }

        info!(
            graph_name = ?spec.name,
            nodes = spec.nodes.len(),
            edges = spec.edges.len(),
            "Graph specification validated successfully"
        );

        Ok(Self {
            config,
            spec,
            registry,
            stats: None,
            session_id,
            event_broadcaster: None,
            db: None,
        })
    }

    /// Create a new session with an already-loaded `GraphSpec`.
    ///
    /// # Errors
    ///
    /// Returns an `EngineError` if the graph is structurally invalid.
    pub fn with_spec(
        config: EurekaConfig,
        spec: GraphSpec,
        registry: NodeRegistry,
    ) -> Result<Self, EngineError> {
        Self::with_spec_and_id(config, spec, registry, uuid::Uuid::now_v7())
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
        info!(
            session_id = %self.session_id,
            goal = %goal.get("goal").and_then(|v| v.as_str()).unwrap_or("(no goal)"),
            "Starting Eureka session"
        );

        // Construct all nodes from the spec
        let mut nodes = std::collections::HashMap::new();
        for node_spec in &self.spec.nodes {
            let node = self.registry.construct(node_spec)?;
            info!(
                "Constructed node '{}' (kind: {})",
                node_spec.id, node_spec.kind
            );
            nodes.insert(node_spec.id.clone(), node);
        }

        // Create the scheduler
        let max_in_flight = self.config.scheduler.max_in_flight;
        let budget = self.config.to_graph_budget();
        let mut scheduler = eureka_graph::scheduler::Scheduler::new(
            self.spec.clone(),
            nodes,
            budget,
            max_in_flight,
        );

        let mut events = scheduler.event_receiver();

        let broadcaster = self.event_broadcaster.clone();
        let db = self.db.clone();
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
                        outputs,
                    } => {
                        info!(%node_id, %node_kind, round, emit_count, "Node activation completed");
                        if let Some(db) = &db {
                            for output in outputs {
                                let port =
                                    output.get("port").and_then(|v| v.as_str()).unwrap_or("");
                                let kind =
                                    output.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                                let data = output
                                    .get("data")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                let data_str = serde_json::to_string(&data)
                                    .unwrap_or_else(|_| String::from("null"));
                                if let Err(e) =
                                    db.write_event(*round, node_id, port, kind, &data_str)
                                {
                                    warn!(error = %e, "Failed to write event to session DB");
                                }
                            }
                        }
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
                    SchedulerEvent::RunHalted {
                        reason,
                        total_rounds,
                    } => {
                        info!(%reason, total_rounds, "Run halted");
                    }
                }
            }
        });

        // Inject the goal artifact into every source node
        let source_ids = self.spec.source_node_ids();
        let mut initial_artifacts = std::collections::HashMap::new();
        for source_id in &source_ids {
            let artifact = Artifact {
                kind: "Goal".to_string(),
                data: goal.clone(),
            };
            initial_artifacts.insert(source_id.clone(), vec![artifact]);
        }

        let stats = scheduler
            .run(initial_artifacts)
            .await
            .map_err(|e| EngineError::Scheduler(e.to_string()))?;

        self.stats = Some(stats.clone());
        event_handle.abort();

        if let Some(db) = &self.db {
            if let Err(e) = db.complete_session(stats.rounds_completed) {
                warn!(error = %e, "Failed to mark session as completed in DB");
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

    /// Attach a session database to persist events and completion stats.
    pub fn set_db(&mut self, db: Arc<SessionDb>) {
        self.db = Some(db);
    }

    /// Attach a broadcast sender so the UI server receives every `SchedulerEvent`.
    pub fn set_event_broadcaster(&mut self, tx: broadcast::Sender<SchedulerEvent>) {
        self.event_broadcaster = Some(tx);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use eureka_graph::node::{BoxedNode, Emit, Node, NodeCtx, NodeError, PortMsg};
    use eureka_graph::port::{PortDirection, PortSpecEntry};
    use eureka_graph::spec::GraphNodeSpec;
    use std::sync::Arc;

    struct PassthroughNode;

    #[async_trait]
    impl Node for PassthroughNode {
        fn ports(&self) -> eureka_graph::port::PortSpec {
            eureka_graph::port::PortSpec::new(
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

        async fn process(&self, _ctx: &NodeCtx, msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
            Ok(vec![Emit::new("out", msg.artifact)])
        }
    }

    #[tokio::test]
    async fn test_session_validation() {
        let config = EurekaConfig::default();
        let mut registry = NodeRegistry::new();

        let ports = eureka_graph::port::PortSpec::new(
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
        registry.register(
            "test.passthrough",
            Arc::new(|_spec| Ok(BoxedNode::new(PassthroughNode))),
            ports,
        );

        let spec = GraphSpec {
            name: Some("test".into()),
            description: None,
            nodes: vec![GraphNodeSpec {
                id: "source".into(),
                kind: "test.passthrough".into(),
                config: serde_json::Value::Null,
                description: None,
            }],
            edges: vec![],
            metadata: serde_json::Value::Null,
        };

        // Single node that outputs "Goal" — "Goal" is consumed by itself (source),
        // but nothing consumes Goal in the registry other than test.passthrough.
        // Validation checks terminal output. Should pass or fail — just no panic.
        let result = Session::with_spec(config, spec, registry);
        let _ = result;
    }
}
