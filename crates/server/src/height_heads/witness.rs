//! Public-only Height identity. Streaming avoids another wire/body allocation.
use net::height::{HeightTile, Replacement};
pub(super) fn body(tile: &HeightTile) -> ([u8; 32], u64) {
    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0;
    let mut put = |data: &[u8]| {
        hasher.update(data);
        bytes += data.len() as u64;
    };
    put(b"public-height-body-v1");
    put(&tile.key.space.to_le_bytes());
    put(&[tile.key.level]);
    put(&tile.key.x.to_le_bytes());
    put(&tile.key.z.to_le_bytes());
    put(&[u8::from(tile.error.is_some())]);
    if let Some(error) = tile.error {
        put(&error.max_error_q1024.to_le_bytes());
        put(&error.min_height.to_le_bytes());
        put(&error.max_height.to_le_bytes());
    }
    for height in tile.heights {
        put(&height.to_le_bytes());
    }
    put(&tile.known);
    for material in tile.atlas {
        put(&material.to_le_bytes());
    }
    for tint in tile.tint {
        put(&tint.to_le_bytes());
    }
    put(&tile.protected);
    match &tile.replacement {
        Replacement::None => put(&[0]),
        Replacement::Refine => put(&[1]),
        Replacement::Column {
            min_y,
            max_y,
            geometry,
        } => {
            put(&[2]);
            put(&min_y.to_le_bytes());
            put(&max_y.to_le_bytes());
            put(&(geometry.len() as u32).to_le_bytes());
            for stamp in geometry {
                put(&stamp.page.space.0.to_le_bytes());
                put(&stamp.page.x.to_le_bytes());
                put(&stamp.page.y.to_le_bytes());
                put(&stamp.page.z.to_le_bytes());
                put(&stamp.source.0.to_le_bytes());
                put(&stamp.view.to_le_bytes());
            }
        }
    }
    (*hasher.finalize().as_bytes(), bytes)
}
