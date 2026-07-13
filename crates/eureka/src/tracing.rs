//! Durable tracing of scheduler events to JSONL files.
//!
//! The [`JsonlTraceWriter`] writes every [`SchedulerEvent`] to a
//! `.traces.jsonl` file co-located with the session's `SQLite` database
//! under `<graph_dir>/.eureka/sessions/`.
//!
//! Tracing is opt-in via the `[tracing]` section in `eureka.toml` or the
//! `EUREKA_TRACING_ENABLED` environment variable.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::config::{Budget, RunStats, TracingConfig};
use crate::scheduler::SchedulerEvent;

/// Writes `SchedulerEvent` entries to a JSONL file.
///
/// ## Invariants
/// - `open` returns `Ok(None)` on any I/O error — never `Err`.
/// - `write_event` catches I/O errors internally and logs via `tracing::warn!`.
/// - The `BufWriter` is flushed after every write.
/// - `close` performs an `fsync` for durability.
pub struct JsonlTraceWriter {
    /// Path to the output traces JSONL file.
    path: PathBuf,
    /// Buffered writer over the trace file.
    writer: BufWriter<File>,
    /// Whether to include full artifacts in trace output.
    include_artifacts: bool,
}

impl JsonlTraceWriter {
    /// Create and open the trace file. Writes the header line immediately.
    ///
    /// Returns `Ok(None)` if the file cannot be created (disk full, permission
    /// denied, etc.) — tracing is best-effort and must never fail a run.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        sessions_dir: &Path,
        session_id: &uuid::Uuid,
        config: &TracingConfig,
        graph_path: &str,
        provider: &str,
        model: &str,
        budget: &Budget,
        max_in_flight: usize,
    ) -> Option<Self> {
        // Ensure the sessions directory exists. Silently disable tracing on
        // failure, matching the SQLite DB behaviour.
        if let Err(e) = fs::create_dir_all(sessions_dir) {
            tracing::warn!(
                error = %e,
                "Failed to create sessions directory '{}'; tracing disabled",
                sessions_dir.display()
            );
            return None;
        }

        let file_name = format!("{session_id}.traces.jsonl");
        let path = sessions_dir.join(&file_name);

        let file = match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to open trace file '{}'; tracing disabled",
                    path.display()
                );
                return None;
            }
        };

        let mut writer = BufWriter::new(file);

        // Write the header line.
        let header = serde_json::json!({
            "type": "traceHeader",
            "sessionId": session_id.to_string(),
            "graph": graph_path,
            "startedAt": iso_now_rfc3339(),
            "config": {
                "provider": provider,
                "model": model,
                "maxInFlight": max_in_flight,
                "budget": {
                    "maxCostUsd": budget.max_cost_usd,
                    "maxTokens": budget.max_tokens,
                    "maxWallclock": budget.max_wallclock,
                    "maxRounds": budget.max_rounds,
                },
            },
        });

        if let Err(e) = writeln!(
            writer,
            "{}",
            serde_json::to_string(&header).unwrap_or_default()
        ) {
            tracing::warn!(
                error = %e,
                "Failed to write trace header; tracing disabled"
            );
            return None;
        }

        if let Err(e) = writer.flush() {
            tracing::warn!(
                error = %e,
                "Failed to flush trace header; tracing disabled"
            );
            return None;
        }

        Some(Self {
            path,
            writer,
            include_artifacts: config.include_artifacts,
        })
    }

    /// Append one event as a JSON line. Flushes after every write.
    ///
    /// On I/O error, logs a warning and drops the event. The run continues.
    pub fn write_event(&mut self, event: &SchedulerEvent) {
        let evt = if self.include_artifacts {
            event.clone()
        } else {
            strip_artifacts(event)
        };

        let line = match serde_json::to_string(&evt) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to serialize scheduler event for tracing"
                );
                return;
            }
        };

        if let Err(e) = writeln!(self.writer, "{line}") {
            tracing::warn!(
                error = %e,
                "Failed to write trace event; event dropped"
            );
            return;
        }

        if let Err(e) = self.writer.flush() {
            tracing::warn!(
                error = %e,
                "Failed to flush trace writer"
            );
        }
    }

    /// Write a footer with final `RunStats`, then flush + fsync.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the footer cannot be written or synced.
    pub fn close(mut self, stats: &RunStats) -> std::io::Result<()> {
        let footer = serde_json::json!({
            "type": "runCompleted",
            "totalCostUsd": stats.total_cost_usd,
            "totalTokens": stats.total_tokens,
            "totalInputTokens": stats.total_input_tokens,
            "totalOutputTokens": stats.total_output_tokens,
            "elapsedSecs": stats.elapsed_secs,
            "roundsCompleted": stats.rounds_completed,
        });

        writeln!(
            self.writer,
            "{}",
            serde_json::to_string(&footer).unwrap_or_default()
        )?;
        self.writer.flush()?;
        // Sync the underlying file to disk.
        self.writer.get_ref().sync_all()?;
        Ok(())
    }

    /// The path to the trace file.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }
}

/// Strip artifact payloads from an `ActivationCompleted` event. All other
/// events pass through unchanged.
fn strip_artifacts(event: &SchedulerEvent) -> SchedulerEvent {
    match event {
        SchedulerEvent::ActivationCompleted {
            node_id,
            node_kind,
            round,
            emit_count,
            outputs: _,
        } => SchedulerEvent::ActivationCompleted {
            node_id: node_id.clone(),
            node_kind: node_kind.clone(),
            round: *round,
            emit_count: *emit_count,
            outputs: vec![],
        },
        _ => event.clone(),
    }
}

/// Produce an RFC 3339 / ISO 8601 UTC timestamp string.
#[must_use]
pub fn iso_now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();

    let time_secs = secs % 86_400;
    let hours = time_secs / 3_600;
    let minutes = (time_secs % 3_600) / 60;
    let seconds = time_secs % 60;

    let days = secs / 86_400;
    let (year, month, day) = days_to_date(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month,
        day,
        hours,
        minutes,
        seconds,
        nanos / 1_000_000,
    )
}

/// Convert days since Unix epoch to (year, month, day) in the Gregorian
/// civil calendar. Uses Howard Hinnant's algorithm.
#[allow(clippy::many_single_char_names)]
#[must_use]
pub const fn days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + 3 - 12 * (mp / 10);
    let y = y + (mp / 10);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TracingConfig;
    use crate::scheduler::SchedulerEvent;
    use std::io::BufRead;

    fn test_budget() -> Budget {
        Budget {
            max_cost_usd: 25.0,
            max_tokens: 5_000_000,
            max_wallclock: 2700.0,
            max_rounds: 12,
        }
    }

    fn test_config() -> TracingConfig {
        TracingConfig {
            enabled: true,
            max_file_bytes: 0,
            include_artifacts: true,
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        let file = File::open(path).unwrap();
        let reader = std::io::BufReader::new(file);
        reader.lines().map(|l| l.unwrap()).collect()
    }

    #[test]
    fn test_open_creates_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let session_id = uuid::Uuid::now_v7();
        let config = test_config();

        let writer = JsonlTraceWriter::open(
            &sessions_dir,
            &session_id,
            &config,
            "test.yml",
            "openrouter",
            "model-x",
            &test_budget(),
            8,
        )
        .expect("writer should be created");

        let trace_path = writer.path().to_path_buf();
        writer
            .close(&RunStats::default())
            .expect("close should succeed");

        assert!(trace_path.exists(), "trace file should exist");
        assert!(
            trace_path.to_string_lossy().ends_with(".traces.jsonl"),
            "file should end with .traces.jsonl"
        );
    }

    #[test]
    fn test_open_writes_header() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let session_id = uuid::Uuid::now_v7();
        let config = test_config();

        let writer = JsonlTraceWriter::open(
            &sessions_dir,
            &session_id,
            &config,
            "example/coscientist.yml",
            "openrouter",
            "deepseek/deepseek-v4-flash",
            &test_budget(),
            8,
        )
        .expect("writer should be created");

        let path = writer.path().to_path_buf();
        writer.close(&RunStats::default()).unwrap();

        let lines = read_lines(&path);
        assert!(!lines.is_empty(), "should have at least a header line");

        let header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(header["type"], "traceHeader");
        assert_eq!(header["sessionId"], session_id.to_string());
        assert_eq!(header["graph"], "example/coscientist.yml");
        assert_eq!(header["config"]["provider"], "openrouter");
        assert_eq!(header["config"]["model"], "deepseek/deepseek-v4-flash");
        assert_eq!(header["config"]["maxInFlight"], 8);
        assert!(
            (header["config"]["budget"]["maxCostUsd"].as_f64().unwrap() - 25.0).abs()
                < f64::EPSILON
        );
        assert_eq!(header["config"]["budget"]["maxRounds"], 12);
    }

    #[test]
    fn test_write_event() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let session_id = uuid::Uuid::now_v7();
        let config = test_config();

        let mut writer = JsonlTraceWriter::open(
            &sessions_dir,
            &session_id,
            &config,
            "test.yml",
            "openrouter",
            "m",
            &test_budget(),
            8,
        )
        .expect("writer should be created");

        writer.write_event(&SchedulerEvent::ActivationStarted {
            node_id: "gen".to_string(),
            node_kind: "generation".to_string(),
            round: 0,
        });

        let path = writer.path().to_path_buf();
        writer.close(&RunStats::default()).unwrap();

        // Find the activationStarted event (skip header line).
        let lines = read_lines(&path);
        assert!(lines.len() >= 2, "header + event = at least 2 lines");
        let event_line = lines
            .iter()
            .find(|l| l.contains("activationStarted"))
            .expect("should find an activationStarted event");

        let event: serde_json::Value =
            serde_json::from_str(event_line).expect("event line should be valid JSON");
        assert_eq!(event["type"], "activationStarted");
        assert_eq!(event["node_id"], "gen");
        assert_eq!(event["node_kind"], "generation");
        assert_eq!(event["round"], 0);
    }

    #[test]
    fn test_include_artifacts_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let session_id = uuid::Uuid::now_v7();
        let config = TracingConfig {
            enabled: true,
            max_file_bytes: 0,
            include_artifacts: false,
        };

        let mut writer = JsonlTraceWriter::open(
            &sessions_dir,
            &session_id,
            &config,
            "test.yml",
            "openrouter",
            "m",
            &test_budget(),
            8,
        )
        .expect("writer should be created");

        writer.write_event(&SchedulerEvent::ActivationCompleted {
            node_id: "gen".to_string(),
            node_kind: "generation".to_string(),
            round: 0,
            emit_count: 1,
            outputs: vec![serde_json::json!({
                "port": "out",
                "kind": "Hypotheses",
                "data": {"text": "hello"}
            })],
        });

        let path = writer.path().to_path_buf();
        writer.close(&RunStats::default()).unwrap();

        let lines = read_lines(&path);
        let event_line = lines
            .iter()
            .find(|l| l.contains("activationCompleted"))
            .expect("should find an activationCompleted event");
        let event: serde_json::Value =
            serde_json::from_str(event_line).expect("event line should be valid JSON");
        assert_eq!(event["type"], "activationCompleted");
        assert_eq!(
            event["outputs"],
            serde_json::Value::Array(vec![]),
            "outputs should be empty when include_artifacts is false"
        );
    }

    #[test]
    fn test_close_writes_footer() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let session_id = uuid::Uuid::now_v7();
        let config = test_config();

        let writer = JsonlTraceWriter::open(
            &sessions_dir,
            &session_id,
            &config,
            "test.yml",
            "openrouter",
            "m",
            &test_budget(),
            8,
        )
        .expect("writer should be created");

        let stats = RunStats {
            total_cost_usd: 1.23,
            total_tokens: 1500,
            total_input_tokens: 1000,
            total_output_tokens: 500,
            elapsed_secs: 12.5,
            rounds_completed: 3,
        };

        let path = writer.path().to_path_buf();
        writer.close(&stats).unwrap();

        let lines = read_lines(&path);
        let last_line = lines.last().unwrap();
        let footer: serde_json::Value = serde_json::from_str(last_line).unwrap();
        assert_eq!(footer["type"], "runCompleted");
        assert_eq!(footer["totalCostUsd"], 1.23);
        assert_eq!(footer["totalTokens"], 1500);
        assert_eq!(footer["roundsCompleted"], 3);
    }

    #[test]
    fn test_open_nonexistent_dir_creates_it() {
        // The sessions_dir path doesn't exist yet; create_dir_all creates it.
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join("new_nested").join("sessions");
        let session_id = uuid::Uuid::now_v7();
        let config = test_config();

        let writer = JsonlTraceWriter::open(
            &sessions_dir,
            &session_id,
            &config,
            "test.yml",
            "openrouter",
            "m",
            &test_budget(),
            8,
        )
        .expect("open should create the directory tree and succeed");

        let path = writer.path().to_path_buf();
        assert!(path.exists(), "trace file should exist after open");
    }

    #[cfg(unix)]
    #[test]
    fn test_open_readonly_root_returns_none() {
        // Test that when the sessions parent is not writable, open returns Ok(None).
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let parent = dir.path().join("readonly_parent");
        fs::create_dir_all(&parent).unwrap();
        let sessions_dir = parent.join("sessions");

        // Make the parent read-only so create_dir_all fails.
        let readonly = fs::Permissions::from_mode(0o444);
        fs::set_permissions(&parent, readonly).unwrap_or(());

        let session_id = uuid::Uuid::now_v7();
        let config = test_config();

        let result = JsonlTraceWriter::open(
            &sessions_dir,
            &session_id,
            &config,
            "test.yml",
            "openrouter",
            "m",
            &test_budget(),
            8,
        );

        // Restore permissions so cleanup works.
        let writable = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&parent, writable).unwrap_or(());

        assert!(result.is_none(), "open should return None on readonly root");
    }

    #[test]
    fn test_strip_artifacts_leaves_other_events() {
        let event = SchedulerEvent::ActivationStarted {
            node_id: "x".to_string(),
            node_kind: "y".to_string(),
            round: 1,
        };
        let stripped = strip_artifacts(&event);
        assert!(matches!(stripped, SchedulerEvent::ActivationStarted { .. }));
    }

    #[test]
    fn test_days_to_date_known() {
        // Unix epoch: 1970-01-01
        assert_eq!(days_to_date(0), (1970, 1, 1));
        // 2025-01-01 = day 20089 (55 years * 365 + 14 leap years)
        assert_eq!(days_to_date(20089), (2025, 1, 1));
        // 2026-07-10 = day 20644 (20089 + 365 for 2025 + 190 for Jan-Jun+10)
        assert_eq!(days_to_date(20644), (2026, 7, 10));
    }
}
