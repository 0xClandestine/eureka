# Session Database

**Status**: Stable
**Crate**: `eureka-db`
**Files**: `crates/eureka-db/src/db.rs`, `crates/eureka-db/src/error.rs`, `crates/eureka-db/src/lib.rs`

Every Eureka run is a **session** — a persistent object with a unique identity. All artifact emissions produced during a session are stored in a per-session SQLite database, giving the full scientific history of the run for inspection and export.

---

## Purpose

`eureka-db` provides a single public struct, `SessionDb`, that:

- Creates and initialises the session database file on disk.
- Records every artifact emission as an `events` row.
- Updates the `sessions` row when the session completes or is interrupted.
- Exposes read-only accessors for the session ID and database path (used by plugins via `EUREKA_DB_PATH`).

---

## Location

```
~/.eureka/sessions/<session-id>/session.db
```

`session-id` is a UUID v4 string passed in by the caller (`eureka-cli`). The directory is created by `SessionDb::create` if it does not already exist.

If `dirs::home_dir()` returns `None` (rare, non-Unix environments), the path falls back to `./.eureka/sessions/<session-id>/session.db`.

The resolved absolute path is exposed to plugins via the `EUREKA_DB_PATH` environment variable (set by `eureka-cli`).

---

## Schema

The schema is embedded as a single SQL string (`SCHEMA` constant in `db.rs`) and is applied with `conn.execute_batch(SCHEMA)` on every new database.

### `sessions`

One row per session. Inserted when the session starts (`status = 'running'`), updated when it ends.

```sql
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT    PRIMARY KEY,           -- UUID v4
    graph_id    TEXT    NOT NULL,              -- graph package identifier
    goal        TEXT    NOT NULL,              -- JSON-encoded initial goal
    status      TEXT    NOT NULL,              -- 'running' | 'stopped' | 'completed'
    created_at  INTEGER NOT NULL,              -- Unix timestamp (milliseconds)
    stopped_at  INTEGER,                       -- NULL while running
    rounds      INTEGER NOT NULL DEFAULT 0
);
```

`created_at` and `stopped_at` are Unix timestamps in **milliseconds** (not seconds).

### `events`

Every artifact emitted by any node is an event row. This is the complete chronological history of the session.

```sql
CREATE TABLE IF NOT EXISTS events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT    NOT NULL REFERENCES sessions(id),
    seq           INTEGER NOT NULL,   -- monotonically increasing within the session
    round         INTEGER NOT NULL,
    node_id       TEXT    NOT NULL,   -- node id from graph.json
    port          TEXT    NOT NULL,   -- output port name
    artifact_kind TEXT    NOT NULL,
    artifact_data TEXT    NOT NULL,   -- JSON-encoded artifact data
    emitted_at    INTEGER NOT NULL    -- Unix timestamp (milliseconds)
);

CREATE INDEX IF NOT EXISTS events_session_seq   ON events(session_id, seq);
CREATE INDEX IF NOT EXISTS events_session_round ON events(session_id, round);
```

### Tables not present

The following tables are **not** in the current implementation: `agent_traces`, `tool_calls`, `elo_ratings`, `budget_snapshots`. They are aspirational design from earlier planning documents but have not been implemented.

---

## `SessionDb` Struct

```rust
pub struct SessionDb {
    conn:       Mutex<Connection>,   // guarded rusqlite connection
    seq:        AtomicU64,           // monotonically increasing event counter
    session_id: String,              // UUID string
    path:       PathBuf,             // absolute path to session.db
}
```

All fields are private. `SessionDb` is `Send + Sync` because the connection is guarded by a `std::sync::Mutex`.

---

## Methods

### `SessionDb::create`

```rust
pub fn create(session_id: &str, graph_id: &str, goal_json: &str) -> Result<Self, DbError>
```

Opens (or creates) the database at `~/.eureka/sessions/<session_id>/session.db`:

1. Creates the session directory with `std::fs::create_dir_all`.
2. Opens the SQLite connection via `rusqlite::Connection::open`.
3. Sets `PRAGMA journal_mode = WAL` (concurrent reads from the UI server).
4. Sets `PRAGMA foreign_keys = ON`.
5. Applies the schema (`CREATE TABLE IF NOT EXISTS …`).
6. Inserts the initial `sessions` row with `status = 'running'` and `created_at = unix_ms()`.
7. Initialises `seq` to `0`.

Returns `DbError` if the directory cannot be created, the database cannot be opened, the schema cannot be applied, or the initial `INSERT` fails.

---

### `write_event`

```rust
pub fn write_event(
    &self,
    round: u32,
    node_id: &str,
    port: &str,
    artifact_kind: &str,
    artifact_data: &str,
) -> Result<(), DbError>
```

Records one artifact emission. Parameter order maps directly to the corresponding `events` columns.

- Atomically fetches-and-increments `seq` with `Ordering::Relaxed` before acquiring the mutex. Sequence numbers are monotonically increasing within a session but are **not** guaranteed to be contiguous under concurrent writers.
- Acquires `Mutex<Connection>`, executes a single `INSERT INTO events …` statement.
- `artifact_data` must already be a JSON string; the caller is responsible for serialisation.

---

### `complete_session`

```rust
pub fn complete_session(&self, rounds: u32) -> Result<(), DbError>
```

Sets `status = 'completed'`, `stopped_at = unix_ms()`, and `rounds = rounds` on the session row. Called when the scheduler terminates normally (governor emits `halt`).

---

### `stop_session`

```rust
pub fn stop_session(&self) -> Result<(), DbError>
```

Sets `status = 'stopped'` and `stopped_at = unix_ms()` on the session row. Called when the session is interrupted (e.g. `Ctrl-C`). Unlike `complete_session`, `rounds` is left unchanged (remains `0` unless previously updated).

---

### `session_id`

```rust
#[must_use]
pub fn session_id(&self) -> &str
```

Returns the session ID string this database was created for.

---

### `path`

```rust
#[must_use]
pub fn path(&self) -> &PathBuf
```

Returns the absolute path to the SQLite file. Used by `eureka-cli` to set `EUREKA_DB_PATH` for plugin subprocesses.

---

## `DbError`

```rust
pub enum DbError {
    Sqlite(rusqlite::Error),   // any SQLite operation failure; #[from] rusqlite::Error
    Io(std::io::Error),        // directory creation or file I/O failure; #[from] std::io::Error
    LockPoisoned,              // Mutex was poisoned by a previous panic
    Serde(serde_json::Error),  // JSON serialisation failure; #[from] serde_json::Error
}
```

`LockPoisoned` is produced by the private `lock()` helper when `Mutex::lock` returns `Err`. The `Serde` variant is present for completeness (callers that serialise before calling `write_event` may forward serde errors through `DbError`); it is not produced internally by `SessionDb` itself.

---

## Design

### WAL Mode

WAL (`PRAGMA journal_mode = WAL`) is set immediately after opening the connection. This allows the UI server (running in a separate thread/process) to read the database concurrently while the scheduler writes event rows. SQLite WAL-mode inserts complete in under 1 ms; write methods are synchronous. Callers in async contexts should use `tokio::task::block_in_place` or `tokio::task::spawn_blocking` to avoid blocking the executor for long-running operations, though in practice the short critical section makes inline calls safe for this workload.

### Atomic Sequence Counter

`seq` is an `AtomicU64` (not a DB sequence). The counter is incremented with `fetch_add(1, Ordering::Relaxed)` before the mutex is acquired for the INSERT. This means the sequence number is reserved before the lock is held, keeping the critical section minimal. The `seq` value is cast to `i64` for storage (SQLite has no unsigned integer type).

### `Mutex` over `async` lock

The connection is guarded by `std::sync::Mutex`, not `tokio::sync::Mutex`. This is intentional: SQLite operations are blocking by nature, and the critical section is short (a single INSERT). Using a synchronous mutex avoids the overhead of async wakeups for an operation that never actually yields.

---

## Invariants

- `seq` starts at `0` and increments by `1` per `write_event` call. The first event has `seq = 0`.
- `status` transitions: `'running'` → `'completed'` (via `complete_session`) or `'running'` → `'stopped'` (via `stop_session`).
- `rounds` is only written by `complete_session`; `stop_session` does not update it.
- All timestamps (`created_at`, `stopped_at`, `emitted_at`) are Unix time in **milliseconds**.
- `foreign_keys` is enabled; every `events.session_id` value must exist in `sessions.id`.
- The `sessions` row is inserted by `create` before any `write_event` call, satisfying the foreign key constraint.

---

## Dependencies

| Crate | Feature | Purpose |
|---|---|---|
| `rusqlite` | `bundled` | SQLite engine (no system SQLite required) |
| `serde_json` | — | `DbError::Serde` variant; callers use it for serialisation |
| `thiserror` | — | `#[derive(Error)]` on `DbError` |
| `dirs` | — | `dirs::home_dir()` for path derivation |
| `tempfile` | — (dev) | In-process test databases in `db.rs` tests |

`uuid` is **not** a dependency of `eureka-db`. UUID generation is the caller's responsibility (`eureka-cli` generates the session ID).

---

## Non-Goals

- Session resume: reopening an existing database and continuing from the last round is not implemented.
- Schema migrations: there is one static schema applied at creation time; no migration framework is used.
- Token / cost tracking: `total_tokens`, `total_cost_usd` columns do not exist; this is tracked as a gap in `specs/INDEX.md`.
- Agent trace or tool call recording: `agent_traces` and `tool_calls` tables are not implemented.
- Elo persistence in the DB: the `elo-ranker` plugin manages its own SQLite state separately.
