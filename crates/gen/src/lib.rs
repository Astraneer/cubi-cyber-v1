//! Public version-4 terrain generation, evaluated only by explicit background tasks.
//!
//! This package contains no ore placement or underground secret generation. Those
//! live in the separate server-only `gen-authority` package. Scratch columns keep
//! the donor's profile-4 dimensions for exact numerical compatibility; published
//! world pages remain 16³ and have signed, unrestricted Y coordinates.
#![forbid(unsafe_code)]

use core_types::{Cell, PAGE_VOLUME, PageId, Version};
use std::{
    cell::Cell as Counter,
    ops::{Index, IndexMut},
    sync::Arc,
};

pub mod public_error;
pub mod public_terrain;
mod v4;
mod wire;
pub use wire::{RESULT_BYTES, TASK_BYTES, WireError};

pub const GENERATOR_VERSION: u32 = v4::GEN_VERSION;
pub const PROFILE_HEIGHT: i32 = 128;
pub const SEA_LEVEL: i32 = 48;
pub const SPAWN_DAY_TIME: f64 = v4::spawn::SKY_DAY;
pub const SPAWN_NIGHT_TIME: f64 = v4::spawn::SKY_NIGHT;
pub const COLUMN_EDGE: i32 = 16;
pub const SCRATCH_EDGE: i32 = 18;
pub const SCRATCH_PADDING: i32 = 1;
pub const PROBE_SEED: u32 = v4::PROBE_SEED;
pub const PROBE_COLUMNS: [(i32, i32); 4] = v4::PROBE_CHUNKS;

/// Public terrain entropy; it carries no information about private ore entropy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainSeed(pub u32);

/// Actual scratch accesses, not an estimate of GPU time or CPU milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenerationWork {
    pub scratch_reads: u64,
    pub scratch_writes: u64,
    pub sampled_columns: u64,
    pub copied_cells: u64,
}

/// Bounded profile-4 scratch bytes. Every indexed generator access is counted.
/// This temporary generator buffer is never the persistent world representation.
#[derive(Debug)]
pub struct DraftBlocks {
    bytes: Vec<u8>,
    reads: Counter<u64>,
    writes: u64,
}
impl DraftBlocks {
    fn new(len: usize) -> Self {
        Self {
            bytes: vec![0; len],
            reads: Counter::new(0),
            writes: len as u64,
        }
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    pub fn iter(&self) -> std::slice::Iter<'_, u8> {
        self.bytes.iter()
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    fn work(&self) -> GenerationWork {
        GenerationWork {
            scratch_reads: self.reads.get(),
            scratch_writes: self.writes,
            sampled_columns: 400,
            copied_cells: 0,
        }
    }
}
impl Index<usize> for DraftBlocks {
    type Output = u8;
    fn index(&self, i: usize) -> &Self::Output {
        self.reads.set(self.reads.get().saturating_add(1));
        &self.bytes[i]
    }
}
impl IndexMut<usize> for DraftBlocks {
    fn index_mut(&mut self, i: usize) -> &mut Self::Output {
        self.writes = self.writes.saturating_add(1);
        &mut self.bytes[i]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationError {
    CoordinateOutOfRange,
    WrongGenerator,
    WrongColumn,
    Cancelled,
}
impl std::fmt::Display for GenerationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GenerationError {}

/// Opaque terrain before vegetation/spawn decoration. The server can insert
/// private features before final decoration without exposing their algorithm.
pub struct TerrainDraft {
    seed: TerrainSeed,
    column: [i32; 2],
    region: v4::Region,
    columns: Vec<v4::Column>,
}
impl TerrainDraft {
    pub fn origin(&self) -> [i32; 2] {
        [self.region.x0, self.region.z0]
    }
    pub fn blocks(&self) -> &DraftBlocks {
        &self.region.blocks
    }
    pub fn blocks_mut(&mut self) -> &mut DraftBlocks {
        &mut self.region.blocks
    }
    pub fn height_field(&self) -> Vec<i32> {
        let cw = (SCRATCH_EDGE + 2) as usize;
        (0..SCRATCH_EDGE)
            .flat_map(|z| {
                (0..SCRATCH_EDGE)
                    .map(move |x| self.columns[(z as usize + 1) * cw + x as usize + 1].h)
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct Generator {
    seed: TerrainSeed,
    inner: Arc<v4::Gen>,
}
impl Generator {
    pub fn new(seed: TerrainSeed) -> Self {
        Self {
            seed,
            inner: Arc::new(v4::Gen::new(seed.0)),
        }
    }
    pub fn seed(&self) -> TerrainSeed {
        self.seed
    }
    pub fn spawn(&self) -> Spawn {
        Spawn {
            center: [self.inner.site.x, self.inner.site.y0, self.inner.site.z],
            protection_radius: 32,
            gate: self.inner.site.gate_box(),
            parkour: self.inner.site.parkour().to_vec(),
        }
    }
    pub fn column_info(&self, x: i32, z: i32) -> ColumnInfo {
        let c = self.inner.column(x, z);
        ColumnInfo {
            height: c.h,
            tint: v4::rgb565(v4::grass_tint(&c)),
            biome: v4::biome_name(&c),
        }
    }
    pub fn protects_spawn(&self, x: i32, z: i32) -> bool {
        self.inner.site.protects(x, z)
    }
    pub fn sky_button(&self, position: [i32; 3]) -> Option<bool> {
        self.inner.site.sky_button(position)
    }
    pub fn in_gate(&self, position: [i32; 3]) -> bool {
        self.inner.site.in_gate(position)
    }
    pub fn draft_column(&self, cx: i32, cz: i32) -> Result<TerrainDraft, GenerationError> {
        check_column(cx, cz)?;
        let (region, columns) = v4::base(&self.inner, cx, cz);
        Ok(TerrainDraft {
            seed: self.seed,
            column: [cx, cz],
            region,
            columns,
        })
    }
    pub fn finish_column(&self, draft: TerrainDraft) -> Result<GeneratedColumn, GenerationError> {
        if draft.seed != self.seed {
            return Err(GenerationError::WrongGenerator);
        }
        let region = v4::decorate(&self.inner, draft.region, draft.columns);
        let legacy_hash = v4::region_hash(&region);
        let work = region.blocks.work();
        Ok(GeneratedColumn {
            column: draft.column,
            blocks: region.blocks.bytes,
            tints: region.tint,
            work,
            legacy_hash,
        })
    }
    pub fn generate_column(&self, cx: i32, cz: i32) -> Result<GeneratedColumn, GenerationError> {
        self.finish_column(self.draft_column(cx, cz)?)
    }
    pub fn generate_page(
        &self,
        page: PageId,
        expected: Option<Version>,
    ) -> Result<GeneratedPage, GenerationError> {
        self.generate_column(page.x, page.z)?.page(page, expected)
    }
}

fn check_column(x: i32, z: i32) -> Result<(), GenerationError> {
    // Intermediate tree and spawn probes require a small margin around scratch.
    const MIN: i32 = (i32::MIN + 4096) / COLUMN_EDGE;
    const MAX: i32 = (i32::MAX - 4096) / COLUMN_EDGE;
    if !(MIN..=MAX).contains(&x) || !(MIN..=MAX).contains(&z) {
        Err(GenerationError::CoordinateOutOfRange)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spawn {
    pub center: [i32; 3],
    pub protection_radius: i32,
    pub gate: [i32; 6],
    pub parkour: Vec<(i32, i32, i32)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnInfo {
    pub height: i32,
    pub tint: u16,
    pub biome: &'static str,
}

/// Immutable completed scratch column. One generation can feed all eight profile
/// pages; callers should reuse it instead of regenerating each vertical page.
pub struct GeneratedColumn {
    column: [i32; 2],
    blocks: Vec<u8>,
    tints: Vec<u16>,
    pub work: GenerationWork,
    legacy_hash: u64,
}
impl GeneratedColumn {
    pub fn legacy_hash(&self) -> u64 {
        self.legacy_hash
    }
    pub fn column(&self) -> [i32; 2] {
        self.column
    }
    pub fn block_padded(&self, x: i32, y: i32, z: i32) -> u16 {
        if !(0..SCRATCH_EDGE).contains(&x)
            || !(0..SCRATCH_EDGE).contains(&z)
            || !(0..PROFILE_HEIGHT).contains(&y)
        {
            return 0;
        }
        self.blocks[((z * SCRATCH_EDGE + x) * PROFILE_HEIGHT + y) as usize] as u16
    }
    pub fn raw_blocks(&self) -> &[u8] {
        &self.blocks
    }
    pub fn tints(&self) -> &[u16] {
        &self.tints
    }
    pub fn page(
        &self,
        page: PageId,
        expected: Option<Version>,
    ) -> Result<GeneratedPage, GenerationError> {
        if [page.x, page.z] != self.column {
            return Err(GenerationError::WrongColumn);
        }
        let mut cells = Box::new([Cell::AIR; PAGE_VOLUME]);
        let mut tints = Box::new([0u16; 256]);
        for z in 0..16i32 {
            for x in 0..16i32 {
                tints[(z * 16 + x) as usize] =
                    self.tints[((z + 1) * SCRATCH_EDGE + x + 1) as usize];
                for y in 0..16i32 {
                    let world_y = i64::from(page.y) * 16 + i64::from(y);
                    let block = if (0..i64::from(PROFILE_HEIGHT)).contains(&world_y) {
                        self.block_padded(x + 1, world_y as i32, z + 1)
                    } else {
                        0
                    };
                    cells[(x + z * 16 + y * 256) as usize] = Cell::from_parts(block, 0);
                }
            }
        }
        let mut work = self.work;
        work.copied_cells = PAGE_VOLUME as u64;
        Ok(GeneratedPage {
            page,
            expected,
            generator_version: GENERATOR_VERSION,
            cells,
            tints,
            work,
        })
    }
}

#[derive(Clone, Debug)]
pub struct GeneratedPage {
    pub page: PageId,
    pub expected: Option<Version>,
    pub generator_version: u32,
    pub cells: Box<[Cell; PAGE_VOLUME]>,
    pub tints: Box<[u16; 256]>,
    pub work: GenerationWork,
}
impl GeneratedPage {
    /// A stable byte-order hash of placement, version and all visible page data.
    pub fn canonical_hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |bytes: &[u8]| {
            for &b in bytes {
                h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
            }
        };
        eat(&self.page.space.0.to_le_bytes());
        for c in [self.page.x, self.page.y, self.page.z] {
            eat(&c.to_le_bytes());
        }
        eat(&self.generator_version.to_le_bytes());
        match self.expected {
            Some(v) => {
                eat(&[1]);
                eat(&v.0.to_le_bytes());
            }
            None => eat(&[0]),
        }
        for c in self.cells.iter() {
            eat(&c.raw().to_le_bytes());
        }
        for t in self.tints.iter() {
            eat(&t.to_le_bytes());
        }
        h
    }
}

/// Public task payload. No authority flag or private seed can enter a client job.
#[derive(Clone, Copy, Debug)]
pub struct GenerationTask {
    pub terrain_seed: TerrainSeed,
    pub page: PageId,
    pub expected: Option<Version>,
}
impl GenerationTask {
    pub fn execute(self, cancel: &jobs::CancellationToken) -> Result<GeneratedPage, String> {
        self.execute_with_cancellation(|| cancel.is_cancelled())
    }
    pub fn execute_with_cancellation(
        self,
        cancel: impl Fn() -> bool,
    ) -> Result<GeneratedPage, String> {
        if cancel() {
            return Err(GenerationError::Cancelled.to_string());
        }
        let generator = worker_generator(self.terrain_seed);
        let draft = generator
            .draft_column(self.page.x, self.page.z)
            .map_err(|e| e.to_string())?;
        if cancel() {
            return Err(GenerationError::Cancelled.to_string());
        }
        let column = generator.finish_column(draft).map_err(|e| e.to_string())?;
        if cancel() {
            return Err(GenerationError::Cancelled.to_string());
        }
        column
            .page(self.page, self.expected)
            .map_err(|e| e.to_string())
    }
    pub fn job_spec(self, priority: jobs::Priority, tick: core_types::Tick) -> jobs::JobSpec {
        // Cells here are bounded input/output slots. Actual scratch read/write
        // counts are reported separately by GeneratedPage::work.
        jobs::JobSpec::derived(
            jobs::JobKey {
                page: self.page,
                kind: 1,
            },
            priority,
            jobs::WorkCost {
                cells: (SCRATCH_EDGE * SCRATCH_EDGE * PROFILE_HEIGHT) as u64 + PAGE_VOLUME as u64,
                ..jobs::WorkCost::ZERO
            },
            128 * 1024,
            tick,
        )
        .with_input(self.page, self.expected)
    }
}
thread_local! {static WORKER_GENERATOR:std::cell::RefCell<Option<Generator>>=const {std::cell::RefCell::new(None)};}
fn worker_generator(seed: TerrainSeed) -> Generator {
    WORKER_GENERATOR.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache
            .as_ref()
            .is_none_or(|generator| generator.seed() != seed)
        {
            *cache = Some(Generator::new(seed));
        }
        cache.as_ref().expect("initialized generator").clone()
    })
}
#[cfg(not(target_arch = "wasm32"))]
pub fn native_pool(
    workers: usize,
    capacity: usize,
) -> Result<jobs::NativePool<GenerationTask, GeneratedPage>, jobs::PoolError> {
    jobs::NativePool::new(workers, capacity, GenerationTask::execute)
}

/// Stable pure primitives shared with the separate server numerical kernel.
/// They do not contain any private feature distribution or private entropy.
pub mod compatibility {
    pub use crate::v4::noise::{Rng, hash};
}

pub fn public_probe() -> [u64; 4] {
    let generator = Generator::new(TerrainSeed(PROBE_SEED));
    PROBE_COLUMNS.map(|(x, z)| {
        generator
            .generate_column(x, z)
            .expect("fixed probe coordinates valid")
            .legacy_hash()
    })
}
