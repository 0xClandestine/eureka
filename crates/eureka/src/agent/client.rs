//! `LlmClient` — type-erased async agentic loop runner.
//!
//! Each agent runs in a loop: it receives an input message, reasons across
//! one or more turns optionally calling shell tools, and terminates by calling
//! the `submit` tool with its structured JSON output.

use std::any::Any;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::graph::node::NodeUsage;
use crate::persistence::RunEnvironment;
use crate::scheduler::SchedulerEvent;
use async_trait::async_trait;
use rig_core::agent::{AgentBuilder, AgentHook, Flow, HookContext, StepEvent};
use rig_core::completion::{CompletionModel, Prompt, Usage};
use rig_core::tool::Tool;
use tokio::sync::mpsc;

use super::def::ToolDef;
use super::error::AgentError;
use super::tool::CommandTool;

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

    /// Run an agent loop while passing run-scoped capabilities to its tools.
    ///
    /// Custom clients retain the legacy behavior by default; the built-in
    /// `RigClient` overrides this method so command tools receive the same
    /// database identity as control-node subprocesses.
    #[allow(clippy::too_many_arguments)]
    async fn run_agent_loop_with_environment(
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
        _environment: &RunEnvironment,
        _live_tokens: Option<Arc<AtomicU64>>,
        _live_input_tokens: Option<Arc<AtomicU64>>,
        _live_output_tokens: Option<Arc<AtomicU64>>,
        _live_cost: Option<Arc<Mutex<f64>>>,
    ) -> Result<(serde_json::Value, NodeUsage), AgentError> {
        self.run_agent_loop(
            preamble,
            output_schema,
            tools,
            initial_message,
            max_iterations,
            temperature,
            node_id,
            node_kind,
            round,
            work_dir,
            event_tx,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// McpConnection
// ---------------------------------------------------------------------------

/// A live MCP server connection held by `RigClient` for the session lifetime.
///
/// Established during `SessionBuilder::build_agent_node` and kept alive for
/// every activation of the agent.  The `_service` field holds the
/// `RunningService` value (type-erased to avoid a transport type parameter);
/// dropping it closes the connection and — for stdio servers — kills the
/// subprocess.
pub(crate) struct McpConnection {
    /// Tools advertised by the server at connect time.
    pub tools: Vec<rmcp::model::Tool>,
    /// Sink for dispatching `call_tool` requests to the server.
    pub sink: rmcp::service::ServerSink,
    /// Keeps the underlying `RunningService` (and its transport) alive.
    /// Type-erased because stdio and HTTP produce different generic types.
    pub _service: Box<dyn Any + Send + Sync>,
}

// ---------------------------------------------------------------------------
// UsageHook — per-turn usage accumulator
// ---------------------------------------------------------------------------

/// A rig `AgentHook` that accumulates token usage after every model turn.
///
/// This is used so that we can recover accurate usage even when the agent
/// loop ends with `MaxTurnsError` (which doesn't carry usage in its error
/// variant). The hook fires on `ModelTurnFinished` after each turn and
/// accumulates into a shared `Arc<Mutex<Usage>>` that the caller reads back
/// after the loop returns, regardless of whether it succeeded or errored.
struct UsageHook {
    acc: Arc<Mutex<Usage>>,
}

impl<M: CompletionModel + Send + Sync + 'static> AgentHook<M> for UsageHook {
    fn on_event(
        &self,
        _ctx: &HookContext,
        event: StepEvent<'_, M>,
    ) -> impl std::future::Future<Output = Flow> + Send {
        if let StepEvent::ModelTurnFinished { usage, .. } = event {
            if let Ok(mut guard) = self.acc.lock() {
                *guard += usage;
            }
        }
        std::future::ready(Flow::cont())
    }
}

/// A rig `AgentHook` that increments live counters after every model turn.
///
/// Paired with `UsageHook`; this one pushes token/cost totals into the
/// shared atomics so the `/api/state` UI updates in real time, per-turn,
/// without waiting for the agent loop to finish.
struct LiveCounterHook {
    live_tokens: Option<Arc<AtomicU64>>,
    live_input_tokens: Option<Arc<AtomicU64>>,
    live_output_tokens: Option<Arc<AtomicU64>>,
    live_cost: Option<Arc<Mutex<f64>>>,
    pricing: Option<crate::config::Pricing>,
}

impl<M: CompletionModel + Send + Sync + 'static> AgentHook<M> for LiveCounterHook {
    fn on_event(
        &self,
        _ctx: &HookContext,
        event: StepEvent<'_, M>,
    ) -> impl std::future::Future<Output = Flow> + Send {
        if let StepEvent::ModelTurnFinished { usage, .. } = event {
            if let Some(ref t) = self.live_tokens {
                t.fetch_add(usage.total_tokens, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(ref t) = self.live_input_tokens {
                t.fetch_add(usage.input_tokens, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(ref t) = self.live_output_tokens {
                t.fetch_add(usage.output_tokens, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(ref c) = self.live_cost {
                let cost = self
                    .pricing
                    .as_ref()
                    .map_or(0.0, |p| p.cost(usage.input_tokens, usage.output_tokens));
                if let Ok(mut guard) = c.lock() {
                    *guard += cost;
                }
            }
        }
        std::future::ready(Flow::cont())
    }
}

// ---------------------------------------------------------------------------
// RigClient
// ---------------------------------------------------------------------------

/// A `LlmClient` backed by any `rig` `CompletionModel`.
pub struct RigClient<M> {
    /// The underlying rig completion model.
    model: M,
    /// Optional per-input/per-output pricing for the cost backstop.
    pricing: Option<crate::config::Pricing>,
    /// Optional RAG index handle and `top_k` for `dynamic_context`.
    rag: Option<(crate::rag::RagIndexHandle, usize)>,
    /// Live MCP server connections for this agent.  Empty for most agents.
    mcp: Vec<McpConnection>,
}

impl<M: CompletionModel + Clone + Send + Sync + 'static> RigClient<M> {
    /// Wrap a rig completion model with optional per-input/per-output pricing
    /// used to populate [`NodeUsage::cost_usd`].
    #[must_use]
    pub const fn new(model: M, pricing: Option<crate::config::Pricing>) -> Self {
        Self { model, pricing, rag: None, mcp: Vec::new() }
    }

    /// Attach a RAG index so every agent turn retrieves the top-`k` most
    /// relevant documents via `AgentBuilder::dynamic_context`.
    #[must_use]
    pub fn with_rag(mut self, index: crate::rag::RagIndexHandle, top_k: usize) -> Self {
        self.rag = Some((index, top_k));
        self
    }

    /// Attach pre-connected MCP server connections.
    ///
    /// Called by `SessionBuilder::build_agent_node` after establishing all
    /// server connections declared in the agent's `mcp_servers` list.
    #[must_use]
    pub(crate) fn with_mcp(mut self, mcp: Vec<McpConnection>) -> Self {
        self.mcp = mcp;
        self
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
        self.run_agent_loop_with_environment(
            preamble,
            output_schema,
            tools,
            initial_message,
            max_iterations,
            temperature,
            node_id,
            node_kind,
            round,
            work_dir,
            event_tx,
            &RunEnvironment::new(node_id, None),
            None,
            None,
            None,
            None,
        )
        .await
    }

    async fn run_agent_loop_with_environment(
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
        environment: &RunEnvironment,
        live_tokens: Option<Arc<AtomicU64>>,
        live_input_tokens: Option<Arc<AtomicU64>>,
        live_output_tokens: Option<Arc<AtomicU64>>,
        live_cost: Option<Arc<Mutex<f64>>>,
    ) -> Result<(serde_json::Value, NodeUsage), AgentError> {
        let command_tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        let mcp_tool_names: Vec<String> =
            self.mcp.iter().flat_map(|c| c.tools.iter().map(|t| t.name.to_string())).collect();
        let all_tool_names: Vec<&str> = command_tool_names
            .iter()
            .copied()
            .chain(mcp_tool_names.iter().map(String::as_str))
            .collect();
        let full_preamble =
            build_preamble(preamble, &all_tool_names, max_iterations, output_schema);

        // Retry loop: provider transient failures (e.g. empty 200 body from OpenRouter)
        // are retried up to MAX_ATTEMPTS times with exponential backoff.
        const MAX_ATTEMPTS: u32 = 4;
        let mut last_err = String::new();
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                let delay = Duration::from_secs(1 << (attempt - 1)); // 1s, 2s, 4s
                tracing::warn!(
                    attempt,
                    ?delay,
                    "retrying agent loop after transient provider error"
                );
                tokio::time::sleep(delay).await;
            }

            // Fresh submit store and usage accumulator each attempt.
            let result: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
            let submit = Submit { schema: output_schema.clone(), result: Arc::clone(&result) };
            let usage_acc: Arc<Mutex<Usage>> = Arc::new(Mutex::new(Usage::new()));

            let command_tools: Vec<Box<dyn rig_core::tool::ToolDyn>> = tools
                .iter()
                .map(|t| -> Box<dyn rig_core::tool::ToolDyn> {
                    Box::new(CommandTool::with_environment(
                        Arc::new(t.clone()),
                        node_id.to_string(),
                        node_kind.to_string(),
                        round,
                        std::path::PathBuf::from(work_dir),
                        environment.clone(),
                        event_tx.clone(),
                    ))
                })
                .collect();

            let mut base_builder = AgentBuilder::new(self.model.clone())
                .preamble(&full_preamble)
                .temperature(temperature);

            if let Some((index, top_k)) = &self.rag {
                base_builder = base_builder.dynamic_context(*top_k, index.clone());
            }

            let mut builder = base_builder.tool(submit).tools(command_tools);
            for conn in &self.mcp {
                builder = builder.rmcp_tools(conn.tools.clone(), conn.sink.clone());
            }
            let agent = builder.build();

            // Use the extended prompt path so we get a PromptResponse with
            // aggregated token usage across all turns of the agent loop.
            // The UsageHook additionally captures per-turn usage so we can
            // recover it even when MaxTurnsError is returned (which doesn't
            // carry usage in its error variant).
            let response = agent
                .prompt(initial_message)
                .max_turns(max_iterations as usize)
                .add_hook(UsageHook { acc: Arc::clone(&usage_acc) })
                .add_hook(LiveCounterHook {
                    live_tokens: live_tokens.clone(),
                    live_input_tokens: live_input_tokens.clone(),
                    live_output_tokens: live_output_tokens.clone(),
                    live_cost: live_cost.clone(),
                    pricing: self.pricing.clone(),
                })
                .extended_details()
                .await;

            // Check submit result before propagating any error: the agent may have
            // called submit on its final turn and Rig still returns MaxTurnsError.
            let submitted = result
                .lock()
                .map_err(|e| AgentError::Provider(format!("lock poisoned: {e}")))?
                .take();
            // Read accumulated per-turn usage (valid even when MaxTurnsError fires).
            let hook_usage = usage_acc.lock().map(|g| *g).unwrap_or_else(|_| Usage::new());

            match (response, submitted) {
                (Ok(resp), Some(v)) => {
                    // Prefer the response's aggregated usage; fall back to the
                    // hook accumulator if the provider returned zeroes.
                    let agg = resp.usage;
                    let effective = if agg.total_tokens > 0 { agg } else { hook_usage };
                    let u = NodeUsage::from_rig_usage(
                        effective.input_tokens,
                        effective.output_tokens,
                        effective.total_tokens,
                        self.pricing.as_ref(),
                    );
                    return Ok((v, u));
                }
                // Submit called on the last turn; Rig raises MaxTurnsError but result is valid.
                // Use the hook-accumulated usage since the error carries none.
                (Err(_), Some(v)) => {
                    let u = NodeUsage::from_rig_usage(
                        hook_usage.input_tokens,
                        hook_usage.output_tokens,
                        hook_usage.total_tokens,
                        self.pricing.as_ref(),
                    );
                    return Ok((v, u));
                }
                (Ok(_), None) => {
                    return Err(AgentError::ExtractionFailed(
                        "Agent exhausted iterations without calling submit".to_string(),
                    ));
                }
                (Err(e), None) => {
                    let msg = e.to_string();
                    if attempt + 1 < MAX_ATTEMPTS && is_retryable_provider_error(&msg) {
                        last_err = msg;
                        continue;
                    }
                    return Err(AgentError::Provider(msg));
                }
            }
        }

        Err(AgentError::Provider(format!(
            "provider failed after {MAX_ATTEMPTS} attempts: {last_err}"
        )))
    }
}

/// Returns `true` for transient provider errors that are safe to retry.
///
/// An empty-body 200 from OpenRouter surfaces as `ProviderResponseError: status 200 OK:`
/// with nothing after the colon. 503 / 529 (overloaded) are also retryable.
fn is_retryable_provider_error(msg: &str) -> bool {
    // Empty 200 body — the most common transient OpenRouter failure.
    msg.contains("ProviderResponseError: status 200 OK:")
        || msg.contains("status 503")
        || msg.contains("status 529")
        || msg.contains("status 502")
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

    fn description(&self) -> String {
        "Submit your final structured output. Call this exactly once \
                          when your answer is complete."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        self.schema.clone()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match self.result.lock() {
            Ok(mut guard) => *guard = Some(args),
            Err(e) => {
                tracing::error!("submit result mutex poisoned: {e}");
                return Err(SubmitError);
            }
        }
        Ok("Output submitted.".to_string())
    }
}
