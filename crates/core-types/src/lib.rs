//! Shared wire-independent identifiers and page coordinates.
//!
//! Block IDs are deliberately not assigned here: the donor registry defines them.

/// A page edge in blocks; pages, rather than regions, are change units.
pub const PAGE_EDGE: usize = 16;
/// Number of cells in one page.
pub const PAGE_VOLUME: usize = PAGE_EDGE * PAGE_EDGE * PAGE_EDGE;

/// Stable identifier of a world space. Space zero is the ground world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpaceId(pub u32);

/// A monotonic simulation tick; the authoritative simulation runs at 20 Hz.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tick(pub u64);

/// Monotonic revision of a page or derived cache input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64);

/// Stable transaction identifier, distinct from ticks and revisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxId(pub u64);

/// Page coordinates have no fixed world-height restriction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId {
    pub space: SpaceId,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Block position within a world space, including negative coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockPos {
    pub space: SpaceId,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    /// Euclidean division keeps local coordinates in 0..16 on both sides of zero.
    pub const fn page(self) -> PageId {
        PageId {
            space: self.space,
            x: self.x.div_euclid(PAGE_EDGE as i32),
            y: self.y.div_euclid(PAGE_EDGE as i32),
            z: self.z.div_euclid(PAGE_EDGE as i32),
        }
    }

    /// X is the fastest-varying axis, followed by Z, then Y.
    pub const fn local_index(self) -> LocalIdx {
        let x = self.x.rem_euclid(PAGE_EDGE as i32) as u16;
        let y = self.y.rem_euclid(PAGE_EDGE as i32) as u16;
        let z = self.z.rem_euclid(PAGE_EDGE as i32) as u16;
        LocalIdx(x + z * PAGE_EDGE as u16 + y * (PAGE_EDGE * PAGE_EDGE) as u16)
    }
}

/// A validated index into a 16³ page. Construction cannot address another page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalIdx(u16);

impl LocalIdx {
    pub const fn new(index: u16) -> Option<Self> {
        if index < PAGE_VOLUME as u16 {
            Some(Self(index))
        } else {
            None
        }
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    pub const fn xyz(self) -> (u8, u8, u8) {
        (
            (self.0 % PAGE_EDGE as u16) as u8,
            (self.0 / (PAGE_EDGE * PAGE_EDGE) as u16) as u8,
            ((self.0 / PAGE_EDGE as u16) % PAGE_EDGE as u16) as u8,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_boundaries_keep_local_coordinates_inside_the_page() {
        for x in [-33, -32, -17, -16, -1, 0, 15, 16, 31, 32] {
            for y in [-17, -16, -1, 0, 15, 16] {
                for z in [-17, -1, 0, 16] {
                    let block = BlockPos {
                        space: SpaceId(7),
                        x,
                        y,
                        z,
                    };
                    let page = block.page();
                    let (lx, ly, lz) = block.local_index().xyz();
                    assert_eq!(page.space, SpaceId(7));
                    assert_eq!(page.x * 16 + i32::from(lx), x);
                    assert_eq!(page.y * 16 + i32::from(ly), y);
                    assert_eq!(page.z * 16 + i32::from(lz), z);
                }
            }
        }
    }

    #[test]
    fn all_page_cells_have_distinct_valid_indices() {
        let mut visited = [false; PAGE_VOLUME];
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let block = BlockPos {
                        space: SpaceId(0),
                        x,
                        y,
                        z,
                    };
                    let index = block.local_index();
                    assert_eq!(index.xyz(), (x as u8, y as u8, z as u8));
                    assert!(!visited[index.get() as usize]);
                    visited[index.get() as usize] = true;
                }
            }
        }
        assert!(visited.iter().all(|value| *value));
        assert_eq!(LocalIdx::new(4096), None);
        assert_eq!(LocalIdx::new(u16::MAX), None);
    }
}
