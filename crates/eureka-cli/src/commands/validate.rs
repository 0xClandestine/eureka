//! `eureka validate` command — validate a graph specification file.
//!
//! Loads the YAML manifest, builds a port registry from agent and control
//! specs declared inline, then validates the graph topology.

use std::path::Path;

use anyhow::{Context, Result};
use eureka::graph::port::{PortDirection, PortSpec, PortSpecEntry};
use eureka::graph::validate::{validate_graph, PortRegistry};
use eureka::manifest::GraphManifest;

/// Execute the `validate` command.
///
/// # Errors
///
/// Returns an error if the graph specification is invalid.
pub fn execute(graph_path: &str) -> Result<()> {
    let manifest = GraphManifest::load(Path::new(graph_path))
        .with_context(|| format!("Failed to load graph manifest from '{graph_path}'"))?;

    let spec = manifest.to_graph_spec();

    println!("Graph: {}", spec.name.as_deref().unwrap_or("(unnamed)"));
    println!("  Nodes: {}", spec.nodes.len());
    println!("  Edges: {}", spec.edges.len());

    let registry = build_registry_from_manifest(&manifest);
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

/// Build a `PortRegistry` from the agents and control nodes declared in a
/// `GraphManifest`. No filesystem scanning needed — everything is inline.
fn build_registry_from_manifest(manifest: &GraphManifest) -> PortRegistry {
    let mut reg = PortRegistry::new();

    for agent in &manifest.agents {
        let inputs = agent
            .inputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Input,
                kind: p.kind.clone(),
                required: true,
            })
            .collect();

        let outputs = agent
            .outputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Output,
                kind: p.kind.clone(),
                required: false,
            })
            .collect();

        reg.register(agent.id.clone(), PortSpec::new(inputs, outputs));
    }

    for ctrl in &manifest.control {
        let inputs = ctrl
            .inputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Input,
                kind: p.kind.clone(),
                required: true,
            })
            .collect();

        let outputs = ctrl
            .outputs
            .iter()
            .map(|p| PortSpecEntry {
                name: p.port.clone(),
                direction: PortDirection::Output,
                kind: p.kind.clone(),
                required: false,
            })
            .collect();

        reg.register(ctrl.kind.clone(), PortSpec::new(inputs, outputs));
    }

    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, yml: &str) -> PathBuf {
        let path = dir.join("test.yml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(yml.as_bytes()).unwrap();
        // Create a dummy prompt file referenced by the manifest
        std::fs::create_dir_all(dir.join("prompts")).unwrap();
        std::fs::write(dir.join("prompts/gen.md"), "test prompt").unwrap();
        path
    }

    #[test]
    fn test_valid_manifest() {
        let dir = TempDir::new().unwrap();
        let path = write_manifest(
            dir.path(),
            r#"
name: test
agents:
  - id: gen
    prompt: prompts/gen.md
    inputs:
      - port: in
        kind: Goal
    outputs:
      - port: out
        kind: Hypotheses
    output_schema:
      type: object
edges:
  - from_node: gen
    from_port: out
    to_node: sink
    to_port: in
"#,
        );
        let manifest = GraphManifest::load(&path).unwrap();
        let reg = build_registry_from_manifest(&manifest);
        assert!(reg.get("gen").is_some());
    }

    #[test]
    fn test_empty_manifest_ok() {
        let dir = TempDir::new().unwrap();
        let path = write_manifest(dir.path(), "name: empty");
        let manifest = GraphManifest::load(&path).unwrap();
        let reg = build_registry_from_manifest(&manifest);
        assert!(reg.get("anything").is_none());
    }
}
