//! `ControlPluginNode` — a graph node backed by a subprocess plugin.
//!
//! When the scheduler activates a `ControlPluginNode`, it:
//! 1. Writes a JSON call envelope to the subprocess stdin.
//! 2. Reads JSON emit envelopes (one per line) from stdout.
//! 3. Returns the parsed emits to the scheduler.
//!
//! Environment variables available to the subprocess:
//! - `EUREKA_SESSION_ID` — the session UUID
//! - `EUREKA_NODE_ID` — the node ID from `graph.json`
//! - `EUREKA_ROUND` — current round number
//! - `EUREKA_CONFIG` — JSON-encoded node `config` object from `graph.json`
//! - `EUREKA_DB_PATH` — path to the session SQLite database (if available)

use std::path::PathBuf;

use async_trait::async_trait;
use eureka_graph::artifact::Artifact;
use eureka_graph::node::{Emit, Node, NodeCtx, NodeError, PortMsg};
use eureka_graph::port::{PortDirection, PortSpec, PortSpecEntry};
use eureka_graph::process::run_subprocess;

use super::manifest::PluginManifest;

/// A graph node implemented by a subprocess plugin.
pub struct ControlPluginNode {
    /// The plugin manifest declaring command, ports, and timeout.
    manifest: PluginManifest,
    /// Absolute path to the directory containing the plugin scripts.
    plugin_dir: PathBuf,
    /// Session ID passed to the subprocess as `EUREKA_SESSION_ID`.
    session_id: String,
    /// Path to the session database, passed as `EUREKA_DB_PATH`.
    db_path: Option<PathBuf>,
    /// The per-node config object from `graph.json`.
    config: serde_json::Value,
}

impl ControlPluginNode {
    /// Create a new plugin node.
    #[must_use]
    pub fn new(
        manifest: PluginManifest,
        plugin_dir: PathBuf,
        session_id: String,
        db_path: Option<PathBuf>,
        config: serde_json::Value,
    ) -> Self {
        Self {
            manifest,
            plugin_dir,
            session_id,
            db_path,
            config,
        }
    }

    /// Invoke the subprocess and collect emit envelopes from stdout.
    async fn invoke(&self, ctx: &NodeCtx, msg: &PortMsg) -> Result<Vec<Emit>, NodeError> {
        let node_cfg = self
            .manifest
            .node
            .as_ref()
            .ok_or_else(|| NodeError::Internal("Plugin has no node role config".to_string()))?;

        // Build the call envelope written to stdin.
        let envelope = serde_json::json!({
            "port": msg.port,
            "artifact": {
                "kind": msg.artifact.kind,
                "data": msg.artifact.data,
            }
        });
        let envelope_str = serde_json::to_string(&envelope)
            .map_err(|e| NodeError::Internal(format!("Failed to serialize call envelope: {e}")))?;

        let config_str = serde_json::to_string(&self.config).unwrap_or_else(|_| "{}".to_string());

        // Build env vars.
        let mut envs: Vec<(&str, String)> = vec![
            ("EUREKA_SESSION_ID", self.session_id.clone()),
            ("EUREKA_NODE_ID", ctx.node_id.clone()),
            ("EUREKA_ROUND", ctx.round.to_string()),
            ("EUREKA_CONFIG", config_str),
        ];
        if let Some(db_path) = &self.db_path {
            envs.push(("EUREKA_DB_PATH", db_path.display().to_string()));
        }

        // Resolve the binary: if relative, resolve from plugin_dir.
        let argv = &self.manifest.command;
        let (binary, rest) = argv
            .split_first()
            .ok_or_else(|| NodeError::Internal("Plugin command array is empty".to_string()))?;

        let output = run_subprocess(
            binary,
            rest,
            Some(&self.plugin_dir),
            &envs,
            &envelope_str,
            node_cfg.timeout_secs,
        )
        .await
        .map_err(|e| NodeError::Internal(format!("Plugin '{}' failed: {e}", self.manifest.name)))?;

        if !output.success {
            let snippet: String = output.stderr.chars().take(500).collect();
            let code = output.exit_code.unwrap_or(-1);
            return Err(NodeError::Internal(format!(
                "Plugin '{}' exited with code {code}: {snippet}",
                self.manifest.name
            )));
        }

        // Parse stdout as newline-delimited JSON emit envelopes.
        let mut emits = Vec::new();
        for line in output.stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let envelope: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                NodeError::Internal(format!("Invalid emit envelope from plugin: {e}"))
            })?;

            let port = envelope
                .get("port")
                .and_then(|v| v.as_str())
                .ok_or_else(|| NodeError::Internal("Emit envelope missing 'port'".to_string()))?
                .to_string();

            let artifact_val = envelope.get("artifact").ok_or_else(|| {
                NodeError::Internal("Emit envelope missing 'artifact'".to_string())
            })?;

            let kind = artifact_val
                .get("kind")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    NodeError::Internal("Emit envelope missing 'artifact.kind'".to_string())
                })?
                .to_string();

            let data = artifact_val
                .get("data")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            emits.push(Emit::new(port, Artifact { kind, data }));
        }

        Ok(emits)
    }
}

#[async_trait]
impl Node for ControlPluginNode {
    fn ports(&self) -> PortSpec {
        let Some(node_cfg) = &self.manifest.node else {
            return PortSpec::default();
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

        PortSpec::new(inputs, outputs)
    }

    async fn process(&self, ctx: &NodeCtx, msg: PortMsg) -> Result<Vec<Emit>, NodeError> {
        self.invoke(ctx, &msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{NodeRoleConfig, PortDef};
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn echo_manifest() -> PluginManifest {
        PluginManifest {
            name: "echo-plugin".to_string(),
            version: "1.0.0".to_string(),
            description: "Echo test plugin.".to_string(),
            runtime: "process".to_string(),
            command: vec!["python3".to_string(), "-c".to_string(),
                // Read the call envelope, emit it back on port "out"
                r#"import sys, json; d=json.load(sys.stdin); print(json.dumps({"port":"out","artifact":{"kind":"Test","data":d["artifact"]["data"]}}))"#.to_string()],
            roles: vec!["node".to_string()],
            node: Some(NodeRoleConfig {
                inputs: vec![PortDef { port: "in".to_string(), kind: "Test".to_string() }],
                outputs: vec![PortDef { port: "out".to_string(), kind: "Test".to_string() }],
                timeout_secs: 10,
            }),
            tool: None,
        }
    }

    #[tokio::test]
    async fn test_echo_plugin_node() {
        // Check python3 is available
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return; // skip if python3 not installed
        }

        let node = ControlPluginNode::new(
            echo_manifest(),
            std::env::temp_dir(),
            "test-session".to_string(),
            None,
            serde_json::Value::Null,
        );

        let cancel = CancellationToken::new();
        let ctx = NodeCtx::new("echo-plugin", "echo-plugin", 0, cancel);
        let msg = PortMsg {
            port: "in".to_string(),
            artifact: Artifact {
                kind: "Test".to_string(),
                data: serde_json::json!({"value": 42}),
            },
        };

        let emits = node.process(&ctx, msg).await.unwrap();
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].port, "out");
        assert_eq!(emits[0].artifact.data["value"], 42);
    }
}
