//! Pure worker builder for authoritative public terrain-baseline height tiles.
//! All committed edits are protected geometry; AIR removals cannot vanish after
//! page eviction or be filled by a coarse baseline. No raw Cell enters the DTO.
use crate::height_index::{EditedColumn, TileEdits, TileKey};
pub const PUBLIC_SURFACE_SCHEMA: u16 = 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicTerrainSurface {
    /// Public canonical surface boundary in integer block coordinates.
    pub height: i32,
    /// Untouched canonical ground above this Y occludes lower edits.
    pub ground_y: i32,
    pub atlas_layer: u16,
    pub tint: u32,
    /// Baseline is insufficient for canonical structures at this column.
    pub protected: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicHeightTile {
    pub key: TileKey,
    pub source: u64,
    pub error: Option<net::height::HeightErrorBound>,
    pub heights: [i32; 289],
    pub known: [u8; 37],
    pub atlas: [u16; 256],
    pub tint: [u32; 256],
    pub protected: [u8; 32],
    pub replacement: ReplacementRequirement,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplacementRequirement {
    None,
    /// Coarse footprints cannot be replaced by a single edited page.
    Refine {
        children: [TileKey; 4],
    },
    /// Replace the WHOLE 16x16 leaf with every listed vertical page. An authority
    /// manifest of exact public page source/view stamps is still required.
    Column {
        source: u64,
        min_page_y: i32,
        max_page_y: i32,
    },
}
impl PublicHeightTile {
    pub fn sample_known(&self, x: usize, z: usize) -> bool {
        if x > 16 || z > 16 {
            return false;
        }
        let index = x + 17 * z;
        self.known[index / 8] & (1 << (index % 8)) != 0
    }
    pub fn cell_protected(&self, x: usize, z: usize) -> bool {
        if x >= 16 || z >= 16 {
            return true;
        }
        let index = x + 16 * z;
        self.protected[index / 8] & (1 << (index % 8)) != 0
    }
}
#[derive(Clone, Copy, Debug)]
pub struct HeightBuildLimits {
    pub max_columns: usize,
    pub max_cells: usize,
    pub atlas_layers: u32,
    pub max_replacement_pages: usize,
    pub max_protection_regions: usize,
}
impl Default for HeightBuildLimits {
    fn default() -> Self {
        Self {
            max_columns: 4096,
            max_cells: 65536,
            atlas_layers: 186,
            max_replacement_pages: 32,
            max_protection_regions: 64,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeightBuildWork {
    pub baseline_columns: u64,
    pub edited_columns: u64,
    pub edited_cells: u64,
    pub protected_cells: u64,
    pub world_reads: u64,
    pub canonical_noise_columns: u64,
    pub error_intervals: u64,
    pub error_unit_cells: u64,
    pub error_noise_evaluations: u64,
    pub error_columns: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightBuildError {
    InvalidLimits,
    InvalidInput,
    Capacity,
    InvalidMaterial,
    ReplacementTooTall,
}
impl std::fmt::Display for HeightBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for HeightBuildError {}
/// The callback is a server-owned immutable canonical BASELINE sampler, never a
/// freshly invented client world and never a 3D feature approximation. Returning
/// None explicitly keeps the sample Unknown. Generate/copy inputs on workers.
pub fn build_tile(
    edits: &TileEdits,
    baseline: impl FnMut(i32, i32) -> Option<PublicTerrainSurface>,
    limits: HeightBuildLimits,
) -> Result<(PublicHeightTile, HeightBuildWork), HeightBuildError> {
    build_tile_with_regions(edits, &[], baseline, limits)
}
pub fn build_tile_with_regions(
    edits: &TileEdits,
    regions: &[[i32; 4]],
    baseline: impl FnMut(i32, i32) -> Option<PublicTerrainSurface>,
    limits: HeightBuildLimits,
) -> Result<(PublicHeightTile, HeightBuildWork), HeightBuildError> {
    build_tile_with_boundary_policy(edits, regions, baseline, limits, false)
}
fn build_tile_with_boundary_policy(
    edits: &TileEdits,
    regions: &[[i32; 4]],
    mut baseline: impl FnMut(i32, i32) -> Option<PublicTerrainSurface>,
    limits: HeightBuildLimits,
    canonical_public_boundary: bool,
) -> Result<(PublicHeightTile, HeightBuildWork), HeightBuildError> {
    if limits.max_columns == 0
        || limits.max_cells == 0
        || limits.atlas_layers == 0
        || limits.atlas_layers > 256
        || limits.max_replacement_pages == 0
        || limits.max_protection_regions == 0
    {
        return Err(HeightBuildError::InvalidLimits);
    }
    if regions.len() > limits.max_protection_regions {
        return Err(HeightBuildError::Capacity);
    }
    if regions.iter().any(|r| r[0] >= r[2] || r[1] >= r[3]) {
        return Err(HeightBuildError::InvalidInput);
    }
    let bounds = edits.key.bounds().ok_or(HeightBuildError::InvalidInput)?;
    if edits.source == 0 {
        return Err(HeightBuildError::InvalidInput);
    }
    let cells = edits
        .columns
        .iter()
        .try_fold(0usize, |sum, c| sum.checked_add(c.cells.len()))
        .ok_or(HeightBuildError::Capacity)?;
    if edits.columns.len() > limits.max_columns || cells > limits.max_cells {
        return Err(HeightBuildError::Capacity);
    }
    if edits.columns.iter().any(|c| {
        c.space != edits.key.space
            || i64::from(c.x) < bounds[0] - 1
            || i64::from(c.x) > bounds[2]
            || i64::from(c.z) < bounds[1] - 1
            || i64::from(c.z) > bounds[3]
            || c.cells.is_empty()
    }) {
        return Err(HeightBuildError::InvalidInput);
    }
    let mut tile = PublicHeightTile {
        key: edits.key,
        source: edits.source,
        error: None,
        heights: [0; 289],
        known: [0; 37],
        atlas: [0; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: ReplacementRequirement::None,
    };
    let mut work = HeightBuildWork {
        edited_columns: edits.columns.len() as u64,
        edited_cells: cells as u64,
        ..HeightBuildWork::default()
    };
    let step = 1_i64 << edits.key.level;
    for z in 0..17 {
        for x in 0..17 {
            let wx = bounds[0] + x as i64 * step;
            let wz = bounds[1] + z as i64 * step;
            let (Ok(wx), Ok(wz)) = (i32::try_from(wx), i32::try_from(wz)) else {
                continue;
            };
            work.baseline_columns += 1;
            let Some(surface) = baseline(wx, wz) else {
                continue;
            };
            if u32::from(surface.atlas_layer) >= limits.atlas_layers {
                return Err(HeightBuildError::InvalidMaterial);
            }
            let index = x + z * 17;
            tile.heights[index] = surface.height;
            tile.known[index / 8] |= 1 << (index % 8);
            if x < 16 && z < 16 {
                let index = x + z * 16;
                tile.atlas[index] = surface.atlas_layer;
                tile.tint[index] = surface.tint;
            }
            if surface.protected {
                // Every adjacent cell depends on this sample. Protect all, including
                // the inward cells at a tile edge, rather than inventing a ramp.
                for dz in [0_i32, -1] {
                    for dx in [0_i32, -1] {
                        protect(&mut tile, x as i32 + dx, z as i32 + dz);
                    }
                }
            }
        }
    }
    let mut highest_replacement_y = r#gen::PROFILE_HEIGHT - 1;
    for column in &edits.columns {
        work.baseline_columns += 1;
        let surface = baseline(column.x, column.z);
        let affects_surface = surface
            .is_none_or(|surface| column.cells.iter().any(|cell| cell.y >= surface.ground_y));
        if !affects_surface {
            continue;
        }
        let x = (i64::from(column.x) - bounds[0]).div_euclid(step) as i32;
        let z = (i64::from(column.z) - bounds[1]).div_euclid(step) as i32;
        let own_body = (0..16).contains(&x) && (0..16).contains(&z);
        if !own_body && canonical_public_boundary && !public_boundary_changed(column, surface) {
            continue;
        }
        if own_body {
            highest_replacement_y = highest_replacement_y.max(
                column
                    .cells
                    .iter()
                    .map(|cell| cell.y)
                    .max()
                    .expect("validated nonempty column"),
            );
        }
        // A foreign rim cell can expose a face OWNED by the nearest in-tile
        // voxel. The authority requests that owner's actual public geometry;
        // it never invents a wall/skirt. Foreign Y cannot extend this column.
        protect(&mut tile, x.clamp(0, 15), z.clamp(0, 15));
    }
    // Region metadata catches structures lying BETWEEN sparse samples.
    for region in regions {
        for z in 0..16 {
            for x in 0..16 {
                let x0 = bounds[0] + i64::from(x) * step;
                let z0 = bounds[1] + i64::from(z) * step;
                if x0 < i64::from(region[2])
                    && x0 + step > i64::from(region[0])
                    && z0 < i64::from(region[3])
                    && z0 + step > i64::from(region[1])
                {
                    protect(&mut tile, x, z);
                }
            }
        }
    }
    if tile.protected.iter().any(|byte| *byte != 0) {
        if edits.key.level > 0 {
            let level = edits.key.level - 1;
            let x = edits
                .key
                .x
                .checked_mul(2)
                .ok_or(HeightBuildError::InvalidInput)?;
            let z = edits
                .key
                .z
                .checked_mul(2)
                .ok_or(HeightBuildError::InvalidInput)?;
            tile.replacement = ReplacementRequirement::Refine {
                children: std::array::from_fn(|i| TileKey {
                    space: edits.key.space,
                    level,
                    x: x + (i as i32 & 1),
                    z: z + (i as i32 >> 1),
                }),
            };
        } else {
            let max_page_y = highest_replacement_y.div_euclid(16);
            if max_page_y as usize + 1 > limits.max_replacement_pages {
                return Err(HeightBuildError::ReplacementTooTall);
            }
            tile.protected.fill(255);
            tile.replacement = ReplacementRequirement::Column {
                source: edits.source,
                min_page_y: 0,
                max_page_y,
            };
        }
    }
    work.protected_cells = tile
        .protected
        .iter()
        .map(|byte| u64::from(byte.count_ones()))
        .sum();
    Ok((tile, work))
}
/// Server worker path sharing the generator's actual undecorated material policy.
/// Private features and final vegetation are not relabelled as baseline terrain.
pub fn build_canonical_tile(
    generator: &r#gen::Generator,
    edits: &TileEdits,
    limits: HeightBuildLimits,
) -> Result<(PublicHeightTile, HeightBuildWork), HeightBuildError> {
    if edits.key.space != core_types::SpaceId(0) {
        return Err(HeightBuildError::InvalidInput);
    }
    let bounds = generator.public_structure_bounds();
    let regions = bounds.as_slice();
    let (mut tile, mut work) = build_tile_with_boundary_policy(
        edits,
        regions,
        |x, z| {
            let c = generator.public_terrain_column(x, z).ok()?;
            let face = rules::blocks::mesh_face(1);
            let tint = if rules::blocks::tinted(c.surface.id(), face) {
                let r = u32::from((c.tint >> 11) & 31) * 255 / 31;
                let g = u32::from((c.tint >> 5) & 63) * 255 / 63;
                let b = u32::from(c.tint & 31) * 255 / 31;
                r | (g << 8) | (b << 16) | 0xff000000
            } else {
                u32::MAX
            };
            Some(PublicTerrainSurface {
                height: c.surface_y + 1,
                ground_y: c.ground_y,
                atlas_layer: rules::blocks::layer_cell(c.surface.id(), 0, face).0 as u16,
                tint,
                protected: c.protected,
            })
        },
        limits,
        true,
    )?;
    let tile_bounds = edits.key.bounds().ok_or(HeightBuildError::InvalidInput)?;
    let Ok(bound) = generator.public_height_error(
        [tile_bounds[0] as i32, tile_bounds[1] as i32],
        edits.key.level,
    ) else {
        // The final legal tile can have a closed endpoint one past i32::MAX.
        // Its unavailable corner stays Unknown; do not invent a certificate.
        work.canonical_noise_columns = work.baseline_columns * 5;
        return Ok((tile, work));
    };
    let scaled = bound.max_error_blocks * 1024.;
    let mut max_error_q1024 = scaled as u64;
    if (max_error_q1024 as f64) < scaled {
        max_error_q1024 += 1;
    }
    tile.error = Some(net::height::HeightErrorBound {
        max_error_q1024,
        min_height: bound.min_height,
        max_height: bound.max_height,
    });
    work.error_intervals = bound.intervals;
    work.error_unit_cells = bound.unit_cells;
    work.error_noise_evaluations = bound.noise_evaluations;
    work.error_columns = bound.sampled_columns;
    work.canonical_noise_columns = work.baseline_columns * 5;
    Ok((tile, work))
}

// Public render/occlusion/fluid geometry, deliberately excluding material IDs,
// texture/tint, private ores, and emission-only state. properties.shape includes
// raw IDs and therefore MUST NOT identify this dependency. The canonical
// undecorated terrain envelope contains only full opaque ground, source water,
// and AIR; a cube-to-cube material replacement cannot expose a neighbour face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PublicBoundaryShape {
    render: u8,
    opaque: bool,
    fluid: u8,
    level: u16,
}
impl PublicBoundaryShape {
    const AIR: Self = Self {
        render: 0,
        opaque: false,
        fluid: 0,
        level: 0,
    };
    const GROUND: Self = Self {
        render: 1,
        opaque: true,
        fluid: 0,
        level: 0,
    };
    const WATER: Self = Self {
        render: 3,
        opaque: false,
        fluid: rules::blocks::FLUID_WATER,
        level: 0,
    };
    fn actual(cell: core_types::Cell) -> Option<Self> {
        let definition = rules::block(cell.id())?;
        let properties = rules::properties(cell);
        let render = match definition.render {
            rules::blocks::Render::None
            | rules::blocks::Render::Cross
            | rules::blocks::Render::Model(_) => 0,
            rules::blocks::Render::Cube => 1,
            rules::blocks::Render::Leaves | rules::blocks::Render::Glass => 2,
            rules::blocks::Render::Water => 3,
        };
        Some(Self {
            render,
            opaque: properties.opaque,
            fluid: definition.fluid,
            level: if definition.fluid == 0 {
                0
            } else {
                cell.state() & 15
            },
        })
    }
}
fn public_boundary_changed(column: &EditedColumn, surface: Option<PublicTerrainSurface>) -> bool {
    let Some(surface) = surface else {
        return true;
    };
    column.cells.iter().any(|edit| {
        // Keep the existing top/exposure domain: a raw buried AIR row can be a
        // closed component sealed by PublicProjection, so it is not an exterior
        // proof. Below-ground tunnel closure requires a separate public witness.
        if edit.y < surface.ground_y {
            return false;
        }
        let baseline = if edit.y <= surface.ground_y {
            PublicBoundaryShape::GROUND
        } else if edit.y < surface.height {
            PublicBoundaryShape::WATER
        } else {
            PublicBoundaryShape::AIR
        };
        PublicBoundaryShape::actual(edit.cell).is_none_or(|actual| actual != baseline)
    })
}

/// A camera may request the exact public representation of a non-flat leaf.
/// This only promotes representation metadata; the existing stream must still
/// obtain all canonical pages and source/view manifests before publication.
/// World source, sampled heights, materials, tints and error remain immutable.
pub fn promote_exact_leaf(tile: &mut PublicHeightTile) -> Result<bool, HeightBuildError> {
    if tile.key.level != 0 {
        return Err(HeightBuildError::InvalidInput);
    }
    if matches!(tile.replacement, ReplacementRequirement::Column { .. }) {
        return Ok(false);
    }
    if tile.error.is_none_or(|error| error.max_error_q1024 == 0) {
        return Ok(false);
    }
    tile.protected.fill(255);
    tile.replacement = ReplacementRequirement::Column {
        source: tile.source,
        min_page_y: 0,
        max_page_y: (r#gen::PROFILE_HEIGHT - 1).div_euclid(16),
    };
    Ok(true)
}

fn protect(tile: &mut PublicHeightTile, x: i32, z: i32) {
    if (0..16).contains(&x) && (0..16).contains(&z) {
        let index = (x + z * 16) as usize;
        tile.protected[index / 8] |= 1 << (index % 8);
    }
}

#[cfg(test)]
mod boundary_shape_tests {
    use super::*;
    fn changed(y: i32, id: u16, state: u16) -> bool {
        public_boundary_changed(
            &EditedColumn {
                space: core_types::SpaceId(0),
                x: 0,
                z: 0,
                cells: vec![crate::height_index::IndexedCell {
                    y,
                    cell: core_types::Cell::from_parts(id, state),
                    source_tx: core_types::TxId(1),
                }],
            },
            Some(PublicTerrainSurface {
                height: 49,
                ground_y: 40,
                atlas_layer: 3,
                tint: u32::MAX,
                protected: false,
            }),
        )
    }
    #[test]
    fn water_kind_level_and_occlusion_are_meaningful_but_opaque_ids_are_not() {
        assert!(!changed(48, rules::ids::WATER, 0));
        assert!(changed(48, rules::ids::WATER, 1));
        assert!(changed(48, rules::ids::AIR, 0));
        assert!(changed(48, rules::ids::LAVA, 0));
        assert!(changed(48, rules::ids::STONE, 0));
        assert!(!changed(49, rules::ids::AIR, 0));
        assert!(!changed(40, rules::ids::STONE, 0));
        assert!(!changed(40, rules::ids::IRON_ORE, 0));
        assert!(changed(40, rules::ids::GLASS, 0));
        assert!(changed(49, 65535, 0));
    }
}
