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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_sets_raw_and_absolute_to_same_for_relative() {
        let path: PromptPath = serde_yaml::from_str("prompts/generation.md").unwrap();
        assert_eq!(path.as_str(), "prompts/generation.md");
        assert!(path.path().ends_with("prompts/generation.md"), "path was {:?}", path.path());
    }

    #[test]
    fn resolve_updates_absolute_path() {
        let base = Path::new("/home/user/mygraph");
        let mut path: PromptPath = serde_yaml::from_str("prompts/test.md").unwrap();
        path.resolve(base);
        assert_eq!(path.as_str(), "prompts/test.md");
        assert_eq!(path.path(), Path::new("/home/user/mygraph/prompts/test.md"));
    }

    #[test]
    fn resolve_leaves_absolute_path_unchanged() {
        let mut path: PromptPath = serde_yaml::from_str("/absolute/prompts/test.md").unwrap();
        let base = Path::new("/home/user/mygraph");
        path.resolve(base);
        assert_eq!(path.path(), Path::new("/absolute/prompts/test.md"));
    }

    #[test]
    fn serialize_writes_raw_string() {
        let path: PromptPath = serde_yaml::from_str("prompts/test.md").unwrap();
        let yaml = serde_yaml::to_string(&path).unwrap();
        assert!(yaml.contains("prompts/test.md"));
    }

    #[test]
    fn read_nonexistent_file_returns_err() {
        let path: PromptPath = serde_yaml::from_str("/nonexistent/path/xyzzy.md").unwrap();
        let result = path.read();
        assert!(result.is_err());
    }

    #[test]
    fn prompt_path_serialize_round_trip() {
        let original: PromptPath = serde_yaml::from_str("prompts/foo.md").unwrap();
        let json = serde_json::to_string(&original).unwrap();
        let restored: PromptPath = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.as_str(), original.as_str());
    }

    #[test]
    fn empty_prompt_path_deserializes() {
        let path: PromptPath = serde_json::from_str("\"\"").unwrap();
        assert_eq!(path.as_str(), "");
    }
}
