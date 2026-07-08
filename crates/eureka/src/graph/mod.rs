//! # Graph
//!
//! Pure topology primitives: typed nodes, ports, artifacts, edges, graph spec,
//! and build-time validator. This module has no I/O, no filesystem, no LLM
//! calls — just data structures and the scheduler-facing traits.

pub mod artifact;
pub mod control;
pub mod edge;
pub mod node;
pub mod port;
pub mod spec;
pub mod validate;

pub use artifact::{Artifact, ArtifactKind};
pub use edge::Edge;
pub use node::{BoxedNode, Emit, Node, NodeCtx, NodeError, NodeUsage, PortMsg};
pub use port::{PortDef, PortDirection, PortId, PortSpec, PortSpecEntry};
pub use spec::{GraphError, GraphNodeSpec, GraphSpec};
pub use validate::{parse_port_ref, validate_graph, PortRegistry, ValidationResult};
