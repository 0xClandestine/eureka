//! # Eureka Graph
//!
//! The framework: typed nodes, ports, artifacts, `GraphSpec` loading,
//! build-time validation, and the event-driven scheduler.
//!
//! This is the heart of Eureka's "topology is data" design.

pub mod artifact;
pub mod control;
pub mod edge;
pub mod node;
pub mod port;
pub mod process;
pub mod scheduler;
pub mod spec;
pub mod validate;

pub use artifact::*;
pub use control::*;
pub use edge::*;
pub use node::*;
pub use port::*;
pub use process::*;
pub use scheduler::*;
pub use spec::*;
pub use validate::*;
