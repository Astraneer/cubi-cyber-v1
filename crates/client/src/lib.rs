//! Shared client-side world/cache owner and bounded background generation/meshing.
//! Platform shells only transport immutable tasks, upload finished meshes, and present.
#![forbid(unsafe_code)]
pub mod audio_bridge;
pub mod benchmark;
pub mod block_models;
pub mod capture;
pub mod celestial_clock;
pub mod demo_interaction;
pub mod edit_shell;
pub mod entities;
pub mod far_water_source;
pub mod far_water_worker;
pub mod gi_source;
pub mod height_cache;
pub mod height_neighbors;
pub mod lighting;
pub mod local_materials;
pub mod lodcache;
pub mod near_profile;
pub mod network;
mod overrides;
pub mod packet_source;
pub mod presentation;
pub mod props_source;
pub mod quality;
mod rebase;
pub mod remote;
pub mod remote_humans;
mod scene;
pub mod self_model;
mod task;
pub mod ui_bridge;
pub mod ui_item_facts;
pub mod ui_window;
pub mod view;
pub mod water;
pub mod water_source;
pub use overrides::{OverrideLimits, OverrideStats};
pub use scene::*;
pub use sim::replay;
#[cfg(not(target_arch = "wasm32"))]
pub use task::native_pool;
pub use task::{
    CompletedMesh, INPUT_PAGES, MAX_MESH_RESULT_BYTES, MESH_CHANNELS, MESH_RETAINED_BYTES,
    MeshTask, SceneOutput, SceneTask, WireError, execute_task, material,
};
