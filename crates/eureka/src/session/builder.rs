//! Node construction and LLM client building for [`super::Session`].
//!
//! Separates the "build from manifest" concerns from the public API and
//! the scheduler execution loop.

use std::sync::Arc;

use crate::agent::def::{AgentConfig, AgentDef, ToolDef};
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

impl super::Session {
    /// Construct a single node from a [`GraphNodeSpec`] using manifest data.
    pub(super) fn construct_node(
        &mut self,
        spec: &GraphNodeSpec,
    ) -> Result<BoxedNode, EngineError> {
        // Find agent or control spec by node ID (clone out to drop immutable borrow before
        // calling build methods which need &mut self for the LLM client cache).
        let agent_spec = self
            .manifest
            .agents
            .iter()
            .find(|a| a.id == spec.id)
            .cloned();
        let ctrl_spec = self
            .manifest
            .control
            .iter()
            .find(|c| c.id == spec.id)
            .cloned();
        let node_config = spec.config.clone();

        if let Some(agent) = agent_spec {
            return self.build_agent_node(&agent);
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
    pub(crate) fn build_agent_node(
        &mut self,
        agent_spec: &AgentSpec,
    ) -> Result<BoxedNode, EngineError> {
        let prompt_content = agent_spec.prompt.read().map_err(|e| {
            EngineError::NodeCreation(format!(
                "Failed to read prompt '{}': {e}",
                agent_spec.prompt.as_str()
            ))
        })?;

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
        };

        // Reject tool names that shadow the terminal `submit` tool — the
        // agent would never be able to terminate.
        for tool in &agent_def.tools {
            if tool.name.eq_ignore_ascii_case("submit") {
                return Err(EngineError::NodeCreation(format!(
                    "Agent '{}' declares a tool named 'submit', which is reserved
                     for the agent's terminal output tool. Rename the tool.",
                    agent_spec.id
                )));
            }
        }

        let model_id = self
            .config
            .provider
            .agent_models
            .get(&agent_spec.id)
            .map_or(self.default_model.as_str(), String::as_str)
            .to_string();

        let base_client = self.get_or_create_client(&model_id).map_err(|e| {
            EngineError::NodeCreation(format!(
                "Failed to build LLM client for model '{model_id}' (agent '{}'): {e}",
                agent_spec.id
            ))
        })?;

        // Attach RAG dynamic_context if the index is ready and this agent is
        // in the configured allow-list (empty list = all agents).
        let client_arc = if let (Some(rag_index), Some(rag_cfg)) = (
            &self.rag_index,
            self.config.rag.as_ref().filter(|r| r.enabled),
        ) {
            if rag_cfg.agent_ids.is_empty() || rag_cfg.agent_ids.contains(&agent_spec.id) {
                build_rag_client(&self.config, &model_id, rag_index.clone(), rag_cfg.top_k)
                    .map_err(|e| {
                        EngineError::NodeCreation(format!(
                            "Failed to build RAG-enabled client for agent '{}': {e}",
                            agent_spec.id
                        ))
                    })?
            } else {
                base_client
            }
        } else {
            base_client
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
            name: format!("{}.{}", ctrl_spec.kind, ctrl_spec.id),
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

    /// Get or create an LLM client for the given model ID.
    fn get_or_create_client(
        &mut self,
        model_id: &str,
    ) -> Result<Arc<dyn LlmClient>, anyhow::Error> {
        if let Some(client) = self.client_cache.get(model_id) {
            return Ok(Arc::clone(client));
        }

        let client = build_llm_client(&self.config, model_id)?;
        self.client_cache
            .insert(model_id.to_string(), Arc::clone(&client));
        Ok(client)
    }
}

/// Build a type-erased [`LlmClient`] from the provider configuration.
pub(super) fn build_llm_client(
    config: &EurekaConfig,
    model_id: &str,
) -> Result<Arc<dyn LlmClient>, anyhow::Error> {
    let pricing = config.provider.pricing.clone();
    macro_rules! provider {
        ($provider:ident, $env_key:expr) => {{
            let client = $provider::Client::from_env()
                .context(concat!($env_key, " environment variable not set"))?;
            Arc::new(RigClient::new(
                client.completion_model(model_id),
                pricing.clone(),
            ))
        }};
        ($provider:ident, $env_key:expr, $msg:expr) => {{
            let client = $provider::Client::from_env().context($msg)?;
            Arc::new(RigClient::new(
                client.completion_model(model_id),
                pricing.clone(),
            ))
        }};
    }
    match config.provider.kind {
        ProviderKind::Anthropic => Ok(provider!(anthropic, "ANTHROPIC_API_KEY")),
        ProviderKind::OpenAI => Ok(provider!(openai, "OPENAI_API_KEY")),
        ProviderKind::OpenRouter => Ok(provider!(openrouter, "OPENROUTER_API_KEY")),
        ProviderKind::Gemini => Ok(provider!(gemini, "GEMINI_API_KEY")),
        ProviderKind::Groq => Ok(provider!(groq, "GROQ_API_KEY")),
        ProviderKind::Mistral => Ok(provider!(mistral, "MISTRAL_API_KEY")),
        ProviderKind::Cohere => Ok(provider!(cohere, "COHERE_API_KEY")),
        ProviderKind::DeepSeek => Ok(provider!(deepseek, "DEEPSEEK_API_KEY")),
        ProviderKind::Perplexity => Ok(provider!(perplexity, "PERPLEXITY_API_KEY")),
        ProviderKind::Together => Ok(provider!(together, "TOGETHER_API_KEY")),
        ProviderKind::XAI => Ok(provider!(xai, "XAI_API_KEY")),
        ProviderKind::Ollama => Ok(provider!(
            ollama,
            "OLLAMA_API_KEY",
            "Failed to initialise Ollama client (check OLLAMA_API_BASE_URL)"
        )),
    }
}

/// Build a type-erased [`LlmClient`] with a RAG index attached via
/// `dynamic_context`. Mirrors [`build_llm_client`] but calls `.with_rag()`
/// before boxing so each RAG-enabled agent gets its own retrieval path.
fn build_rag_client(
    config: &EurekaConfig,
    model_id: &str,
    rag_index: crate::rag::RagIndexHandle,
    top_k: usize,
) -> Result<Arc<dyn LlmClient>, anyhow::Error> {
    let pricing = config.provider.pricing.clone();
    macro_rules! provider_rag {
        ($provider:ident, $env_key:expr) => {{
            let client = $provider::Client::from_env()
                .context(concat!($env_key, " environment variable not set"))?;
            let rig_client = RigClient::new(client.completion_model(model_id), pricing.clone())
                .with_rag(rag_index, top_k);
            let r: Arc<dyn LlmClient> = Arc::new(rig_client);
            r
        }};
        ($provider:ident, $env_key:expr, $msg:expr) => {{
            let client = $provider::Client::from_env().context($msg)?;
            let rig_client = RigClient::new(client.completion_model(model_id), pricing.clone())
                .with_rag(rag_index, top_k);
            let r: Arc<dyn LlmClient> = Arc::new(rig_client);
            r
        }};
    }
    match config.provider.kind {
        ProviderKind::Anthropic => Ok(provider_rag!(anthropic, "ANTHROPIC_API_KEY")),
        ProviderKind::OpenAI => Ok(provider_rag!(openai, "OPENAI_API_KEY")),
        ProviderKind::OpenRouter => Ok(provider_rag!(openrouter, "OPENROUTER_API_KEY")),
        ProviderKind::Gemini => Ok(provider_rag!(gemini, "GEMINI_API_KEY")),
        ProviderKind::Groq => Ok(provider_rag!(groq, "GROQ_API_KEY")),
        ProviderKind::Mistral => Ok(provider_rag!(mistral, "MISTRAL_API_KEY")),
        ProviderKind::Cohere => Ok(provider_rag!(cohere, "COHERE_API_KEY")),
        ProviderKind::DeepSeek => Ok(provider_rag!(deepseek, "DEEPSEEK_API_KEY")),
        ProviderKind::Perplexity => Ok(provider_rag!(perplexity, "PERPLEXITY_API_KEY")),
        ProviderKind::Together => Ok(provider_rag!(together, "TOGETHER_API_KEY")),
        ProviderKind::XAI => Ok(provider_rag!(xai, "XAI_API_KEY")),
        ProviderKind::Ollama => Ok(provider_rag!(
            ollama,
            "OLLAMA_API_KEY",
            "Failed to initialise Ollama client (check OLLAMA_API_BASE_URL)"
        )),
    }
}
