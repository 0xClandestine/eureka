use thiserror::Error;

/// Errors that can occur when running a control subprocess.
#[derive(Debug, Error)]
pub enum ControlError {
    /// The process could not be spawned.
    #[error("Failed to spawn process '{binary}': {source}")]
    Spawn {
        /// The binary that failed to spawn.
        binary: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Writing to stdin failed.
    #[error("Failed to write to process stdin: {0}")]
    StdinWrite(#[source] std::io::Error),

    /// Waiting for the process to finish failed.
    #[error("Failed to wait for process: {0}")]
    Wait(#[source] std::io::Error),

    /// The process timed out.
    #[error("Process timed out after {timeout_secs}s")]
    Timeout {
        /// The timeout that was exceeded.
        timeout_secs: u32,
    },
}
