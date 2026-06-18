//! Plugin registry — discovers and loads plugin manifests from the filesystem.
//!
//! Discovery searches two locations in order, with later entries overriding
//! earlier ones (higher priority wins):
//!
//! 1. User-global: `~/.eureka/plugins/<name>/plugin.json`
//! 2. Graph-local: `<graph_dir>/plugins/<name>/plugin.json`
//!
//! Graph-local plugins shadow user-global ones with the same name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::manifest::PluginManifest;

/// A loaded plugin: its parsed manifest and the directory it lives in.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// The parsed `plugin.json` manifest.
    pub manifest: PluginManifest,
    /// Absolute path to the directory containing `plugin.json` and the scripts.
    pub plugin_dir: PathBuf,
}

/// Registry of all discovered plugins, keyed by plugin name.
#[derive(Debug, Default)]
pub struct PluginRegistry {
    /// Loaded plugins, keyed by name. Graph-local entries shadow global ones.
    plugins: HashMap<String, PluginEntry>,
}

impl PluginRegistry {
    /// Create an empty registry (no plugins discovered).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    /// Discover plugins from `graph_dir/plugins/` and `~/.eureka/plugins/`.
    ///
    /// Both directories are scanned; graph-local entries override user-global
    /// entries with the same name. Missing directories are silently skipped.
    ///
    /// # Errors
    ///
    /// Returns a [`PluginError`] if a directory exists but cannot be read,
    /// or a `plugin.json` file cannot be parsed.
    pub fn discover(graph_dir: &Path) -> Result<Self, PluginError> {
        let mut plugins = HashMap::new();

        // User-global (lower priority — discovered first so graph-local can override)
        if let Some(global_dir) = dirs::home_dir().map(|h| h.join(".eureka").join("plugins")) {
            if global_dir.is_dir() {
                discover_in_dir(&global_dir, &mut plugins)?;
            }
        }

        // Graph-local (higher priority — overrides global)
        let local_dir = graph_dir.join("plugins");
        if local_dir.is_dir() {
            discover_in_dir(&local_dir, &mut plugins)?;
        }

        Ok(Self { plugins })
    }

    /// Returns the number of discovered plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns `true` if no plugins were discovered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Get a plugin entry by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PluginEntry> {
        self.plugins.get(name)
    }

    /// Iterate over all plugin entries.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PluginEntry)> {
        self.plugins.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// Scan one directory level and load all `<entry>/plugin.json` manifests.
fn discover_in_dir(
    dir: &Path,
    plugins: &mut HashMap<String, PluginEntry>,
) -> Result<(), PluginError> {
    let entries = std::fs::read_dir(dir).map_err(|e| PluginError::DirectoryScan {
        path: dir.display().to_string(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| PluginError::DirectoryScan {
            path: dir.display().to_string(),
            source: e,
        })?;

        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }

        let manifest_path = plugin_dir.join("plugin.json");
        if !manifest_path.exists() {
            continue;
        }

        let raw =
            std::fs::read_to_string(&manifest_path).map_err(|e| PluginError::ManifestRead {
                path: manifest_path.display().to_string(),
                source: e,
            })?;

        let manifest: PluginManifest =
            serde_json::from_str(&raw).map_err(|e| PluginError::InvalidManifest {
                path: manifest_path.display().to_string(),
                message: e.to_string(),
            })?;

        tracing::debug!(
            name = %manifest.name,
            dir = %plugin_dir.display(),
            "Discovered plugin"
        );

        plugins.insert(
            manifest.name.clone(),
            PluginEntry {
                manifest,
                plugin_dir,
            },
        );
    }

    Ok(())
}

/// Errors that can occur while loading or invoking a plugin.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// A `plugin.json` manifest could not be read.
    #[error("Failed to read plugin manifest at '{path}': {source}")]
    ManifestRead {
        /// Path to the manifest file.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A `plugin.json` manifest is not valid JSON or has missing fields.
    #[error("Invalid plugin manifest at '{path}': {message}")]
    InvalidManifest {
        /// Path to the manifest file.
        path: String,
        /// Description of the parse/validation error.
        message: String,
    },

    /// The plugin directory could not be scanned.
    #[error("Failed to scan plugin directory '{path}': {source}")]
    DirectoryScan {
        /// Path to the directory.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The plugin manifest declares the `node` role but `node` is absent.
    #[error("Plugin '{name}' declares role 'node' but has no 'node' config")]
    MissingNodeConfig {
        /// Plugin name.
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn write_plugin(base: &Path, name: &str, json: &str) {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("plugin.json")).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }

    fn governor_json() -> &'static str {
        r#"{
            "name": "round-governor",
            "version": "1.0.0",
            "description": "Governs rounds.",
            "runtime": "process",
            "command": ["python3", "governor.py"],
            "roles": ["node"],
            "node": {
                "inputs":  [{"port": "in",       "kind": "Hypotheses"}],
                "outputs": [
                    {"port": "continue", "kind": "Hypotheses"},
                    {"port": "halt",     "kind": "Control"}
                ]
            }
        }"#
    }

    #[test]
    fn test_discover_graph_local() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        write_plugin(&plugins_dir, "round-governor", governor_json());

        let registry = PluginRegistry::discover(tmp.path()).unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.get("round-governor").is_some());
    }

    #[test]
    fn test_discover_empty() {
        let tmp = TempDir::new().unwrap();
        let registry = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn test_local_overrides_global() {
        // Write global plugin
        let global_tmp = TempDir::new().unwrap();
        write_plugin(
            global_tmp.path(),
            "round-governor",
            r#"{
                "name": "round-governor",
                "version": "0.1.0",
                "description": "Old.",
                "runtime": "process",
                "command": ["python3", "old.py"],
                "roles": ["node"],
                "node": {
                    "inputs":  [{"port": "in", "kind": "Hypotheses"}],
                    "outputs": [{"port": "continue", "kind": "Control"}]
                }
            }"#,
        );

        let graph_tmp = TempDir::new().unwrap();
        let plugins_dir = graph_tmp.path().join("plugins");
        write_plugin(&plugins_dir, "round-governor", governor_json());

        // Manually discover both dirs in order (global first, local second)
        let mut plugins = HashMap::new();
        discover_in_dir(global_tmp.path(), &mut plugins).unwrap();
        discover_in_dir(&plugins_dir, &mut plugins).unwrap();

        let entry = plugins.get("round-governor").unwrap();
        assert_eq!(entry.manifest.version, "1.0.0"); // local wins
    }
}
