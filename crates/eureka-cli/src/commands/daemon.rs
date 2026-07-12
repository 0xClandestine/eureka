//! `eureka daemon` — background process that manages research sessions.
//!
//! The daemon runs persistently (in foreground; use `&` or a service manager),
//! hosting an HTTP API that the `eureka start` and `eureka session` commands
//! connect to. State is stored in a configurable data directory (default:
//! `~/.eureka/`).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use eureka::config::EurekaConfig;
use eureka::run::{CheckpointStore, FileRunStore, RunCheckpoint, RunStore, SqliteRunPersistence};
use eureka::RunManager;
use serde::{Deserialize, Serialize};
use tokio::signal;
use tower_http::cors::CorsLayer;

/// Metadata about a running daemon, written to `daemon.json`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DaemonInfo {
    pub(crate) pid: u32,
    pub(crate) port: u16,
    pub(crate) data_dir: String,
    pub(crate) started_at: String,
}

/// Shared daemon state injected into axum route handlers.
#[derive(Clone)]
struct DaemonState {
    manager: RunManager,
    data_dir: PathBuf,
}

/// Start the daemon: load config, set up persistence, start HTTP server.
///
/// Runs in the foreground until Ctrl+C/SIGTERM.
///
/// # Errors
/// Returns an error if the config cannot be loaded or the server fails to bind.
pub async fn execute_start(port: u16, config_path: Option<String>, data_dir: &Path) -> Result<()> {
    // Create the data directory
    std::fs::create_dir_all(data_dir).with_context(|| {
        format!(
            "Failed to create daemon data directory '{}'",
            data_dir.display()
        )
    })?;

    // Load configuration
    let config = {
        let config_path = config_path.as_deref().unwrap_or("eureka.toml");
        if Path::new(config_path).exists() {
            EurekaConfig::load(Some(Path::new(config_path)))
                .with_context(|| format!("Failed to load config from '{config_path}'"))?
        } else {
            tracing::warn!("Config file '{}' not found; using defaults", config_path);
            EurekaConfig::load(None)?
        }
    };

    // Set up session persistence
    let sessions_dir = data_dir.join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;

    let (run_store, checkpoint_store): (Arc<dyn RunStore>, Arc<dyn CheckpointStore>) = {
        // Try SQLite; fall back to file-based store
        let db_path = sessions_dir.join("eureka.db");
        match SqliteRunPersistence::open(&db_path).await {
            Ok(sqlite) => {
                let sqlite = Arc::new(sqlite);
                (sqlite.clone(), sqlite)
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "SQLite persistence unavailable; using file-based store"
                );
                let file = Arc::new(FileRunStore::new(&sessions_dir));
                (file.clone(), file)
            }
        }
    };

    let manager = RunManager::new(
        config,
        Arc::clone(&run_store),
        Arc::clone(&checkpoint_store),
        Some(sessions_dir),
    );

    // Write daemon info file
    let daemon_info = DaemonInfo {
        pid: std::process::id(),
        port,
        data_dir: data_dir.to_string_lossy().to_string(),
        started_at: chrono_now(),
    };
    write_daemon_info(data_dir, &daemon_info)?;

    tracing::info!(
        pid = daemon_info.pid,
        port = daemon_info.port,
        data_dir = %data_dir.display(),
        "Eureka daemon started"
    );

    // Build the daemon state and HTTP router
    let state = DaemonState {
        manager,
        data_dir: data_dir.to_path_buf(),
    };

    let app = build_daemon_router(state);

    // Bind and serve
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind daemon to port {port}"))?;

    tracing::info!("Daemon HTTP server → http://127.0.0.1:{port}");

    // Run until Ctrl+C or SIGTERM
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Clean up daemon info on graceful shutdown
    remove_daemon_info(data_dir);
    tracing::info!("Daemon stopped");
    Ok(())
}

/// Build the axum router for the daemon's HTTP API.
fn build_daemon_router(state: DaemonState) -> Router {
    Router::new()
        // Run lifecycle
        .route("/runs", post(create_run_handler).get(list_runs_handler))
        .route("/runs/{id}", get(get_run_handler))
        .route("/runs/{id}/checkpoint", get(checkpoint_run_handler))
        .route("/runs/{id}/pause", post(pause_run_handler))
        .route("/runs/{id}/resume", post(resume_run_handler))
        .route("/runs/{id}/cancel", post(cancel_run_handler))
        .route("/runs/{id}/input", post(input_run_handler))
        // Daemon metadata
        .route("/daemon/status", get(daemon_status_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Stop the daemon by sending SIGTERM to its process.
///
/// # Errors
/// Returns an error if no daemon is running or the PID cannot be read.
pub fn execute_stop(data_dir: &Path) -> Result<()> {
    let info = read_daemon_info(data_dir)?;
    let pid = nix::unistd::Pid::from_raw(info.pid as i32);

    // Check if the process is actually running
    let running = nix::sys::signal::kill(pid, None).is_ok();
    if !running {
        remove_daemon_info(data_dir);
        anyhow::bail!(
            "Daemon (PID {}) is not running; cleaned up stale info",
            info.pid
        );
    }

    println!("Stopping daemon (PID {})...", info.pid);

    // Send SIGTERM
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM)
        .with_context(|| format!("Failed to send SIGTERM to daemon (PID {})", info.pid))?;

    // Wait for the process to exit (poll every 200ms, max 5s)
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if nix::sys::signal::kill(pid, None).is_err() {
            // Process exited
            remove_daemon_info(data_dir);
            println!("Daemon stopped.");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    anyhow::bail!(
        "Daemon (PID {}) did not exit within 5 seconds; try `kill {}`",
        info.pid,
        info.pid
    );
}

/// Check if the daemon is running and print its status.
///
/// # Errors
/// Returns an error if the daemon info file cannot be read.
pub fn execute_status(data_dir: &Path) -> Result<()> {
    let info = read_daemon_info(data_dir)?;
    let pid = nix::unistd::Pid::from_raw(info.pid as i32);

    if nix::sys::signal::kill(pid, None).is_ok() {
        println!("✓ Daemon is running");
        println!("  PID:       {}", info.pid);
        println!("  Port:      {}", info.port);
        println!("  Data dir:  {}", info.data_dir);
        println!("  Started:   {}", info.started_at);
        Ok(())
    } else {
        remove_daemon_info(data_dir);
        anyhow::bail!(
            "Daemon (PID {}) is not running; stale info cleaned up",
            info.pid
        );
    }
}

// ---------------------------------------------------------------------------
// HTTP Handlers
// ---------------------------------------------------------------------------

/// Extract the RunManager from state or return 404.
fn manager_from_state(state: &DaemonState) -> Result<RunManager, StatusCode> {
    Ok(state.manager.clone())
}

/// `POST /runs` — create and start a new research run.
async fn create_run_handler(
    State(state): State<DaemonState>,
    Json(body): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let manager = manager_from_state(&state)?;
    let goal = body.get("goal").cloned().unwrap_or(body);
    let request = eureka::CreateRunRequest { goal };
    let id = manager
        .create_run(request)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({ "id": id }))))
}

/// `GET /runs` — list all runs.
async fn list_runs_handler(
    State(state): State<DaemonState>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    let manager = manager_from_state(&state)?;
    let records = manager
        .list_runs()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let values: Vec<serde_json::Value> = records
        .into_iter()
        .map(|r| serde_json::to_value(r).unwrap_or_default())
        .collect();
    Ok(Json(values))
}

/// `GET /runs/{id}` — get a single run record.
async fn get_run_handler(
    State(state): State<DaemonState>,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let manager = manager_from_state(&state)?;
    let record = manager
        .get_run(id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::to_value(record).unwrap_or_default()))
}

/// `GET /runs/{id}/checkpoint` — get the latest checkpoint (outputs, round, etc.).
async fn checkpoint_run_handler(
    State(state): State<DaemonState>,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Json<RunCheckpoint>, StatusCode> {
    let manager = manager_from_state(&state)?;
    let checkpoint = manager
        .get_checkpoint(id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(checkpoint))
}

/// `POST /runs/{id}/pause` — pause a running session.
async fn pause_run_handler(
    State(state): State<DaemonState>,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<StatusCode, StatusCode> {
    let manager = manager_from_state(&state)?;
    manager
        .pause_run(id)
        .await
        .map(|()| StatusCode::ACCEPTED)
        .map_err(|_| StatusCode::CONFLICT)
}

/// `POST /runs/{id}/resume` — resume a paused session.
async fn resume_run_handler(
    State(state): State<DaemonState>,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<StatusCode, StatusCode> {
    let manager = manager_from_state(&state)?;
    manager
        .resume_run(id)
        .await
        .map(|()| StatusCode::ACCEPTED)
        .map_err(|_| StatusCode::CONFLICT)
}

/// `POST /runs/{id}/cancel` — cancel a running or paused session.
async fn cancel_run_handler(
    State(state): State<DaemonState>,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<StatusCode, StatusCode> {
    let manager = manager_from_state(&state)?;
    manager
        .cancel_run(id)
        .await
        .map(|()| StatusCode::ACCEPTED)
        .map_err(|_| StatusCode::CONFLICT)
}

/// `POST /runs/{id}/input` — inject an artifact into a paused session.
async fn input_run_handler(
    State(state): State<DaemonState>,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<RunCheckpoint>, StatusCode> {
    let manager = manager_from_state(&state)?;

    let node_id = body
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let port = body
        .get("port")
        .and_then(|v| v.as_str())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let artifact_kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("Goal");
    let artifact_data = body.get("data").cloned().unwrap_or(serde_json::Value::Null);

    let artifact = eureka::graph::artifact::Artifact {
        kind: artifact_kind.to_string(),
        data: artifact_data,
    };

    manager
        .submit_input(id, node_id, port, artifact)
        .await
        .map(Json)
        .map_err(|_| StatusCode::CONFLICT)
}

/// `GET /daemon/status` — daemon health check.
async fn daemon_status_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "running",
        "pid": std::process::id(),
        "data_dir": state.data_dir,
    }))
}

// ---------------------------------------------------------------------------
// Daemon info file management
// ---------------------------------------------------------------------------

fn daemon_info_path(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.json")
}

fn write_daemon_info(data_dir: &Path, info: &DaemonInfo) -> Result<()> {
    let content = serde_json::to_string_pretty(info)?;
    std::fs::write(daemon_info_path(data_dir), content)?;
    Ok(())
}

pub(crate) fn read_daemon_info(data_dir: &Path) -> Result<DaemonInfo> {
    let path = daemon_info_path(data_dir);
    let content = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "No daemon info at '{}'. Is the daemon running?",
            path.display()
        )
    })?;
    let info: DaemonInfo = serde_json::from_str(&content).context("Failed to parse daemon info")?;
    Ok(info)
}

fn remove_daemon_info(data_dir: &Path) {
    let path = daemon_info_path(data_dir);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
}

/// Wait for Ctrl+C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = signal::ctrl_c();

    // Set up SIGTERM handler; fall back to Ctrl+C only on failure
    let term_fut: Pin<Box<dyn Future<Output = ()> + Send>> = match signal::unix::signal(
        signal::unix::SignalKind::terminate(),
    ) {
        Ok(mut sig) => Box::pin(async move {
            sig.recv().await;
        }),
        Err(error) => {
            tracing::warn!(error = %error, "Failed to set up SIGTERM handler; using Ctrl+C only");
            Box::pin(std::future::pending())
        }
    };

    tokio::select! {
        _ = ctrl_c => {}
        _ = term_fut => {}
    }
}

/// Get a stable timestamp string for daemon info.
fn chrono_now() -> String {
    // Use a simple UTC timestamp without a chrono dependency
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Format as ISO-like: YYYY-MM-DDTHH:MM:SSZ
    // This avoids a chrono dependency
    let days_since_epoch = secs / 86400;
    let time_in_day = secs % 86400;
    let hours = time_in_day / 3600;
    let minutes = (time_in_day % 3600) / 60;
    let seconds = time_in_day % 60;

    // Compute year/month/day from days since epoch (rough algorithm)
    let (year, month, day) = days_to_date(days_since_epoch as i64);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: i64) -> (i64, u32, u32) {
    // Algorithm from Howard Hinnant
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_daemon_info_read_write() {
        let dir = TempDir::new().unwrap();
        let info = DaemonInfo {
            pid: 12345,
            port: 7773,
            data_dir: "/tmp/.eureka".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
        };
        write_daemon_info(dir.path(), &info).unwrap();
        let read = read_daemon_info(dir.path()).unwrap();
        assert_eq!(read.pid, 12345);
        assert_eq!(read.port, 7773);
        assert_eq!(read.data_dir, "/tmp/.eureka");
    }

    #[test]
    fn test_daemon_info_not_found() {
        let dir = TempDir::new().unwrap();
        let result = read_daemon_info(dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Is the daemon running"));
    }

    #[test]
    fn test_daemon_info_remove() {
        let dir = TempDir::new().unwrap();
        let info = DaemonInfo {
            pid: 12345,
            port: 7773,
            data_dir: "/tmp/.eureka".to_string(),
            started_at: "now".to_string(),
        };
        write_daemon_info(dir.path(), &info).unwrap();
        assert!(daemon_info_path(dir.path()).exists());
        remove_daemon_info(dir.path());
        assert!(!daemon_info_path(dir.path()).exists());
    }

    #[test]
    fn test_days_to_date() {
        // Unix epoch: 1970-01-01
        let (y, m, d) = days_to_date(0);
        assert_eq!(y, 1970);
        assert_eq!(m, 1);
        assert_eq!(d, 1);

        // Known date: 2026-07-11 = days since epoch
        let july_11_2026 = days_since_ymd(2026, 7, 11);
        let (y, m, d) = days_to_date(july_11_2026);
        assert_eq!(y, 2026);
        assert_eq!(m, 7);
        assert_eq!(d, 11);

        // Far future
        let (y, m, d) = days_to_date(days_since_ymd(2099, 12, 31));
        assert_eq!(y, 2099);
        assert_eq!(m, 12);
        assert_eq!(d, 31);
    }

    /// Days since Unix epoch for a given date.
    fn days_since_ymd(year: i64, month: u32, day: u32) -> i64 {
        let (y, m) = if month <= 2 {
            (year - 1, month + 9)
        } else {
            (year, month - 3)
        };
        let era = y / 400;
        let yoe = y - era * 400;
        let doy = (153 * i64::from(m) + 2) / 5 + i64::from(day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }
}
