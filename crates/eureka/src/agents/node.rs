//! `LlmAgentNode` — a single generic graph node that drives any agent definition.
//!
//! Given an `AgentDef` and an `LlmClient`, this node:
//! 1. Serializes the input artifact's data as the initial loop message.
//! 2. Runs the agentic loop: the LLM reasons freely, then calls `submit(json)`.
//! 3. Splits the returned JSON across the declared output ports.

use std::path::PathBuf;
use std::sync::Arc;

use crate::graph::artifact::Artifact;
use crate::graph::node::{Emit, Node, NodeCtx, NodeError, PortMsg};
use crate::graph::port::PortSpec;
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
}

impl LlmAgentNode {
    /// Create a new node from an agent definition, an LLM client, and the
    /// working directory used when spawning tool subprocesses.
    #[must_use]
    pub fn new(def: Arc<AgentDef>, client: Arc<dyn LlmClient>, work_dir: PathBuf) -> Self {
        Self {
            def,
            client,
            work_dir,
        }
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

    async fn process(&self, ctx: &NodeCtx, msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
        let initial_message = if self.def.inputs.len() > 1 {
            let data = serde_json::to_string_pretty(&msg.artifact.data)
                .map_err(|e| NodeError::Internal(e.to_string()))?;
            format!("Port: {}\n\n{data}", msg.port)
        } else {
            serde_json::to_string_pretty(&msg.artifact.data)
                .map_err(|e| NodeError::Internal(e.to_string()))?
        };

        let output_json = self
            .client
            .run_agent_loop(
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
            )
            .await
            .map_err(|e| NodeError::Agent(e.to_string()))?;

        if self.def.outputs.len() == 1 {
            let port = &self.def.outputs[0];
            Ok(vec![Emit::new(
                &port.port,
                Artifact {
                    kind: port.kind.clone(),
                    data: output_json,
                },
            )])
        } else {
            let mut emits = Vec::with_capacity(self.def.outputs.len());
            for port in &self.def.outputs {
                let data = output_json
                    .get(&port.port)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                emits.push(Emit::new(
                    &port.port,
                    Artifact {
                        kind: port.kind.clone(),
                        data,
                    },
                ));
            }
            Ok(emits)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::def::{AgentConfig, AgentPort};
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
        ) -> Result<serde_json::Value, AgentError> {
            Ok(serde_json::json!({ "echo": initial_message }))
        }
    }

    fn make_def(name: &str, input_kind: &str, output_kind: &str) -> Arc<AgentDef> {
        Arc::new(AgentDef {
            name: name.to_string(),
            description: None,
            preamble: "test preamble".to_string(),
            inputs: vec![AgentPort {
                kind: input_kind.to_string(),
                port: "in".to_string(),
            }],
            outputs: vec![AgentPort {
                kind: output_kind.to_string(),
                port: "out".to_string(),
            }],
            config: AgentConfig::default(),
            output_schema: serde_json::json!({ "type": "object" }),
            tools: vec![],
        })
    }

    #[tokio::test]
    async fn test_single_output_node() {
        let def = make_def("generation", "Goal", "Hypotheses");
        let node = LlmAgentNode::new(def, Arc::new(EchoClient), std::path::PathBuf::from("."));

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = crate::graph::node::NodeCtx::new("generation", "generation", 0, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact {
                kind: "Goal".to_string(),
                data: serde_json::json!({ "goal": "Test Goal" }),
            },
        };

        let emits = node.process(&ctx, msg).await.unwrap();
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
            ) -> Result<serde_json::Value, AgentError> {
                Ok(serde_json::json!({
                    "insights": { "recurring_patterns": [] },
                    "overview": { "summary": "Done" }
                }))
            }
        }

        let def = Arc::new(AgentDef {
            name: "meta_review".to_string(),
            description: None,
            preamble: "preamble".to_string(),
            inputs: vec![AgentPort {
                kind: "Ranking".to_string(),
                port: "in".to_string(),
            }],
            outputs: vec![
                AgentPort {
                    kind: "Insights".to_string(),
                    port: "insights".to_string(),
                },
                AgentPort {
                    kind: "Overview".to_string(),
                    port: "overview".to_string(),
                },
            ],
            config: AgentConfig::default(),
            output_schema: serde_json::json!({ "type": "object" }),
            tools: vec![],
        });

        let node = LlmAgentNode::new(def, Arc::new(SplitClient), std::path::PathBuf::from("."));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = crate::graph::node::NodeCtx::new("meta_review", "meta_review", 1, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact {
                kind: "Ranking".to_string(),
                data: serde_json::json!({}),
            },
        };

        let emits = node.process(&ctx, msg).await.unwrap();
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
                AgentPort {
                    kind: "Goal".to_string(),
                    port: "in".to_string(),
                },
                AgentPort {
                    kind: "Insights".to_string(),
                    port: "context".to_string(),
                },
            ],
            outputs: vec![AgentPort {
                kind: "Hypotheses".to_string(),
                port: "out".to_string(),
            }],
            config: AgentConfig::default(),
            output_schema: serde_json::json!({ "type": "object" }),
            tools: vec![],
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
            ) -> Result<serde_json::Value, AgentError> {
                *self.captured.lock().unwrap() = Some(initial_message.to_string());
                Ok(serde_json::json!({ "hypotheses": [] }))
            }
        }

        let client = Arc::new(CaptureClient {
            captured: std::sync::Mutex::new(None),
        });
        let client_dyn: Arc<dyn LlmClient> = client.clone();
        let node = LlmAgentNode::new(def, client_dyn, std::path::PathBuf::from("."));

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = crate::graph::node::NodeCtx::new("generation", "generation", 0, cancel);
        let msg = PortMsg {
            port: "context".into(),
            artifact: Artifact {
                kind: "Insights".to_string(),
                data: serde_json::json!({ "recurring_patterns": [] }),
            },
        };

        node.process(&ctx, msg).await.unwrap();
        let prompt = client.captured.lock().unwrap().clone().unwrap();
        assert!(prompt.starts_with("Port: context"));
    }
}
