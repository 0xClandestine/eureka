//! Registry builder — constructs a [`NodeRegistry`] from agent definitions and
//! plugin manifests discovered on the filesystem.
//!
//! This module consolidates the discovery + registration logic that was
//! previously split across the CLI and plugin crates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use eureka_agents::{AgentDef, LlmAgentNode, LlmClient, RigClient};
use eureka_config::model::{EurekaConfig, ProviderKind};
use eureka_graph::node::BoxedNode;
use eureka_graph::port::{PortDirection, PortSpec, PortSpecEntry};
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::providers::{
    anthropic, cohere, deepseek, gemini, groq, mistral, ollama, openai, openrouter, perplexity,
    together, xai,
};

use super::plugin::{ControlPluginNode, PluginRegistry};
use super::registry::NodeRegistry;

/// Build the node registry.
///
/// Derives the agents directory as `<graph_dir>/agents/` and scans it for
/// `.md` + `.json` pairs, registering each as an [`LlmAgentNode`].
/// Plugins are discovered from `<graph_dir>/plugins/` and `~/.eureka/plugins/`
/// and registered as [`ControlPluginNode`]s.
///
/// # Errors
///
/// Returns an error if the agents directory cannot be read or a provider client
/// cannot be constructed (e.g. missing API key environment variable).
pub fn build_registry(
    config: &EurekaConfig,
    session_id: &str,
    db_path: Option<PathBuf>,
) -> Result<NodeRegistry> {
    let graph_dir = Path::new(&config.graph).parent().unwrap_or(Path::new("."));
    let agents_dir = graph_dir.join("agents");
    let mut registry = NodeRegistry::new();

    // ------------------------------------------------------------------ agents

    let defs = AgentDef::load_all(&agents_dir).with_context(|| {
        format!(
            "Failed to load agents from '{}'\n\
             Tip: run from the repo root so 'graphs/' is found",
            agents_dir.display()
        )
    })?;

    tracing::info!(count = defs.len(), dir = %agents_dir.display(), "Loaded agent definitions");

    // Build LLM clients — one per distinct model ID to avoid redundant
    // provider connections. Per-agent overrides use agent_models; others
    // fall back to generation_model.
    let default_model = config
        .provider
        .generation_model
        .as_deref()
        .unwrap_or("deepseek/deepseek-v4-flash");
    let mut client_cache: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();

    for def in defs {
        let def_arc = Arc::new(def);
        let ports = def_arc.to_port_spec();
        let name = def_arc.name.clone();

        let model_id = config
            .provider
            .agent_models
            .get(&name)
            .map(|s| s.as_str())
            .unwrap_or(default_model)
            .to_string();

        let client_arc = if let Some(c) = client_cache.get(&model_id) {
            Arc::clone(c)
        } else {
            let c = build_llm_client(config, &model_id)?;
            client_cache.insert(model_id.clone(), Arc::clone(&c));
            c
        };

        tracing::debug!(agent = %name, "Registering agent node");

        registry.register(
            name,
            Arc::new(move |_spec| {
                Ok(BoxedNode::new(LlmAgentNode::new(
                    Arc::clone(&def_arc),
                    Arc::clone(&client_arc),
                )))
            }),
            ports,
        );
    }

    // ----------------------------------------------------------------- plugins

    match PluginRegistry::discover(graph_dir) {
        Ok(plugin_registry) => {
            tracing::info!(count = plugin_registry.len(), "Discovered plugins");

            for (_name, entry) in plugin_registry.iter() {
                if !entry.manifest.has_node_role() {
                    continue;
                }
                let Some(node_cfg) = &entry.manifest.node else {
                    tracing::warn!(
                        plugin = %entry.manifest.name,
                        "Plugin declares node role but has no node config; skipping"
                    );
                    continue;
                };

                let ports = {
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
                    PortSpec::new(inputs, outputs)
                };

                let manifest = entry.manifest.clone();
                let plugin_dir = entry.plugin_dir.clone();
                let sid = session_id.to_string();
                let dbp = db_path.clone();

                tracing::debug!(plugin = %manifest.name, "Registering plugin node");

                registry.register(
                    manifest.name.clone(),
                    Arc::new(move |spec| {
                        Ok(BoxedNode::new(ControlPluginNode::new(
                            manifest.clone(),
                            plugin_dir.clone(),
                            sid.clone(),
                            dbp.clone(),
                            spec.config.clone(),
                        )))
                    }),
                    ports,
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Plugin discovery failed; no plugins will be available");
        }
    }

    Ok(registry)
}

/// Build a type-erased `LlmClient` from the provider configuration.
///
/// Each provider reads its API key from the corresponding environment variable:
///
/// | Provider    | Environment variable   |
/// |-------------|------------------------|
/// | anthropic   | `ANTHROPIC_API_KEY`    |
/// | openai      | `OPENAI_API_KEY`       |
/// | openrouter  | `OPENROUTER_API_KEY`   |
/// | gemini      | `GEMINI_API_KEY`       |
/// | groq        | `GROQ_API_KEY`         |
/// | mistral     | `MISTRAL_API_KEY`      |
/// | cohere      | `COHERE_API_KEY`       |
/// | deepseek    | `DEEPSEEK_API_KEY`     |
/// | perplexity  | `PERPLEXITY_API_KEY`   |
/// | together    | `TOGETHER_API_KEY`     |
/// | xai         | `XAI_API_KEY`          |
/// | ollama      | `OLLAMA_API_BASE_URL` (optional, defaults to `http://localhost:11434`) |
///
/// # Errors
///
/// Returns an error if the required API key environment variable is not set.
fn build_llm_client(config: &EurekaConfig, model_id: &str) -> Result<Arc<dyn LlmClient>> {
    match config.provider.kind {
        ProviderKind::Anthropic => {
            let client = anthropic::Client::from_env()
                .context("ANTHROPIC_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::OpenAI => {
            let client = openai::Client::from_env()
                .context("OPENAI_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::OpenRouter => {
            let client = openrouter::Client::from_env()
                .context("OPENROUTER_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::Gemini => {
            let client = gemini::Client::from_env()
                .context("GEMINI_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::Groq => {
            let client =
                groq::Client::from_env().context("GROQ_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::Mistral => {
            let client = mistral::Client::from_env()
                .context("MISTRAL_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::Cohere => {
            let client = cohere::Client::from_env()
                .context("COHERE_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::DeepSeek => {
            let client = deepseek::Client::from_env()
                .context("DEEPSEEK_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::Perplexity => {
            let client = perplexity::Client::from_env()
                .context("PERPLEXITY_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::Together => {
            let client = together::Client::from_env()
                .context("TOGETHER_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::XAI => {
            let client =
                xai::Client::from_env().context("XAI_API_KEY environment variable not set")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
        ProviderKind::Ollama => {
            let client = ollama::Client::from_env()
                .context("Failed to initialise Ollama client (check OLLAMA_API_BASE_URL)")?;
            Ok(Arc::new(RigClient::new(client.completion_model(model_id))))
        }
    }
}
