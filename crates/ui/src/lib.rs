//! Shared Rust native/browser interface, retained pixel rendering and typed intents.
#![forbid(unsafe_code)]
pub mod app;
pub mod atlas;
pub mod bindings;
#[cfg(all(target_arch = "wasm32", debug_assertions))]
mod browser_probe;
pub mod font;
pub mod gpu;
pub mod layer;
pub mod paint;
pub mod profiler;
pub mod settings;
