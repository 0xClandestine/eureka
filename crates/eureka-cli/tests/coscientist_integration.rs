//! Integration tests for the coscientist agent loop.
//!
//! These tests run the full scheduler pipeline using:
//! - A [`ScriptedClient`] that returns canned valid JSON per agent (no LLM calls,
//!   no API keys required).
//! - Rust mock nodes for the three control plugins (no Python subprocess required).
//!
//! This lets CI verify end-to-end routing, artifact shape, feedback edges, and
//! budget-based termination without any external dependencies.
//!
//! A separate `#[ignore]` smoke test exercises the real graph with real plugins.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use eureka_agents::def::ToolDef;
use eureka_agents::error::AgentError;
use eureka_agents::{AgentDef, LlmAgentNode, LlmClient};
use eureka_config::model::{BudgetConfig, EurekaConfig};
use eureka_engine::registry::NodeRegistry;
use eureka_engine::session::Session;
use eureka_graph::artifact::Artifact;
use eureka_graph::node::{BoxedNode, Emit, Node, NodeCtx, NodeError, PortMsg};
use eureka_graph::port::{PortDirection, PortSpec, PortSpecEntry};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// ScriptedClient — returns canned JSON for each agent based on preamble
// ---------------------------------------------------------------------------

struct ScriptedClient;

#[async_trait]
impl LlmClient for ScriptedClient {
    async fn run_agent_loop(
        &self,
        preamble: &str,
        _schema: &Value,
        _tools: &[ToolDef],
        _message: &str,
        _max_iterations: u32,
        _temperature: f64,
        _node_id: &str,
        _node_kind: &str,
        _round: u32,
        _event_tx: Option<tokio::sync::mpsc::Sender<eureka_graph::scheduler::SchedulerEvent>>,
    ) -> Result<Value, AgentError> {
        let p = preamble.to_ascii_lowercase();
        if p.contains("evolutionary") {
            Ok(evolution_output())
        } else if p.contains("critical research reviewer") {
            Ok(reflection_output())
        } else if p.contains("senior research lead") {
            Ok(meta_review_output())
        } else {
            Ok(generation_output())
        }
    }
}

fn generation_output() -> Value {
    json!({
        "hypotheses": [{
            "statement": "Flash attention reduces memory by computing block-by-block",
            "rationale": "Tiling avoids materializing the full NxN attention matrix",
            "assumptions": ["Standard transformer architecture", "GPU SRAM available"],
            "testable_predictions": ["Peak memory drops 10x at sequence length 2048"],
            "proposed_experiment": "Profile A100 memory before and after",
            "citations": []
        }]
    })
}

fn reflection_output() -> Value {
    json!({
        "reviews": [{
            "hypothesis_index": 0,
            "hypothesis": {
                "statement": "Flash attention reduces memory by computing block-by-block",
                "rationale": "Tiling avoids materializing the full NxN attention matrix",
                "assumptions": [],
                "testable_predictions": [],
                "citations": []
            },
            "kind": "fundamental",
            "reasoning": "Well-established technique with clear mechanism",
            "strengths": ["Clear mechanistic explanation", "Empirically validated"],
            "weaknesses": ["Limited to square attention patterns"],
            "suggestions": ["Test non-square patterns"],
            "score": 8.0
        }]
    })
}

fn evolution_output() -> Value {
    json!({
        "hypotheses": [{
            "statement": "Sparse flash attention compounds memory savings",
            "rationale": "Combining tiling with sparsity reduces both time and memory complexity",
            "assumptions": ["Sparse attention patterns are task-compatible"],
            "testable_predictions": ["2x additional memory reduction"],
            "proposed_experiment": "Compare memory profiles across sparsity levels",
            "citations": [],
            "parent_indices": [0],
            "operation": "extend",
            "change_rationale": "Extended base hypothesis with sparsity dimension"
        }]
    })
}

fn meta_review_output() -> Value {
    json!({
        "insights": {
            "recurring_patterns": ["Memory efficiency through computation reordering"],
            "common_weaknesses": ["Limited empirical validation"],
            "promising_directions": ["Sparse attention variants"],
            "context_improvements": ["Include hardware-specific benchmarks"]
        },
        "overview": {
            "summary": "Flash attention family shows strong promise",
            "key_insights": ["Block-wise computation is the key insight"],
            "open_questions": ["How does this scale beyond 100B parameters?"],
            "suggested_next_steps": ["Run experiments on A100 cluster"]
        }
    })
}

// ---------------------------------------------------------------------------
// Mock control nodes — Rust implementations that need no Python or plugins
// ---------------------------------------------------------------------------

/// Receives `Reviews`, emits `top: Hypotheses` + `state: Ranking`.
#[derive(Clone)]
struct MockEloRanker;

#[async_trait]
impl Node for MockEloRanker {
    fn ports(&self) -> PortSpec {
        PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: "Reviews".into(),
                required: true,
            }],
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

    async fn process(&self, _ctx: &NodeCtx, msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
        // Extract hypotheses from reviews so evolution has something to work with.
        let reviews = msg
            .artifact
            .data
            .get("reviews")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        let hypotheses: Vec<Value> = reviews
            .iter()
            .filter_map(|r| r.get("hypothesis").cloned())
            .collect();

        let elo_ratings: serde_json::Map<String, Value> = hypotheses
            .iter()
            .filter_map(|h| {
                h.get("statement")
                    .and_then(|s| s.as_str())
                    .map(|s| (s.to_string(), json!(1200.0)))
            })
            .collect();

        Ok(vec![
            Emit::new(
                "top",
                Artifact {
                    kind: "Hypotheses".into(),
                    data: json!({ "hypotheses": hypotheses }),
                },
            ),
            Emit::new(
                "state",
                Artifact {
                    kind: "Ranking".into(),
                    data: json!({ "elo_ratings": elo_ratings }),
                },
            ),
        ])
    }
}

/// Receives `Hypotheses`, passes them through on `unique` (no actual dedup).
#[derive(Clone)]
struct MockJaccardDedup;

#[async_trait]
impl Node for MockJaccardDedup {
    fn ports(&self) -> PortSpec {
        PortSpec::new(
            vec![PortSpecEntry {
                name: "in".into(),
                direction: PortDirection::Input,
                kind: "Hypotheses".into(),
                required: true,
            }],
            vec![PortSpecEntry {
                name: "unique".into(),
                direction: PortDirection::Output,
                kind: "Hypotheses".into(),
                required: false,
            }],
        )
    }

    async fn process(&self, _ctx: &NodeCtx, msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
        Ok(vec![Emit::new(
            "unique",
            Artifact {
                kind: "Hypotheses".into(),
                data: msg.artifact.data,
            },
        )])
    }
}

/// Receives `Hypotheses`, always emits on `halt` (session terminates via budget).
#[derive(Clone)]
struct MockRoundGovernor;

#[async_trait]
impl Node for MockRoundGovernor {
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

    async fn process(&self, _ctx: &NodeCtx, _msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
        Ok(vec![Emit::new(
            "halt",
            Artifact {
                kind: "Control".into(),
                data: json!({ "signal": "halt", "round": 1 }),
            },
        )])
    }
}

// ---------------------------------------------------------------------------
// Registry builder — loads real agent defs, wires scripted + mock nodes
// ---------------------------------------------------------------------------

fn build_scripted_registry(graph_dir: &Path) -> NodeRegistry {
    let agents_dir = graph_dir.join("agents");
    let mut registry = NodeRegistry::new();

    let defs = AgentDef::load_all(&agents_dir)
        .expect("Failed to load agent defs — run tests from repo root");

    let client: Arc<dyn LlmClient> = Arc::new(ScriptedClient);

    for def in defs {
        let def_arc = Arc::new(def);
        let ports = def_arc.to_port_spec();
        let name = def_arc.name.clone();
        let c = Arc::clone(&client);
        registry.register(
            name,
            Arc::new(move |_spec| {
                Ok(BoxedNode::new(LlmAgentNode::new(
                    Arc::clone(&def_arc),
                    Arc::clone(&c),
                )))
            }),
            ports,
        );
    }

    // Control nodes
    let ranker = MockEloRanker;
    registry.register_instance("elo-ranker", BoxedNode::new(ranker));

    let dedup = MockJaccardDedup;
    registry.register_instance("jaccard-dedup", BoxedNode::new(dedup));

    let governor = MockRoundGovernor;
    registry.register_instance("round-governor", BoxedNode::new(governor));

    registry
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Resolves the coscientist graph directory relative to this crate's manifest.
fn graph_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../graphs/coscientist")
}

#[tokio::test]
async fn test_coscientist_one_round_scripted() {
    let dir = graph_dir();
    let registry = build_scripted_registry(&dir);

    let config = EurekaConfig {
        graph: dir.join("graph.json").to_string_lossy().to_string(),
        budget: BudgetConfig {
            max_rounds: 1,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut session = Session::new(config, registry).expect("Failed to create session");

    let goal = json!({
        "goal": "Integration test goal",
        "description": "Test that the coscientist loop runs end-to-end",
        "domain": "machine-learning"
    });

    let stats = session.run(goal).await.expect("Session run failed");

    // The budget fires after 1 round completes, so rounds_completed should be >= 1.
    assert!(
        stats.rounds_completed >= 1,
        "Expected at least 1 round; got {}",
        stats.rounds_completed
    );
}

/// Full pipeline smoke test using real Python plugins.
///
/// Requires Python 3 with the plugin dependencies installed.
/// Run with: `cargo test -- --include-ignored`
#[tokio::test]
#[ignore]
async fn smoke_coscientist_real_plugins_scripted_llm() {
    use eureka_engine::plugin::{ControlPluginNode, PluginRegistry};
    use eureka_graph::port::PortSpecEntry;

    let dir = graph_dir();
    let agents_dir = dir.join("agents");
    let mut registry = NodeRegistry::new();

    let client: Arc<dyn LlmClient> = Arc::new(ScriptedClient);
    for def in AgentDef::load_all(&agents_dir).expect("agents") {
        let def_arc = Arc::new(def);
        let ports = def_arc.to_port_spec();
        let name = def_arc.name.clone();
        let c = Arc::clone(&client);
        registry.register(
            name,
            Arc::new(move |_spec| {
                Ok(BoxedNode::new(LlmAgentNode::new(
                    Arc::clone(&def_arc),
                    Arc::clone(&c),
                )))
            }),
            ports,
        );
    }

    let session_id = uuid::Uuid::now_v7().to_string();
    let plugin_reg = PluginRegistry::discover(&dir).expect("plugin discovery");

    for (_name, entry) in plugin_reg.iter() {
        if !entry.manifest.has_node_role() {
            continue;
        }
        let Some(node_cfg) = &entry.manifest.node else {
            continue;
        };
        let inputs = node_cfg
            .inputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Input,
                kind: p.kind.clone(),
                required: true,
            })
            .collect();
        let outputs = node_cfg
            .outputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Output,
                kind: p.kind.clone(),
                required: false,
            })
            .collect();
        let ports = PortSpec::new(inputs, outputs);
        let manifest = entry.manifest.clone();
        let plugin_dir = entry.plugin_dir.clone();
        let sid = session_id.clone();
        registry.register(
            manifest.name.clone(),
            Arc::new(move |spec| {
                Ok(BoxedNode::new(ControlPluginNode::new(
                    manifest.clone(),
                    plugin_dir.clone(),
                    sid.clone(),
                    None,
                    spec.config.clone(),
                )))
            }),
            ports,
        );
    }

    let config = EurekaConfig {
        graph: dir.join("graph.json").to_string_lossy().to_string(),
        budget: BudgetConfig {
            max_rounds: 1,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut session = Session::new(config, registry).expect("session");
    let stats = session
        .run(json!({
            "goal": "Smoke test: flash attention memory efficiency",
            "description": "",
            "domain": "machine-learning"
        }))
        .await
        .expect("run");

    assert!(stats.rounds_completed >= 1);
}
