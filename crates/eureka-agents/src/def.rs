//! Agent definition — loaded from a `.md` preamble + `.json` schema pair.
//!
//! Agents may declare shell tools in their `.json` file. Each tool is a command
//! the LLM can call during its loop; the framework spawns the subprocess and
//! returns stdout as the tool result. The `submit` tool is always implicit.

use std::path::Path;

use eureka_graph::artifact::ArtifactKind;
use eureka_graph::port::{PortDirection, PortSpec, PortSpecEntry};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A single input or output port declared by an agent definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPort {
    /// The artifact kind this port produces or consumes.
    pub kind: ArtifactKind,
    /// The port name used in graph edges.
    pub port: String,
}

/// A shell tool the agent may call during its loop.
///
/// When the LLM calls this tool, the framework runs `command` as a subprocess,
/// writes the args JSON to stdin, and returns stdout to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    /// Name the LLM uses to call this tool. Must not be `"submit"`.
    pub name: String,
    /// Description shown to the LLM in the tool schema.
    pub description: String,
    /// Subprocess argv. `command[0]` is the binary. Tokens of the form
    /// `{{arg_name}}` are substituted from the LLM's call arguments.
    pub command: Vec<String>,
    /// JSON Schema for the arguments the LLM must supply.
    pub args_schema: serde_json::Value,
    /// Maximum wall-clock seconds to wait for the subprocess. Default: 30.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,
}

const fn default_timeout_secs() -> u32 {
    30
}

/// Per-agent LLM configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Sampling temperature (higher = more diverse output).
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// Maximum number of loop iterations before the agent is forcibly halted.
    /// Each iteration is one LLM call. The agent should call `submit` before
    /// this limit is reached.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
}

const fn default_temperature() -> f64 {
    0.7
}

const fn default_max_iterations() -> u32 {
    10
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            temperature: default_temperature(),
            max_iterations: default_max_iterations(),
        }
    }
}

/// A fully loaded agent definition.
///
/// Produced by reading `<name>.md` (preamble) and `<name>.json` (schema + config)
/// from the agents directory at startup.
#[derive(Debug, Clone)]
pub struct AgentDef {
    /// The agent's name (matches the file stem, e.g. `"generation"`).
    pub name: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// The system preamble loaded from `<name>.md`.
    pub preamble: String,
    /// One or more input port specifications.
    pub inputs: Vec<AgentPort>,
    /// One or more output port specifications.
    pub outputs: Vec<AgentPort>,
    /// LLM configuration for this agent.
    pub config: AgentConfig,
    /// JSON Schema the LLM must conform to (becomes the `submit` tool's parameter schema).
    pub output_schema: serde_json::Value,
    /// Shell tools available to the agent during its loop. May be empty.
    pub tools: Vec<ToolDef>,
}

impl AgentDef {
    /// Load an agent definition from a directory.
    ///
    /// Reads `<dir>/<name>.md` and `<dir>/<name>.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if either file cannot be read or the JSON is malformed.
    pub fn load(dir: &Path, name: &str) -> Result<Self, AgentLoadError> {
        let md_path = dir.join(format!("{name}.md"));
        let json_path = dir.join(format!("{name}.json"));

        let preamble = std::fs::read_to_string(&md_path).map_err(|e| AgentLoadError::Io {
            path: md_path.display().to_string(),
            source: e,
        })?;

        let json_str = std::fs::read_to_string(&json_path).map_err(|e| AgentLoadError::Io {
            path: json_path.display().to_string(),
            source: e,
        })?;

        let raw: RawAgentDef =
            serde_json::from_str(&json_str).map_err(|e| AgentLoadError::Parse {
                path: json_path.display().to_string(),
                source: e,
            })?;

        // Resolve inputs: prefer the plural `inputs` array; fall back to the
        // legacy singular `input` field for backward compatibility.
        let inputs = match (raw.inputs, raw.input) {
            (Some(v), _) if !v.is_empty() => v,
            (_, Some(port)) => vec![port],
            _ => {
                return Err(AgentLoadError::NoInputs {
                    name: name.to_string(),
                })
            }
        };

        if raw.outputs.is_empty() {
            return Err(AgentLoadError::NoOutputs {
                name: name.to_string(),
            });
        }

        Ok(Self {
            name: raw.name.unwrap_or_else(|| name.to_string()),
            description: raw.description,
            preamble,
            inputs,
            outputs: raw.outputs,
            config: raw.config.unwrap_or_default(),
            output_schema: raw.output_schema,
            tools: raw.tools,
        })
    }

    /// Scan a directory and load all agent definitions found there.
    ///
    /// Any `.json` file in `dir` that also has a matching `.md` file is
    /// treated as an agent definition.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read or any definition
    /// is malformed.
    pub fn load_all(dir: &Path) -> Result<Vec<Self>, AgentLoadError> {
        let mut defs = Vec::new();

        let entries = std::fs::read_dir(dir).map_err(|e| AgentLoadError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| AgentLoadError::Io {
                path: dir.display().to_string(),
                source: e,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let md_path = dir.join(format!("{stem}.md"));
            if !md_path.exists() {
                continue; // skip JSON files without a matching preamble
            }
            defs.push(Self::load(dir, &stem)?);
        }

        defs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(defs)
    }

    /// Build a `PortSpec` for this agent (used by the node registry).
    #[must_use]
    pub fn to_port_spec(&self) -> PortSpec {
        let inputs = self
            .inputs
            .iter()
            .map(|i| PortSpecEntry {
                name: i.port.clone(),
                direction: PortDirection::Input,
                kind: i.kind.clone(),
                required: true,
            })
            .collect();

        let outputs = self
            .outputs
            .iter()
            .map(|o| PortSpecEntry {
                name: o.port.clone(),
                direction: PortDirection::Output,
                kind: o.kind.clone(),
                required: false,
            })
            .collect();

        PortSpec::new(inputs, outputs)
    }
}

/// Raw (deserializable) form of an agent definition JSON file.
///
/// Supports both the legacy `"input": {...}` (singular) and the current
/// `"inputs": [...]` (array) formats.
#[derive(Debug, Deserialize)]
struct RawAgentDef {
    name: Option<String>,
    description: Option<String>,
    /// Legacy single-input format (backward compat).
    #[serde(default)]
    input: Option<AgentPort>,
    /// Current multi-input format.
    #[serde(default)]
    inputs: Option<Vec<AgentPort>>,
    outputs: Vec<AgentPort>,
    config: Option<AgentConfig>,
    output_schema: serde_json::Value,
    #[serde(default)]
    tools: Vec<ToolDef>,
}

/// Errors that can occur when loading an agent definition.
#[derive(Debug, Error)]
pub enum AgentLoadError {
    /// A file could not be read.
    #[error("Failed to read '{path}': {source}")]
    Io {
        /// The file path.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The JSON could not be parsed.
    #[error("Failed to parse '{path}': {source}")]
    Parse {
        /// The file path.
        path: String,
        /// The underlying parse error.
        #[source]
        source: serde_json::Error,
    },

    /// The agent definition declares no input ports.
    #[error("Agent '{name}' declares no input ports")]
    NoInputs {
        /// The agent name.
        name: String,
    },

    /// The agent definition declares no output ports.
    #[error("Agent '{name}' declares no output ports")]
    NoOutputs {
        /// The agent name.
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_agent(dir: &TempDir, name: &str, json: &str, md: &str) {
        let mut f = std::fs::File::create(dir.path().join(format!("{name}.json"))).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        let mut f = std::fs::File::create(dir.path().join(format!("{name}.md"))).unwrap();
        f.write_all(md.as_bytes()).unwrap();
    }

    #[test]
    fn test_load_agent_def_new_format() {
        let dir = TempDir::new().unwrap();
        write_agent(
            &dir,
            "generation",
            r#"{
                "inputs": [
                    { "kind": "Goal",     "port": "in"      },
                    { "kind": "Insights", "port": "context" }
                ],
                "outputs": [{ "kind": "Hypotheses", "port": "out" }],
                "config":  { "temperature": 0.9 },
                "output_schema": { "type": "object" }
            }"#,
            "You are a generation agent.",
        );

        let def = AgentDef::load(dir.path(), "generation").unwrap();
        assert_eq!(def.name, "generation");
        assert_eq!(def.inputs.len(), 2);
        assert_eq!(def.inputs[0].kind, "Goal");
        assert_eq!(def.inputs[1].kind, "Insights");
        assert_eq!(def.outputs.len(), 1);
        assert_eq!(def.outputs[0].kind, "Hypotheses");
        assert!((def.config.temperature - 0.9).abs() < f64::EPSILON);
        assert_eq!(def.preamble, "You are a generation agent.");
    }

    #[test]
    fn test_load_agent_def_legacy_format() {
        let dir = TempDir::new().unwrap();
        write_agent(
            &dir,
            "generation",
            r#"{
                "input":   { "kind": "Goal",       "port": "in"  },
                "outputs": [{ "kind": "Hypotheses", "port": "out" }],
                "config":  { "temperature": 0.9 },
                "output_schema": { "type": "object" }
            }"#,
            "You are a generation agent.",
        );

        let def = AgentDef::load(dir.path(), "generation").unwrap();
        assert_eq!(def.name, "generation");
        assert_eq!(def.inputs.len(), 1);
        assert_eq!(def.inputs[0].kind, "Goal");
    }

    #[test]
    fn test_load_all_skips_json_without_md() {
        let dir = TempDir::new().unwrap();
        write_agent(
            &dir,
            "generation",
            r#"{
                "inputs":  [{ "kind": "Goal",       "port": "in"  }],
                "outputs": [{ "kind": "Hypotheses", "port": "out" }],
                "output_schema": { "type": "object" }
            }"#,
            "Preamble.",
        );
        // A JSON-only file with no matching .md
        std::fs::write(dir.path().join("orphan.json"), "{}").unwrap();

        let defs = AgentDef::load_all(dir.path()).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "generation");
    }

    #[test]
    fn test_to_port_spec() {
        let dir = TempDir::new().unwrap();
        write_agent(
            &dir,
            "meta_review",
            r#"{
                "inputs":  [{ "kind": "Ranking",  "port": "in" }],
                "outputs": [
                    { "kind": "Insights", "port": "insights" },
                    { "kind": "Overview", "port": "overview" }
                ],
                "output_schema": { "type": "object" }
            }"#,
            "Preamble.",
        );
        let def = AgentDef::load(dir.path(), "meta_review").unwrap();
        let ports = def.to_port_spec();
        assert_eq!(ports.input_names(), vec!["in"]);
        assert_eq!(ports.output_names().len(), 2);
    }

    #[test]
    fn test_no_inputs_error() {
        let dir = TempDir::new().unwrap();
        write_agent(
            &dir,
            "bad",
            r#"{
                "outputs": [{ "kind": "Hypotheses", "port": "out" }],
                "output_schema": { "type": "object" }
            }"#,
            "Preamble.",
        );
        let err = AgentDef::load(dir.path(), "bad").unwrap_err();
        assert!(matches!(err, AgentLoadError::NoInputs { .. }));
    }
}
