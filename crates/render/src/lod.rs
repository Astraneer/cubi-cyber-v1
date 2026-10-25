//! Versioned summaries of public, actual world data and bounded clipmap plans.
//!
//! This module never receives a generator, world seed, block registry or raw
//! authoritative cells. The server supplies already redacted public voxel views.
//! Unknown is represented explicitly and never silently replaced with air.

use crate::terrain_mesh::{
    CellMaterial, MeshClass, MeshScratch, PageHalo, PageMesh, mesh_page_indexed,
};
use std::collections::{BTreeMap, BTreeSet};

pub const PAGE_EDGE: usize = 16;
pub const PAGE_VOLUME: usize = PAGE_EDGE * PAGE_EDGE * PAGE_EDGE;
pub const SUMMARY_EDGE: usize = 4;
pub const SUMMARY_VOLUME: usize = SUMMARY_EDGE * SUMMARY_EDGE * SUMMARY_EDGE;
pub const MAX_LEVEL: u8 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum SurfaceKind {
    Opaque,
    Cutout,
    Water,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublicVoxel {
    #[default]
    Unknown,
    Air,
    Solid {
        material: u16,
        tint: u32,
        kind: SurfaceKind,
    },
}

impl PublicVoxel {
    pub const fn solid(material: u16, tint: u32) -> Self {
        Self::Solid {
            material,
            tint,
            kind: SurfaceKind::Opaque,
        }
    }
    pub const fn occupied(self) -> bool {
        matches!(self, Self::Solid { .. })
    }
    pub const fn known(self) -> bool {
        !matches!(self, Self::Unknown)
    }
    fn material(self) -> CellMaterial {
        match self {
            Self::Unknown => CellMaterial {
                occludes: true,
                casts_ao: true,
                ..CellMaterial::EMPTY
            },
            Self::Air => CellMaterial::EMPTY,
            Self::Solid {
                material,
                tint,
                kind,
            } => CellMaterial {
                face_materials: [material; 6],
                face_uv_rotations: [0; 6],
                tint,
                tint_faces: 0x3f,
                class: match kind {
                    SurfaceKind::Opaque => MeshClass::Opaque,
                    SurfaceKind::Cutout => MeshClass::Cutout,
                    SurfaceKind::Water => MeshClass::Water,
                },
                occludes: kind == SurfaceKind::Opaque,
                casts_ao: kind != SurfaceKind::Water,
                cull_group: if kind == SurfaceKind::Opaque {
                    0
                } else {
                    material.wrapping_add(1)
                },
            },
        }
    }
}

/// Already-public input: callers must apply visibility/secret policy before this boundary.
#[derive(Clone, Debug)]
pub struct PublicPage {
    cells: [PublicVoxel; PAGE_VOLUME],
}

impl PublicPage {
    pub const fn filled(voxel: PublicVoxel) -> Self {
        Self {
            cells: [voxel; PAGE_VOLUME],
        }
    }
    pub fn from_exposed(mut sample: impl FnMut([u8; 3]) -> PublicVoxel) -> Self {
        let mut out = Self::filled(PublicVoxel::Unknown);
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    out.set([x, y, z], sample([x, y, z]));
                }
            }
        }
        out
    }
    pub fn get(&self, [x, y, z]: [u8; 3]) -> PublicVoxel {
        assert!(x < 16 && y < 16 && z < 16, "page coordinate");
        self.cells[usize::from(x) + 16 * (usize::from(z) + 16 * usize::from(y))]
    }
    pub fn set(&mut self, [x, y, z]: [u8; 3], voxel: PublicVoxel) {
        assert!(x < 16 && y < 16 && z < 16, "page coordinate");
        self.cells[usize::from(x) + 16 * (usize::from(z) + 16 * usize::from(y))] = voxel;
    }
    pub fn cells(&self) -> &[PublicVoxel; PAGE_VOLUME] {
        &self.cells
    }
    pub fn sample(&self, [x, y, z]: [i8; 3]) -> PublicVoxel {
        if (0..16).contains(&x) && (0..16).contains(&y) && (0..16).contains(&z) {
            self.get([x as u8, y as u8, z as u8])
        } else {
            PublicVoxel::Unknown
        }
    }
    pub fn canonical_hash(&self) -> u64 {
        hash_voxels(self.cells.iter().copied())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LodKey {
    pub space: u32,
    pub level: u8,
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl LodKey {
    pub const fn page(space: u32, x: i64, y: i64, z: i64) -> Self {
        Self {
            space,
            level: 0,
            x,
            y,
            z,
        }
    }
    pub fn parent(self) -> Option<Self> {
        (self.level < MAX_LEVEL).then(|| Self {
            space: self.space,
            level: self.level + 1,
            x: self.x.div_euclid(2),
            y: self.y.div_euclid(2),
            z: self.z.div_euclid(2),
        })
    }
    pub fn child(self, index: u8) -> Option<Self> {
        if self.level == 0 || index >= 8 {
            return None;
        }
        Some(Self {
            space: self.space,
            level: self.level - 1,
            x: self.x.checked_mul(2)?.checked_add(i64::from(index & 1))?,
            y: self
                .y
                .checked_mul(2)?
                .checked_add(i64::from((index >> 2) & 1))?,
            z: self
                .z
                .checked_mul(2)?
                .checked_add(i64::from((index >> 1) & 1))?,
        })
    }
    pub fn extent_blocks(self) -> Option<i64> {
        (self.level <= MAX_LEVEL).then(|| 16_i64 << self.level)
    }
    pub fn origin_blocks(self) -> Option<[i64; 3]> {
        let scale = self.extent_blocks()?;
        Some([
            self.x.checked_mul(scale)?,
            self.y.checked_mul(scale)?,
            self.z.checked_mul(scale)?,
        ])
    }
}

/// Integer camera anchor keeps huge absolute coordinates out of f32 arithmetic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraOrigin {
    pub block: [i64; 3],
    pub fractional: [f32; 3],
}

impl CameraOrigin {
    pub fn relative(self, key: LodKey) -> Option<[f32; 3]> {
        if self
            .fractional
            .into_iter()
            .any(|value| !value.is_finite() || !(0.0..1.0).contains(&value))
        {
            return None;
        }
        let origin = key.origin_blocks()?;
        let mut out = [0.0; 3];
        for axis in 0..3 {
            let delta = origin[axis].checked_sub(self.block[axis])?;
            if delta.unsigned_abs() > (1 << 24) {
                return None;
            }
            out[axis] = delta as f32 - self.fractional[axis];
        }
        Some(out)
    }
}

#[derive(Clone, Debug)]
pub enum SummaryGrid {
    Uniform(PublicVoxel),
    Dense(Box<[PublicVoxel; SUMMARY_VOLUME]>),
}

impl SummaryGrid {
    fn new(cells: [PublicVoxel; SUMMARY_VOLUME]) -> Self {
        if cells.iter().all(|cell| *cell == cells[0]) {
            Self::Uniform(cells[0])
        } else {
            Self::Dense(Box::new(cells))
        }
    }
    pub fn get(&self, [x, y, z]: [u8; 3]) -> PublicVoxel {
        assert!(x < 4 && y < 4 && z < 4, "summary coordinate");
        match self {
            Self::Uniform(cell) => *cell,
            Self::Dense(cells) => cells[usize::from(x) + 4 * (usize::from(z) + 4 * usize::from(y))],
        }
    }
    pub fn payload_bytes(&self) -> usize {
        match self {
            Self::Uniform(_) => std::mem::size_of::<PublicVoxel>(),
            Self::Dense(_) => SUMMARY_VOLUME * std::mem::size_of::<PublicVoxel>(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LodSummary {
    pub key: LodKey,
    pub source_version: u64,
    pub revision: u64,
    pub public_hash: u64,
    pub complete: bool,
    pub occupied_cells: u64,
    pub protected_pages: u64,
    pub geometric_error_blocks: f64,
    pub grid: SummaryGrid,
}

impl LodSummary {
    pub fn projected_error(&self, distance_blocks: f64, projection_pixels: f64) -> f64 {
        if !distance_blocks.is_finite()
            || !projection_pixels.is_finite()
            || projection_pixels <= 0.0
        {
            return f64::INFINITY;
        }
        self.geometric_error_blocks * projection_pixels / distance_blocks.max(1.0)
    }
    pub fn expanded_page(&self) -> PublicPage {
        PublicPage::from_exposed(|[x, y, z]| self.grid.get([x / 4, y / 4, z / 4]))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateWork {
    pub changed: bool,
    pub nodes_written: u32,
    pub source_cells_read: u32,
    pub child_cells_read: u32,
    pub feature_samples_read: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    InvalidLevel,
    InvalidCoordinate,
    StaleVersion,
    ConflictingVersion,
    RevisionExhausted,
    NodeBudgetExceeded,
}

/// Sparse forest: a change touches one page and its ancestors, never siblings.
pub struct LodForest {
    max_level: u8,
    max_nodes: usize,
    revision: u64,
    nodes: BTreeMap<LodKey, LodSummary>,
}

impl LodForest {
    pub fn new(max_level: u8) -> Result<Self, UpdateError> {
        Self::with_limits(max_level, 65_536)
    }
    pub fn with_limits(max_level: u8, max_nodes: usize) -> Result<Self, UpdateError> {
        if max_level > MAX_LEVEL {
            return Err(UpdateError::InvalidLevel);
        }
        if max_nodes < usize::from(max_level) + 1 {
            return Err(UpdateError::NodeBudgetExceeded);
        }
        Ok(Self {
            max_level,
            max_nodes,
            revision: 0,
            nodes: BTreeMap::new(),
        })
    }
    pub fn get(&self, key: LodKey) -> Option<&LodSummary> {
        self.nodes.get(&key)
    }
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn upsert_page(
        &mut self,
        key: LodKey,
        version: u64,
        page: &PublicPage,
    ) -> Result<UpdateWork, UpdateError> {
        if key.level != 0 {
            return Err(UpdateError::InvalidLevel);
        }
        key.origin_blocks().ok_or(UpdateError::InvalidCoordinate)?;
        if let Some(old) = self.nodes.get(&key) {
            if version < old.source_version {
                return Err(UpdateError::StaleVersion);
            }
            if version == old.source_version {
                return if page.canonical_hash() == old.public_hash {
                    Ok(UpdateWork {
                        source_cells_read: PAGE_VOLUME as u32,
                        ..UpdateWork::default()
                    })
                } else {
                    Err(UpdateError::ConflictingVersion)
                };
            }
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(UpdateError::RevisionExhausted)?;
        let protection = protection_groups(page, 4);
        let protected = protection.count != 0;
        let mut cells = [PublicVoxel::Unknown; SUMMARY_VOLUME];
        for y in 0..4 {
            for z in 0..4 {
                for x in 0..4 {
                    cells[x + 4 * (z + 4 * y)] = reduce((0..4).flat_map(|dy| {
                        (0..4).flat_map(move |dz| {
                            (0..4).map(move |dx| {
                                page.get([
                                    (x * 4 + dx) as u8,
                                    (y * 4 + dy) as u8,
                                    (z * 4 + dz) as u8,
                                ])
                            })
                        })
                    }));
                }
            }
        }
        let grid = SummaryGrid::new(cells);
        let mut complete = true;
        let mut uniform = true;
        let mut occupied_cells = 0;
        let mut public_hash = 0xcbf2_9ce4_8422_2325;
        for cell in page.cells.iter().copied() {
            complete &= cell.known();
            uniform &= cell == page.cells[0];
            occupied_cells += u64::from(cell.occupied());
            public_hash = hash_voxel(public_hash, cell);
        }
        let mut current = LodSummary {
            key,
            source_version: version,
            revision,
            public_hash,
            complete,
            occupied_cells,
            protected_pages: u64::from(protected),
            geometric_error_blocks: if uniform { 0.0 } else { 4.0 * 3.0_f64.sqrt() },
            grid,
        };
        let mut staged = Vec::with_capacity(usize::from(self.max_level) + 1);
        let mut work = UpdateWork {
            changed: true,
            source_cells_read: (PAGE_VOLUME * 2) as u32,
            feature_samples_read: protection.sample_reads,
            ..UpdateWork::default()
        };
        staged.push(current.clone());
        while current.key.level < self.max_level {
            let parent = current.key.parent().ok_or(UpdateError::InvalidLevel)?;
            parent
                .origin_blocks()
                .ok_or(UpdateError::InvalidCoordinate)?;
            let children: [Option<&LodSummary>; 8] = std::array::from_fn(|i| {
                let key = parent.child(i as u8)?;
                if key == current.key {
                    Some(&current)
                } else {
                    self.nodes.get(&key)
                }
            });
            let mut cells = [PublicVoxel::Unknown; SUMMARY_VOLUME];
            for y in 0..4 {
                for z in 0..4 {
                    for x in 0..4 {
                        let mut samples = [PublicVoxel::Unknown; 8];
                        for dy in 0..2 {
                            for dz in 0..2 {
                                for dx in 0..2 {
                                    let [gx, gy, gz] = [x * 2 + dx, y * 2 + dy, z * 2 + dz];
                                    let child = gx / 4 + 2 * (gz / 4) + 4 * (gy / 4);
                                    samples[dx + 2 * dz + 4 * dy] =
                                        children[child].map_or(PublicVoxel::Unknown, |node| {
                                            node.grid.get([
                                                (gx % 4) as u8,
                                                (gy % 4) as u8,
                                                (gz % 4) as u8,
                                            ])
                                        });
                                }
                            }
                        }
                        cells[x + 4 * (z + 4 * y)] = reduce(samples.into_iter());
                        work.child_cells_read += 8;
                    }
                }
            }
            let complete = children
                .iter()
                .all(|child| child.is_some_and(|node| node.complete));
            let occupied_cells = children
                .iter()
                .flatten()
                .fold(0_u64, |n, child| n.saturating_add(child.occupied_cells));
            let protected_pages = children
                .iter()
                .flatten()
                .fold(0_u64, |n, child| n.saturating_add(child.protected_pages));
            let public_hash =
                children
                    .iter()
                    .enumerate()
                    .fold(0xcbf2_9ce4_8422_2325, |hash, (i, child)| {
                        hash_word(
                            hash_word(hash, i as u64),
                            child.map_or(0, |node| node.public_hash),
                        )
                    });
            let grid = SummaryGrid::new(cells);
            let geometric_error_blocks = if complete
                && matches!(grid, SummaryGrid::Uniform(_))
                && children
                    .iter()
                    .flatten()
                    .all(|child| child.geometric_error_blocks == 0.0)
            {
                0.0
            } else {
                (4_u64 << parent.level) as f64 * 3.0_f64.sqrt()
            };
            let source_version = children
                .iter()
                .flatten()
                .map(|child| child.source_version)
                .max()
                .unwrap_or(version);
            current = LodSummary {
                key: parent,
                source_version,
                revision,
                public_hash,
                complete,
                occupied_cells,
                protected_pages,
                geometric_error_blocks,
                grid,
            };
            staged.push(current.clone());
        }
        work.nodes_written = staged.len() as u32;
        let new_nodes = staged
            .iter()
            .filter(|node| !self.nodes.contains_key(&node.key))
            .count();
        if self
            .nodes
            .len()
            .checked_add(new_nodes)
            .is_none_or(|count| count > self.max_nodes)
        {
            return Err(UpdateError::NodeBudgetExceeded);
        }
        for node in staged {
            self.nodes.insert(node.key, node);
        }
        self.revision = revision;
        Ok(work)
    }
}

fn reduce(samples: impl Iterator<Item = PublicVoxel>) -> PublicVoxel {
    // At most64 source voxels feed a summary cell; deterministic majority among
    // occupied materials preserves even a thin solid instead of erasing it.
    let mut choices = [(PublicVoxel::Unknown, 0_u32); 64];
    let mut overflow: Vec<(PublicVoxel, u32)> = Vec::new();
    let mut len = 0;
    let mut unknown = false;
    for voxel in samples {
        if voxel == PublicVoxel::Unknown {
            unknown = true;
        }
        if !voxel.occupied() {
            continue;
        }
        if let Some(index) = choices[..len].iter().position(|(value, _)| *value == voxel) {
            choices[index].1 += 1;
        } else if len < choices.len() {
            choices[len] = (voxel, 1);
            len += 1;
        } else if let Some((_, count)) = overflow.iter_mut().find(|(value, _)| *value == voxel) {
            *count += 1;
        } else {
            overflow.push((voxel, 1));
        }
    }
    choices[..len]
        .iter()
        .chain(&overflow)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map_or(
            if unknown {
                PublicVoxel::Unknown
            } else {
                PublicVoxel::Air
            },
            |(value, _)| *value,
        )
}

fn hash_word(mut hash: u64, word: u64) -> u64 {
    for byte in word.to_le_bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    hash
}
fn hash_voxels(voxels: impl Iterator<Item = PublicVoxel>) -> u64 {
    voxels.fold(0xcbf2_9ce4_8422_2325, hash_voxel)
}
fn hash_voxel(hash: u64, voxel: PublicVoxel) -> u64 {
    match voxel {
        PublicVoxel::Unknown => hash_word(hash, 0),
        PublicVoxel::Air => hash_word(hash, 1),
        PublicVoxel::Solid {
            material,
            tint,
            kind,
        } => hash_word(
            hash_word(hash, 2 | (u64::from(material) << 8) | ((kind as u64) << 24)),
            u64::from(tint),
        ),
    }
}

#[derive(Clone, Debug)]
pub struct ProtectionMask {
    pub words: [u64; 64],
    pub count: u32,
    pub coarse_block_edge: u8,
    pub sample_reads: u64,
}

impl ProtectionMask {
    pub fn contains(&self, group: usize) -> bool {
        self.words[group / 64] & (1 << (group % 64)) != 0
    }
    fn insert(&mut self, group: usize) {
        let bit = 1 << (group % 64);
        if self.words[group / 64] & bit == 0 {
            self.words[group / 64] |= bit;
            self.count += 1;
        }
    }
}

/// Local topology heuristic, deliberately evaluated against independent fixtures.
/// It protects thin beams/towers and bounded narrow passages; it is not a claim
/// that arbitrary architectural topology can be preserved within a fixed budget.
pub fn protection_groups(page: &PublicPage, block_edge: u8) -> ProtectionMask {
    assert!(
        [1, 2, 4, 8, 16].contains(&block_edge),
        "power-of-two coarse block edge"
    );
    let mut result = ProtectionMask {
        words: [0; 64],
        count: 0,
        coarse_block_edge: block_edge,
        sample_reads: 0,
    };
    let groups_edge = 16 / usize::from(block_edge);
    let sample = |position, reads: &mut u64| {
        *reads += 1;
        page.sample(position)
    };
    for y in 0..16_i8 {
        for z in 0..16_i8 {
            for x in 0..16_i8 {
                let position = [x, y, z];
                let voxel = sample(position, &mut result.sample_reads);
                let mut thin_axes = 0;
                let mut passage = false;
                let mut air_sides = [false; 3];
                for axis in 0..3 {
                    let mut lo = position;
                    lo[axis] -= 1;
                    let mut hi = position;
                    hi[axis] += 1;
                    air_sides[axis] = sample(lo, &mut result.sample_reads) == PublicVoxel::Air
                        && sample(hi, &mut result.sample_reads) == PublicVoxel::Air;
                    thin_axes += usize::from(air_sides[axis]);
                }
                if voxel == PublicVoxel::Air {
                    for axis in 0..3 {
                        if !(0..3).any(|other| other != axis && air_sides[other]) {
                            continue;
                        }
                        let mut distance = [0_u8; 2];
                        for (direction, sign) in [-1_i8, 1].into_iter().enumerate() {
                            for step in 1..=15_i8 {
                                let mut next = position;
                                next[axis] += sign * step;
                                let value = sample(next, &mut result.sample_reads);
                                if value.occupied() {
                                    distance[direction] = step as u8;
                                    break;
                                }
                                if value != PublicVoxel::Air {
                                    break;
                                }
                            }
                        }
                        if distance[0] != 0 && distance[1] != 0 {
                            passage = true;
                            break;
                        }
                    }
                }
                if (voxel.occupied() && thin_axes >= 2) || passage {
                    let [gx, gy, gz] = [
                        x as usize / usize::from(block_edge),
                        y as usize / usize::from(block_edge),
                        z as usize / usize::from(block_edge),
                    ];
                    result.insert(gx + groups_edge * (gz + groups_edge * gy));
                }
            }
        }
    }
    result
}

#[derive(Clone, Debug)]
pub struct CoarsenedPage {
    pub page: PublicPage,
    pub protection: ProtectionMask,
}

/// H4 prototype only. Selected coarse groups are restored from public fine data.
/// It never substitutes a fresh generator baseline for edited world voxels.
pub fn coarsen_page(page: &PublicPage, block_edge: u8, protect: bool) -> CoarsenedPage {
    let mut protection = protection_groups(page, block_edge);
    if !protect {
        protection.words.fill(0);
        protection.count = 0;
    }
    let step = usize::from(block_edge);
    let groups_edge = 16 / step;
    let mut output = PublicPage::filled(PublicVoxel::Unknown);
    for gy in 0..groups_edge {
        for gz in 0..groups_edge {
            for gx in 0..groups_edge {
                let group = gx + groups_edge * (gz + groups_edge * gy);
                let coarse = reduce((0..step).flat_map(|dy| {
                    (0..step).flat_map(move |dz| {
                        (0..step).map(move |dx| {
                            page.get([
                                (gx * step + dx) as u8,
                                (gy * step + dy) as u8,
                                (gz * step + dz) as u8,
                            ])
                        })
                    })
                }));
                for dy in 0..step {
                    for dz in 0..step {
                        for dx in 0..step {
                            let at = [
                                (gx * step + dx) as u8,
                                (gy * step + dy) as u8,
                                (gz * step + dz) as u8,
                            ];
                            output.set(
                                at,
                                if protection.contains(group) {
                                    page.get(at)
                                } else {
                                    coarse
                                },
                            );
                        }
                    }
                }
            }
        }
    }
    CoarsenedPage {
        page: output,
        protection,
    }
}

/// Mesh already-public data with an explicit exterior policy. Callers must pass
/// Unknown for an unavailable neighbour; tests may use known Air outside fixtures.
pub fn mesh_public_page(
    page: &PublicPage,
    exterior: PublicVoxel,
    scratch: &mut MeshScratch,
    output: &mut PageMesh,
) {
    let halo = PageHalo::filled(0);
    mesh_page_indexed(
        &halo,
        |_, [x, y, z]| {
            if (0..16).contains(&x) && (0..16).contains(&y) && (0..16).contains(&z) {
                page.get([x as u8, y as u8, z as u8]).material()
            } else {
                exterior.material()
            }
        },
        scratch,
        output,
    );
}

/// Pixel-error refinement state. Single-pixel camera jitter inside the band
/// cannot flip an existing choice on every frame.
#[derive(Clone, Copy, Debug)]
pub struct RefinementHysteresis {
    threshold_px: f64,
    band_px: f64,
    refined: bool,
}

impl RefinementHysteresis {
    pub fn new(threshold_px: f64, band_px: f64) -> Option<Self> {
        if !threshold_px.is_finite()
            || !band_px.is_finite()
            || threshold_px <= band_px
            || band_px < 0.0
        {
            return None;
        }
        Some(Self {
            threshold_px,
            band_px,
            refined: false,
        })
    }
    pub fn update(&mut self, projected_error_px: f64) -> bool {
        if !projected_error_px.is_finite() || projected_error_px > self.threshold_px + self.band_px
        {
            self.refined = true;
        } else if projected_error_px < self.threshold_px - self.band_px {
            self.refined = false;
        }
        self.refined
    }
    pub fn refined(&self) -> bool {
        self.refined
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClipmapTile {
    pub level: u8,
    pub x: i64,
    pub z: i64,
}

impl ClipmapTile {
    pub fn bounds_pages(self) -> Option<[i64; 4]> {
        if self.level > MAX_LEVEL {
            return None;
        }
        let size = 1_i64 << self.level;
        let x = self.x.checked_mul(size)?;
        let z = self.z.checked_mul(size)?;
        Some([x, z, x.checked_add(size)?, z.checked_add(size)?])
    }
}

#[derive(Clone, Debug)]
pub struct ClipmapPlan {
    pub tiles: Vec<ClipmapTile>,
    pub outer_bounds_pages: [i64; 4],
    pub max_level: u8,
    pub visited_nodes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    InvalidLevel,
    CoordinateOverflow,
    BudgetExceeded,
}

/// Dyadic coverage partition. Refinement follows distance, never iterates every
/// page inside the horizon; an additional distance octave adds a bounded ring.
pub fn clipmap_plan(
    camera_page: [i64; 2],
    max_level: u8,
    max_tiles: usize,
) -> Result<ClipmapPlan, PlanError> {
    if max_level > MAX_LEVEL || max_tiles < 16 {
        return Err(PlanError::InvalidLevel);
    }
    let size = 1_i64 << max_level;
    let center = [
        camera_page[0].div_euclid(size),
        camera_page[1].div_euclid(size),
    ];
    let minimum = [
        center[0]
            .checked_sub(2)
            .ok_or(PlanError::CoordinateOverflow)?,
        center[1]
            .checked_sub(2)
            .ok_or(PlanError::CoordinateOverflow)?,
    ];
    let maximum = [
        center[0]
            .checked_add(2)
            .ok_or(PlanError::CoordinateOverflow)?,
        center[1]
            .checked_add(2)
            .ok_or(PlanError::CoordinateOverflow)?,
    ];
    let outer_bounds_pages = [
        minimum[0]
            .checked_mul(size)
            .ok_or(PlanError::CoordinateOverflow)?,
        minimum[1]
            .checked_mul(size)
            .ok_or(PlanError::CoordinateOverflow)?,
        maximum[0]
            .checked_mul(size)
            .ok_or(PlanError::CoordinateOverflow)?,
        maximum[1]
            .checked_mul(size)
            .ok_or(PlanError::CoordinateOverflow)?,
    ];
    let mut pending = Vec::with_capacity(usize::from(max_level) * 16 + 16);
    for z in minimum[1]..maximum[1] {
        for x in minimum[0]..maximum[0] {
            pending.push(ClipmapTile {
                level: max_level,
                x,
                z,
            });
        }
    }
    let mut tiles = Vec::with_capacity(max_tiles.min(512));
    let mut visited_nodes = 0;
    while let Some(tile) = pending.pop() {
        visited_nodes += 1;
        let bounds = tile.bounds_pages().ok_or(PlanError::CoordinateOverflow)?;
        let span = 1_i64 << tile.level;
        let distance = |coordinate: i64, lo: i64, hi: i64| -> i128 {
            if coordinate < lo {
                i128::from(lo) - i128::from(coordinate)
            } else if coordinate >= hi {
                i128::from(coordinate) - i128::from(hi) + 1
            } else {
                0
            }
        };
        let close = distance(camera_page[0], bounds[0], bounds[2]).max(distance(
            camera_page[1],
            bounds[1],
            bounds[3],
        )) <= i128::from(span);
        if tile.level > 0 && close {
            for z in 0..2 {
                for x in 0..2 {
                    pending.push(ClipmapTile {
                        level: tile.level - 1,
                        x: tile
                            .x
                            .checked_mul(2)
                            .and_then(|v| v.checked_add(x))
                            .ok_or(PlanError::CoordinateOverflow)?,
                        z: tile
                            .z
                            .checked_mul(2)
                            .and_then(|v| v.checked_add(z))
                            .ok_or(PlanError::CoordinateOverflow)?,
                    });
                }
            }
        } else {
            tiles.push(tile);
        }
        if tiles.len() + pending.len() > max_tiles {
            return Err(PlanError::BudgetExceeded);
        }
    }
    tiles.sort_unstable();
    Ok(ClipmapPlan {
        tiles,
        outer_bounds_pages,
        max_level,
        visited_nodes,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    West,
    East,
    North,
    South,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LodSeam {
    pub tile: usize,
    pub neighbour: usize,
    pub edge: Edge,
    pub finer_level: u8,
}

impl ClipmapPlan {
    /// Edge adjacency is exact in integer page coordinates. Consumers can stitch
    /// or skirt only these unequal-resolution edges, rather than every draw.
    pub fn seams(&self) -> Vec<LodSeam> {
        let mut seams = Vec::new();
        for (index, tile) in self.tiles.iter().enumerate() {
            let a = tile.bounds_pages().expect("validated tile");
            for (other, neighbour) in self.tiles.iter().enumerate().skip(index + 1) {
                if tile.level == neighbour.level {
                    continue;
                }
                let b = neighbour.bounds_pages().expect("validated tile");
                let overlap_z = a[1].max(b[1]) < a[3].min(b[3]);
                let overlap_x = a[0].max(b[0]) < a[2].min(b[2]);
                let edge = if a[0] == b[2] && overlap_z {
                    Some(Edge::West)
                } else if a[2] == b[0] && overlap_z {
                    Some(Edge::East)
                } else if a[1] == b[3] && overlap_x {
                    Some(Edge::North)
                } else if a[3] == b[1] && overlap_x {
                    Some(Edge::South)
                } else {
                    None
                };
                if let Some(edge) = edge {
                    seams.push(LodSeam {
                        tile: index,
                        neighbour: other,
                        edge,
                        finer_level: tile.level.min(neighbour.level),
                    });
                }
            }
        }
        seams
    }

    pub fn exact_coverage(&self) -> bool {
        let outer = self.outer_bounds_pages;
        let expected = (i128::from(outer[2]) - i128::from(outer[0]))
            * (i128::from(outer[3]) - i128::from(outer[1]));
        let mut area = 0_i128;
        let mut seen = BTreeSet::new();
        for (index, tile) in self.tiles.iter().enumerate() {
            if !seen.insert(*tile) {
                return false;
            }
            let Some(a) = tile.bounds_pages() else {
                return false;
            };
            if a[0] < outer[0] || a[1] < outer[1] || a[2] > outer[2] || a[3] > outer[3] {
                return false;
            }
            area += (i128::from(a[2]) - i128::from(a[0])) * (i128::from(a[3]) - i128::from(a[1]));
            for other in self.tiles.iter().skip(index + 1) {
                let Some(b) = other.bounds_pages() else {
                    return false;
                };
                if a[0].max(b[0]) < a[2].min(b[2]) && a[1].max(b[1]) < a[3].min(b[3]) {
                    return false;
                }
            }
        }
        area == expected
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SelectionOptions {
    pub min_y_page: i64,
    pub max_y_page: i64,
    pub projection_pixels: f64,
    pub error_threshold_px: f64,
    pub hysteresis_px: f64,
    pub max_objects: usize,
    pub max_visits: usize,
    pub max_requests: usize,
}

impl Default for SelectionOptions {
    fn default() -> Self {
        Self {
            min_y_page: -4,
            max_y_page: 4,
            projection_pixels: 720.0,
            error_threshold_px: 2.0,
            hysteresis_px: 1.0,
            max_objects: 4096,
            max_visits: 16_384,
            max_requests: 4096,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometrySource {
    FinePage,
    SummaryGrid,
}

/// Compatible with the existing 16-byte descriptor plus object-table transform.
/// FinePage references the exact terrain mesh; SummaryGrid uses expanded_page().
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodObject {
    pub key: LodKey,
    pub revision: u64,
    pub origin_relative: [f32; 3],
    pub cell_scale: u32,
    pub geometry: GeometrySource,
}

#[derive(Clone, Debug, Default)]
pub struct ObjectSelection {
    pub objects: Vec<LodObject>,
    pub requests: Vec<LodKey>,
    /// The caller must retain a previous valid covering result for these nodes.
    pub deferred: Vec<LodKey>,
    pub coverage_complete: bool,
    pub visited_nodes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectError {
    InvalidOptions,
    CoordinateOverflow,
    InitialBudgetExceeded,
}

struct NodeChoice {
    state: RefinementHysteresis,
    epoch: u64,
}

/// Run selection on view/summary changes as a budgeted job, not every static frame.
#[derive(Default)]
pub struct SelectionState {
    choices: BTreeMap<LodKey, NodeChoice>,
    epoch: u64,
}

impl SelectionState {
    pub fn retained_choices(&self) -> usize {
        self.choices.len()
    }
    pub fn select(
        &mut self,
        forest: &LodForest,
        plan: &ClipmapPlan,
        space: u32,
        camera: CameraOrigin,
        options: SelectionOptions,
    ) -> Result<ObjectSelection, SelectError> {
        if options.min_y_page > options.max_y_page
            || options.max_visits == 0
            || options.max_objects == 0
            || options.max_requests == 0
            || !options.projection_pixels.is_finite()
            || options.projection_pixels <= 0.0
            || RefinementHysteresis::new(options.error_threshold_px, options.hysteresis_px)
                .is_none()
            || camera
                .fractional
                .into_iter()
                .any(|value| !value.is_finite() || !(0.0..1.0).contains(&value))
        {
            return Err(SelectError::InvalidOptions);
        }
        self.epoch = self.epoch.checked_add(1).unwrap_or_else(|| {
            self.choices.clear();
            1
        });
        let mut pending = Vec::new();
        for tile in &plan.tiles {
            let scale = 1_i64 << tile.level;
            let lo = options.min_y_page.div_euclid(scale);
            let hi = options.max_y_page.div_euclid(scale);
            let count = i128::from(hi) - i128::from(lo) + 1;
            if count > options.max_visits as i128
                || pending.len() as i128 + count > options.max_visits as i128
            {
                return Err(SelectError::InitialBudgetExceeded);
            }
            for y in lo..=hi {
                pending.push(LodKey {
                    space,
                    level: tile.level,
                    x: tile.x,
                    y,
                    z: tile.z,
                });
            }
        }
        let mut result = ObjectSelection {
            coverage_complete: true,
            ..ObjectSelection::default()
        };
        let mut requests = BTreeSet::new();
        while let Some(key) = pending.pop() {
            if result.visited_nodes >= options.max_visits {
                result.coverage_complete = false;
                result.deferred.push(key);
                result.deferred.append(&mut pending);
                break;
            }
            result.visited_nodes += 1;
            let Some(summary) = forest.get(key) else {
                result.coverage_complete = false;
                if requests.len() < options.max_requests {
                    requests.insert(key);
                } else {
                    result.deferred.push(key);
                }
                continue;
            };
            let origin = camera
                .relative(key)
                .ok_or(SelectError::CoordinateOverflow)?;
            let extent = key.extent_blocks().ok_or(SelectError::CoordinateOverflow)? as f64;
            let distance = origin
                .into_iter()
                .fold(0.0_f64, |distance, start| {
                    let start = f64::from(start);
                    let axis = if start > 0.0 {
                        start
                    } else if start + extent < 0.0 {
                        -start - extent
                    } else {
                        0.0
                    };
                    distance + axis * axis
                })
                .sqrt()
                .max(1.0);
            let projected = summary.projected_error(distance, options.projection_pixels);
            let choice = self.choices.entry(key).or_insert_with(|| NodeChoice {
                state: RefinementHysteresis::new(options.error_threshold_px, options.hysteresis_px)
                    .expect("validated options"),
                epoch: self.epoch,
            });
            choice.state.threshold_px = options.error_threshold_px;
            choice.state.band_px = options.hysteresis_px;
            choice.epoch = self.epoch;
            // Keep a protected one-block opening/beam exact while it is visible
            // at least one pixel. Do not promise that this costs <=25% primitives.
            let protected_visible =
                summary.protected_pages != 0 && options.projection_pixels / distance >= 1.0;
            let refine = !summary.complete || choice.state.update(projected) || protected_visible;
            if key.level > 0 && refine {
                if result.visited_nodes + pending.len() + 8 > options.max_visits {
                    result.coverage_complete = false;
                    result.deferred.push(key);
                    continue;
                }
                for index in 0..8 {
                    pending.push(key.child(index).ok_or(SelectError::CoordinateOverflow)?);
                }
                continue;
            }
            if !summary.complete {
                result.coverage_complete = false;
                if requests.len() < options.max_requests {
                    requests.insert(key);
                } else {
                    result.deferred.push(key);
                }
                continue;
            }
            if summary.occupied_cells == 0 {
                continue;
            }
            if result.objects.len() == options.max_objects {
                result.coverage_complete = false;
                result.deferred.push(key);
                continue;
            }
            result.objects.push(LodObject {
                key,
                revision: summary.revision,
                origin_relative: origin,
                cell_scale: 1_u32 << key.level,
                geometry: if key.level == 0 {
                    GeometrySource::FinePage
                } else {
                    GeometrySource::SummaryGrid
                },
            });
        }
        self.choices.retain(|_, choice| choice.epoch == self.epoch);
        result.objects.sort_unstable_by_key(|object| object.key);
        result.requests = requests.into_iter().collect();
        Ok(result)
    }
}

/// H4b CPU experiment: a fixed box plus voxel masks, moving work from geometry
/// into fragment traversal. This is not selected by the renderer or a claim that
/// the failed H4 <=25% primitive hypothesis has passed. A GPU implementation must
/// still measure shading, alpha/depth, page seams and actual pixel overdraw.
///
/// Only typed public samples enter this object. `from_protected` preserves fine
/// occupancy in the same protected groups as the H4 experiment; other groups use
/// its coarse public material. Unknown has a separate mask and stops traversal.
pub struct MaskedRayProxy {
    known: [u64; 64],
    occupied: [u64; 64],
    rank: [u16; 65],
    materials: Vec<u16>,
    palette: Vec<PublicVoxel>,
    pub protection: ProtectionMask,
}

pub const MAX_PROXY_RAY_STEPS: u16 = 46;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyMemory {
    pub inline_bytes: usize,
    pub heap_payload_bytes: usize,
    pub heap_capacity_bytes: usize,
}

impl ProxyMemory {
    pub fn resident_bytes(self) -> usize {
        self.inline_bytes + self.heap_capacity_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyRayError {
    NonFinite,
    ZeroDirection,
    InvalidDistance,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProxyTrace {
    Miss {
        steps: u16,
    },
    Hit {
        cell: [u8; 3],
        voxel: PublicVoxel,
        t: f64,
        normal: [i8; 3],
        steps: u16,
    },
    Unknown {
        cell: [u8; 3],
        t: f64,
        steps: u16,
    },
    BudgetExceeded {
        steps: u16,
    },
}

impl ProxyTrace {
    pub const fn steps(self) -> u16 {
        match self {
            Self::Miss { steps }
            | Self::Hit { steps, .. }
            | Self::Unknown { steps, .. }
            | Self::BudgetExceeded { steps } => steps,
        }
    }
}

impl MaskedRayProxy {
    pub fn from_protected(page: &PublicPage, block_edge: u8) -> Self {
        let guarded = coarsen_page(page, block_edge, true);
        Self::from_public_page(&guarded.page, guarded.protection)
    }

    /// Exact occupancy/material masks for this already-public page. Rank-indexed
    /// palette entries retain material, RGBA tint and render class, including full
    /// u16 layer IDs. No underlying authoritative voxel/seed is accepted.
    pub fn from_public_page(page: &PublicPage, protection: ProtectionMask) -> Self {
        let mut result = Self {
            known: [0; 64],
            occupied: [0; 64],
            rank: [0; 65],
            materials: Vec::new(),
            palette: Vec::new(),
            protection,
        };
        let mut palette = BTreeMap::new();
        for (index, voxel) in page.cells.iter().copied().enumerate() {
            let bit = 1 << (index % 64);
            if voxel.known() {
                result.known[index / 64] |= bit;
            }
            if voxel.occupied() {
                result.occupied[index / 64] |= bit;
                let layer = *palette.entry(voxel).or_insert_with(|| {
                    let slot = result.palette.len() as u16;
                    result.palette.push(voxel);
                    slot
                });
                result.materials.push(layer);
            }
        }
        for word in 0..64 {
            result.rank[word + 1] = result.rank[word] + result.occupied[word].count_ones() as u16;
        }
        result
    }

    pub fn memory(&self) -> ProxyMemory {
        ProxyMemory {
            inline_bytes: std::mem::size_of::<Self>(),
            heap_payload_bytes: self.materials.len() * std::mem::size_of::<u16>()
                + self.palette.len() * std::mem::size_of::<PublicVoxel>(),
            heap_capacity_bytes: self.materials.capacity() * std::mem::size_of::<u16>()
                + self.palette.capacity() * std::mem::size_of::<PublicVoxel>(),
        }
    }

    pub fn occupied_cells(&self) -> usize {
        usize::from(self.rank[64])
    }

    pub fn voxel(&self, cell: [u8; 3]) -> PublicVoxel {
        assert!(cell.iter().all(|axis| *axis < 16));
        let index = usize::from(cell[0]) + 16 * (usize::from(cell[2]) + 16 * usize::from(cell[1]));
        let word = index / 64;
        let bit = 1_u64 << (index % 64);
        if self.known[word] & bit == 0 {
            return PublicVoxel::Unknown;
        }
        if self.occupied[word] & bit == 0 {
            return PublicVoxel::Air;
        }
        let preceding = self.occupied[word] & (bit - 1);
        let rank = usize::from(self.rank[word]) + preceding.count_ones() as usize;
        self.palette[usize::from(self.materials[rank])]
    }

    /// Six proxy faces only; shader traversal would select the actual material
    /// and depth. These placeholder layer-0 descriptors are not terrain surfaces.
    pub fn proxy_quads(&self) -> [crate::terrain_mesh::QuadDescriptor; 6] {
        use crate::terrain_mesh::{Face, QuadDescriptor};
        let origins = [
            [0, 0, 0],
            [0, 16, 16],
            [16, 0, 0],
            [0, 0, 16],
            [0, 0, 0],
            [16, 0, 16],
        ];
        std::array::from_fn(|face| {
            QuadDescriptor::new(
                origins[face],
                Face::ALL[face],
                [16, 16],
                0,
                [3; 4],
                u32::MAX,
            )
            .expect("fixed proxy box is page-local")
        })
    }

    /// Page-local Amanatides/Woo traversal, bounded independently of world size.
    /// `t` uses the supplied direction scale; normalize it to measure distances.
    /// At simultaneous grid crossings all tied axes advance, so edge-only touches
    /// do not invent occupied cells. Unknown/budget exits must retain/request a
    /// prior valid representation instead of being rendered as empty space.
    pub fn trace(
        &self,
        origin: [f64; 3],
        direction: [f64; 3],
        max_t: f64,
        step_budget: u16,
    ) -> Result<ProxyTrace, ProxyRayError> {
        if origin
            .into_iter()
            .chain(direction)
            .any(|value| !value.is_finite())
        {
            return Err(ProxyRayError::NonFinite);
        }
        if !max_t.is_finite() || max_t <= 0.0 {
            return Err(ProxyRayError::InvalidDistance);
        }
        if direction == [0.0; 3] {
            return Err(ProxyRayError::ZeroDirection);
        }
        let mut enter = 0.0_f64;
        let mut exit = max_t;
        let mut normal = [0; 3];
        for axis in 0..3 {
            if direction[axis] == 0.0 {
                if !(0.0..16.0).contains(&origin[axis]) {
                    return Ok(ProxyTrace::Miss { steps: 0 });
                }
                continue;
            }
            if !direction[axis].recip().is_finite() {
                return Err(ProxyRayError::NonFinite);
            }
            let a = -origin[axis] / direction[axis];
            let b = (16.0 - origin[axis]) / direction[axis];
            let near = a.min(b);
            if near > enter || (near == enter && normal == [0; 3]) {
                enter = near;
                normal = [0; 3];
                normal[axis] = if direction[axis] > 0.0 { -1 } else { 1 };
            }
            exit = exit.min(a.max(b));
        }
        if exit <= enter {
            return Ok(ProxyTrace::Miss { steps: 0 });
        }
        let mut cell = std::array::from_fn::<_, 3, _>(|axis| {
            let position = origin[axis] + direction[axis] * enter;
            let floor = position.floor();
            let coordinate = if direction[axis] < 0.0 && position == floor {
                floor - 1.0
            } else {
                floor
            };
            (coordinate as i32).clamp(0, 15)
        });
        let mut next = std::array::from_fn::<_, 3, _>(|axis| {
            if direction[axis] == 0.0 {
                f64::INFINITY
            } else {
                let boundary = cell[axis] + i32::from(direction[axis] > 0.0);
                ((f64::from(boundary) - origin[axis]) / direction[axis]).max(enter)
            }
        });
        let delta = direction.map(|value| {
            if value == 0.0 {
                f64::INFINITY
            } else {
                value.recip().abs()
            }
        });
        let budget = step_budget.min(MAX_PROXY_RAY_STEPS);
        let mut t = enter;
        for step in 0..budget {
            let at = cell.map(|axis| axis as u8);
            let steps = step + 1;
            match self.voxel(at) {
                PublicVoxel::Unknown => return Ok(ProxyTrace::Unknown { cell: at, t, steps }),
                PublicVoxel::Air => {}
                voxel => {
                    return Ok(ProxyTrace::Hit {
                        cell: at,
                        voxel,
                        t,
                        normal,
                        steps,
                    });
                }
            }
            let crossing = next.into_iter().fold(f64::INFINITY, f64::min);
            if crossing >= exit {
                return Ok(ProxyTrace::Miss { steps });
            }
            normal = [0; 3];
            for axis in 0..3 {
                if next[axis] == crossing {
                    let sign = if direction[axis] > 0.0 { 1 } else { -1 };
                    cell[axis] += sign;
                    if normal == [0; 3] {
                        normal[axis] = -sign as i8;
                    }
                    next[axis] += delta[axis];
                }
            }
            if cell.iter().any(|axis| !(0..16).contains(axis)) {
                return Ok(ProxyTrace::Miss { steps });
            }
            t = crossing;
        }
        Ok(ProxyTrace::BudgetExceeded { steps: budget })
    }
}
