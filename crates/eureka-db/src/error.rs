//! Error types for the session database.

/// Errors that can occur when using the session database.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A SQLite operation failed.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// An I/O operation failed (e.g. creating the session directory).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A mutex was poisoned, indicating a previous panic held the lock.
    #[error("Internal lock poisoned")]
    LockPoisoned,

    /// JSON serialization of artifact data failed.
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}
