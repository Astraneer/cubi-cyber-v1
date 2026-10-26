//! Canonical undecorated terrain baseline for server-owned distant sampling.
//! It shares material decisions with base(); plants, trees, spawn models and
//! persisted changes remain separate geometry and are never claimed as this data.
use crate::{GenerationError, Generator, SEA_LEVEL, v4};
use core_types::Cell;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicTerrainColumn {
    /// Solid ground voxel and the highest baseline voxel (possibly water).
    pub ground_y: i32,
    pub surface_y: i32,
    pub ground: Cell,
    pub surface: Cell,
    pub tint: u16,
    pub protected: bool,
    pub sampled_columns: u8,
}
impl Generator {
    /// Worker-only authority sampling: five original numerical columns, no page
    /// generation, private ore input, decoration or per-call heap allocation.
    pub fn public_terrain_column(
        &self,
        x: i32,
        z: i32,
    ) -> Result<PublicTerrainColumn, GenerationError> {
        let west = x
            .checked_sub(1)
            .ok_or(GenerationError::CoordinateOutOfRange)?;
        let east = x
            .checked_add(1)
            .ok_or(GenerationError::CoordinateOutOfRange)?;
        let north = z
            .checked_sub(1)
            .ok_or(GenerationError::CoordinateOutOfRange)?;
        let south = z
            .checked_add(1)
            .ok_or(GenerationError::CoordinateOutOfRange)?;
        let c = self.inner.column(x, z);
        let low = [
            self.inner.column(west, z).h,
            self.inner.column(east, z).h,
            self.inner.column(x, north).h,
            self.inner.column(x, south).h,
        ]
        .into_iter()
        .fold(c.h, i32::min);
        let (top, _, _) = v4::base_materials(&c, low);
        let ground = Cell::from_parts(v4::base_top(&c, top), 0);
        let water = c.h < SEA_LEVEL;
        Ok(PublicTerrainColumn {
            ground_y: c.h,
            surface_y: c.h.max(SEA_LEVEL),
            ground,
            surface: if water {
                Cell::from_parts(rules::ids::WATER, 0)
            } else {
                ground
            },
            tint: v4::rgb565(v4::grass_tint(&c)),
            protected: self.inner.site.protects(x, z),
            sampled_columns: 5,
        })
    }
    /// Conservative complete extent of the canonical spawn overlay, including
    /// cells between height samples. Half-open X/Z block bounds, server metadata.
    pub fn public_structure_bounds(&self) -> Option<[i32; 4]> {
        let spawn = self.spawn();
        if !self.protects_spawn(spawn.center[0], spawn.center[2]) {
            return None;
        }
        let radius = spawn.protection_radius;
        Some([
            spawn.center[0].checked_sub(radius)?,
            spawn.center[2].checked_sub(radius)?,
            spawn.center[0].checked_add(radius)?.checked_add(1)?,
            spawn.center[2].checked_add(radius)?.checked_add(1)?,
        ])
    }
}
