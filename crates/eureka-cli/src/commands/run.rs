//! `eureka run` command — execute a research session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use eureka_agents::{AgentDef, LlmAgentNode, RigClient};
use eureka_config::model::{EurekaConfig, ProviderKind};
use eureka_db::SessionDb;
use eureka_engine::registry::NodeRegistry;
use eureka_engine::session::Session;
use eureka_graph::node::BoxedNode;
use eureka_graph::port::{PortDirection, PortSpec, PortSpecEntry};
use eureka_plugins::{ControlPluginNode, PluginRegistry};
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::providers::{
    anthropic, cohere, deepseek, gemini, groq, mistral, ollama, openai, openrouter, perplexity,
    together, xai,
};
use tokio::sync::broadcast;

/// Arguments for the `run` command.
#[derive(Debug)]
pub struct RunArgs {
    /// Path to the config file.
    pub config_path: String,
    /// Research goal title.
    pub goal: String,
    /// Description of the research question.
    pub description: Option<String>,
    /// Domain of study.
    pub domain: String,
    /// Maximum number of rounds.
    pub max_rounds: Option<u32>,
    /// Output file path.
    pub output: Option<String>,
    /// Verbose output.
    pub verbose: bool,
    /// Port for the UI server (0 = disabled).
    pub port: u16,
}

/// Execute the `run` command.
///
/// # Errors
///
/// Returns an error if the session fails.
pub async fn execute(args: RunArgs) -> Result<()> {
    // Load config
    let config = if Path::new(&args.config_path).exists() {
        let config_str = std::fs::read_to_string(&args.config_path)
            .with_context(|| format!("Failed to read config file: {}", args.config_path))?;
        EurekaConfig::from_toml(&config_str)?
    } else {
        tracing::warn!(
            "Config file '{}' not found, using defaults",
            args.config_path
        );
        EurekaConfig::default()
    };

    // Override max rounds if specified
    let config = if let Some(max_rounds) = args.max_rounds {
        EurekaConfig {
            budget: eureka_config::model::BudgetConfig {
                max_rounds,
                ..config.budget
            },
            ..config
        }
    } else {
        config
    };

    // Generate session ID early so it can be shared with the DB and plugins.
    let session_id = uuid::Uuid::now_v7();

    let goal = serde_json::json!({
        "goal": args.goal,
        "description": args.description.clone().unwrap_or_default(),
        "domain": args.domain,
    });

    // Create session DB. Failure is non-fatal — the session still runs without persistence.
    let graph_id = Path::new(&config.graph)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let goal_json = serde_json::to_string(&goal).unwrap_or_else(|_| "{}".to_string());
    let db: Option<Arc<SessionDb>> =
        match SessionDb::create(&session_id.to_string(), &graph_id, &goal_json) {
            Ok(db) => {
                tracing::info!(
                    session_id = %session_id,
                    path = %db.path().display(),
                    "Session database created"
                );
                Some(Arc::new(db))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to create session database; events will not be persisted"
                );
                None
            }
        };
    let db_path: Option<PathBuf> = db.as_ref().map(|d| d.path().clone());

    // Build the node registry — loads all agents and plugins from the graph directory
    let registry = build_registry(&config, &session_id.to_string(), db_path)
        .context("Failed to build node registry")?;

    tracing::info!(
        goal = %args.goal,
        domain = %args.domain,
        session_id = %session_id,
        "Starting Eureka research session"
    );

    // Create the session with the pre-generated session ID
    let mut session =
        Session::new_with_id(config, registry, session_id).context("Failed to create session")?;

    if let Some(db) = db {
        session.set_db(db);
    }

    // Set up the broadcast channel for real-time observability
    let (event_tx, _) = broadcast::channel(256);
    session.set_event_broadcaster(event_tx.clone());

    // Start the UI server (skip if port = 0)
    let _server_handle;
    let _tracker_handle;
    if args.port > 0 {
        let live_state = Arc::new(tokio::sync::Mutex::new(crate::server::LiveState::default()));
        _tracker_handle =
            crate::server::track_live_state(event_tx.subscribe(), Arc::clone(&live_state));
        _server_handle =
            crate::server::start_server(session.spec().clone(), event_tx, live_state, args.port);
    } else {
        drop(event_tx);
        _tracker_handle = tokio::spawn(async {});
        _server_handle = tokio::spawn(async {});
    }

    let stats = session.run(goal).await.context("Failed to run session")?;

    tracing::info!(
        rounds = stats.rounds_completed,
        elapsed_secs = stats.elapsed_secs,
        "Research session completed"
    );

    if let Some(output_path) = args.output {
        let output_json =
            serde_json::to_string_pretty(&stats).context("Failed to serialize run stats")?;
        std::fs::write(&output_path, output_json)
            .with_context(|| format!("Failed to write output to {output_path}"))?;
        tracing::info!("Results written to {output_path}");
    }

    Ok(())
}

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
    let mut client_cache: HashMap<String, Arc<dyn eureka_agents::LlmClient>> = HashMap::new();

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
fn build_llm_client(
    config: &EurekaConfig,
    model_id: &str,
) -> Result<Arc<dyn eureka_agents::LlmClient>> {
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
