//! `CommandTool` — shell-backed agent tool.
//!
//! Each `ToolDef` declared in an agent's `.json` file becomes a `CommandTool`.
//! When the LLM calls the tool, the framework spawns the command as a subprocess,
//! writes the JSON args to stdin, and returns stdout to the agent.

use std::sync::Arc;

use eureka_graph::process::run_subprocess;
use eureka_graph::scheduler::SchedulerEvent;
use rig_core::completion::ToolDefinition;
use rig_core::tool::{ToolDyn, ToolError};
use rig_core::wasm_compat::WasmBoxedFuture;
use tokio::sync::mpsc;

use crate::def::ToolDef;

/// A Rig tool backed by a shell command.
///
/// Created from a [`ToolDef`] at agent startup. One instance per tool per agent.
pub struct CommandTool {
    pub(crate) def: Arc<ToolDef>,
    node_id: String,
    node_kind: String,
    round: u32,
    event_tx: Option<mpsc::Sender<SchedulerEvent>>,
}

impl CommandTool {
    /// Create a new command tool from a definition.
    pub fn new(
        def: Arc<ToolDef>,
        node_id: String,
        node_kind: String,
        round: u32,
        event_tx: Option<mpsc::Sender<SchedulerEvent>>,
    ) -> Self {
        Self {
            def,
            node_id,
            node_kind,
            round,
            event_tx,
        }
    }

    async fn execute(&self, args_json: String) -> Result<String, ToolError> {
        let args_val: serde_json::Value = serde_json::from_str(&args_json)
            .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));

        // Notify the live feed that this tool is being called.
        if let Some(tx) = &self.event_tx {
            let summary = args_summary(&args_val);
            let _ = tx.try_send(SchedulerEvent::ToolCalled {
                node_id: self.node_id.clone(),
                node_kind: self.node_kind.clone(),
                round: self.round,
                tool: self.def.name.clone(),
                args_summary: summary,
            });
        }

        let argv = interpolate(&self.def.command, &args_val);

        let (binary, rest) = match argv.split_first() {
            Some(pair) => pair,
            None => return Err(ToolError::ToolCallError("command array is empty".into())),
        };

        let result = run_subprocess(binary, rest, None, &[], &args_json, self.def.timeout_secs)
            .await
            .map_err(|e| ToolError::ToolCallError(e.to_string().into()))?;

        if result.success {
            Ok(result.stdout)
        } else {
            let snippet: String = result.stderr.chars().take(500).collect();
            let code = result.exit_code.unwrap_or(-1);
            Ok(format!("Error (exit {code}): {snippet}"))
        }
    }
}

impl ToolDyn for CommandTool {
    fn name(&self) -> String {
        self.def.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        let def = ToolDefinition {
            name: self.def.name.clone(),
            description: self.def.description.clone(),
            parameters: self.def.args_schema.clone(),
        };
        Box::pin(std::future::ready(def))
    }

    fn call<'a>(&'a self, args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(self.execute(args))
    }
}

/// Build a concise human-readable summary of a tool call's arguments.
///
/// Returns the first string value found in the args object, truncated to 80
/// characters. This is intentionally generic — the engine has no knowledge of
/// specific tool names; the first meaningful string argument (query, id, etc.)
/// is always the most relevant thing to show in the live feed.
fn args_summary(args: &serde_json::Value) -> String {
    args.as_object()
        .and_then(|obj| obj.values().find_map(|v| v.as_str()))
        .map(|s| s.chars().take(80).collect())
        .unwrap_or_default()
}

/// Substitute `{{key}}` tokens in each argv element using values from `args`.
///
/// Non-string values are serialized as compact JSON. Missing keys are replaced
/// with an empty string. Template substitution is purely token-level — no shell
/// is involved, so argument values cannot escape into shell syntax.
fn interpolate(command: &[String], args: &serde_json::Value) -> Vec<String> {
    command
        .iter()
        .map(|token| {
            let mut result = token.clone();
            // Substitute every known key.
            if let Some(obj) = args.as_object() {
                for (key, val) in obj {
                    let placeholder = format!("{{{{{key}}}}}");
                    let replacement = match val {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    result = result.replace(&placeholder, &replacement);
                }
            }
            // Clear any remaining {{...}} placeholders (missing keys → empty string).
            while let Some(start) = result.find("{{") {
                match result[start..].find("}}") {
                    Some(rel_end) => result.replace_range(start..start + rel_end + 2, ""),
                    None => break,
                }
            }
            result
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpolate_string_arg() {
        let cmd = vec![
            "search".to_string(),
            "--query".to_string(),
            "{{query}}".to_string(),
        ];
        let args = serde_json::json!({ "query": "CO2 catalysts" });
        let result = interpolate(&cmd, &args);
        assert_eq!(result, vec!["search", "--query", "CO2 catalysts"]);
    }

    #[test]
    fn test_interpolate_integer_arg() {
        let cmd = vec![
            "search".to_string(),
            "--count".to_string(),
            "{{count}}".to_string(),
        ];
        let args = serde_json::json!({ "count": 5 });
        let result = interpolate(&cmd, &args);
        assert_eq!(result, vec!["search", "--count", "5"]);
    }

    #[test]
    fn test_interpolate_inline_template() {
        let cmd = vec![
            "fetch-url".to_string(),
            "https://api.example.com/{{path}}".to_string(),
        ];
        let args = serde_json::json!({ "path": "results" });
        let result = interpolate(&cmd, &args);
        assert_eq!(result, vec!["fetch-url", "https://api.example.com/results"]);
    }

    #[test]
    fn test_interpolate_missing_key_becomes_empty() {
        let cmd = vec!["cmd".to_string(), "{{missing}}".to_string()];
        let args = serde_json::json!({});
        let result = interpolate(&cmd, &args);
        assert_eq!(result, vec!["cmd", ""]);
    }

    #[test]
    fn test_interpolate_no_templates_unchanged() {
        let cmd = vec!["ls".to_string(), "-la".to_string()];
        let args = serde_json::json!({ "query": "ignored" });
        let result = interpolate(&cmd, &args);
        assert_eq!(result, vec!["ls", "-la"]);
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let def = Arc::new(ToolDef {
            name: "echo".to_string(),
            description: "echo test".to_string(),
            command: vec!["echo".to_string(), "hello".to_string()],
            args_schema: serde_json::json!({ "type": "object" }),
            timeout_secs: 5,
        });
        let tool = CommandTool::new(def, "test".into(), "test".into(), 0, None);
        let result = tool.execute("{}".to_string()).await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn test_execute_nonzero_exit() {
        let def = Arc::new(ToolDef {
            name: "fail".to_string(),
            description: "fail test".to_string(),
            command: vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()],
            args_schema: serde_json::json!({ "type": "object" }),
            timeout_secs: 5,
        });
        let tool = CommandTool::new(def, "test".into(), "test".into(), 0, None);
        let result = tool.execute("{}".to_string()).await.unwrap();
        assert!(result.starts_with("Error (exit 1)"));
    }

    #[tokio::test]
    async fn test_execute_stdin_json() {
        // Command reads from stdin and echoes it back
        let def = Arc::new(ToolDef {
            name: "cat".to_string(),
            description: "cat test".to_string(),
            command: vec!["cat".to_string()],
            args_schema: serde_json::json!({ "type": "object" }),
            timeout_secs: 5,
        });
        let tool = CommandTool::new(def, "test".into(), "test".into(), 0, None);
        let result = tool
            .execute(r#"{"key":"value"}"#.to_string())
            .await
            .unwrap();
        assert_eq!(result, r#"{"key":"value"}"#);
    }
}
