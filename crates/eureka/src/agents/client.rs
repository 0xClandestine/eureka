//! `LlmClient` — type-erased async agentic loop runner.
//!
//! Each agent runs in a loop: it receives an input message, reasons across
//! one or more turns optionally calling shell tools, and terminates by calling
//! the `submit` tool with its structured JSON output.

use std::sync::{Arc, Mutex};

use crate::graph::node::NodeUsage;
use crate::scheduler::SchedulerEvent;
use async_trait::async_trait;
use rig_core::agent::AgentBuilder;
use rig_core::completion::{CompletionModel, Prompt, ToolDefinition};
use rig_core::tool::Tool;
use tokio::sync::mpsc;

use super::def::ToolDef;
use super::error::AgentError;
use super::tools::CommandTool;

/// Type-erased LLM client: runs an agentic loop until `submit` is called.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait LlmClient: Send + Sync {
    /// Run the agent loop until the agent calls `submit(json)`.
    ///
    /// `work_dir` is passed to spawned tool subprocesses as their working
    /// directory, so relative script paths (e.g. `tools/arxiv_search.py`)
    /// resolve against the graph directory rather than the eureka process's
    /// cwd.
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
        work_dir: &str,
        event_tx: Option<mpsc::Sender<SchedulerEvent>>,
    ) -> Result<(serde_json::Value, NodeUsage), AgentError>;
}

/// A `LlmClient` backed by any `rig` `CompletionModel`.
pub struct RigClient<M> {
    /// The underlying rig completion model.
    model: M,
    /// Optional per-input/per-output pricing for the cost backstop.
    pricing: Option<crate::config::Pricing>,
}

impl<M: CompletionModel + Clone + Send + Sync + 'static> RigClient<M> {
    /// Wrap a rig completion model with optional per-input/per-output pricing
    /// used to populate [`NodeUsage::cost_usd`].
    pub fn new(model: M, pricing: Option<crate::config::Pricing>) -> Self {
        Self { model, pricing }
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
        work_dir: &str,
        event_tx: Option<mpsc::Sender<SchedulerEvent>>,
    ) -> Result<(serde_json::Value, NodeUsage), AgentError> {
        let result: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));

        let submit = Submit {
            schema: output_schema.clone(),
            result: Arc::clone(&result),
        };

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        let full_preamble = build_preamble(preamble, &tool_names, max_iterations, output_schema);

        let command_tools: Vec<Box<dyn rig_core::tool::ToolDyn>> = tools
            .iter()
            .map(|t| -> Box<dyn rig_core::tool::ToolDyn> {
                Box::new(CommandTool::new(
                    Arc::new(t.clone()),
                    node_id.to_string(),
                    node_kind.to_string(),
                    round,
                    std::path::PathBuf::from(work_dir),
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

        // Use the extended prompt path so we get a PromptResponse with
        // aggregated token usage across all turns of the agent loop.
        let response = agent
            .prompt(initial_message)
            .max_turns(max_iterations as usize)
            .extended_details()
            .await
            .map_err(|e| AgentError::Provider(e.to_string()))?;

        let usage = NodeUsage::from_rig_usage(
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.total_tokens,
            self.pricing.as_ref(),
        );

        let submitted = result
            .lock()
            .map_err(|e| AgentError::Provider(format!("lock poisoned: {e}")))?
            .take();

        let value = submitted.ok_or_else(|| {
            AgentError::ExtractionFailed(
                "Agent exhausted iterations without calling submit".to_string(),
            )
        })?;

        Ok((value, usage))
    }
}

/// Build the full system preamble by appending schema, submit, and tool instructions.
fn build_preamble(
    preamble: &str,
    tool_names: &[&str],
    max_iterations: u32,
    output_schema: &serde_json::Value,
) -> String {
    let schema_section = serde_json::to_string_pretty(output_schema)
        .ok()
        .filter(|s| s != "null" && s != "{}")
        .map(|s| {
            format!(
                "\n\n# Output Schema\n\
                 Your `submit` call must conform to this JSON Schema:\n\
                 ```json\n{s}\n```"
            )
        })
        .unwrap_or_default();

    if tool_names.is_empty() {
        format!(
            "{preamble}{schema_section}\n\n\
             You have at most {max_iterations} turns. When you have completed your \
             analysis, call the `submit` tool with your structured output. \
             Do not write JSON directly — always use submit. \
             IMPORTANT: You must call `submit` before your turns run out."
        )
    } else {
        let names = tool_names.join(", ");
        format!(
            "{preamble}{schema_section}\n\n\
             You have at most {max_iterations} turns (each tool call or response \
             counts as one turn). Available tools: {names}. Use them to \
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
struct Submit {
    /// The JSON schema for the agent's structured output.
    schema: serde_json::Value,
    /// Shared storage for the submitted value.
    result: Arc<Mutex<Option<serde_json::Value>>>,
}

/// Error type for the `submit` tool (never actually returned).
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
