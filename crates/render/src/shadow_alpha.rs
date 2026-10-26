//! Exact binary directional-shadow coverage facts of the currently shared atlas.
//! Y bit0 is tint; these bits do not touch emission/fallback words Z/W or TAA IDs.
use crate::terrain_gpu::AtlasImage;
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};
pub const PASS_ALL: u32 = 1 << 1;
pub const FAIL_ALL: u32 = 1 << 2;
pub const FLAGS: u32 = PASS_ALL | FAIL_ALL;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidAtlas;
pub fn classify_mip0(atlas: &AtlasImage) -> Result<Box<[u32]>, InvalidAtlas> {
    if atlas.tile_size == 0 || atlas.layers == 0 {
        return Err(InvalidAtlas);
    }
    let mip = atlas.mips.first().ok_or(InvalidAtlas)?;
    let pixels = (atlas.tile_size as usize)
        .checked_mul(atlas.tile_size as usize)
        .ok_or(InvalidAtlas)?;
    let layer_bytes = pixels.checked_mul(4).ok_or(InvalidAtlas)?;
    let expected = layer_bytes
        .checked_mul(atlas.layers as usize)
        .ok_or(InvalidAtlas)?;
    if mip.size != atlas.tile_size || mip.rgba.len() != expected {
        return Err(InvalidAtlas);
    }
    Ok(mip
        .rgba
        .chunks_exact(layer_bytes)
        .map(|layer| {
            let mut minimum = 255u8;
            let mut maximum = 0u8;
            for pixel in layer.chunks_exact(4) {
                minimum = minimum.min(pixel[3]);
                maximum = maximum.max(pixel[3]);
            }
            if minimum >= 128 {
                PASS_ALL
            } else if maximum < 128 {
                FAIL_ALL
            } else {
                0
            }
        })
        .collect::<Vec<_>>()
        .into_boxed_slice())
}
pub fn merge(mut material: [u32; 4], classification: &[u32], valid: bool) -> [u32; 4] {
    material[1] &= !FLAGS;
    if valid {
        material[1] |= classification
            .get(material[0] as usize)
            .copied()
            .unwrap_or(0)
            & FLAGS;
    }
    material
}
/// The actual shared color array can be updated in place. Such an update first
/// validates/reserves its revision, queues all uploads and clears cached facts,
/// then publishes the revision. Metadata publication shares this update gate.
#[derive(Default)]
pub struct AtlasState {
    revision: AtomicU64,
    gate: Mutex<()>,
}
impl AtlasState {
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, ()>, ()> {
        self.gate.lock().map_err(|_| ())
    }
    pub(crate) fn next_revision(&self) -> Option<u64> {
        self.revision().checked_add(1)
    }
    pub(crate) fn publish_uploaded(&self, revision: u64) {
        self.revision.store(revision, Ordering::Release);
    }
}
/// Exact same resources as the original alpha-only shadow shader. Guard both
/// lookups BEFORE specialization, so malformed IDs/layers never become opaque.
pub const WGSL: &str = r#"
fn shadow_alpha_pass(material_id:u32,uv:vec2<f32>)->bool{
 if material_id>=arrayLength(&materials)||!all(abs(uv)<vec2(1e30)){return false;}
 let material=materials[material_id];if material.x>=textureNumLayers(colors){return false;}
 if(material.y&4u)!=0u{return false;}if(material.y&2u)!=0u{return true;}
 return textureSampleLevel(colors,texels,uv,i32(material.x),0.).a>=.5;
}
"#;
