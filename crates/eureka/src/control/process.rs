//! Shared subprocess runner used by both agent tools and control nodes.
//!
//! Both [`CommandTool`](crate::agents::CommandTool) and
//! [`ControlNode`](crate::control::ControlNode) follow the same
//! pattern: spawn a process, write JSON to stdin, read line-delimited JSON
//! from stdout, and return results. This module extracts the common
//! spawn-communicate-wait-with-timeout logic so it isn't duplicated.

use std::path::Path;
use std::time::Duration;

use super::error::ControlError;

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
    /// Create a `ProcessOutput` from a `tokio::process::Output`.
    fn from_output(output: &std::process::Output) -> Self {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = stdout.trim();
        let capped = if stdout.len() > MAX_OUTPUT_BYTES {
            format!("{}\n[truncated]", &stdout[..MAX_OUTPUT_BYTES])
        } else {
            stdout.to_string()
        };
        Self {
            stdout: capped,
            stderr: stderr.to_string(),
            exit_code: output.status.code(),
            success: output.status.success(),
        }
    }
}

/// Run a subprocess with the given configuration.
///
/// Spawns `binary` with `args`, writes `stdin_data` to the process's stdin,
/// then waits up to `timeout_secs` for the process to complete.
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
        .stderr(std::process::Stdio::piped());

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

    // Write stdin data then close the pipe.
    if let Some(mut stdin) = child.stdin.take() {
        tokio::io::AsyncWriteExt::write_all(&mut stdin, stdin_data.as_bytes())
            .await
            .map_err(ControlError::StdinWrite)?;
        // stdin drops here — EOF sent to child
    }

    // Wait with timeout.
    let timeout = Duration::from_secs(u64::from(timeout_secs));
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => ProcessOutput::from_output(&out),
        Ok(Err(e)) => return Err(ControlError::Wait(e)),
        Err(_elapsed) => {
            return Err(ControlError::Timeout { timeout_secs });
        }
    };

    Ok(output)
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
}
