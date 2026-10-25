//! Positional audio and local-only resource imports (M12).
#![forbid(unsafe_code)]
#[cfg(target_arch = "wasm32")]
pub mod browser;
pub mod catalog;
pub mod echo;
pub mod events;
#[cfg(not(target_arch = "wasm32"))]
pub mod local_files;
pub mod mixer;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
pub mod resources;
pub mod scene;
pub use mixer::{
    AudioError, Category, ClipId, EmitterId, Listener, MixWork, Mixer, Pcm, VoiceHandle,
    VoiceRequest,
};
