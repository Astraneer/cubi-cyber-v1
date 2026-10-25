//! Public geometry for selected protected LOD leaves. No raw block IDs, block
//! states, inventories, generation entropy or private ore data are represented.
use core_types::{LocalIdx, PageId, Version};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum PublicSurfaceKind {
    Opaque = 0,
    Cutout = 1,
    Water = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicSurface {
    pub atlas_layer: u16,
    pub tint: u32,
    pub kind: PublicSurfaceKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicGeometry {
    pub page: PageId,
    pub source_version: Version,
    pub view_version: u64,
    pub known: [u8; 512],
    pub occupied: [u8; 512],
    pub palette: Vec<PublicSurface>,
    pub index_bits: u8,
    pub indices: Vec<u8>,
}
impl PublicGeometry {
    pub fn is_known(&self, index: LocalIdx) -> bool {
        let i = usize::from(index.get());
        self.known[i / 8] & (1 << (i % 8)) != 0
    }
    pub fn palette_index(&self, index: LocalIdx) -> Option<u16> {
        let i = usize::from(index.get());
        if self.occupied[i / 8] & (1 << (i % 8)) == 0 || self.index_bits > 12 {
            return None;
        }
        let value = self.unpack(i)?;
        self.palette.get(usize::from(value))?;
        Some(value)
    }
    fn unpack(&self, index: usize) -> Option<u16> {
        let mut value = 0;
        let start = index * usize::from(self.index_bits);
        for bit in 0..usize::from(self.index_bits) {
            let at = start + bit;
            value |= u16::from((self.indices.get(at / 8)? >> (at % 8)) & 1) << bit;
        }
        Some(value)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.source_version.0 == 0 || self.view_version == 0 || self.palette.len() > 4096 {
            return Err("geometry version/palette bound");
        }
        let expected = if self.palette.len() <= 1 {
            0
        } else {
            (usize::BITS - (self.palette.len() - 1).leading_zeros()) as u8
        };
        if self.index_bits != expected || self.indices.len() != 512 * usize::from(expected) {
            return Err("geometry packed index bounds");
        }
        let mut unique = std::collections::BTreeSet::new();
        for surface in &self.palette {
            if surface.atlas_layer >= 4096 || !unique.insert(*surface) {
                return Err("invalid geometry palette");
            }
        }
        for index in 0..4096 {
            let known = self.known[index / 8] & (1 << (index % 8)) != 0;
            let occupied = self.occupied[index / 8] & (1 << (index % 8)) != 0;
            let palette = self.unpack(index).ok_or("truncated geometry indices")?;
            if occupied && (!known || usize::from(palette) >= self.palette.len())
                || !occupied && palette != 0
            {
                return Err("invalid geometry voxel");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::SpaceId;
    fn empty() -> PublicGeometry {
        PublicGeometry {
            page: PageId {
                space: SpaceId(0),
                x: 0,
                y: 0,
                z: 0,
            },
            source_version: Version(1),
            view_version: 1,
            known: [255; 512],
            occupied: [0; 512],
            palette: Vec::new(),
            index_bits: 0,
            indices: Vec::new(),
        }
    }
    #[test]
    fn known_air_unknown_and_occupied_are_distinct_and_indices_are_canonical() {
        let mut geometry = empty();
        geometry.validate().unwrap();
        let index = LocalIdx::new(0).unwrap();
        assert!(geometry.is_known(index));
        assert_eq!(geometry.palette_index(index), None);
        geometry.known[0] = 0;
        geometry.validate().unwrap();
        assert!(!geometry.is_known(index));
        geometry.occupied[0] = 1;
        assert!(geometry.validate().is_err());
        geometry.known[0] = 1;
        geometry.palette.push(PublicSurface {
            atlas_layer: 3,
            tint: u32::MAX,
            kind: PublicSurfaceKind::Opaque,
        });
        geometry.validate().unwrap();
        assert_eq!(geometry.palette_index(index), Some(0));
        geometry.palette.push(PublicSurface {
            atlas_layer: 4,
            tint: u32::MAX,
            kind: PublicSurfaceKind::Water,
        });
        geometry.index_bits = 1;
        geometry.indices = vec![0; 512];
        geometry.indices[0] = 1;
        geometry.validate().unwrap();
        assert_eq!(geometry.palette_index(index), Some(1));
        geometry.indices[0] |= 2;
        assert!(
            geometry.validate().is_err(),
            "air cannot carry a hidden palette index"
        );
    }
    #[test]
    fn malformed_lengths_versions_palette_and_index_ranges_are_rejected() {
        let mut geometry = empty();
        geometry.view_version = 0;
        assert!(geometry.validate().is_err());
        geometry.view_version = 1;
        geometry.palette = vec![
            PublicSurface {
                atlas_layer: 3,
                tint: u32::MAX,
                kind: PublicSurfaceKind::Opaque
            };
            2
        ];
        geometry.index_bits = 1;
        geometry.indices = vec![0; 512];
        assert!(geometry.validate().is_err());
        geometry.palette[1].atlas_layer = 4;
        geometry.indices.pop();
        assert!(geometry.validate().is_err());
        geometry.indices.push(0);
        geometry.validate().unwrap();
        geometry.palette.push(PublicSurface {
            atlas_layer: 5,
            tint: u32::MAX,
            kind: PublicSurfaceKind::Cutout,
        });
        geometry.index_bits = 2;
        geometry.indices = vec![0; 1024];
        geometry.occupied[0] = 1;
        geometry.indices[0] = 3;
        assert!(geometry.validate().is_err());
        geometry.indices[0] = 0;
        geometry.palette[0].atlas_layer = 4096;
        assert!(geometry.validate().is_err());
    }

    #[test]
    fn geometry_frames_roundtrip_masks_palette_without_raw_cells() {
        let mut geometry = empty();
        geometry.occupied = [255; 512];
        geometry.palette = vec![
            PublicSurface {
                atlas_layer: 3,
                tint: u32::MAX,
                kind: PublicSurfaceKind::Opaque,
            },
            PublicSurface {
                atlas_layer: 17,
                tint: 0xff5fa34b,
                kind: PublicSurfaceKind::Cutout,
            },
        ];
        geometry.index_bits = 1;
        geometry.indices = vec![0x55; 512];
        let message = crate::ServerMessage::Geometry(Box::new(geometry));
        let bytes = crate::encode_server(&message).unwrap();
        assert!(bytes.len() < 1700);
        assert_eq!(crate::decode_server(&bytes).unwrap(), message);
        let request = crate::ClientMessage::RequestGeometry {
            page: PageId {
                space: core_types::SpaceId(0),
                x: 1024,
                y: 0,
                z: 0,
            },
            known: Some(Version(5)),
        };
        assert_eq!(
            crate::decode_client(&crate::encode_client(&request).unwrap()).unwrap(),
            request
        );
        let forget = crate::ClientMessage::ForgetGeometry(PageId::default());
        assert_eq!(
            crate::decode_client(&crate::encode_client(&forget).unwrap()).unwrap(),
            forget
        );
    }
}
