//! `eureka session` and `eureka start` — interact with the daemon's runs.
//!
//! These commands connect to the running daemon's HTTP API to start new runs,
//! inspect their status and outputs, pause/resume/cancel, inject inputs, and
//! wait for completion.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::commands::daemon;

/// Daemon HTTP client base URL.
fn daemon_base_url(data_dir: &Path) -> Result<String> {
    let info = daemon::read_daemon_info(data_dir)?;
    Ok(format!("http://127.0.0.1:{}", info.port))
}

/// Build a reqwest client.
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Failed to create HTTP client")
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
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;

    let mut goal_obj = serde_json::json!({
        "goal": goal,
        "description": description.unwrap_or_default(),
        "domain": domain,
    });
    if let Some(rounds) = max_rounds {
        goal_obj["max_rounds"] = serde_json::json!(rounds);
    }

    let body = serde_json::json!({ "goal": goal_obj });

    let resp = client
        .post(format!("{base}/runs"))
        .json(&body)
        .send()
        .await
        .context("Failed to connect to daemon. Is it running? (`eureka daemon status`)")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon returned {status}: {text}");
    }

    let result: Value = resp.json().await?;
    let id = result["id"]
        .as_str()
        .map_or_else(|| "unknown", |s| s)
        .to_string();

    println!("Started run: {id}");
    Ok(())
}

/// List all sessions managed by the daemon.
///
/// # Errors
/// Returns an error if the daemon is not reachable.
pub async fn execute_list(data_dir: &Path) -> Result<()> {
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;

    let resp = client
        .get(format!("{base}/runs"))
        .send()
        .await
        .context("Failed to connect to daemon. Is it running?")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon returned {status}: {text}");
    }

    let runs: Vec<Value> = resp.json().await?;

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
        let elapsed_str = format_elapsed(elapsed);

        println!("{id:<38} {status:<12} {rounds:<8} {elapsed_str:<10} {goal}");
    }

    Ok(())
}

/// Show detailed status of a session.
///
/// # Errors
/// Returns an error if the daemon is not reachable or the session is not found.
pub async fn execute_status(data_dir: &Path, id: &str) -> Result<()> {
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;

    let uuid = parse_uuid(id)?;
    let resp = client
        .get(format!("{base}/runs/{uuid}"))
        .send()
        .await
        .context("Failed to connect to daemon")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("Session '{id}' not found");
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon returned {status}: {text}");
    }

    let run: Value = resp.json().await?;

    let run_status = run["status"].as_str().unwrap_or("unknown");
    let goal = run["goal"]["goal"]
        .as_str()
        .or_else(|| run["goal"].as_str())
        .unwrap_or("?");
    let graph = run["graph"].as_str().unwrap_or("?");
    let created = run["created_at"].as_str().unwrap_or("?");
    let error = run["error"].as_str();

    let session_stats = &run["stats"];

    println!("Session:      {id}");
    println!("Status:       {run_status}");
    println!("Goal:         {goal}");
    println!("Graph:        {graph}");
    println!("Created:      {created}");

    if let Some(err) = error {
        println!("Error:        {err}");
    }

    if !session_stats.is_null() {
        let rounds = session_stats["rounds_completed"].as_u64().unwrap_or(0);
        let elapsed = session_stats["elapsed_secs"].as_f64().unwrap_or(0.0);
        let input_tokens = session_stats["total_input_tokens"].as_u64().unwrap_or(0);
        let output_tokens = session_stats["total_output_tokens"].as_u64().unwrap_or(0);
        let cost = session_stats["total_cost_usd"].as_f64().unwrap_or(0.0);

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
/// Returns an error if the daemon is not reachable or the session is not found.
pub async fn execute_output(
    data_dir: &Path,
    id: &str,
    node_filter: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;

    let uuid = parse_uuid(id)?;
    let resp = client
        .get(format!("{base}/runs/{uuid}/checkpoint"))
        .send()
        .await
        .context("Failed to connect to daemon")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("No checkpoint found for session '{id}' (session may still be running or no outputs produced yet)");
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon returned {status}: {text}");
    }

    let checkpoint: Value = resp.json().await?;
    let outputs = checkpoint["outputs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();

    if outputs.is_empty() {
        println!("No outputs in checkpoint.");
        return Ok(());
    }

    // Filter by node if requested
    let filtered: Vec<&Value> = node_filter.map_or_else(
        || outputs.iter().collect(),
        |node| {
            outputs
                .iter()
                .filter(|o| o["node_id"].as_str() == Some(node))
                .collect()
        },
    );

    if filtered.is_empty() {
        if let Some(node) = node_filter {
            println!("No outputs from node '{node}'.");
        } else {
            println!("No outputs found.");
        }
        return Ok(());
    }

    // Group by node for clean display
    let mut by_node: std::collections::BTreeMap<String, Vec<&Value>> =
        std::collections::BTreeMap::new();
    for output in &filtered {
        let node = output["node_id"].as_str().unwrap_or("unknown").to_string();
        by_node.entry(node).or_default().push(output);
    }

    for (node_id, node_outputs) in &by_node {
        if !json_output {
            println!("\n── {node_id} ──");
        }
        for output in node_outputs {
            let port = output["port"].as_str().unwrap_or("?");
            let round = output["round"].as_u64().unwrap_or(0);
            let artifact = &output["artifact"];

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(output).unwrap_or_default()
                );
            } else {
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
                        // Print compact summary for objects/arrays
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
        }
    }

    Ok(())
}

/// Wait for a session to complete (poll until non-Running status).
///
/// # Errors
/// Returns an error if the daemon is not reachable or the session is not found.
pub async fn execute_wait(data_dir: &Path, id: &str, timeout_secs: Option<u64>) -> Result<()> {
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;
    let uuid = parse_uuid(id)?;

    let timeout = timeout_secs.unwrap_or(3600);
    let start = std::time::Instant::now();

    println!("Waiting for session {id} to complete...");

    loop {
        if start.elapsed().as_secs() > timeout {
            anyhow::bail!("Timed out after {timeout}s waiting for session {id}");
        }

        let resp = client
            .get(format!("{base}/runs/{uuid}"))
            .send()
            .await
            .context("Failed to connect to daemon")?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("Session '{id}' not found");
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Daemon returned {status}: {text}");
        }

        let run: Value = resp.json().await?;
        let run_status = run["status"].as_str().unwrap_or("unknown");

        match run_status {
            "Running" | "Paused" => {
                let elapsed = run["stats"]["elapsed_secs"].as_f64().unwrap_or(0.0);
                let rounds = run["stats"]["rounds_completed"].as_u64().unwrap_or(0);
                print!(
                    "\r  Status: {run_status:<12} Rounds: {rounds:<4} Elapsed: {}",
                    format_elapsed(elapsed)
                );
                std::io::stdout().flush()?;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            "Completed" | "Failed" | "Cancelled" => {
                println!("\nSession {id} finished with status: {run_status}");
                let session_stats = &run["stats"];
                if !session_stats.is_null() {
                    let rounds = session_stats["rounds_completed"].as_u64().unwrap_or(0);
                    let elapsed = session_stats["elapsed_secs"].as_f64().unwrap_or(0.0);
                    let cost = session_stats["total_cost_usd"].as_f64().unwrap_or(0.0);
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
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

/// Pause a running session.
///
/// # Errors
/// Returns an error if the daemon is not reachable or the session cannot be paused.
pub async fn execute_pause(data_dir: &Path, id: &str) -> Result<()> {
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;
    let uuid = parse_uuid(id)?;

    let resp = client
        .post(format!("{base}/runs/{uuid}/pause"))
        .send()
        .await
        .context("Failed to connect to daemon")?;

    if resp.status().is_success() {
        println!("Session {id} paused.");
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to pause session: {status} {text}");
    }
    Ok(())
}

/// Resume a paused session.
///
/// # Errors
/// Returns an error if the daemon is not reachable or the session cannot be resumed.
pub async fn execute_resume(data_dir: &Path, id: &str) -> Result<()> {
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;
    let uuid = parse_uuid(id)?;

    let resp = client
        .post(format!("{base}/runs/{uuid}/resume"))
        .send()
        .await
        .context("Failed to connect to daemon")?;

    if resp.status().is_success() {
        println!("Session {id} resumed.");
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to resume session: {status} {text}");
    }
    Ok(())
}

/// Cancel a running or paused session.
///
/// # Errors
/// Returns an error if the daemon is not reachable or the session cannot be cancelled.
pub async fn execute_cancel(data_dir: &Path, id: &str) -> Result<()> {
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;
    let uuid = parse_uuid(id)?;

    let resp = client
        .post(format!("{base}/runs/{uuid}/cancel"))
        .send()
        .await
        .context("Failed to connect to daemon")?;

    if resp.status().is_success() {
        println!("Session {id} cancelled.");
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to cancel session: {status} {text}");
    }
    Ok(())
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
    let base = daemon_base_url(data_dir)?;
    let client = http_client()?;
    let uuid = parse_uuid(id)?;

    // Parse the data as JSON
    let data_value: Value = serde_json::from_str(data).context("Failed to parse data as JSON")?;

    let body = serde_json::json!({
        "node_id": node,
        "port": port,
        "kind": "Goal",
        "data": data_value,
    });

    let resp = client
        .post(format!("{base}/runs/{uuid}/input"))
        .json(&body)
        .send()
        .await
        .context("Failed to connect to daemon")?;

    if resp.status().is_success() {
        println!("Artifact injected into {node}.{port}.");
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to inject input: {status} {text}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a UUID string, accepting short prefixes (minimum 8 chars).
fn parse_uuid(id: &str) -> Result<uuid::Uuid> {
    // Try full UUID first
    if let Ok(u) = uuid::Uuid::parse_str(id) {
        return Ok(u);
    }

    // Try as a short prefix match — we can't really query by prefix without
    // listing runs, so just support full UUIDs for now.
    anyhow::bail!(
        "'{id}' is not a valid UUID. Use the full session ID shown by `eureka session list`."
    );
}

/// Format elapsed seconds into a human-readable string.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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
