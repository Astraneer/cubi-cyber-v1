#![cfg(not(target_arch = "wasm32"))]
pub use render::gpu_time;
pub use render::terrain_gpu;
#[path = "../src/taa.rs"]
#[allow(dead_code)]
mod taa;
