//! `eureka session` and `eureka start` — interact with the daemon's runs.
//!
//! These commands connect to the running daemon's HTTP API to start new runs,
//! inspect their status and outputs, pause/resume/cancel, inject inputs, and
//! wait for completion.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::commands::daemon;

/// HTTP client for the daemon API.
#[derive(Clone)]
struct DaemonClient {
    /// Daemon base URL (http://127.0.0.1:{port}).
    base_url: String,
    /// Shared HTTP client.
    client: reqwest::Client,
}

impl DaemonClient {
    /// Connect to the daemon by reading its info file.
    ///
    /// # Errors
    /// Returns an error if the daemon info file cannot be read or the client cannot be built.
    fn connect(data_dir: &Path) -> Result<Self> {
        let info = daemon::read_daemon_info(data_dir)?;
        Ok(Self {
            base_url: format!("http://127.0.0.1:{}", info.port),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .context("Failed to create HTTP client")?,
        })
    }

    /// POST JSON to the daemon, returning the JSON response.
    ///
    /// # Errors
    /// Returns an error on connection failure, non-2xx status, or invalid JSON.
    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .await
            .context("Failed to connect to daemon. Is it running? (`eureka daemon status`)")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Daemon returned {status}: {text}");
        }
        resp.json().await.context("Invalid JSON from daemon")
    }

    /// GET from the daemon, returning the JSON response.
    ///
    /// `not_found_msg` is shown when the daemon returns 404.
    ///
    /// # Errors
    /// Returns an error on connection failure, non-2xx status, or invalid JSON.
    async fn get(&self, path: &str, not_found_msg: &str) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await
            .context("Failed to connect to daemon")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("{not_found_msg}");
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Daemon returned {status}: {text}");
        }
        resp.json().await.context("Invalid JSON from daemon")
    }

    /// POST to the daemon, printing a success message on 2xx.
    ///
    /// # Errors
    /// Returns an error on connection failure or non-2xx status.
    async fn post_simple(&self, path: &str, ok_msg: &str) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .send()
            .await
            .context("Failed to connect to daemon")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed: {status} {text}");
        }
        println!("{ok_msg}");
        Ok(())
    }
}

fn parse_uuid(id: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(id).with_context(|| {
        format!("'{id}' is not a valid UUID. Use the full session ID from `eureka session list`.")
    })
}

/// Start a new research run via the daemon.
///
/// # Errors
/// Returns an error if the daemon is not running or the request fails.
pub async fn execute_start(
    data_dir: &Path,
    goal: &str,
    description: Option<&str>,
    domain: &str,
    max_rounds: Option<u32>,
) -> Result<()> {
    let client = DaemonClient::connect(data_dir)?;

    let mut goal_obj = serde_json::json!({
        "goal": goal,
        "description": description.unwrap_or_default(),
        "domain": domain,
    });
    if let Some(rounds) = max_rounds {
        goal_obj["max_rounds"] = serde_json::json!(rounds);
    }

    let body = serde_json::json!({ "goal": goal_obj });
    let result = client.post("/runs", &body).await?;
    let id = result["id"].as_str().unwrap_or("unknown");
    println!("Started run: {id}");
    Ok(())
}

/// List all sessions managed by the daemon.
///
/// # Errors
/// Returns an error if the daemon is not reachable.
pub async fn execute_list(data_dir: &Path) -> Result<()> {
    let client = DaemonClient::connect(data_dir)?;
    let runs: Vec<Value> = serde_json::from_value(client.get("/runs", "No runs found").await?)?;

    if runs.is_empty() {
        println!("No runs found.");
        return Ok(());
    }

    println!(
        "{:<38} {:<12} {:<8} {:<10} Goal",
        "ID", "Status", "Rounds", "Elapsed"
    );
    println!("{}", "-".repeat(100));

    for run in &runs {
        let id = run["id"].as_str().unwrap_or("?");
        let status = run["status"].as_str().unwrap_or("?");
        let rounds = run["stats"]["rounds_completed"].as_u64().unwrap_or(0);
        let elapsed = run["stats"]["elapsed_secs"].as_f64().unwrap_or(0.0);
        let goal = run["goal"]["goal"]
            .as_str()
            .or_else(|| run["goal"].as_str())
            .unwrap_or("?");

        println!(
            "{id:<38} {status:<12} {rounds:<8} {:<10} {goal}",
            format_elapsed(elapsed)
        );
    }
    Ok(())
}

/// Show detailed status of a session.
///
/// # Errors
/// Returns an error if the daemon is not reachable or the session is not found.
pub async fn execute_status(data_dir: &Path, id: &str) -> Result<()> {
    let client = DaemonClient::connect(data_dir)?;
    let uuid = parse_uuid(id)?;
    let run = client
        .get(
            &format!("/runs/{uuid}"),
            &format!("Session '{id}' not found"),
        )
        .await?;

    let status = run["status"].as_str().unwrap_or("unknown");
    let goal = run["goal"]["goal"]
        .as_str()
        .or_else(|| run["goal"].as_str())
        .unwrap_or("?");
    let graph = run["graph"].as_str().unwrap_or("?");
    let created = run["created_at"].as_str().unwrap_or("?");
    let error = run["error"].as_str();
    let stats = &run["stats"];

    println!("Session:      {id}");
    println!("Status:       {status}");
    println!("Goal:         {goal}");
    println!("Graph:        {graph}");
    println!("Created:      {created}");
    if let Some(err) = error {
        println!("Error:        {err}");
    }
    if !stats.is_null() {
        let rounds = stats["rounds_completed"].as_u64().unwrap_or(0);
        let elapsed = stats["elapsed_secs"].as_f64().unwrap_or(0.0);
        let input_tokens = stats["total_input_tokens"].as_u64().unwrap_or(0);
        let output_tokens = stats["total_output_tokens"].as_u64().unwrap_or(0);
        let cost = stats["total_cost_usd"].as_f64().unwrap_or(0.0);
        println!();
        println!("Stats:");
        println!("  Rounds:       {rounds}");
        println!("  Elapsed:      {}", format_elapsed(elapsed));
        println!("  Input tokens: {input_tokens}");
        println!("  Output tokens:{output_tokens}");
        println!("  Cost:         ${cost:.4}");
    }
    Ok(())
}

/// Show outputs from a session's latest checkpoint.
///
/// # Errors
/// Returns an error if the daemon is not reachable or no checkpoint exists.
pub async fn execute_output(
    data_dir: &Path,
    id: &str,
    node_filter: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let client = DaemonClient::connect(data_dir)?;
    let uuid = parse_uuid(id)?;
    let checkpoint = client.get(
        &format!("/runs/{uuid}/checkpoint"),
        &format!("No checkpoint found for session '{id}' (session may still be running or no outputs produced yet)"),
    ).await?;

    let outputs = checkpoint["outputs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();

    if outputs.is_empty() {
        println!("No outputs in checkpoint.");
        return Ok(());
    }

    let by_node = group_outputs_by_node(outputs, node_filter);
    if by_node.is_empty() {
        println!(
            "No outputs found{}",
            node_filter.map_or(String::new(), |n| format!(" from node '{n}'"))
        );
        return Ok(());
    }

    for (node_id, node_outputs) in &by_node {
        if !json_output {
            println!("\n── {node_id} ──");
        }
        for output in node_outputs {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(output).unwrap_or_default()
                );
            } else {
                print_output_summary(output);
            }
        }
    }
    Ok(())
}

/// Wait for a session to complete (poll until terminal status).
///
/// # Errors
/// Returns an error if the daemon is not reachable or the wait times out.
pub async fn execute_wait(data_dir: &Path, id: &str, timeout_secs: Option<u64>) -> Result<()> {
    let client = DaemonClient::connect(data_dir)?;
    let uuid = parse_uuid(id)?;
    let timeout = timeout_secs.unwrap_or(3600);
    let start = std::time::Instant::now();

    println!("Waiting for session {id} to complete...");

    loop {
        if start.elapsed().as_secs() > timeout {
            anyhow::bail!("Timed out after {timeout}s waiting for session {id}");
        }
        let run = client
            .get(
                &format!("/runs/{uuid}"),
                &format!("Session '{id}' not found"),
            )
            .await?;
        let status = run["status"].as_str().unwrap_or("unknown");

        match status {
            "Running" | "Paused" => {
                let elapsed = run["stats"]["elapsed_secs"].as_f64().unwrap_or(0.0);
                let rounds = run["stats"]["rounds_completed"].as_u64().unwrap_or(0);
                print!(
                    "\r  Status: {status:<12} Rounds: {rounds:<4} Elapsed: {}",
                    format_elapsed(elapsed)
                );
                std::io::stdout().flush()?;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            "Completed" | "Failed" | "Cancelled" => {
                println!("\nSession {id} finished with status: {status}");
                let stats = &run["stats"];
                if !stats.is_null() {
                    let rounds = stats["rounds_completed"].as_u64().unwrap_or(0);
                    let elapsed = stats["elapsed_secs"].as_f64().unwrap_or(0.0);
                    let cost = stats["total_cost_usd"].as_f64().unwrap_or(0.0);
                    println!(
                        "  Rounds: {rounds}, Elapsed: {}, Cost: ${cost:.4}",
                        format_elapsed(elapsed)
                    );
                }
                if let Some(error) = run["error"].as_str() {
                    println!("  Error: {error}");
                }
                return Ok(());
            }
            other => {
                println!("\nSession {id} status: {other}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// Send a simple lifecycle signal (pause/resume/cancel) to a run.
pub async fn execute_signal(data_dir: &Path, id: &str, action: &str) -> Result<()> {
    let client = DaemonClient::connect(data_dir)?;
    let uuid = parse_uuid(id)?;
    client
        .post_simple(
            &format!("/runs/{uuid}/{action}"),
            &format!("Session {id} {action}d."),
        )
        .await
}

/// Pause a running session.
pub async fn execute_pause(data_dir: &Path, id: &str) -> Result<()> {
    execute_signal(data_dir, id, "pause").await
}

/// Resume a paused session.
pub async fn execute_resume(data_dir: &Path, id: &str) -> Result<()> {
    execute_signal(data_dir, id, "resume").await
}

/// Cancel a running or paused session.
pub async fn execute_cancel(data_dir: &Path, id: &str) -> Result<()> {
    execute_signal(data_dir, id, "cancel").await
}

/// Inject an artifact into a paused session.
///
/// # Errors
/// Returns an error if the daemon is not reachable or the injection fails.
pub async fn execute_inject(
    data_dir: &Path,
    id: &str,
    node: &str,
    port: &str,
    data: &str,
) -> Result<()> {
    let client = DaemonClient::connect(data_dir)?;
    let uuid = parse_uuid(id)?;
    let data_value: Value = serde_json::from_str(data).context("Failed to parse data as JSON")?;
    let body = serde_json::json!({
        "node_id": node,
        "port": port,
        "kind": "Goal",
        "data": data_value,
    });
    client.post(&format!("/runs/{uuid}/input"), &body).await?;
    println!("Artifact injected into {node}.{port}.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Group checkpoint outputs by node ID, optionally filtering to one node.
fn group_outputs_by_node<'a>(
    outputs: &'a [Value],
    node_filter: Option<&str>,
) -> std::collections::BTreeMap<String, Vec<&'a Value>> {
    let mut by_node: std::collections::BTreeMap<String, Vec<&Value>> =
        std::collections::BTreeMap::new();
    for output in outputs {
        let node_id = output["node_id"].as_str().unwrap_or("unknown");
        if node_filter.is_none_or(|f| f == node_id) {
            by_node.entry(node_id.to_string()).or_default().push(output);
        }
    }
    by_node
}

/// Print a human-readable summary of one checkpoint output.
fn print_output_summary(output: &Value) {
    let port = output["port"].as_str().unwrap_or("?");
    let round = output["round"].as_u64().unwrap_or(0);
    let artifact = &output["artifact"];
    println!("  port:  {port}");
    println!("  round: {round}");
    if let Some(kind) = artifact["kind"].as_str() {
        println!("  kind:  {kind}");
    }
    let data = &artifact["data"];
    if !data.is_null() {
        if let Some(s) = data.as_str() {
            println!("  data:  {s}");
        } else {
            let preview = serde_json::to_string(data).unwrap_or_default();
            if preview.len() > 500 {
                println!("  data: {} ... (truncated)", &preview[..200]);
            } else {
                println!("  data: {preview}");
            }
        }
    }
    println!();
}

/// Format elapsed seconds into a human-readable string.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
fn format_elapsed(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.0}s")
    } else if secs < 3600.0 {
        let m = (secs / 60.0) as u64;
        let s = (secs % 60.0) as u64;
        format!("{m}m{s:02}s")
    } else {
        let h = (secs / 3600.0) as u64;
        let m = ((secs % 3600.0) / 60.0) as u64;
        format!("{h}h{m:02}m")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn test_format_elapsed() {
        assert_eq!(format_elapsed(0.0), "0s");
        assert_eq!(format_elapsed(30.0), "30s");
        assert_eq!(format_elapsed(90.0), "1m30s");
        assert_eq!(format_elapsed(3661.0), "1h01m");
    }

    #[test]
    fn test_parse_uuid_full() {
        let u = parse_uuid("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        assert_eq!(u.to_string(), "a1b2c3d4-e5f6-7890-abcd-ef1234567890");
    }

    #[test]
    fn test_parse_uuid_invalid() {
        let result = parse_uuid("not-a-uuid");
        assert!(result.is_err());
    }
}
