//! `LlmAgentNode` — a single generic graph node that drives any agent definition.
//!
//! Given an `AgentDef` and an `LlmClient`, this node:
//! 1. Serializes the input artifact's data as the initial loop message.
//! 2. Runs the agentic loop: the LLM reasons freely, then calls `submit(json)`.
//! 3. Splits the returned JSON across the declared output ports.

use std::sync::Arc;

use async_trait::async_trait;
use eureka_graph::artifact::Artifact;
use eureka_graph::node::{Emit, Node, NodeCtx, NodeError, PortMsg};
use eureka_graph::port::PortSpec;

use crate::client::LlmClient;
use crate::def::AgentDef;

/// A graph node that runs an LLM agent defined by an `AgentDef`.
pub struct LlmAgentNode {
    def: Arc<AgentDef>,
    client: Arc<dyn LlmClient>,
}

impl LlmAgentNode {
    /// Create a new node from an agent definition and an LLM client.
    #[must_use]
    pub fn new(def: Arc<AgentDef>, client: Arc<dyn LlmClient>) -> Self {
        Self { def, client }
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

    async fn process(&self, _ctx: &NodeCtx, msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
        // For multi-input agents, prefix the message with the port name so the
        // LLM knows which kind of artifact it is receiving.
        let initial_message = if self.def.inputs.len() > 1 {
            let data = serde_json::to_string_pretty(&msg.artifact.data)
                .map_err(|e| NodeError::Internal(e.to_string()))?;
            format!("Port: {}\n\n{data}", msg.port)
        } else {
            serde_json::to_string_pretty(&msg.artifact.data)
                .map_err(|e| NodeError::Internal(e.to_string()))?
        };

        // Run the agentic loop — returns the JSON submitted by the agent.
        let output_json = self
            .client
            .run_agent_loop(
                &self.def.preamble,
                &self.def.output_schema,
                &self.def.tools,
                &initial_message,
                self.def.config.max_iterations,
                self.def.config.temperature,
            )
            .await
            .map_err(|e| NodeError::Agent(e.to_string()))?;

        // Emit on output ports.
        //
        // Single-output agents: the whole JSON object goes to the one port.
        // Multi-output agents: the JSON object must have a key per port name;
        // each key's value is emitted on the corresponding port.
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
    use super::*;
    use crate::def::{AgentConfig, AgentPort};
    use crate::error::AgentError;

    struct EchoClient;

    #[async_trait]
    impl LlmClient for EchoClient {
        async fn run_agent_loop(
            &self,
            _preamble: &str,
            _output_schema: &serde_json::Value,
            _tools: &[crate::def::ToolDef],
            initial_message: &str,
            _max_iterations: u32,
            _temperature: f64,
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
        let node = LlmAgentNode::new(def, Arc::new(EchoClient));

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("generation", "generation", 0, cancel);
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
                _tools: &[crate::def::ToolDef],
                _initial_message: &str,
                _max_iterations: u32,
                _temperature: f64,
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

        let node = LlmAgentNode::new(def, Arc::new(SplitClient));
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("meta_review", "meta_review", 1, cancel);
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
                _tools: &[crate::def::ToolDef],
                initial_message: &str,
                _max_iterations: u32,
                _temperature: f64,
            ) -> Result<serde_json::Value, AgentError> {
                *self.captured.lock().unwrap() = Some(initial_message.to_string());
                Ok(serde_json::json!({ "hypotheses": [] }))
            }
        }

        let client = Arc::new(CaptureClient {
            captured: std::sync::Mutex::new(None),
        });
        let node = LlmAgentNode::new(def, Arc::clone(&client) as Arc<dyn LlmClient>);

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("generation", "generation", 0, cancel);
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
