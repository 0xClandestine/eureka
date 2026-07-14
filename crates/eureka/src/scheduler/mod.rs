//! Scheduler — walks the graph, activates nodes when inputs arrive,
//! enforces budget and cycles, and manages the run lifecycle.

/// Scheduler error types.
pub mod error;
/// Scheduler event and signal types.
pub mod event;
/// Event-driven graph executor.
pub mod executor;

pub use error::SchedulerError;
pub use event::{SchedulerEvent, SchedulerSignal};
pub use executor::Scheduler;
