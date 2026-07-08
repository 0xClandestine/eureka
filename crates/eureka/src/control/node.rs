//! `ControlNode` — a graph node backed by a subprocess.
//!
//! Defined inline in the YAML manifest's `control[]` array. When the
//! scheduler activates a `ControlNode`, it:
//! 1. Writes a JSON call envelope to the subprocess stdin.
//! 2. Reads JSON emit envelopes (one per line) from stdout.
//! 3. Returns the parsed emits to the scheduler.
//!
//! Environment variables available to the subprocess:
//! - `EUREKA_SESSION_ID` — the session UUID
//! - `EUREKA_NODE_ID` — the node ID from the manifest
//! - `EUREKA_ROUND` — current round number
//! - `EUREKA_CONFIG` — JSON-encoded node `config` object from the manifest
//! - `EUREKA_DB_PATH` — path to the session `SQLite` database (if available)

use std::path::PathBuf;

use super::process::run_subprocess;
use crate::graph::artifact::Artifact;
use crate::graph::node::{Emit, Node, NodeCtx, NodeError, PortMsg};
use crate::graph::port::{PortDef, PortSpec};
use async_trait::async_trait;

/// A graph node implemented by a subprocess plugin.
pub struct ControlNode {
    /// Display name for error messages.
    name: String,
    /// Absolute path to the directory containing the scripts.
    work_dir: PathBuf,
    /// Session ID passed to the subprocess as `EUREKA_SESSION_ID`.
    session_id: String,
    /// Path to the session database, passed as `EUREKA_DB_PATH`.
    db_path: Option<PathBuf>,
    /// The per-node config object from the manifest.
    config: serde_json::Value,
    /// Subprocess argv.
    command: Vec<String>,
    /// Port spec for this node.
    port_spec: PortSpec,
    /// Subprocess timeout in seconds.
    timeout_secs: u32,
}

impl ControlNode {
    /// Create a new control node from a [`ControlNodeDef`].
    #[must_use]
    pub fn new(
        def: ControlNodeDef,
        session_id: String,
        db_path: Option<PathBuf>,
        config: serde_json::Value,
    ) -> Self {
        let inputs = def.inputs.iter().map(PortDef::to_input_spec).collect();
        let outputs = def.outputs.iter().map(PortDef::to_output_spec).collect();

        Self {
            name: def.name,
            work_dir: def.work_dir,
            command: def.command,
            port_spec: PortSpec::new(inputs, outputs),
            timeout_secs: def.timeout_secs,
            session_id,
            db_path,
            config,
        }
    }

    /// Invoke the subprocess and collect emit envelopes from stdout.
    async fn invoke(&self, ctx: &NodeCtx, msg: &PortMsg) -> Result<Vec<Emit>, NodeError> {
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

        let mut envs: Vec<(&str, String)> = vec![
            ("EUREKA_SESSION_ID", self.session_id.clone()),
            ("EUREKA_NODE_ID", ctx.node_id.clone()),
            ("EUREKA_ROUND", ctx.round.to_string()),
            ("EUREKA_CONFIG", config_str),
        ];
        if let Some(db_path) = &self.db_path {
            envs.push(("EUREKA_DB_PATH", db_path.display().to_string()));
        }

        let (binary, rest) = self.command.split_first().ok_or_else(|| {
            NodeError::Internal("Control node command array is empty".to_string())
        })?;

        let output = run_subprocess(
            binary,
            rest,
            Some(&self.work_dir),
            &envs,
            &envelope_str,
            self.timeout_secs,
        )
        .await
        .map_err(|e| NodeError::Internal(format!("Control node '{}' failed: {e}", self.name)))?;

        if !output.success {
            let snippet: String = output.stderr.chars().take(500).collect();
            let code = output.exit_code.unwrap_or(-1);
            return Err(NodeError::Internal(format!(
                "Control node '{}' exited with code {code}: {snippet}",
                self.name
            )));
        }

        let mut emits = Vec::new();
        for line in output.stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let envelope: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                NodeError::Internal(format!("Invalid emit envelope from control node: {e}"))
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
impl Node for ControlNode {
    fn ports(&self) -> PortSpec {
        self.port_spec.clone()
    }

    async fn process(
        &self,
        ctx: &NodeCtx,
        inputs: Vec<PortMsg>,
    ) -> Result<Vec<Emit>, NodeError> {
        // Control nodes currently consume a single call envelope per
        // invocation. When multiple inputs arrive together, forward the first
        // (primary) input; the subprocess protocol for multi-input control
        // nodes can be extended later.
        let msg = inputs
            .into_iter()
            .next()
            .ok_or_else(|| NodeError::Internal("control node activated with no inputs".into()))?;
        self.invoke(ctx, &msg).await
    }
}

/// Lightweight definition for constructing a [`ControlNode`].
///
/// Populated from the YAML manifest's `control[]` entries.
pub struct ControlNodeDef {
    /// Display name (for diagnostics).
    pub name: String,
    /// Working directory (absolute path to graph dir).
    pub work_dir: PathBuf,
    /// Subprocess argv.
    pub command: Vec<String>,
    /// Input port declarations.
    pub inputs: Vec<PortDef>,
    /// Output port declarations.
    pub outputs: Vec<PortDef>,
    /// Subprocess timeout in seconds.
    pub timeout_secs: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn echo_def() -> ControlNodeDef {
        ControlNodeDef {
            name: "echo-node".to_string(),
            work_dir: std::env::temp_dir(),
            command: vec![
                "python3".to_string(),
                "-c".to_string(),
                r#"import sys, json; d=json.load(sys.stdin); print(json.dumps({"port":"out","artifact":{"kind":"Test","data":d["artifact"]["data"]}}))"#.to_string(),
            ],
            inputs: vec![PortDef {
                port: "in".into(),
                kind: "Test".into(),
            }],
            outputs: vec![PortDef {
                port: "out".into(),
                kind: "Test".into(),
            }],
            timeout_secs: 10,
        }
    }

    #[tokio::test]
    async fn test_echo_control_node() {
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let node = ControlNode::new(
            echo_def(),
            "test-session".to_string(),
            None,
            serde_json::Value::Null,
        );

        let cancel = CancellationToken::new();
        let ctx = NodeCtx::new("echo-node", "echo-node", 0, cancel);
        let msg = PortMsg {
            port: "in".to_string(),
            artifact: Artifact {
                kind: "Test".to_string(),
                data: serde_json::json!({"value": 42}),
            },
        };

        let emits = node.process(&ctx, vec![msg]).await.unwrap();
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].port, "out");
        assert_eq!(emits[0].artifact.data["value"], 42);
    }
}
