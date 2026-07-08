// Integration tests use unwrap/expect liberally; the workspace deny policy
// for unwrap_used/expect_used applies to production code, not tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use eureka::agents::error::AgentError;
use eureka::agents::{AgentDef, AgentPort, LlmAgentNode, LlmClient, ToolDef};
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
    fn ports(&self) -> PortSpec { PortSpec::new(
        vec![
            PortSpecEntry{name:"in".into(),direction:PortDirection::Input,kind:"Reviews".into(),required:true},
            PortSpecEntry{name:"graph".into(),direction:PortDirection::Input,kind:"ProximityGraph".into(),required:false},
        ],
        vec![
            PortSpecEntry{name:"top".into(),direction:PortDirection::Output,kind:"Hypotheses".into(),required:false},
            PortSpecEntry{name:"state".into(),direction:PortDirection::Output,kind:"Ranking".into(),required:false},
        ],
    )}
    async fn process(&self,_:&NodeCtx,_:PortMsg)->Result<Vec<Emit>,NodeError>{Ok(vec![])}
}

struct MockProx;
#[async_trait]
impl Node for MockProx {
    fn ports(&self) -> PortSpec { PortSpec::new(
        vec![PortSpecEntry{name:"in".into(),direction:PortDirection::Input,kind:"Hypotheses".into(),required:true}],
        vec![
            PortSpecEntry{name:"unique".into(),direction:PortDirection::Output,kind:"Hypotheses".into(),required:false},
            PortSpecEntry{name:"graph".into(),direction:PortDirection::Output,kind:"ProximityGraph".into(),required:false},
        ],
    )}
    async fn process(&self,_:&NodeCtx,_:PortMsg)->Result<Vec<Emit>,NodeError>{Ok(vec![])}
}

struct MockSup;
#[async_trait]
impl Node for MockSup {
    fn ports(&self) -> PortSpec { PortSpec::new(
        vec![PortSpecEntry{name:"in".into(),direction:PortDirection::Input,kind:"Hypotheses".into(),required:true}],
        vec![
            PortSpecEntry{name:"continue".into(),direction:PortDirection::Output,kind:"Hypotheses".into(),required:false},
            PortSpecEntry{name:"halt".into(),direction:PortDirection::Output,kind:"Control".into(),required:false},
        ],
    )}
    async fn process(&self,_:&NodeCtx,_:PortMsg)->Result<Vec<Emit>,NodeError>{Ok(vec![])}
}

struct DummyClient;
#[async_trait]
impl LlmClient for DummyClient {
    async fn run_agent_loop(&self,_:&str,_:&Value,_:&[ToolDef],_:&str,_:u32,_:f64,_:&str,_:&str,_:u32,_:&str,_:Option<tokio::sync::mpsc::Sender<SchedulerEvent>>)->Result<Value,AgentError>{Ok(json!({}))}
}

#[test]
fn test_coscientist_validates() {
    let dir = graph_dir();
    let manifest = GraphManifest::load(&dir.join("coscientist.yml"))
        .expect("Failed to load manifest");
    let spec = manifest.to_graph_spec();
    let client: Arc<dyn LlmClient> = Arc::new(DummyClient);
    let mut nodes: HashMap<String, BoxedNode> = HashMap::new();

    for agent_spec in &manifest.agents {
        let pc = agent_spec.prompt.read().unwrap();
        let def = AgentDef {
            name: agent_spec.id.clone(),
            description: agent_spec.description.clone(),
            preamble: pc,
            inputs: agent_spec.inputs.iter().map(|p| AgentPort{kind:p.kind.clone(),port:p.port.clone()}).collect(),
            outputs: agent_spec.outputs.iter().map(|p| AgentPort{kind:p.kind.clone(),port:p.port.clone()}).collect(),
            config: Default::default(),
            output_schema: agent_spec.output_schema.clone(),
            tools: agent_spec.tools.iter().map(|t| ToolDef{
                name:t.name.clone(),description:t.description.clone(),
                command:t.command.clone(),args_schema:t.args_schema.clone(),timeout_secs:t.timeout_secs,
            }).collect(),
        };
        nodes.insert(agent_spec.id.clone(), BoxedNode::new(LlmAgentNode::new(Arc::new(def), Arc::clone(&client), std::path::PathBuf::from("."))));
    }
    nodes.insert("ranking".to_string(), BoxedNode::new(MockRanker));
    nodes.insert("proximity".to_string(), BoxedNode::new(MockProx));
    nodes.insert("supervisor".to_string(), BoxedNode::new(MockSup));

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
