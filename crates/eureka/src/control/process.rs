//! Shared subprocess runner used by both agent tools and control nodes.
//!
//! Both [`CommandTool`](crate::agents::CommandTool) and
//! [`ControlNode`](crate::control::ControlNode) follow the same
//! pattern: spawn a process, write JSON to stdin, read line-delimited JSON
//! from stdout, and return results. This module extracts the common
//! spawn-communicate-wait-with-timeout logic so it isn't duplicated.
//!
//! The runner drives stdin, stdout, and stderr concurrently so a child that
//! writes a large amount of output before reading its stdin cannot deadlock
//! the pipes. Timed-out children are killed explicitly (and reaped via
//! `kill_on_drop`) so no orphan processes leak.

use std::path::Path;
use std::time::Duration;

use super::error::ControlError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Maximum bytes captured from subprocess stdout.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Captured output from a subprocess invocation.
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    /// Stdout content, trimmed and capped at `MAX_OUTPUT_BYTES`.
    pub stdout: String,
    /// Stderr content (first 500 chars excerpt on failure, full on success).
    pub stderr: String,
    /// Exit code, or `None` if terminated by signal.
    pub exit_code: Option<i32>,
    /// Whether the process exited successfully (exit code 0).
    pub success: bool,
}

impl ProcessOutput {
    /// Build a `ProcessOutput` from raw stdout/stderr bytes and an exit status.
    fn from_parts(stdout: Vec<u8>, stderr: Vec<u8>, status: Option<i32>, success: bool) -> Self {
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);
        let stdout = stdout.trim();
        // Cap on a character boundary to avoid splitting a multi-byte UTF-8
        // sequence (which would panic on `str` slicing).
        let capped = if stdout.len() > MAX_OUTPUT_BYTES {
            let cut = stdout.floor_char_boundary(MAX_OUTPUT_BYTES);
            format!("{}\n[truncated]", &stdout[..cut])
        } else {
            stdout.to_string()
        };
        Self {
            stdout: capped,
            stderr: stderr.to_string(),
            exit_code: status,
            success,
        }
    }
}

/// Run a subprocess with the given configuration.
///
/// Spawns `binary` with `args`, writes `stdin_data` to the process's stdin,
/// reads stdout/stderr concurrently, then waits up to `timeout_secs` for the
/// process to complete. The child is spawned with `kill_on_drop(true)` and is
/// explicitly killed on timeout, so no orphan process is left behind.
///
/// # Errors
///
/// Returns a [`ControlError`] if the process cannot be spawned, communication
/// fails, or the timeout is exceeded.
pub async fn run_subprocess(
    binary: &str,
    args: &[String],
    current_dir: Option<&Path>,
    envs: &[(&str, String)],
    stdin_data: &str,
    timeout_secs: u32,
) -> Result<ProcessOutput, ControlError> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Reap/kill the child if the future is dropped (e.g. on timeout) so
        // we never leak an orphaned process.
        .kill_on_drop(true);

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    for (key, val) in envs {
        cmd.env(key, val);
    }

    let mut child = cmd.spawn().map_err(|e| ControlError::Spawn {
        binary: binary.to_string(),
        source: e,
    })?;

    // Take the pipes so we can drive stdin/stdout/stderr concurrently.
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    // Concurrently: write stdin, read stdout, read stderr.
    let stdin_task = tokio::spawn({
        let stdin_data = stdin_data.to_string();
        async move {
            if let Some(stdin) = stdin.as_mut() {
                // Ignore broken pipe: the child may exit before reading all input.
                let _ = stdin.write_all(stdin_data.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
        }
    });

    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_end(&mut buf).await;
        }
        buf
    });

    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_end(&mut buf).await;
        }
        buf
    });

    let timeout = Duration::from_secs(u64::from(timeout_secs));
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(ControlError::Wait(e)),
        Err(_elapsed) => {
            // Timeout: kill the child explicitly and await its termination so
            // the process is reaped and its PID is not left dangling.
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(ControlError::Timeout { timeout_secs });
        }
    };

    // The child has exited; the read tasks finish immediately.
    let _ = stdin_task.await;
    let stdout_buf = stdout_task.await.unwrap_or_default();
    let stderr_buf = stderr_task.await.unwrap_or_default();

    Ok(ProcessOutput::from_parts(
        stdout_buf,
        stderr_buf,
        status.code(),
        status.success(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_echo() {
        let out = run_subprocess("echo", &["hello".into()], None, &[], "", 5)
            .await
            .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, "hello");
    }

    #[tokio::test]
    async fn test_stdin_passthrough() {
        let out = run_subprocess("cat", &[], None, &[], "hello stdin", 5)
            .await
            .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, "hello stdin");
    }

    #[tokio::test]
    async fn test_nonzero_exit() {
        let out = run_subprocess("sh", &["-c".into(), "exit 1".into()], None, &[], "", 5)
            .await
            .unwrap();
        assert!(!out.success);
        assert_eq!(out.exit_code, Some(1));
    }

    #[tokio::test]
    async fn test_timeout() {
        let err = run_subprocess("sleep", &["10".into()], None, &[], "", 1)
            .await
            .unwrap_err();
        assert!(matches!(err, ControlError::Timeout { .. }));
    }

    #[tokio::test]
    async fn test_env_vars() {
        let out = run_subprocess(
            "sh",
            &["-c".into(), "echo $MY_VAR".into()],
            None,
            &[("MY_VAR", "hello_env".into())],
            "",
            5,
        )
        .await
        .unwrap();
        assert_eq!(out.stdout, "hello_env");
    }

    #[tokio::test]
    async fn test_large_output_does_not_deadlock() {
        // Regression for H2: a child that writes a large amount to stdout
        // before reading stdin must not deadlock the pipes. We write a large
        // stdin payload AND have the child emit a large stdout payload; the
        // runner must return (not hang) within the timeout.
        let big_stdin = "x".repeat(200_000);
        let out = run_subprocess(
            "sh",
            &[
                "-c".into(),
                "yes hello | head -c 200000; cat >/dev/null".into(),
            ],
            None,
            &[],
            &big_stdin,
            10,
        )
        .await
        .unwrap();
        assert!(out.success);
        // Output is capped, but the run completed without deadlocking.
        assert!(out.stdout.ends_with("[truncated]"));
    }

    #[tokio::test]
    async fn test_multibyte_truncation_does_not_panic() {
        // Regression for M1: capping output on a non-ASCII boundary must not
        // panic when slicing into a multi-byte UTF-8 sequence. Pipe a large run
        // of a multibyte char through `cat` so stdout exceeds the cap.
        let payload = "\u{e9}".repeat(MAX_OUTPUT_BYTES + 100);
        let out = run_subprocess("cat", &[], None, &[], &payload, 10)
            .await
            .unwrap();
        assert!(out.success);
        assert!(out.stdout.ends_with("[truncated]"));
    }
}
