//! Already-public server height tiles. No generator or raw world cells enter the
//! renderer. Unknown samples and protected cells never become surface triangles.
use crate::lod::{ClipmapPlan, Edge, PlanError, clipmap_plan};
pub const GRID_EDGE: usize = 17;
pub const SAMPLES: usize = 289;
pub const CELLS: usize = 256;
pub const MAX_LEVEL: u8 = 10;
pub const WEST: u32 = 1;
pub const EAST: u32 = 2;
pub const NORTH: u32 = 4;
pub const SOUTH: u32 = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HeightTileKey {
    pub space: u32,
    pub level: u8,
    pub x: i32,
    pub z: i32,
}
impl HeightTileKey {
    pub fn step(self) -> Option<i64> {
        (self.level <= MAX_LEVEL).then(|| 1_i64 << self.level)
    }
    pub fn bounds(self) -> Option<[i64; 4]> {
        let span = self.step()?.checked_mul(16)?;
        let x = i64::from(self.x).checked_mul(span)?;
        let z = i64::from(self.z).checked_mul(span)?;
        let b = [x, z, x.checked_add(span)?, z.checked_add(span)?];
        b.iter()
            .all(|v| (i64::from(i32::MIN)..=i64::from(i32::MAX) + 1).contains(v))
            .then_some(b)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeightTile {
    pub key: HeightTileKey,
    pub source: u64,
    pub heights: [i32; SAMPLES],
    pub known: [u8; 37],
    pub atlas: [u16; CELLS],
    pub tint: [u32; CELLS],
    pub protected: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightError {
    InvalidKey,
    InvalidSource,
    InvalidMask,
    InvalidMaterial,
    InvalidSeams,
    SourceMismatch,
    NotAdjacent,
    NotTwoToOne,
    UnknownBoundary,
}
impl std::fmt::Display for HeightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for HeightError {}
impl HeightTile {
    pub fn validate(&self, atlas_layers: u32) -> Result<(), HeightError> {
        self.key.bounds().ok_or(HeightError::InvalidKey)?;
        if self.source == 0 {
            return Err(HeightError::InvalidSource);
        }
        if self.known[36] & !1 != 0 {
            return Err(HeightError::InvalidMask);
        }
        if self
            .atlas
            .iter()
            .any(|layer| u32::from(*layer) >= atlas_layers)
        {
            return Err(HeightError::InvalidMaterial);
        }
        if self
            .heights
            .iter()
            .enumerate()
            .any(|(i, h)| !self.sample_known(i % 17, i / 17) && *h != 0)
        {
            return Err(HeightError::InvalidMask);
        }
        Ok(())
    }
    pub fn sample_known(&self, x: usize, z: usize) -> bool {
        if x > 16 || z > 16 {
            return false;
        }
        let i = x + 17 * z;
        self.known[i / 8] & (1 << (i % 8)) != 0
    }
    pub fn cell_protected(&self, x: usize, z: usize) -> bool {
        if x >= 16 || z >= 16 {
            return true;
        }
        let i = x + 16 * z;
        self.protected[i / 8] & (1 << (i % 8)) != 0
    }
    pub fn height(&self, x: usize, z: usize, seams: u32) -> Option<f64> {
        if seams & !15 != 0 || !self.sample_known(x, z) {
            return None;
        }
        let horizontal =
            ((z == 0 && seams & NORTH != 0) || (z == 16 && seams & SOUTH != 0)) && x % 2 == 1;
        let vertical =
            ((x == 0 && seams & WEST != 0) || (x == 16 && seams & EAST != 0)) && z % 2 == 1;
        if horizontal {
            if !self.sample_known(x - 1, z) || !self.sample_known(x + 1, z) {
                return None;
            }
            return Some(
                (f64::from(self.heights[x - 1 + 17 * z]) + f64::from(self.heights[x + 1 + 17 * z]))
                    * 0.5,
            );
        }
        if vertical {
            if !self.sample_known(x, z - 1) || !self.sample_known(x, z + 1) {
                return None;
            }
            return Some(
                (f64::from(self.heights[x + 17 * (z - 1)])
                    + f64::from(self.heights[x + 17 * (z + 1)]))
                    * 0.5,
            );
        }
        Some(f64::from(self.heights[x + 17 * z]))
    }
    pub fn cell_drawable(&self, x: usize, z: usize, seams: u32) -> bool {
        !self.cell_protected(x, z)
            && [(x, z), (x + 1, z), (x, z + 1), (x + 1, z + 1)]
                .into_iter()
                .all(|(x, z)| self.height(x, z, seams).is_some())
    }
    pub fn drawable_cells(&self, seams: u32) -> u32 {
        (0..16)
            .flat_map(|z| (0..16).map(move |x| (x, z)))
            .filter(|(x, z)| self.cell_drawable(*x, *z, seams))
            .count() as u32
    }
    pub fn canonical_hash(&self) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for byte in self
            .heights
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .chain(self.known)
            .chain(self.atlas.iter().flat_map(|v| v.to_le_bytes()))
            .chain(self.tint.iter().flat_map(|v| v.to_le_bytes()))
            .chain(self.protected)
        {
            h = (h ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        }
        h
    }
    /// Check a true shared edge before publishing a fine/coarse cover. Fine even
    /// endpoints must equal the actual coarse samples; different source versions
    /// can coexist only when these shared values agree.
    pub fn compatible_coarse(&self, coarse: &Self, edge: Edge) -> Result<(), HeightError> {
        if self.key.space != coarse.key.space
            || self.key.level.checked_add(1) != Some(coarse.key.level)
        {
            return Err(HeightError::NotTwoToOne);
        }
        let a = self.key.bounds().ok_or(HeightError::InvalidKey)?;
        let b = coarse.key.bounds().ok_or(HeightError::InvalidKey)?;
        let adjacent = match edge {
            Edge::West => a[0] == b[2] && a[1] >= b[1] && a[3] <= b[3],
            Edge::East => a[2] == b[0] && a[1] >= b[1] && a[3] <= b[3],
            Edge::North => a[1] == b[3] && a[0] >= b[0] && a[2] <= b[2],
            Edge::South => a[3] == b[1] && a[0] >= b[0] && a[2] <= b[2],
        };
        if !adjacent {
            return Err(HeightError::NotAdjacent);
        }
        let step = self.key.step().expect("validated fine");
        let coarse_step = step * 2;
        for along in (0..=16).step_by(2) {
            let (x, z) = match edge {
                Edge::West => (0, along),
                Edge::East => (16, along),
                Edge::North => (along, 0),
                Edge::South => (along, 16),
            };
            let world_x = a[0] + x as i64 * step;
            let world_z = a[1] + z as i64 * step;
            let cx = ((world_x - b[0]) / coarse_step) as usize;
            let cz = ((world_z - b[1]) / coarse_step) as usize;
            if !self.sample_known(x, z) || !coarse.sample_known(cx, cz) {
                return Err(HeightError::UnknownBoundary);
            }
            if self.heights[x + 17 * z] != coarse.heights[cx + 17 * cz] {
                return Err(HeightError::SourceMismatch);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannedHeightTile {
    pub key: HeightTileKey,
    pub seams: u32,
}
pub struct HeightPlan {
    pub tiles: Vec<PlannedHeightTile>,
    pub visited_nodes: u32,
    pub outer_bounds_pages: [i64; 4],
}
impl HeightPlan {
    pub fn new(
        space: u32,
        camera_page: [i64; 2],
        level: u8,
        max_tiles: usize,
    ) -> Result<Self, PlanError> {
        if level > MAX_LEVEL {
            return Err(PlanError::InvalidLevel);
        }
        let plan = clipmap_plan(camera_page, level, max_tiles)?;
        Self::from_clipmap(space, &plan)
    }
    pub fn from_clipmap(space: u32, plan: &ClipmapPlan) -> Result<Self, PlanError> {
        let mut tiles = plan
            .tiles
            .iter()
            .map(|t| {
                Ok(PlannedHeightTile {
                    key: HeightTileKey {
                        space,
                        level: t.level,
                        x: i32::try_from(t.x).map_err(|_| PlanError::CoordinateOverflow)?,
                        z: i32::try_from(t.z).map_err(|_| PlanError::CoordinateOverflow)?,
                    },
                    seams: 0,
                })
            })
            .collect::<Result<Vec<_>, PlanError>>()?;
        for seam in plan.seams() {
            let (index, edge) = if plan.tiles[seam.tile].level < plan.tiles[seam.neighbour].level {
                (seam.tile, seam.edge)
            } else {
                (
                    seam.neighbour,
                    match seam.edge {
                        Edge::West => Edge::East,
                        Edge::East => Edge::West,
                        Edge::North => Edge::South,
                        Edge::South => Edge::North,
                    },
                )
            };
            let other = if index == seam.tile {
                seam.neighbour
            } else {
                seam.tile
            };
            if tiles[index].key.level.checked_add(1) != Some(tiles[other].key.level) {
                return Err(PlanError::InvalidLevel);
            }
            tiles[index].seams |= match edge {
                Edge::West => WEST,
                Edge::East => EAST,
                Edge::North => NORTH,
                Edge::South => SOUTH,
            };
        }
        if tiles.iter().any(|t| t.key.bounds().is_none()) {
            return Err(PlanError::CoordinateOverflow);
        }
        Ok(Self {
            tiles,
            visited_nodes: plan.visited_nodes,
            outer_bounds_pages: plan.outer_bounds_pages,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tile(key: HeightTileKey) -> HeightTile {
        let b = key.bounds().unwrap();
        let step = key.step().unwrap();
        let mut t = HeightTile {
            key,
            source: 1,
            heights: [0; 289],
            known: [255; 37],
            atlas: [3; 256],
            tint: [u32::MAX; 256],
            protected: [0; 32],
        };
        t.known[36] = 1;
        for z in 0..17 {
            for x in 0..17 {
                let wx = b[0] + x as i64 * step;
                let wz = b[1] + z as i64 * step;
                t.heights[x + 17 * z] = ((wx * wx + 3 * wz * wz).rem_euclid(23)) as i32;
            }
        }
        t
    }
    #[test]
    fn actual_shared_samples_stitch_fine_odd_vertices_to_coarse_linear_edge() {
        let fine = tile(HeightTileKey {
            space: 0,
            level: 0,
            x: 1,
            z: 0,
        });
        let coarse = tile(HeightTileKey {
            space: 0,
            level: 1,
            x: 1,
            z: 0,
        });
        fine.compatible_coarse(&coarse, Edge::East).unwrap();
        for z in 0..17 {
            let actual = fine.height(16, z, EAST).unwrap();
            let expected = if z % 2 == 0 {
                f64::from(coarse.heights[17 * (z / 2)])
            } else {
                (f64::from(coarse.heights[17 * (z / 2)])
                    + f64::from(coarse.heights[17 * (z / 2 + 1)]))
                    * 0.5
            };
            assert_eq!(actual, expected);
        }
        let mut changed = coarse.clone();
        changed.heights[0] += 1;
        assert_eq!(
            fine.compatible_coarse(&changed, Edge::East),
            Err(HeightError::SourceMismatch)
        );
    }
    #[test]
    fn unknown_and_protected_cells_are_never_filled() {
        let mut t = tile(HeightTileKey {
            space: 0,
            level: 0,
            x: 0,
            z: 0,
        });
        assert_eq!(t.drawable_cells(0), 256);
        t.protected[0] = 1;
        assert_eq!(t.drawable_cells(0), 255);
        let i = 8 + 17 * 8;
        t.known[i / 8] &= !(1 << (i % 8));
        t.heights[i] = 0;
        assert_eq!(t.drawable_cells(0), 251);
        t.validate(186).unwrap();
        t.known[36] |= 2;
        assert_eq!(t.validate(186), Err(HeightError::InvalidMask));
    }
    #[test]
    fn true_1024_chunk_plan_is_logarithmic_and_seams_are_only_two_to_one() {
        let p = HeightPlan::new(0, [0, 0], 9, 512).unwrap();
        assert!(p.tiles.len() < 350);
        assert!(p.visited_nodes < 512);
        assert!(p.outer_bounds_pages[0] <= -1024 && p.outer_bounds_pages[2] >= 1024);
        assert!(p.tiles.iter().any(|t| t.seams != 0));
        let moved = HeightPlan::new(0, [7, -3], 9, 512).unwrap();
        assert!(moved.tiles.len() < 350);
        assert_eq!(p.tiles.len(), moved.tiles.len());
        println!(
            "height horizon1024 plan tiles{} visits{} movingTiles{}",
            p.tiles.len(),
            p.visited_nodes,
            moved.tiles.len()
        );
    }
}
