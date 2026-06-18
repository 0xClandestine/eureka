//! `eureka validate` command — validate a graph specification file.
//!
//! Builds a port registry by loading the real agent definitions and plugin
//! manifests from the graph's directory, then validates the graph topology
//! against them. This means validation is fully data-driven: no node kind is
//! hardcoded here.

use std::path::Path;

use anyhow::{Context, Result};
use eureka_agents::AgentDef;
use eureka_engine::plugin::PluginRegistry;
use eureka_graph::port::{PortDirection, PortSpec, PortSpecEntry};
use eureka_graph::spec::GraphSpec;
use eureka_graph::validate::{validate_graph, PortRegistry};

/// Execute the `validate` command.
///
/// # Errors
///
/// Returns an error if the graph specification is invalid.
pub async fn execute(graph_path: String) -> Result<()> {
    let content = std::fs::read_to_string(&graph_path)
        .with_context(|| format!("Failed to read graph spec: {graph_path}"))?;

    let spec = if graph_path.ends_with(".json") {
        GraphSpec::from_json(&content)
            .with_context(|| format!("Failed to parse JSON graph spec: {graph_path}"))?
    } else {
        GraphSpec::from_toml(&content)
            .with_context(|| format!("Failed to parse TOML graph spec: {graph_path}"))?
    };

    println!("Graph: {}", spec.name.as_deref().unwrap_or("(unnamed)"));
    println!("  Nodes: {}", spec.nodes.len());
    println!("  Edges: {}", spec.edges.len());

    let graph_dir = Path::new(&graph_path).parent().unwrap_or(Path::new("."));

    let registry =
        build_registry_for_validation(graph_dir).context("Failed to build validation registry")?;

    let result = validate_graph(&spec, &registry);

    if result.valid {
        println!("✓ Graph specification is valid.");
        Ok(())
    } else {
        println!(
            "✗ Graph validation failed with {} errors:",
            result.errors.len()
        );
        for (i, err) in result.errors.iter().enumerate() {
            println!("  {}. {err}", i + 1);
        }
        anyhow::bail!("Graph validation failed");
    }
}

/// Build a `PortRegistry` by scanning `graph_dir` for agents and plugins.
///
/// - Agents are loaded from `<graph_dir>/agents/`.
/// - Plugins are discovered from `<graph_dir>/plugins/` and `~/.eureka/plugins/`.
///
/// Missing directories are silently skipped so validation works even for
/// partial graph packages.
///
/// # Errors
///
/// Returns an error if a present `agents/` directory or `plugin.json` file
/// cannot be read.
fn build_registry_for_validation(graph_dir: &Path) -> Result<PortRegistry> {
    let mut reg = PortRegistry::new();

    // Register agent kinds from the agents directory.
    let agents_dir = graph_dir.join("agents");
    if agents_dir.is_dir() {
        let defs = AgentDef::load_all(&agents_dir)
            .with_context(|| format!("Failed to load agents from '{}'", agents_dir.display()))?;
        for def in defs {
            reg.register(def.name.clone(), def.to_port_spec());
        }
    }

    // Register plugin node kinds from plugin manifests.
    let plugin_registry = PluginRegistry::discover(graph_dir)
        .with_context(|| format!("Failed to discover plugins in '{}'", graph_dir.display()))?;

    for (_name, entry) in plugin_registry.iter() {
        if !entry.manifest.has_node_role() {
            continue;
        }
        let Some(node_cfg) = &entry.manifest.node else {
            continue;
        };

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

        reg.register(entry.manifest.name.clone(), PortSpec::new(inputs, outputs));
    }

    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn write_agent(dir: &Path, name: &str, json: &str, md: &str) {
        let agents = dir.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::File::create(agents.join(format!("{name}.json")))
            .unwrap()
            .write_all(json.as_bytes())
            .unwrap();
        std::fs::File::create(agents.join(format!("{name}.md")))
            .unwrap()
            .write_all(md.as_bytes())
            .unwrap();
    }

    fn write_plugin(dir: &Path, name: &str, json: &str) {
        let plugin_dir = dir.join("plugins").join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::File::create(plugin_dir.join("plugin.json"))
            .unwrap()
            .write_all(json.as_bytes())
            .unwrap();
    }

    #[test]
    fn test_registry_loads_agents_and_plugins() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_agent(
            dir,
            "generation",
            r#"{"inputs":[{"kind":"Goal","port":"in"}],"outputs":[{"kind":"Hypotheses","port":"out"}],"output_schema":{"type":"object"}}"#,
            "Preamble.",
        );

        write_plugin(
            dir,
            "round-governor",
            r#"{
                "name": "round-governor",
                "version": "1.0.0",
                "description": "Governor.",
                "runtime": "process",
                "command": ["python3", "governor.py"],
                "roles": ["node"],
                "node": {
                    "inputs":  [{"port": "in",       "kind": "Hypotheses"}],
                    "outputs": [
                        {"port": "continue", "kind": "Control"},
                        {"port": "halt",     "kind": "Control"}
                    ]
                }
            }"#,
        );

        let reg = build_registry_for_validation(dir).unwrap();
        assert!(reg.get("generation").is_some());
        assert!(reg.get("round-governor").is_some());
    }

    #[test]
    fn test_registry_empty_dirs_ok() {
        let tmp = TempDir::new().unwrap();
        let reg = build_registry_for_validation(tmp.path()).unwrap();
        assert!(reg.get("anything").is_none());
    }
}
