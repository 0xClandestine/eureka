//! `SessionDb` — SQLite-backed session database.
//!
//! One database per Eureka run, stored at
//! `~/.eureka/sessions/<session-id>/session.db`.
//! The database uses WAL mode so the UI server can read concurrently while
//! the session is running.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::error::DbError;

/// SQL schema executed on every new database.
const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT    PRIMARY KEY,
    graph_id    TEXT    NOT NULL,
    goal        TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    stopped_at  INTEGER,
    rounds      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT    NOT NULL REFERENCES sessions(id),
    seq           INTEGER NOT NULL,
    round         INTEGER NOT NULL,
    node_id       TEXT    NOT NULL,
    port          TEXT    NOT NULL,
    artifact_kind TEXT    NOT NULL,
    artifact_data TEXT    NOT NULL,
    emitted_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS events_session_seq   ON events(session_id, seq);
CREATE INDEX IF NOT EXISTS events_session_round ON events(session_id, round);
";

/// Returns the current time as a Unix timestamp in milliseconds.
fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// SQLite-backed session database.
///
/// `SessionDb` is `Send + Sync` because its connection is guarded by a
/// `Mutex`. All write methods are synchronous; callers in async contexts
/// should use `tokio::task::block_in_place` or `tokio::task::spawn_blocking`
/// to avoid blocking the executor for long-running operations. In practice,
/// SQLite WAL-mode inserts complete in < 1 ms and are safe to call inline.
pub struct SessionDb {
    /// The open SQLite connection.
    conn: Mutex<Connection>,
    /// Monotonically increasing event sequence counter for this session.
    seq: AtomicU64,
    /// The session ID (UUID string).
    session_id: String,
    /// Absolute path to the database file.
    path: PathBuf,
}

impl SessionDb {
    /// Open (or create) the session database for `session_id`.
    ///
    /// Creates `~/.eureka/sessions/<session_id>/` if needed, opens the
    /// database, applies the schema, and inserts the initial sessions row.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the directory or database cannot be created,
    /// the schema cannot be applied, or the initial row cannot be inserted.
    pub fn create(session_id: &str, graph_id: &str, goal_json: &str) -> Result<Self, DbError> {
        let dir = session_dir(session_id);
        std::fs::create_dir_all(&dir)?;

        let path = dir.join("session.db");
        let conn = Connection::open(&path)?;

        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;

        conn.execute(
            "INSERT INTO sessions (id, graph_id, goal, status, created_at) \
             VALUES (?1, ?2, ?3, 'running', ?4)",
            rusqlite::params![session_id, graph_id, goal_json, unix_ms()],
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            seq: AtomicU64::new(0),
            session_id: session_id.to_string(),
            path,
        })
    }

    /// Record one artifact emission as an event row.
    ///
    /// Assigns the next sequence number atomically. Sequence numbers are
    /// monotonically increasing within a session but not guaranteed to be
    /// contiguous if writes happen concurrently.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the mutex is poisoned or the INSERT fails.
    pub fn write_event(
        &self,
        round: u32,
        node_id: &str,
        port: &str,
        artifact_kind: &str,
        artifact_data: &str,
    ) -> Result<(), DbError> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO events \
             (session_id, seq, round, node_id, port, artifact_kind, artifact_data, emitted_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                self.session_id,
                seq as i64,
                round as i64,
                node_id,
                port,
                artifact_kind,
                artifact_data,
                unix_ms(),
            ],
        )?;
        Ok(())
    }

    /// Mark the session as completed with final statistics.
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the mutex is poisoned or the UPDATE fails.
    pub fn complete_session(&self, rounds: u32) -> Result<(), DbError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions \
             SET status = 'completed', stopped_at = ?1, rounds = ?2 \
             WHERE id = ?3",
            rusqlite::params![unix_ms(), rounds as i64, self.session_id],
        )?;
        Ok(())
    }

    /// Mark the session as stopped (interrupted before completion).
    ///
    /// # Errors
    ///
    /// Returns a [`DbError`] if the mutex is poisoned or the UPDATE fails.
    pub fn stop_session(&self) -> Result<(), DbError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET status = 'stopped', stopped_at = ?1 WHERE id = ?2",
            rusqlite::params![unix_ms(), self.session_id],
        )?;
        Ok(())
    }

    /// The session ID this database was created for.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Absolute path to the SQLite database file.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Acquire the connection lock, mapping a poison error to [`DbError`].
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, DbError> {
        self.conn.lock().map_err(|_| DbError::LockPoisoned)
    }
}

/// Returns the path to `~/.eureka/sessions/<session_id>/`.
fn session_dir(session_id: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".eureka")
        .join("sessions")
        .join(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_db(id: &str) -> (tempfile::TempDir, SessionDb) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.db");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, graph_id, goal, status, created_at) VALUES (?1, ?2, ?3, 'running', ?4)",
            rusqlite::params![id, "test-graph", "{}", 0i64],
        ).unwrap();
        let db = SessionDb {
            conn: Mutex::new(conn),
            seq: AtomicU64::new(0),
            session_id: id.to_string(),
            path,
        };
        (tmp, db)
    }

    #[test]
    fn test_write_event() {
        let (_tmp, db) = temp_db("test-session-1");
        db.write_event(0, "generation", "out", "Hypotheses", r#"{"items":[]}"#)
            .unwrap();
        let conn = db.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_sequence_increments() {
        let (_tmp, db) = temp_db("test-session-2");
        let db = Arc::new(db);
        for i in 0..5 {
            db.write_event(i, "node", "out", "Kind", "{}").unwrap();
        }
        let conn = db.conn.lock().unwrap();
        let max_seq: i64 = conn
            .query_row("SELECT MAX(seq) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max_seq, 4);
    }

    #[test]
    fn test_complete_session() {
        let (_tmp, db) = temp_db("test-session-3");
        db.complete_session(5).unwrap();
        let conn = db.conn.lock().unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM sessions WHERE id = 'test-session-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "completed");
    }
}
