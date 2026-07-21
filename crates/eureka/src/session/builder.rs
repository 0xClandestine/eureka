//! Node construction and LLM client building for [`super::Session`].
//!
//! Separates the "build from manifest" concerns from the public API and
//! the scheduler execution loop.

use std::path::Path;
use std::sync::Arc;

use crate::agent::client::McpConnection;
use crate::agent::def::{AgentConfig, AgentDef, McpServerDef, McpTransport, ToolDef};
use crate::agent::{LlmAgentNode, LlmClient, RigClient};
use crate::config::{EurekaConfig, ProviderKind};
use crate::control::{ControlNode, ControlNodeDef};
use crate::error::EngineError;
use crate::graph::node::BoxedNode;
use crate::graph::spec::GraphNodeSpec;
use crate::manifest::{AgentSpec, ControlSpec};
use crate::persistence::RunEnvironment;
use anyhow::Context;
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::providers::{
    anthropic, cohere, deepseek, gemini, groq, mistral, ollama, openai, openrouter, perplexity,
    together, xai,
};
use rmcp::ServiceExt as _;

impl super::Session {
    /// Construct a single node from a [`GraphNodeSpec`] using manifest data.
    pub(super) async fn construct_node(
        &mut self,
        spec: &GraphNodeSpec,
    ) -> Result<BoxedNode, EngineError> {
        // Find agent or control spec by node ID (clone out to drop immutable borrow before
        // calling build methods which need &mut self for the LLM client cache).
        let agent_spec = self.manifest.agents.iter().find(|a| a.id == spec.id).cloned();
        let ctrl_spec = self.manifest.control.iter().find(|c| c.id == spec.id).cloned();
        let node_config = spec.config.clone();

        if let Some(agent) = agent_spec {
            return self.build_agent_node(&agent).await;
        }

        if let Some(ctrl) = ctrl_spec {
            return Ok(self.build_control_node(&ctrl, &node_config));
        }

        Err(EngineError::UnknownNodeKind(format!(
            "Node '{}' (kind: '{}') not found in manifest agents or control nodes. \
             Every node in the graph must have a matching entry in the YAML manifest.",
            spec.id, spec.kind
        )))
    }

    /// Build an LLM agent node from an [`AgentSpec`].
    // Orchestrates prompt loading, AgentDef construction, MCP connection/validation,
    // RAG resolution, and LLM client selection in a single sequential flow.
    // Splitting into smaller helpers would increase overall LOC without improving clarity.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn build_agent_node(
        &mut self,
        agent_spec: &AgentSpec,
    ) -> Result<BoxedNode, EngineError> {
        let prompt_content = agent_spec.prompt.read().map_err(|e| {
            EngineError::NodeCreation(format!(
                "Failed to read prompt '{}': {e}",
                agent_spec.prompt.as_str()
            ))
        })?;

        // Convert manifest MCP server specs to runtime defs.
        let mcp_server_defs: Vec<McpServerDef> = agent_spec
            .mcp_servers
            .iter()
            .map(|s| McpServerDef {
                name: s.name.clone(),
                transport: match &s.transport {
                    crate::manifest::agent::McpTransportSpec::Stdio { command, env } => {
                        McpTransport::Stdio { command: command.clone(), env: env.clone() }
                    }
                    crate::manifest::agent::McpTransportSpec::Http { uri } => {
                        McpTransport::Http { uri: uri.clone() }
                    }
                },
            })
            .collect();

        let agent_def = AgentDef {
            name: agent_spec.id.clone(),
            description: agent_spec.description.clone(),
            preamble: prompt_content,
            inputs: agent_spec.inputs.clone(),
            outputs: agent_spec.outputs.clone(),
            config: AgentConfig::resolve_for(
                &self.config.agent,
                &self.config.agent_overrides,
                &agent_spec.id,
            ),
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
            mcp_servers: mcp_server_defs,
        };

        // Reject CommandTool names that shadow the terminal `submit` tool.
        for tool in &agent_def.tools {
            if tool.name.eq_ignore_ascii_case("submit") {
                return Err(EngineError::NodeCreation(format!(
                    "Agent '{}' declares a tool named 'submit', which is reserved \
                     for the agent's terminal output tool. Rename the tool.",
                    agent_spec.id
                )));
            }
        }

        // Connect MCP servers declared by this agent.
        let mcp_connections = connect_mcp_servers(&agent_def.mcp_servers, &self.graph_dir).await?;

        // Validate MCP tool names: must not be 'submit', must not clash with
        // CommandTools, and must be unique across all MCP servers for this agent.
        let command_names: Vec<&str> = agent_def.tools.iter().map(|t| t.name.as_str()).collect();
        let mut seen_mcp: std::collections::HashSet<String> = std::collections::HashSet::new();
        for conn in &mcp_connections {
            for mcp_tool in &conn.tools {
                let lower = mcp_tool.name.to_ascii_lowercase();
                if lower == "submit" {
                    return Err(EngineError::NodeCreation(format!(
                        "Agent '{}': MCP server exposes a tool named 'submit', \
                         which is reserved. Rename the MCP tool or use a different server.",
                        agent_spec.id
                    )));
                }
                if command_names.iter().any(|n| n.eq_ignore_ascii_case(&mcp_tool.name)) {
                    return Err(EngineError::NodeCreation(format!(
                        "Agent '{}': MCP tool '{}' conflicts with a CommandTool of the \
                         same name. Tool names must be unique across all sources.",
                        agent_spec.id, mcp_tool.name
                    )));
                }
                if !seen_mcp.insert(lower) {
                    return Err(EngineError::NodeCreation(format!(
                        "Agent '{}': MCP tool name '{}' appears on more than one server. \
                         Tool names must be unique across all MCP servers for this agent.",
                        agent_spec.id, mcp_tool.name
                    )));
                }
            }
        }

        let model_id = self
            .config
            .provider
            .agent_models
            .get(&agent_spec.id)
            .map_or(self.default_model.as_str(), String::as_str)
            .to_string();

        // If this agent has MCP connections, build a fresh dedicated client
        // (not cached) so the MCP state is not shared with other agents.
        // Otherwise, use the cached base client with optional RAG.
        let rag = self.rag_for(&agent_spec.id);
        let client_arc: Arc<dyn LlmClient> = if mcp_connections.is_empty() {
            let base = self.get_or_create_client(&model_id).map_err(|e| {
                EngineError::NodeCreation(format!(
                    "Failed to build LLM client for model '{model_id}' (agent '{}'): {e}",
                    agent_spec.id
                ))
            })?;
            if let Some(rag) = rag {
                build_client(&self.config, &model_id, Some(rag), Vec::new()).map_err(|e| {
                    EngineError::NodeCreation(format!(
                        "Failed to build LLM client for agent '{}': {e}",
                        agent_spec.id
                    ))
                })?
            } else {
                base
            }
        } else {
            build_client(&self.config, &model_id, rag, mcp_connections).map_err(|e| {
                EngineError::NodeCreation(format!(
                    "Failed to build LLM client for model '{model_id}' (agent '{}'): {e}",
                    agent_spec.id
                ))
            })?
        };

        Ok(BoxedNode::new(LlmAgentNode::with_environment(
            Arc::new(agent_def),
            client_arc,
            self.graph_dir.clone(),
            RunEnvironment::new(self.session_id.to_string(), self.db_path.clone()),
        )))
    }

    /// Build a control plugin node from a [`ControlSpec`].
    ///
    /// The command is used verbatim: the manifest's path resolver
    /// (`ControlSpec::resolve_paths`) already rewrites relative `command[0]`
    /// to an absolute path when the resolved binary exists on disk; for bare
    /// interpreter names like `python3` it leaves them alone so the OS PATH
    /// lookup is used. The control node spawns with `current_dir = graph_dir`,
    /// so relative script arguments (e.g. `control/ranker.py`) resolve against
    /// the graph directory automatically. Rewriting `command[0]` here would
    /// turn bare `python3` into `<graph_dir>/python3`, which does not exist.
    pub(crate) fn build_control_node(
        &self,
        ctrl_spec: &ControlSpec,
        node_config: &serde_json::Value,
    ) -> BoxedNode {
        let def = ControlNodeDef {
            name: format!("{}::{}", ctrl_spec.kind, ctrl_spec.id),
            work_dir: self.graph_dir.clone(),
            command: ctrl_spec.command.clone(),
            inputs: ctrl_spec.inputs.clone(),
            outputs: ctrl_spec.outputs.clone(),
            timeout_secs: ctrl_spec.timeout_secs,
        };

        BoxedNode::new(ControlNode::with_environment(
            def,
            RunEnvironment::new(self.session_id.to_string(), self.db_path.clone()),
            node_config.clone(),
        ))
    }

    /// Resolve the RAG parameters for `agent_id`.
    ///
    /// Returns `None` when RAG is disabled globally, the agent is excluded by
    /// the `rag.agent_ids` filter, or the agent's own `rag.enabled = false`.
    /// Per-agent `top_k` overrides the global value when set.
    fn rag_for(&self, agent_id: &str) -> Option<(crate::rag::RagIndexHandle, usize)> {
        let rag_index = self.rag_index.as_ref()?;
        let rag_cfg = self.config.rag.as_ref().filter(|r| r.enabled)?;

        // Global agent_ids inclusion filter.
        if !rag_cfg.agent_ids.is_empty() && !rag_cfg.agent_ids.iter().any(|id| id == agent_id) {
            return None;
        }

        // Per-agent NodeRagConfig: enabled flag and top_k override.
        let node_rag =
            self.manifest.agents.iter().find(|a| a.id == agent_id).and_then(|a| a.rag.as_ref());

        match node_rag {
            Some(nr) if !nr.enabled => None,
            Some(nr) => Some((rag_index.clone(), nr.top_k.unwrap_or(rag_cfg.top_k))),
            None => Some((rag_index.clone(), rag_cfg.top_k)),
        }
    }

    /// Get or create an LLM client for the given model ID.
    fn get_or_create_client(
        &mut self,
        model_id: &str,
    ) -> Result<Arc<dyn LlmClient>, anyhow::Error> {
        if let Some(client) = self.client_cache.get(model_id) {
            return Ok(Arc::clone(client));
        }

        let client = build_client(&self.config, model_id, None, Vec::new())?;
        self.client_cache.insert(model_id.to_string(), Arc::clone(&client));
        Ok(client)
    }
}

// ---------------------------------------------------------------------------
// MCP server connection helpers
// ---------------------------------------------------------------------------

/// Connect all MCP servers declared by an agent, returning live `McpConnection`s.
///
/// Individual server failures are non-fatal: a warning is logged and the
/// server is skipped so the agent runs without its tools rather than aborting
/// the entire run.  This allows optional servers (e.g. `lean-lsp-mcp`) to be
/// listed in the manifest without requiring them to be installed on every
/// machine.
async fn connect_mcp_servers(
    specs: &[McpServerDef],
    graph_dir: &Path,
) -> Result<Vec<McpConnection>, EngineError> {
    let mut connections = Vec::with_capacity(specs.len());
    for spec in specs {
        match connect_one_server(spec, graph_dir).await {
            Ok(conn) => {
                tracing::info!(
                    server = %spec.name,
                    tools = conn.tools.len(),
                    "MCP server connected"
                );
                connections.push(conn);
            }
            Err(e) => {
                tracing::warn!(
                    server = %spec.name,
                    error = %e,
                    "MCP server unavailable — agent will run without its tools"
                );
            }
        }
    }
    Ok(connections)
}

/// Establish a single MCP server connection and list its tools.
async fn connect_one_server(
    spec: &McpServerDef,
    graph_dir: &Path,
) -> Result<McpConnection, anyhow::Error> {
    match &spec.transport {
        McpTransport::Stdio { command, env } => {
            anyhow::ensure!(!command.is_empty(), "MCP stdio command must not be empty");
            let mut cmd = tokio::process::Command::new(&command[0]);
            cmd.args(&command[1..]).current_dir(graph_dir).envs(env);
            let transport = rmcp::transport::TokioChildProcess::new(cmd)
                .context("Failed to spawn MCP stdio subprocess")?;
            let service = rmcp::model::ClientInfo::default()
                .serve(transport)
                .await
                .context("MCP stdio handshake failed")?;
            let tools = service
                .peer()
                .list_all_tools()
                .await
                .context("Failed to list tools from MCP stdio server")?;
            let sink = service.peer().clone();
            Ok(McpConnection { tools, sink, _service: Box::new(service) })
        }
        McpTransport::Http { uri } => {
            let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(uri.as_str());
            let service = rmcp::model::ClientInfo::default()
                .serve(transport)
                .await
                .context("MCP HTTP handshake failed")?;
            let tools = service
                .peer()
                .list_all_tools()
                .await
                .context("Failed to list tools from MCP HTTP server")?;
            let sink = service.peer().clone();
            Ok(McpConnection { tools, sink, _service: Box::new(service) })
        }
    }
}

// ---------------------------------------------------------------------------
// LLM client builders
// ---------------------------------------------------------------------------

/// Build a type-erased [`LlmClient`] from the provider configuration.
///
/// `rag` and `mcp` are optional — pass `None` / `Vec::new()` for a plain
/// client.  The resulting client is NOT cached; caching is the caller's
/// responsibility (see [`super::Session::get_or_create_client`]).
pub(super) fn build_client(
    config: &EurekaConfig,
    model_id: &str,
    rag: Option<(crate::rag::RagIndexHandle, usize)>,
    mcp: Vec<McpConnection>,
) -> Result<Arc<dyn LlmClient>, anyhow::Error> {
    let pricing = config.provider.pricing.clone();
    macro_rules! make {
        ($provider:ident, $env_key:expr) => {{
            let client = $provider::Client::from_env()
                .context(concat!($env_key, " environment variable not set"))?;
            let mut rig_client = RigClient::new(client.completion_model(model_id), pricing.clone());
            if let Some((idx, top_k)) = rag {
                rig_client = rig_client.with_rag(idx, top_k);
            }
            let r: Arc<dyn LlmClient> = Arc::new(rig_client.with_mcp(mcp));
            r
        }};
        ($provider:ident, $env_key:expr, $msg:expr) => {{
            let client = $provider::Client::from_env().context($msg)?;
            let mut rig_client = RigClient::new(client.completion_model(model_id), pricing.clone());
            if let Some((idx, top_k)) = rag {
                rig_client = rig_client.with_rag(idx, top_k);
            }
            let r: Arc<dyn LlmClient> = Arc::new(rig_client.with_mcp(mcp));
            r
        }};
    }
    match config.provider.kind {
        ProviderKind::Anthropic => Ok(make!(anthropic, "ANTHROPIC_API_KEY")),
        ProviderKind::OpenAI => Ok(make!(openai, "OPENAI_API_KEY")),
        ProviderKind::OpenRouter => Ok(make!(openrouter, "OPENROUTER_API_KEY")),
        ProviderKind::Gemini => Ok(make!(gemini, "GEMINI_API_KEY")),
        ProviderKind::Groq => Ok(make!(groq, "GROQ_API_KEY")),
        ProviderKind::Mistral => Ok(make!(mistral, "MISTRAL_API_KEY")),
        ProviderKind::Cohere => Ok(make!(cohere, "COHERE_API_KEY")),
        ProviderKind::DeepSeek => Ok(make!(deepseek, "DEEPSEEK_API_KEY")),
        ProviderKind::Perplexity => Ok(make!(perplexity, "PERPLEXITY_API_KEY")),
        ProviderKind::Together => Ok(make!(together, "TOGETHER_API_KEY")),
        ProviderKind::XAI => Ok(make!(xai, "XAI_API_KEY")),
        ProviderKind::Ollama => Ok(make!(
            ollama,
            "OLLAMA_API_KEY",
            "Failed to initialise Ollama client (check OLLAMA_API_BASE_URL)"
        )),
    }
}
