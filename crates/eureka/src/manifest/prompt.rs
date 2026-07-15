use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A prompt file path, resolved at load time.
///
/// Serializes as a string path (e.g. `"prompts/generation.md"`) but
/// stores the resolved absolute path after loading.
#[derive(Debug, Clone)]
pub struct PromptPath {
    /// The original string from the YAML (e.g. `"prompts/generation.md"`).
    pub(super) raw: String,
    /// The absolute path resolved at load time.
    pub(super) absolute: PathBuf,
}

impl PromptPath {
    /// Get the resolved absolute path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.absolute
    }

    /// Get the original string as written in the YAML.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Resolve this path against a base directory.
    pub(super) fn resolve(&mut self, base: &Path) {
        let p = Path::new(&self.raw);
        if p.is_relative() {
            self.absolute = base.join(&self.raw);
        } else {
            self.absolute = p.to_path_buf();
        }
    }

    /// Read the prompt file content.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be read.
    pub fn read(&self) -> std::io::Result<String> {
        std::fs::read_to_string(&self.absolute)
    }
}

impl Serialize for PromptPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PromptPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self { absolute: PathBuf::from(&raw), raw })
    }
}
