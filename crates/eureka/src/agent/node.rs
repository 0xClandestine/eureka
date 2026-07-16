//! `LlmAgentNode` — a single generic graph node that drives any agent definition.
//!
//! Given an `AgentDef` and an `LlmClient`, this node:
//! 1. Serializes the input artifact's data as the initial loop message.
//! 2. Runs the agentic loop: the LLM reasons freely, then calls `submit(json)`.
//! 3. Splits the returned JSON across the declared output ports.
//!
//! When `config.workers > 1` the node spawns `workers` parallel agentic loops
//! with identical inputs and merges their JSON outputs: array-valued fields are
//! concatenated, scalar fields take the last non-null value.

use std::path::PathBuf;
use std::sync::Arc;

use crate::graph::artifact::Artifact;
use crate::graph::node::{Emit, Node, NodeCtx, NodeError, NodeUsage, PortMsg};
use crate::graph::port::PortSpec;
use crate::persistence::RunEnvironment;
use async_trait::async_trait;

use super::client::LlmClient;
use super::def::AgentDef;

/// A graph node that runs an LLM agent defined by an `AgentDef`.
pub struct LlmAgentNode {
    /// The agent definition (prompt, schema, ports, config).
    def: Arc<AgentDef>,
    /// The LLM client used to run the agentic loop.
    client: Arc<dyn LlmClient>,
    /// Working directory for tool subprocesses (the graph directory).
    work_dir: PathBuf,
    /// Run-scoped identity and database capability for agent tools.
    environment: RunEnvironment,
}

impl LlmAgentNode {
    /// Create a new node from an agent definition, an LLM client, and the
    /// working directory used when spawning tool subprocesses.
    #[must_use]
    pub fn new(def: Arc<AgentDef>, client: Arc<dyn LlmClient>, work_dir: PathBuf) -> Self {
        let environment = RunEnvironment::new("", None);
        Self::with_environment(def, client, work_dir, environment)
    }

    /// Create an agent node with an explicit run environment for its tools.
    #[must_use]
    pub fn with_environment(
        def: Arc<AgentDef>,
        client: Arc<dyn LlmClient>,
        work_dir: PathBuf,
        environment: RunEnvironment,
    ) -> Self {
        Self { def, client, work_dir, environment }
    }

    /// The agent name (used as the `kind` string in the node registry).
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.def.name
    }
}

#[async_trait]
impl Node for LlmAgentNode {
    fn ports(&self) -> PortSpec {
        self.def.to_port_spec()
    }

    async fn process(
        &self,
        ctx: &NodeCtx,
        inputs: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, NodeUsage), NodeError> {
        // Build the initial loop message from all available inputs. For a
        // single-input agent, this is just the artifact payload. For a
        // multi-input agent, each input is labelled with its port name so the
        // model can distinguish goal vs. context, etc.
        let initial_message = if self.def.inputs.len() > 1 {
            let mut parts = Vec::with_capacity(inputs.len());
            for msg in &inputs {
                let data = serde_json::to_string_pretty(&msg.artifact.data)
                    .map_err(|e| NodeError::Internal(e.to_string()))?;
                parts.push(format!("Port: {}\n\n{data}", msg.port));
            }
            parts.join("\n\n---\n\n")
        } else {
            // Single (or undeclared) input: use the first available payload
            // verbatim, falling back to null if somehow empty.
            let data =
                inputs.into_iter().next().map_or(serde_json::Value::Null, |m| m.artifact.data);
            serde_json::to_string_pretty(&data).map_err(|e| NodeError::Internal(e.to_string()))?
        };

        let workers = self.def.config.workers.max(1) as usize;

        let (output_json, usage) = if workers == 1 {
            self.client
                .run_agent_loop_with_environment(
                    &self.def.preamble,
                    &self.def.output_schema,
                    &self.def.tools,
                    &initial_message,
                    self.def.config.max_iterations,
                    self.def.config.temperature,
                    &ctx.node_id,
                    &ctx.node_kind,
                    ctx.round,
                    &self.work_dir.to_string_lossy(),
                    ctx.event_tx.clone(),
                    &self.environment,
                )
                .await
                .map_err(|e| NodeError::Agent(e.to_string()))?
        } else {
            // Scatter: spawn `workers` independent LLM calls in parallel.
            let mut handles = Vec::with_capacity(workers);
            for worker_idx in 0..workers {
                let client = Arc::clone(&self.client);
                let preamble = self.def.preamble.clone();
                let schema = self.def.output_schema.clone();
                let tools = self.def.tools.clone();
                let message = initial_message.clone();
                let max_iter = self.def.config.max_iterations;
                let temperature = self.def.config.temperature;
                let node_id = format!("{}[{}]", ctx.node_id, worker_idx);
                let node_kind = ctx.node_kind.clone();
                let round = ctx.round;
                let work_dir = self.work_dir.to_string_lossy().into_owned();
                let event_tx = ctx.event_tx.clone();
                let environment = self.environment.clone();
                handles.push(tokio::spawn(async move {
                    client
                        .run_agent_loop_with_environment(
                            &preamble,
                            &schema,
                            &tools,
                            &message,
                            max_iter,
                            temperature,
                            &node_id,
                            &node_kind,
                            round,
                            &work_dir,
                            event_tx,
                            &environment,
                        )
                        .await
                }));
            }

            // Gather: collect results, propagate the first error.
            let mut outputs: Vec<serde_json::Value> = Vec::with_capacity(workers);
            let mut total_usage = NodeUsage::default();
            for handle in handles {
                let (json, usage) = handle
                    .await
                    .map_err(|e| NodeError::Internal(format!("worker task panicked: {e}")))?
                    .map_err(|e| NodeError::Agent(e.to_string()))?;
                outputs.push(json);
                total_usage = total_usage + usage;
            }

            (merge_worker_outputs(outputs), total_usage)
        };

        let emits = if self.def.outputs.len() == 1 {
            let port = &self.def.outputs[0];
            vec![Emit::new(&port.port, Artifact { kind: port.kind.clone(), data: output_json })]
        } else {
            let mut emits = Vec::with_capacity(self.def.outputs.len());
            for port in &self.def.outputs {
                let data = output_json.get(&port.port).cloned().unwrap_or(serde_json::Value::Null);
                emits.push(Emit::new(&port.port, Artifact { kind: port.kind.clone(), data }));
            }
            emits
        };

        Ok((emits, usage))
    }
}

/// Merge the JSON outputs of multiple parallel workers.
///
/// For every key present across all outputs:
/// - If every worker produced an array for that key, concatenate the arrays.
/// - Otherwise take the last non-null value (scalar / object fields).
fn merge_worker_outputs(outputs: Vec<serde_json::Value>) -> serde_json::Value {
    if outputs.is_empty() {
        return serde_json::Value::Null;
    }
    if outputs.len() == 1 {
        return outputs.into_iter().next().unwrap_or(serde_json::Value::Null);
    }

    // Collect all keys that appear in any output.
    let mut all_keys: Vec<String> = outputs
        .iter()
        .filter_map(|v| v.as_object())
        .flat_map(|m| m.keys().cloned())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    all_keys.sort();

    let mut merged = serde_json::Map::new();
    for key in &all_keys {
        let values: Vec<&serde_json::Value> = outputs.iter().filter_map(|o| o.get(key)).collect();

        let all_arrays = values.iter().all(|v| v.is_array());
        if all_arrays {
            let concatenated: Vec<serde_json::Value> = values
                .iter()
                .flat_map(|v| v.as_array().map_or(&[][..], |a| a.as_slice()).iter().cloned())
                .collect();
            merged.insert(key.clone(), serde_json::Value::Array(concatenated));
        } else {
            let val = values
                .iter()
                .rev()
                .find(|v| !v.is_null())
                .copied()
                .unwrap_or(&serde_json::Value::Null)
                .clone();
            merged.insert(key.clone(), val);
        }
    }
    serde_json::Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use super::super::def::{AgentConfig, PortDef};
    use super::super::error::AgentError;
    use super::*;

    struct EchoClient;

    #[async_trait]
    impl LlmClient for EchoClient {
        async fn run_agent_loop(
            &self,
            _preamble: &str,
            _output_schema: &serde_json::Value,
            _tools: &[super::super::def::ToolDef],
            initial_message: &str,
            _max_iterations: u32,
            _temperature: f64,
            _node_id: &str,
            _node_kind: &str,
            _round: u32,
            _work_dir: &str,
            _event_tx: Option<tokio::sync::mpsc::Sender<crate::scheduler::SchedulerEvent>>,
        ) -> Result<(serde_json::Value, crate::graph::node::NodeUsage), AgentError> {
            Ok((
                serde_json::json!({ "echo": initial_message }),
                crate::graph::node::NodeUsage::default(),
            ))
        }
    }

    fn make_def(name: &str, input_kind: &str, output_kind: &str) -> Arc<AgentDef> {
        Arc::new(AgentDef {
            name: name.to_string(),
            description: None,
            preamble: "test preamble".to_string(),
            inputs: vec![PortDef {
                kind: input_kind.to_string(),
                port: "in".to_string(),
                ..Default::default()
            }],
            outputs: vec![PortDef {
                kind: output_kind.to_string(),
                port: "out".to_string(),
                ..Default::default()
            }],
            config: AgentConfig::default(),
            output_schema: serde_json::json!({ "type": "object" }),
            tools: vec![],
            mcp_servers: vec![],
        })
    }

    #[tokio::test]
    async fn test_single_output_node() {
        let def = make_def("generation", "Goal", "Hypotheses");
        let node = LlmAgentNode::new(def, Arc::new(EchoClient), PathBuf::from("."));

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("generation", "generation", 0, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact {
                kind: "Goal".to_string(),
                data: serde_json::json!({ "goal": "Test Goal" }),
            },
        };

        let (emits, _) = node.process(&ctx, vec![msg]).await.unwrap();
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].port, "out");
        assert_eq!(emits[0].artifact.kind, "Hypotheses");
    }

    #[tokio::test]
    async fn test_multi_output_node() {
        struct SplitClient;

        #[async_trait]
        impl LlmClient for SplitClient {
            async fn run_agent_loop(
                &self,
                _preamble: &str,
                _output_schema: &serde_json::Value,
                _tools: &[super::super::def::ToolDef],
                _initial_message: &str,
                _max_iterations: u32,
                _temperature: f64,
                _node_id: &str,
                _node_kind: &str,
                _round: u32,
                _work_dir: &str,
                _event_tx: Option<tokio::sync::mpsc::Sender<crate::scheduler::SchedulerEvent>>,
            ) -> Result<(serde_json::Value, crate::graph::node::NodeUsage), AgentError>
            {
                Ok((
                    serde_json::json!({
                        "insights": { "recurring_patterns": [] },
                        "overview": { "summary": "Done" }
                    }),
                    crate::graph::node::NodeUsage::default(),
                ))
            }
        }

        let def = Arc::new(AgentDef {
            name: "meta_review".to_string(),
            description: None,
            preamble: "preamble".to_string(),
            inputs: vec![PortDef {
                kind: "Ranking".to_string(),
                port: "in".to_string(),
                ..Default::default()
            }],
            outputs: vec![
                PortDef {
                    kind: "Insights".to_string(),
                    port: "insights".to_string(),
                    ..Default::default()
                },
                PortDef {
                    kind: "Overview".to_string(),
                    port: "overview".to_string(),
                    ..Default::default()
                },
            ],
            config: AgentConfig::default(),
            output_schema: serde_json::json!({ "type": "object" }),
            tools: vec![],
            mcp_servers: vec![],
        });

        let node = LlmAgentNode::new(def, Arc::new(SplitClient), PathBuf::from("."));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("meta_review", "meta_review", 1, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact { kind: "Ranking".to_string(), data: serde_json::json!({}) },
        };

        let (emits, _) = node.process(&ctx, vec![msg]).await.unwrap();
        assert_eq!(emits.len(), 2);

        let ports: Vec<&str> = emits.iter().map(|e| e.port.as_str()).collect();
        assert!(ports.contains(&"insights"));
        assert!(ports.contains(&"overview"));
    }

    #[tokio::test]
    async fn test_multi_input_prompt_includes_port_name() {
        let def = Arc::new(AgentDef {
            name: "generation".to_string(),
            description: None,
            preamble: "preamble".to_string(),
            inputs: vec![
                PortDef { kind: "Goal".to_string(), port: "in".to_string(), ..Default::default() },
                PortDef {
                    kind: "Insights".to_string(),
                    port: "context".to_string(),
                    ..Default::default()
                },
            ],
            outputs: vec![PortDef {
                kind: "Hypotheses".to_string(),
                port: "out".to_string(),
                ..Default::default()
            }],
            config: AgentConfig::default(),
            output_schema: serde_json::json!({ "type": "object" }),
            tools: vec![],
            mcp_servers: vec![],
        });

        struct CaptureClient {
            captured: std::sync::Mutex<Option<String>>,
        }

        #[async_trait]
        impl LlmClient for CaptureClient {
            async fn run_agent_loop(
                &self,
                _preamble: &str,
                _output_schema: &serde_json::Value,
                _tools: &[super::super::def::ToolDef],
                initial_message: &str,
                _max_iterations: u32,
                _temperature: f64,
                _node_id: &str,
                _node_kind: &str,
                _round: u32,
                _work_dir: &str,
                _event_tx: Option<tokio::sync::mpsc::Sender<crate::scheduler::SchedulerEvent>>,
            ) -> Result<(serde_json::Value, crate::graph::node::NodeUsage), AgentError>
            {
                *self.captured.lock().unwrap() = Some(initial_message.to_string());
                Ok((
                    serde_json::json!({ "hypotheses": [] }),
                    crate::graph::node::NodeUsage::default(),
                ))
            }
        }

        let client = Arc::new(CaptureClient { captured: std::sync::Mutex::new(None) });
        let client_dyn: Arc<dyn LlmClient> = client.clone();
        let node = LlmAgentNode::new(def, client_dyn, PathBuf::from("."));

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("generation", "generation", 0, cancel);
        let msg = PortMsg {
            port: "context".into(),
            artifact: Artifact {
                kind: "Insights".to_string(),
                data: serde_json::json!({ "recurring_patterns": [] }),
            },
        };

        node.process(&ctx, vec![msg]).await.unwrap();
        let prompt = client.captured.lock().unwrap().clone().unwrap();
        assert!(prompt.starts_with("Port: context"));
    }
}

#[cfg(test)]
mod worker_tests {
    use std::sync::Arc;
    use std::path::PathBuf;
    use async_trait::async_trait;
    use crate::graph::artifact::Artifact;
    use crate::graph::node::{NodeCtx, PortMsg};
    use crate::graph::node::Node as _;
    use super::super::def::{AgentConfig, AgentDef, PortDef};
    use super::super::error::AgentError;
    use super::super::client::LlmClient;
    use super::{LlmAgentNode, merge_worker_outputs};

    #[test]
    fn test_merge_arrays_concatenated() {
        let a = serde_json::json!({ "hypotheses": [{"id": 1}], "meta": "a" });
        let b = serde_json::json!({ "hypotheses": [{"id": 2}, {"id": 3}], "meta": "b" });
        let merged = merge_worker_outputs(vec![a, b]);
        assert_eq!(merged["hypotheses"].as_array().unwrap().len(), 3);
        assert_eq!(merged["meta"].as_str().unwrap(), "b");
    }

    #[test]
    fn test_merge_single_passthrough() {
        let v = serde_json::json!({ "hypotheses": [{"id": 1}] });
        assert_eq!(merge_worker_outputs(vec![v.clone()]), v);
    }

    #[test]
    fn test_merge_empty() {
        assert_eq!(merge_worker_outputs(vec![]), serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_workers_spawn_parallel_calls() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingClient { calls: Arc<AtomicUsize> }

        #[async_trait]
        impl LlmClient for CountingClient {
            async fn run_agent_loop(
                &self,
                _preamble: &str, _output_schema: &serde_json::Value,
                _tools: &[crate::agent::def::ToolDef],
                _initial_message: &str, _max_iterations: u32, _temperature: f64,
                _node_id: &str, _node_kind: &str, _round: u32, _work_dir: &str,
                _event_tx: Option<tokio::sync::mpsc::Sender<crate::scheduler::SchedulerEvent>>,
            ) -> Result<(serde_json::Value, crate::graph::node::NodeUsage), AgentError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok((serde_json::json!({ "items": [1] }), crate::graph::node::NodeUsage::default()))
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let def = Arc::new(AgentDef {
            name: "gen".to_string(), description: None, preamble: "p".to_string(),
            inputs: vec![PortDef { kind: "Goal".to_string(), port: "in".to_string(), ..Default::default() }],
            outputs: vec![PortDef { kind: "Items".to_string(), port: "out".to_string(), ..Default::default() }],
            config: AgentConfig { workers: 3, ..AgentConfig::default() },
            output_schema: serde_json::json!({ "type": "object" }),
            tools: vec![], mcp_servers: vec![],
        });
        let node = LlmAgentNode::new(def, Arc::new(CountingClient { calls: Arc::clone(&calls) }), PathBuf::from("."));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("gen", "gen", 0, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact { kind: "Goal".to_string(), data: serde_json::json!({}) },
        };
        let (emits, _) = node.process(&ctx, vec![msg]).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3, "3 workers must each call the LLM once");
        assert_eq!(emits[0].artifact.data["items"].as_array().unwrap().len(), 3);
    }
}
