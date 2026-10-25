//! Bounded work queues and executors. World ownership stays on the caller thread.
//!
//! A worker receives an immutable snapshot payload. Its result is useful only
//! after [`Scheduler::finish`] checks the captured inputs again. Declared memory
//! includes a job's snapshot and expected output until that completion is handled.
//! Expired events are returned to their owner; they are never silently discarded.
#![forbid(unsafe_code)]

mod scheduler;
pub use scheduler::*;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
