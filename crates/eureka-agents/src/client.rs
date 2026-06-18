//! `LlmClient` — type-erased async agentic loop runner.
//!
//! Each agent runs in a loop: it receives an input message, reasons across
//! one or more turns optionally calling shell tools, and terminates by calling
//! the `submit` tool with its structured JSON output.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eureka_graph::scheduler::SchedulerEvent;
use rig_core::agent::AgentBuilder;
use rig_core::completion::{CompletionModel, Prompt, ToolDefinition};
use rig_core::tool::Tool;
use tokio::sync::mpsc;

use crate::def::ToolDef;
use crate::error::AgentError;
use crate::tools::CommandTool;

/// Type-erased LLM client: runs an agentic loop until `submit` is called.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Run the agent loop until the agent calls `submit(json)`.
    ///
    /// The agent receives `initial_message` and may reason freely across
    /// multiple turns, calling any declared `tools` along the way. When
    /// satisfied, it calls `submit`, terminating the loop.
    ///
    /// `output_schema` becomes the parameter schema for the `submit` tool.
    /// `tools` are the shell tools declared in the agent's `.json` file.
    ///
    /// # Errors
    ///
    /// Returns `AgentError` if the provider call fails or `max_iterations`
    /// is exhausted before `submit` is called.
    async fn run_agent_loop(
        &self,
        preamble: &str,
        output_schema: &serde_json::Value,
        tools: &[ToolDef],
        initial_message: &str,
        max_iterations: u32,
        temperature: f64,
        node_id: &str,
        node_kind: &str,
        round: u32,
        event_tx: Option<mpsc::Sender<SchedulerEvent>>,
    ) -> Result<serde_json::Value, AgentError>;
}

/// A `LlmClient` backed by any `rig` `CompletionModel`.
pub struct RigClient<M> {
    model: M,
}

impl<M: CompletionModel + Clone + Send + Sync + 'static> RigClient<M> {
    /// Wrap a rig completion model.
    pub fn new(model: M) -> Self {
        Self { model }
    }
}

#[async_trait]
impl<M: CompletionModel + Clone + Send + Sync + 'static> LlmClient for RigClient<M> {
    async fn run_agent_loop(
        &self,
        preamble: &str,
        output_schema: &serde_json::Value,
        tools: &[ToolDef],
        initial_message: &str,
        max_iterations: u32,
        temperature: f64,
        node_id: &str,
        node_kind: &str,
        round: u32,
        event_tx: Option<mpsc::Sender<SchedulerEvent>>,
    ) -> Result<serde_json::Value, AgentError> {
        let result: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));

        let submit = Submit {
            schema: output_schema.clone(),
            result: Arc::clone(&result),
        };

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        let full_preamble = build_preamble(preamble, &tool_names, max_iterations);

        let command_tools: Vec<Box<dyn rig_core::tool::ToolDyn>> = tools
            .iter()
            .map(|t| -> Box<dyn rig_core::tool::ToolDyn> {
                Box::new(CommandTool::new(
                    Arc::new(t.clone()),
                    node_id.to_string(),
                    node_kind.to_string(),
                    round,
                    event_tx.clone(),
                ))
            })
            .collect();

        let agent = AgentBuilder::new(self.model.clone())
            .preamble(&full_preamble)
            .temperature(temperature)
            .tool(submit)
            .tools(command_tools)
            .build();

        agent
            .prompt(initial_message)
            .max_turns(max_iterations as usize)
            .await
            .map_err(|e| AgentError::Provider(e.to_string()))?;

        let submitted = result
            .lock()
            .map_err(|e| AgentError::Provider(format!("lock poisoned: {e}")))?
            .take();

        submitted.ok_or_else(|| {
            AgentError::ExtractionFailed(
                "Agent exhausted iterations without calling submit".to_string(),
            )
        })
    }
}

/// Build the full system preamble by appending submit/tool instructions.
fn build_preamble(preamble: &str, tool_names: &[&str], max_iterations: u32) -> String {
    if tool_names.is_empty() {
        format!(
            "{preamble}\n\n\
             You have at most {max_iterations} turns. When you have completed your \
             analysis, call the `submit` tool with your structured output. \
             Do not write JSON directly — always use submit. \
             IMPORTANT: You must call `submit` before your turns run out."
        )
    } else {
        let names = tool_names.join(", ");
        format!(
            "{preamble}\n\n\
             You have at most {max_iterations} turns (each tool call or response \
             counts as one turn). You have access to tools: {names}. Use them to \
             gather information, but budget your turns — leave at least one turn \
             to call `submit`. When you are ready to deliver your final result, \
             call the `submit` tool. Do not write JSON directly — always use submit. \
             IMPORTANT: You must call `submit` before your turns run out."
        )
    }
}

// ---------------------------------------------------------------------------
// Submit tool
// ---------------------------------------------------------------------------

/// The terminal tool injected into every agent loop.
///
/// The agent calls `submit(json)` to deliver its structured output. Depositing
/// the value in the shared slot signals the loop to terminate.
struct Submit {
    schema: serde_json::Value,
    result: Arc<Mutex<Option<serde_json::Value>>>,
}

#[derive(Debug)]
struct SubmitError;

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("submit error")
    }
}

impl std::error::Error for SubmitError {}

impl Tool for Submit {
    const NAME: &'static str = "submit";

    type Args = serde_json::Value;
    type Output = String;
    type Error = SubmitError;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Submit your final structured output. Call this exactly once \
                          when your answer is complete."
                .to_string(),
            parameters: self.schema.clone(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if let Ok(mut guard) = self.result.lock() {
            *guard = Some(args);
        }
        Ok("Output submitted.".to_string())
    }
}
