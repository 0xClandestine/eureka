//! Run-scoped environment descriptor shared with executable nodes.

use std::path::PathBuf;

/// Current version of the runtime database environment contract.
///
/// This is deliberately independent from the plugin schemas stored in the
/// per-run database. It lets subprocesses reject an environment they do not
/// understand without requiring Eureka to own every plugin migration.
pub const DATABASE_SCHEMA_VERSION: u32 = 1;

/// Run-scoped database and identity information shared with executable nodes.
///
/// The runtime keeps this as a capability descriptor rather than handing graph
/// nodes a raw database connection. Control nodes and agent tools can use the
/// descriptor to open the same per-run database, while the Rust scheduler keeps
/// ownership of its own persistence transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnvironment {
    /// Unique session identifier.
    session_id: String,
    /// Optional path to the per-run `SQLite` database.
    db_path: Option<PathBuf>,
    /// Expected database schema version for compatibility checks.
    database_schema_version: u32,
}

impl RunEnvironment {
    /// Create a run environment using the current database contract version.
    #[must_use]
    pub fn new(session_id: impl Into<String>, db_path: Option<PathBuf>) -> Self {
        Self {
            session_id: session_id.into(),
            db_path,
            database_schema_version: DATABASE_SCHEMA_VERSION,
        }
    }

    /// Return the stable session/run identifier.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Return the optional path to the per-run database.
    #[must_use]
    pub const fn db_path(&self) -> Option<&PathBuf> {
        self.db_path.as_ref()
    }

    /// Return the runtime database contract version.
    #[must_use]
    pub const fn database_schema_version(&self) -> u32 {
        self.database_schema_version
    }

    /// Build the environment passed to one executable node invocation.
    ///
    /// The returned values are owned because subprocess execution can outlive
    /// the caller's stack frame. `EUREKA_DB_NAMESPACE` identifies the node's
    /// plugin namespace; it does not grant access to runtime-owned tables.
    #[must_use]
    pub fn subprocess_env(
        &self,
        node_id: &str,
        round: u32,
        config_json: &str,
    ) -> Vec<(String, String)> {
        let mut env = vec![
            ("EUREKA_SESSION_ID".to_string(), self.session_id.clone()),
            ("EUREKA_NODE_ID".to_string(), node_id.to_string()),
            ("EUREKA_ROUND".to_string(), round.to_string()),
            ("EUREKA_CONFIG".to_string(), config_json.to_string()),
            ("EUREKA_DB_SCHEMA_VERSION".to_string(), self.database_schema_version.to_string()),
            ("EUREKA_DB_NAMESPACE".to_string(), node_id.to_string()),
        ];
        if let Some(path) = &self.db_path {
            env.push(("EUREKA_DB_PATH".to_string(), path.display().to_string()));
        }
        env
    }
}
