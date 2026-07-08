//! Scheduler — walks the graph, activates nodes when inputs arrive,
//! enforces budget and cycles, and manages the run lifecycle.

pub mod error;
pub mod event;
pub mod scheduler;

pub use error::SchedulerError;
pub use event::{SchedulerEvent, SchedulerSignal};
pub use scheduler::Scheduler;
