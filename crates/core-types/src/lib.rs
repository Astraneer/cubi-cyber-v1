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

/// Stable block-entity identity. IDs are never inferred from memory addresses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentId(pub u64);

/// Wire-compatible block ID (low 16 bits) and state (high 16 bits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Cell(u32);

impl Cell {
    pub const AIR: Self = Self(0);

    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }
    pub const fn raw(self) -> u32 {
        self.0
    }
    pub const fn from_parts(id: u16, state: u16) -> Self {
        Self(id as u32 | ((state as u32) << 16))
    }
    pub const fn id(self) -> u16 {
        self.0 as u16
    }
    pub const fn state(self) -> u16 {
        (self.0 >> 16) as u16
    }
}

/// Observable inputs of derived systems, separate from cell storage layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChannelMask(pub u16);

impl ChannelMask {
    pub const NONE: Self = Self(0);
    pub const SHAPE: Self = Self(1 << 0);
    pub const COLLISION: Self = Self(1 << 1);
    pub const OPACITY: Self = Self(1 << 2);
    pub const EMISSION: Self = Self(1 << 3);
    pub const FLUID: Self = Self(1 << 4);
    pub const CONTENTS: Self = Self(1 << 5);
    pub const CONNECTIONS: Self = Self(1 << 6);
    pub const LIGHTING: Self = Self(1 << 7);
    pub const TRANSFORM: Self = Self(1 << 8);
    pub const EVENTS: Self = Self(1 << 9);
    pub const ALL: Self = Self((1 << 10) - 1);
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for ChannelMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
impl std::ops::BitOrAssign for ChannelMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
impl std::ops::BitAnd for ChannelMask {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Registry-derived properties. `shape` includes geometry/material changes, but
/// deliberately excludes emission-only state such as a lamp's light colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellProperties {
    pub shape: u32,
    pub occupied: bool,
    pub opaque: bool,
    pub collidable: bool,
    pub blocks_projectile: bool,
    pub fluid: bool,
    pub emission: [u8; 3],
    pub light_opacity: u8,
}

impl CellProperties {
    pub const AIR: Self = Self {
        shape: 0,
        occupied: false,
        opaque: false,
        collidable: false,
        blocks_projectile: false,
        fluid: false,
        emission: [0; 3],
        light_opacity: 0,
    };
    pub const SOLID: Self = Self {
        shape: 1,
        occupied: true,
        opaque: true,
        collidable: true,
        blocks_projectile: true,
        fluid: false,
        emission: [0; 3],
        light_opacity: 15,
    };
    pub const fn emitter(self) -> bool {
        self.emission[0] != 0 || self.emission[1] != 0 || self.emission[2] != 0
    }
    pub fn changed_channels(self, other: Self) -> ChannelMask {
        let mut result = ChannelMask::NONE;
        if self.shape != other.shape || self.occupied != other.occupied {
            result |= ChannelMask::SHAPE;
        }
        if self.collidable != other.collidable
            || self.blocks_projectile != other.blocks_projectile
            || (self.shape != other.shape
                && (self.collidable
                    || other.collidable
                    || self.blocks_projectile
                    || other.blocks_projectile))
        {
            result |= ChannelMask::COLLISION;
        }
        if self.opaque != other.opaque || self.light_opacity != other.light_opacity {
            result |= ChannelMask::OPACITY;
        }
        if self.emission != other.emission {
            result |= ChannelMask::EMISSION;
        }
        if self.fluid != other.fluid || (self.shape != other.shape && (self.fluid || other.fluid)) {
            result |= ChannelMask::FLUID;
        }
        result
    }
}

/// Region is a streaming/save grouping of 32×32 page columns, not a rebuild unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionId {
    pub space: SpaceId,
    pub x: i32,
    pub z: i32,
}

impl RegionId {
    pub const fn from_page(page: PageId) -> Self {
        Self {
            space: page.space,
            x: page.x.div_euclid(32),
            z: page.z.div_euclid(32),
        }
    }
}

/// Transform of a movable grid. Rotation is a unit quaternion [x,y,z,w].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceTransform {
    pub translation: [f64; 3],
    pub rotation: [f64; 4],
}

impl SpaceTransform {
    pub const IDENTITY: Self = Self {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
    };
    pub fn is_valid(self) -> bool {
        self.translation
            .into_iter()
            .chain(self.rotation)
            .all(f64::is_finite)
            && (self.rotation.into_iter().map(|v| v * v).sum::<f64>() - 1.0).abs() <= 1e-6
    }
}
impl Default for SpaceTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Keyed deterministic integer RNG. This is a simulation primitive, not a
/// cryptographic generator; subsystems must assign stable numeric domain IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeterministicRng {
    state: u64,
}
impl DeterministicRng {
    pub const fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }
    pub fn from_key(seed: u64, subsystem: u64, id: u64, event: u64) -> Self {
        let mut state = mix64(seed ^ 0xd1b54a32d192ed03);
        for key in [subsystem, id, event] {
            state = mix64(state.wrapping_add(key).wrapping_add(0x9e3779b97f4a7c15));
        }
        Self::from_seed(state)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        mix64(self.state)
    }
    /// Uniform 0..upper, using rejection instead of biased modulo reduction.
    pub fn below(&mut self, upper: u64) -> Option<u64> {
        if upper == 0 {
            return None;
        }
        let threshold = upper.wrapping_neg() % upper;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return Some(value % upper);
            }
        }
    }
}
const fn mix64(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

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
    fn wire_cell_layout_preserves_full_id_and_state_words() {
        for id in [0, 44, 300, u16::MAX] {
            for state in [0, 10, 0x8005, u16::MAX] {
                let cell = Cell::from_parts(id, state);
                assert_eq!(cell.id(), id);
                assert_eq!(cell.state(), state);
                assert_eq!(Cell::from_raw(cell.raw()), cell);
            }
        }
        assert_eq!(Cell::from_parts(u16::MAX, u16::MAX).raw(), u32::MAX);
    }

    #[test]
    fn geometry_changes_invalidate_physical_and_fluid_inputs() {
        let stone = CellProperties::SOLID;
        let turned_shape = CellProperties { shape: 17, ..stone };
        assert!(
            stone
                .changed_channels(turned_shape)
                .contains(ChannelMask::COLLISION)
        );
        let water = CellProperties {
            shape: 3,
            occupied: true,
            fluid: true,
            ..CellProperties::AIR
        };
        let flowed = CellProperties {
            shape: 0x10003,
            ..water
        };
        assert!(water.changed_channels(flowed).contains(ChannelMask::FLUID));
        let lamp = CellProperties {
            emission: [1, 0, 0],
            ..stone
        };
        let recoloured = CellProperties {
            emission: [0, 1, 0],
            ..lamp
        };
        assert_eq!(lamp.changed_channels(recoloured), ChannelMask::EMISSION);
    }

    #[test]
    fn deterministic_rng_has_frozen_sequence_and_separated_domains() {
        let mut rng = DeterministicRng::from_seed(0);
        assert_eq!(rng.next_u64(), 0xe220a8397b1dcdaf);
        assert_eq!(rng.next_u64(), 0x6e789e6aa1b965f4);
        assert_eq!(rng.next_u64(), 0x06c45d188009454f);
        let mut same = DeterministicRng::from_key(1, 2, 3, 4);
        let mut repeat = same;
        let mut other = DeterministicRng::from_key(1, 3, 2, 4);
        assert_eq!(same.next_u64(), repeat.next_u64());
        assert_ne!(same.next_u64(), other.next_u64());
        assert_eq!(rng.below(0), None);
        for upper in [1, 2, 7, u32::MAX as u64, u64::MAX] {
            for _ in 0..128 {
                assert!(rng.below(upper).unwrap() < upper);
            }
        }
    }

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
