//! Shared client-side world/cache owner and bounded background generation/meshing.
//! Platform shells only transport immutable tasks, upload finished meshes, and present.
#![forbid(unsafe_code)]
pub mod audio_bridge;
pub mod benchmark;
pub mod block_models;
pub mod capture;
pub mod demo_interaction;
pub mod edit_shell;
pub mod entities;
pub mod lighting;
pub mod lodcache;
pub mod network;
mod overrides;
pub mod presentation;
mod rebase;
pub mod remote;
mod scene;
pub mod self_model;
mod task;
pub mod ui_bridge;
pub mod view;
pub use overrides::{OverrideLimits, OverrideStats};
pub use scene::*;
#[cfg(not(target_arch = "wasm32"))]
pub use task::native_pool;
pub use task::{
    CompletedMesh, INPUT_PAGES, MAX_MESH_RESULT_BYTES, MESH_CHANNELS, MESH_RETAINED_BYTES,
    MeshTask, SceneOutput, SceneTask, WireError, execute_task, material,
};
