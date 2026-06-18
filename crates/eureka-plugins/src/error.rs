//! Error types for the plugin system.

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
