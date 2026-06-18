//! # Eureka Engine
//!
//! Wiring: build the node registry, load + validate a `GraphSpec`,
//! create the scheduler, and run a session.
//!
//! The orchestration *policy* lives in the graph data; the engine only
//! provides mechanism.

pub mod error;
pub mod registry;
pub mod run;
pub mod session;

pub use error::*;
pub use registry::*;
pub use run::*;
pub use session::*;
