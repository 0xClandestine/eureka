//! End-to-end validation coverage for the shipped co-scientist graph.

// Integration tests use unwrap/expect liberally; the workspace deny policy
// for unwrap_used/expect_used applies to production code, not tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use eureka::agent::error::AgentError;
use eureka::agent::{AgentDef, LlmAgentNode, LlmClient, PortDef, ToolDef};
use eureka::config::{EmbeddingProvider, EurekaConfig};
use eureka::graph::node::{BoxedNode, Emit, Node, NodeCtx, NodeError, PortMsg};
use eureka::graph::port::{PortDirection, PortSpec, PortSpecEntry};
use eureka::graph::validate::{validate_graph, PortRegistry};
use eureka::manifest::GraphManifest;
use eureka::scheduler::SchedulerEvent;
use serde_json::{json, Value};

fn graph_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../example")
}

struct MockRanker;
#[async_trait]
impl Node for MockRanker {
    fn ports(&self) -> PortSpec {
        PortSpec::new(
            vec![
                PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Reviews".into(),
                    required: true,
                },
                PortSpecEntry {
                    name: "graph".into(),
                    direction: PortDirection::Input,
                    kind: "ProximityGraph".into(),
                    required: false,
                },
            ],
            vec![
                PortSpecEntry {
                    name: "top".into(),
                    direction: PortDirection::Output,
                    kind: "Hypotheses".into(),
                    required: false,
                },
                PortSpecEntry {
                    name: "state".into(),
                    direction: PortDirection::Output,
                    kind: "Ranking".into(),
                    required: false,
                },
            ],
        )
    }
    async fn process(
        &self,
        _: &NodeCtx,
        _: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, eureka::graph::node::NodeUsage), NodeError> {
        Ok((vec![], eureka::graph::node::NodeUsage::default()))
    }
}

struct MockProx;
#[async_trait]
impl Node for MockProx {
    fn ports(&self) -> PortSpec {
        PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: "Hypotheses".into(),
                required: true,
            }],
            vec![
                PortSpecEntry {
                    name: "unique".into(),
                    direction: PortDirection::Output,
                    kind: "Hypotheses".into(),
                    required: false,
                },
                PortSpecEntry {
                    name: "graph".into(),
                    direction: PortDirection::Output,
                    kind: "ProximityGraph".into(),
                    required: false,
                },
            ],
        )
    }
    async fn process(
        &self,
        _: &NodeCtx,
        _: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, eureka::graph::node::NodeUsage), NodeError> {
        Ok((vec![], eureka::graph::node::NodeUsage::default()))
    }
}

struct MockScatter;
#[async_trait]
impl Node for MockScatter {
    fn ports(&self) -> PortSpec {
        PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: "Hypotheses".into(),
                required: true,
            }],
            vec![PortSpecEntry {
                name: "item".into(),
                direction: PortDirection::Output,
                kind: "HypothesisItem".into(),
                required: false,
            }],
        )
    }
    async fn process(
        &self,
        _: &NodeCtx,
        _: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, eureka::graph::node::NodeUsage), NodeError> {
        Ok((vec![], eureka::graph::node::NodeUsage::default()))
    }
}

struct MockGather;
#[async_trait]
impl Node for MockGather {
    fn ports(&self) -> PortSpec {
        PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: "ReviewItem".into(),
                required: true,
            }],
            vec![PortSpecEntry {
                name: "out".into(),
                direction: PortDirection::Output,
                kind: "Reviews".into(),
                required: false,
            }],
        )
    }
    async fn process(
        &self,
        _: &NodeCtx,
        _: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, eureka::graph::node::NodeUsage), NodeError> {
        Ok((vec![], eureka::graph::node::NodeUsage::default()))
    }
}

struct MockSup;
#[async_trait]
impl Node for MockSup {
    fn ports(&self) -> PortSpec {
        PortSpec::new(
            vec![
                PortSpecEntry {
                    name: "in".into(),
                    direction: PortDirection::Input,
                    kind: "Hypotheses".into(),
                    required: true,
                },
                PortSpecEntry {
                    name: "ranking".into(),
                    direction: PortDirection::Input,
                    kind: "Ranking".into(),
                    required: false,
                },
            ],
            vec![
                PortSpecEntry {
                    name: "continue".into(),
                    direction: PortDirection::Output,
                    kind: "Hypotheses".into(),
                    required: false,
                },
                PortSpecEntry {
                    name: "halt".into(),
                    direction: PortDirection::Output,
                    kind: "Control".into(),
                    required: false,
                },
            ],
        )
    }
    async fn process(
        &self,
        _: &NodeCtx,
        _: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, eureka::graph::node::NodeUsage), NodeError> {
        Ok((vec![], eureka::graph::node::NodeUsage::default()))
    }
}

struct DummyClient;
#[async_trait]
impl LlmClient for DummyClient {
    async fn run_agent_loop(
        &self,
        _: &str,
        _: &Value,
        _: &[ToolDef],
        _: &str,
        _: u32,
        _: f64,
        _: &str,
        _: &str,
        _: u32,
        _: &str,
        _: Option<tokio::sync::mpsc::Sender<SchedulerEvent>>,
    ) -> Result<(Value, eureka::graph::node::NodeUsage), AgentError> {
        Ok((json!({}), eureka::graph::node::NodeUsage::default()))
    }
}

#[test]
fn test_coscientist_validates() {
    let dir = graph_dir();
    let manifest =
        GraphManifest::load(&dir.join("coscientist.yml")).expect("Failed to load manifest");
    let spec = manifest.to_graph_spec();
    let client: Arc<dyn LlmClient> = Arc::new(DummyClient);
    let mut nodes: HashMap<String, BoxedNode> = HashMap::new();

    for agent_spec in &manifest.agents {
        let pc = agent_spec.prompt.read().unwrap();
        let def = AgentDef {
            name: agent_spec.id.clone(),
            description: agent_spec.description.clone(),
            preamble: pc,
            inputs: agent_spec
                .inputs
                .iter()
                .map(|p| PortDef {
                    kind: p.kind.clone(),
                    port: p.port.clone(),
                    ..Default::default()
                })
                .collect(),
            outputs: agent_spec
                .outputs
                .iter()
                .map(|p| PortDef {
                    kind: p.kind.clone(),
                    port: p.port.clone(),
                    ..Default::default()
                })
                .collect(),
            config: eureka::config::AgentConfig::default(),
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
            mcp_servers: vec![],
        };
        nodes.insert(
            agent_spec.id.clone(),
            BoxedNode::new(LlmAgentNode::new(
                Arc::new(def),
                Arc::clone(&client),
                std::path::PathBuf::from("."),
            )),
        );
    }
    nodes.insert("ranking".to_string(), BoxedNode::new(MockRanker));
    nodes.insert("proximity".to_string(), BoxedNode::new(MockProx));
    nodes.insert("supervisor".to_string(), BoxedNode::new(MockSup));
    nodes.insert("scatter".to_string(), BoxedNode::new(MockScatter));
    nodes.insert("gather".to_string(), BoxedNode::new(MockGather));

    let mut registry = PortRegistry::new();
    for (node_id, node) in &nodes {
        registry.register(node_id.clone(), node.ports());
    }
    for node_spec in &spec.nodes {
        if let Some(node) = nodes.get(&node_spec.id) {
            registry.register(node_spec.kind.clone(), node.ports());
        }
    }

    let result = validate_graph(&spec, &registry);
    if !result.valid {
        for e in &result.errors {
            eprintln!("VALIDATION ERROR: {e}");
        }
    }
    assert!(result.valid, "Validation failed");
}

/// Verify the shipped `eureka.toml` parses cleanly and the RAG section is
/// correctly configured for OpenRouter + Gemini embeddings.
#[test]
fn test_coscientist_config_loads() {
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eureka.toml");
    let config = EurekaConfig::load(Some(&config_path)).expect("Failed to load eureka.toml");

    // RAG must be enabled with the OpenRouter Gemini embedding model.
    let rag = config.rag.expect("rag section must be present");
    assert!(rag.enabled, "rag.enabled must be true");
    assert_eq!(rag.embedding_provider, EmbeddingProvider::OpenRouter);
    assert_eq!(rag.embedding_model, "google/gemini-embedding-2");
    assert!(rag.top_k > 0, "rag.top_k must be positive");

    // Per-hypothesis parallelism is provided by scatter/gather, not workers.
    // All agents should have workers == 1 (the default) so there is no
    // cross-product fan-in collision at downstream single-replica nodes.
    let gen_workers = config
        .agent_overrides
        .get("generation")
        .and_then(|o| o.workers)
        .unwrap_or(config.agent.workers);
    assert_eq!(gen_workers, 1, "generation workers must be 1; scatter provides parallelism");
    let evo_workers = config
        .agent_overrides
        .get("evolution")
        .and_then(|o| o.workers)
        .unwrap_or(config.agent.workers);
    assert_eq!(evo_workers, 1, "evolution workers must be 1; scatter provides parallelism");
}

/// The generation and evolution output schemas must declare `hypotheses` as an
/// array so the workers scatter-gather merge (which concatenates arrays) works
/// correctly and each worker contributes exactly one item.
#[test]
fn test_agent_output_schemas() {
    let dir = graph_dir();
    let manifest =
        GraphManifest::load(&dir.join("coscientist.yml")).expect("Failed to load manifest");

    for id in ["generation", "evolution"] {
        let agent = manifest.agents.iter().find(|a| a.id == id).unwrap_or_else(|| {
            panic!("agent '{id}' not found in manifest");
        });
        let schema = &agent.output_schema;
        let hyp_type = schema["properties"]["hypotheses"]["type"]
            .as_str()
            .unwrap_or_else(|| panic!("'{id}' output_schema.hypotheses must have a type"));
        assert_eq!(
            hyp_type, "array",
            "'{id}' output_schema.hypotheses must be an array so parallel workers can be merged"
        );
        let item_props = &schema["properties"]["hypotheses"]["items"]["properties"];
        assert!(
            item_props["statement"].is_object(),
            "'{id}' hypothesis items must have a 'statement' field"
        );
    }
}

/// Regression guard: the generation prompt must instruct the agent to produce
/// a batch of hypotheses (5–8) so that scatter has a meaningful array to fan
/// out. The evolution prompt still produces an array of evolved variants.
#[test]
fn test_batch_hypothesis_prompts() {
    let dir = graph_dir();

    let gen = std::fs::read_to_string(dir.join("prompts/generation.md"))
        .expect("Failed to read prompts/generation.md");
    assert!(
        gen.contains("5 and 8") || gen.contains("5–8") || gen.contains("5-8"),
        "generation prompt must instruct a batch of 5–8 hypotheses for scatter fan-out"
    );

    // Evolution still produces an array; verify the output schema supports it.
    let manifest = GraphManifest::load(&dir.join("coscientist.yml"))
        .expect("Failed to load manifest");
    let evo = manifest.agents.iter().find(|a| a.id == "evolution")
        .expect("evolution agent must exist");
    assert_eq!(
        evo.output_schema["properties"]["hypotheses"]["type"].as_str(),
        Some("array"),
        "evolution output schema must declare hypotheses as an array"
    );
}

/// All nine nodes declared in the manifest must be registered (no orphan node).
#[test]
fn test_all_manifest_nodes_present() {
    let dir = graph_dir();
    let manifest =
        GraphManifest::load(&dir.join("coscientist.yml")).expect("Failed to load manifest");
    let spec = manifest.to_graph_spec();

    let agent_ids: std::collections::HashSet<_> = manifest.agents.iter().map(|a| &a.id).collect();
    let control_ids: std::collections::HashSet<_> =
        manifest.control.iter().map(|c| &c.id).collect();

    for node in &spec.nodes {
        assert!(
            agent_ids.contains(&node.id) || control_ids.contains(&node.id),
            "node '{}' in spec is not declared in agents or control_nodes",
            node.id
        );
    }
}
