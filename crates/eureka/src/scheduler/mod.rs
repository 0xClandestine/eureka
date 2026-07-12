//! Scheduler — walks the graph, activates nodes when inputs arrive,
//! enforces budget and cycles, and manages the run lifecycle.

/// Scheduler error types.
pub mod error;
/// Scheduler event and signal types.
pub mod event;
/// Event-driven graph executor.
#[allow(clippy::module_inception)]
pub mod scheduler;

pub use error::SchedulerError;
pub use event::{SchedulerEvent, SchedulerSignal};
pub use scheduler::Scheduler;
