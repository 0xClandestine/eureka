//! `ControlNodeDef` — lightweight definition for constructing a [`ControlNode`](super::node::ControlNode).

use std::path::PathBuf;

use crate::graph::port::PortDef;

/// Lightweight definition for constructing a [`ControlNode`](super::node::ControlNode).
///
/// Populated from the YAML manifest's `control[]` entries.
pub struct ControlNodeDef {
    /// Display name (for diagnostics).
    pub name: String,
    /// Working directory (absolute path to graph dir).
    pub work_dir: PathBuf,
    /// Subprocess argv.
    pub command: Vec<String>,
    /// Input port declarations.
    pub inputs: Vec<PortDef>,
    /// Output port declarations.
    pub outputs: Vec<PortDef>,
    /// Subprocess timeout in seconds.
    pub timeout_secs: u32,
}
