//! Cryptographic witnesses cover deterministic public bodies only. They let a
//! lightweight leased head survive body LRU eviction without consulting a
//! private source version as a proxy for public content equality.
use crate::lod::{PublicGeometryData, PublicSurfaceKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentWitness {
    pub geometry: [u8; 32],
    pub water: [u8; 32],
}
pub(super) fn build(
    geometry: Option<&PublicGeometryData>,
    water: &[u8; 5832],
    tints: &[u16; 256],
) -> (ContentWitness, u64) {
    let mut h = blake3::Hasher::new_derive_key("cubi public geometry content witness v1");
    let mut bytes = 1;
    h.update(&[u8::from(geometry.is_some())]);
    if let Some(g) = geometry {
        h.update(&g.known);
        h.update(&g.occupied);
        h.update(&(g.palette.len() as u32).to_le_bytes());
        for material in &g.palette {
            h.update(&material.atlas_layer.to_le_bytes());
            h.update(&material.tint.to_le_bytes());
            h.update(&[match material.kind {
                PublicSurfaceKind::Opaque => 0,
                PublicSurfaceKind::Cutout => 1,
                PublicSurfaceKind::Water => 2,
            }]);
        }
        h.update(&[g.index_bits]);
        h.update(&(g.indices.len() as u32).to_le_bytes());
        h.update(&g.indices);
        bytes += 1024 + 4 + g.palette.len() as u64 * 7 + 1 + 4 + g.indices.len() as u64;
    }
    let geometry = *h.finalize().as_bytes();
    let mut h = blake3::Hasher::new_derive_key("cubi public water content witness v1");
    h.update(water);
    for tint in tints {
        h.update(&tint.to_le_bytes());
    }
    bytes += 5832 + 512;
    (
        ContentWitness {
            geometry,
            water: *h.finalize().as_bytes(),
        },
        bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn witness_ignores_work_metadata_and_separates_geometry_from_water() {
        let mut geometry = PublicGeometryData {
            known: [255; 512],
            occupied: [0; 512],
            palette: Vec::new(),
            index_bits: 0,
            indices: Vec::new(),
            scanned_cells: 4096,
        };
        let mut water = [1; 5832];
        let mut tints = [u16::MAX; 256];
        let initial = build(Some(&geometry), &water, &tints).0;
        geometry.scanned_cells = 0;
        assert_eq!(build(Some(&geometry), &water, &tints).0, initial);
        water[19] = 2;
        let changed = build(Some(&geometry), &water, &tints).0;
        assert_eq!(changed.geometry, initial.geometry);
        assert_ne!(changed.water, initial.water);
        tints[0] = 0;
        assert_ne!(
            build(Some(&geometry), &water, &tints).0.water,
            changed.water
        );
        geometry.known[0] = 0;
        assert_ne!(
            build(Some(&geometry), &water, &tints).0.geometry,
            initial.geometry
        );
    }
}
