//! Bounded light-to-GPU invalidation. Geometry is never touched here.
//!
//! The journal owner feeds entries individually and retains its offset on
//! backpressure. A color change touches at most eight page/halo volumes. GPU
//! publication is acknowledged only after its upload succeeds.
use core_types::{BlockPos, LocalIdx, PageId, Version};
use render::light_volume::{LIGHT_SAMPLES, LightUpdate, PackedLight, light_index};
use std::collections::BTreeMap;
use world::{LightChange, World};
const WORDS: usize = LIGHT_SAMPLES.div_ceil(64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightBackpressure;
struct Dirty {
    version: Version,
    full: bool,
    cells: [u64; WORDS],
    order: u64,
}
impl Dirty {
    fn new(version: Version, order: u64) -> Self {
        Self {
            version,
            full: false,
            cells: [0; WORDS],
            order,
        }
    }
}
pub struct LightDirtyTracker {
    max_pages: usize,
    pages: BTreeMap<PageId, Dirty>,
    ordered: BTreeSet<(u64, PageId)>,
    next_order: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightSampleWork {
    pub known_reads: u64,
    pub unknown_reads: u64,
    pub samples: u64,
}
pub struct LightUploadScratch {
    pub samples: Box<[PackedLight; LIGHT_SAMPLES]>,
    pub updates: Vec<LightUpdate>,
}
impl Default for LightUploadScratch {
    fn default() -> Self {
        Self {
            samples: Box::new([PackedLight::default(); LIGHT_SAMPLES]),
            updates: Vec::with_capacity(LIGHT_SAMPLES),
        }
    }
}
pub enum PreparedLight<'a> {
    Full {
        page: PageId,
        version: Version,
        samples: &'a [PackedLight; LIGHT_SAMPLES],
        work: LightSampleWork,
    },
    Patch {
        page: PageId,
        version: Version,
        updates: &'a [LightUpdate],
        work: LightSampleWork,
    },
}
impl LightDirtyTracker {
    pub fn new(max_pages: usize) -> Self {
        Self {
            max_pages,
            pages: BTreeMap::new(),
            ordered: BTreeSet::new(),
            next_order: 0,
        }
    }
    pub fn pending_pages(&self) -> usize {
        self.pages.len()
    }
    pub fn pending_version(&self, page: PageId) -> Option<Version> {
        self.pages.get(&page).map(|dirty| dirty.version)
    }
    pub fn first_page(&self) -> Option<PageId> {
        self.ordered.first().map(|(_, page)| *page)
    }
    /// Metadata only: skipped resident entries do not block later nonresidents.
    /// The cursor bounds each call and keeps progress across repeated GPU waits.
    fn retire_nonresident(
        &mut self,
        cursor: &mut Option<(u64, PageId)>,
        limit: usize,
        mut resident: impl FnMut(PageId) -> bool,
    ) -> usize {
        use std::ops::Bound::{Excluded, Unbounded};
        let mut retired = 0;
        for _ in 0..limit.min(self.pages.len()) {
            let next = cursor
                .and_then(|at| {
                    self.ordered
                        .range((Excluded(at), Unbounded))
                        .next()
                        .copied()
                })
                .or_else(|| self.ordered.first().copied());
            let Some((order, page)) = next else {
                *cursor = None;
                break;
            };
            *cursor = Some((order, page));
            if !resident(page) {
                let version = self.pages[&page].version;
                self.acknowledge(page, version);
                retired += 1;
            }
        }
        retired
    }
    /// New/reused resident slots always need a full snapshot, even at version 0.
    pub fn mark_full(&mut self, page: PageId, version: Version) -> Result<(), LightBackpressure> {
        self.preflight(&[page])?;
        let dirty = self.ensure(page, version);
        dirty.version = dirty.version.max(version);
        dirty.full = true;
        Ok(())
    }
    /// Page creation/removal changes the neighboring light halo. A caller can
    /// ignore nonresident pages while draining; no world scan is needed.
    pub fn mark_page_boundary(
        &mut self,
        page: PageId,
        version: Version,
    ) -> Result<(), LightBackpressure> {
        let mut targets = [page; 27];
        let mut count = 0;
        for y in -1..=1 {
            for z in -1..=1 {
                for x in -1..=1 {
                    if let (Some(x), Some(y), Some(z)) = (
                        page.x.checked_add(x),
                        page.y.checked_add(y),
                        page.z.checked_add(z),
                    ) {
                        targets[count] = PageId {
                            space: page.space,
                            x,
                            y,
                            z,
                        };
                        count += 1;
                    }
                }
            }
        }
        self.preflight(&targets[..count])?;
        for target in targets[..count].iter().copied() {
            self.mark_full(target, version)
                .expect("preflighted page capacity");
        }
        Ok(())
    }
    pub fn mark_light(
        &mut self,
        change: &LightChange,
        version: Version,
    ) -> Result<u32, LightBackpressure> {
        let mut targets = [(change.page, 0usize); 8];
        let count = light_receivers(change.page, change.index, &mut targets);
        let pages = targets.map(|(page, _)| page);
        self.preflight(&pages[..count])?;
        for (page, index) in targets[..count].iter().copied() {
            let dirty = self.ensure(page, version);
            dirty.version = dirty.version.max(version);
            dirty.cells[index / 64] |= 1 << (index % 64);
        }
        Ok(count as u32)
    }
    fn ensure(&mut self, page: PageId, version: Version) -> &mut Dirty {
        if !self.pages.contains_key(&page) {
            let order = self.next_order;
            self.next_order += 1;
            self.pages.insert(page, Dirty::new(version, order));
            self.ordered.insert((order, page));
        }
        self.pages.get_mut(&page).expect("retained dirty page")
    }
    fn preflight(&self, targets: &[PageId]) -> Result<(), LightBackpressure> {
        let additional = targets
            .iter()
            .enumerate()
            .filter(|(i, page)| !targets[..*i].contains(page) && !self.pages.contains_key(page))
            .count();
        if self.pages.len() + additional > self.max_pages
            || self.next_order.checked_add(additional as u64).is_none()
        {
            Err(LightBackpressure)
        } else {
            Ok(())
        }
    }
    pub fn prepare<'a>(
        &self,
        world: &World,
        page: PageId,
        scratch: &'a mut LightUploadScratch,
    ) -> Option<PreparedLight<'a>> {
        self.prepare_filtered(world, page, scratch, |_| true)
    }
    pub fn prepare_filtered<'a>(
        &self,
        world: &World,
        page: PageId,
        scratch: &'a mut LightUploadScratch,
        known: impl Fn(PageId) -> bool,
    ) -> Option<PreparedLight<'a>> {
        let dirty = self.pages.get(&page)?;
        let mut work = LightSampleWork::default();
        if dirty.full {
            for (index, value) in scratch.samples.iter_mut().enumerate() {
                *value = sample(world, page, index, &mut work, &known);
            }
            Some(PreparedLight::Full {
                page,
                version: dirty.version,
                samples: &scratch.samples,
                work,
            })
        } else {
            scratch.updates.clear();
            for (word_index, word) in dirty.cells.iter().copied().enumerate() {
                let mut bits = word;
                while bits != 0 {
                    let bit = bits.trailing_zeros() as usize;
                    let index = word_index * 64 + bit;
                    if index < LIGHT_SAMPLES {
                        scratch.updates.push(LightUpdate {
                            index: index as u16,
                            value: sample(world, page, index, &mut work, &known),
                        });
                    }
                    bits &= bits - 1;
                }
            }
            Some(PreparedLight::Patch {
                page,
                version: dirty.version,
                updates: &scratch.updates,
                work,
            })
        }
    }
    /// A later dirty event must survive acknowledgement of an older upload.
    pub fn acknowledge(&mut self, page: PageId, version: Version) -> bool {
        if self
            .pages
            .get(&page)
            .is_some_and(|dirty| dirty.version == version)
        {
            if let Some(dirty) = self.pages.remove(&page) {
                self.ordered.remove(&(dirty.order, page));
            }
            if self.pages.is_empty() {
                self.next_order = 0;
            }
            true
        } else {
            false
        }
    }
}
fn light_receivers(page: PageId, index: LocalIdx, output: &mut [(PageId, usize); 8]) -> usize {
    let (x, y, z) = index.xyz();
    let local = [i32::from(x), i32::from(y), i32::from(z)];
    let offsets = local.map(|v| {
        if v == 0 {
            [0, -1]
        } else if v == 15 {
            [0, 1]
        } else {
            [0, 0]
        }
    });
    let counts = local.map(|v| if v == 0 || v == 15 { 2 } else { 1 });
    let mut count = 0;
    for iy in 0..counts[1] {
        for iz in 0..counts[2] {
            for ix in 0..counts[0] {
                let offset = [offsets[0][ix], offsets[1][iy], offsets[2][iz]];
                if let (Some(x), Some(y), Some(z)) = (
                    page.x.checked_add(offset[0]),
                    page.y.checked_add(offset[1]),
                    page.z.checked_add(offset[2]),
                ) {
                    let receiver = PageId {
                        space: page.space,
                        x,
                        y,
                        z,
                    };
                    let position = std::array::from_fn(|axis| local[axis] - offset[axis] * 16);
                    output[count] = (
                        receiver,
                        light_index(position).expect("neighbor halo index"),
                    );
                    count += 1;
                }
            }
        }
    }
    count
}
fn sample(
    world: &World,
    page: PageId,
    index: usize,
    work: &mut LightSampleWork,
    known: &impl Fn(PageId) -> bool,
) -> PackedLight {
    work.samples += 1;
    let local = [
        (index % 18) as i64 - 1,
        (index / (18 * 18)) as i64 - 1,
        ((index / 18) % 18) as i64 - 1,
    ];
    let world_coords = [
        i64::from(page.x) * 16 + local[0],
        i64::from(page.y) * 16 + local[1],
        i64::from(page.z) * 16 + local[2],
    ];
    let (Ok(x), Ok(y), Ok(z)) = (
        i32::try_from(world_coords[0]),
        i32::try_from(world_coords[1]),
        i32::try_from(world_coords[2]),
    ) else {
        work.unknown_reads += 1;
        return PackedLight::default();
    };
    let position = BlockPos {
        space: page.space,
        x,
        y,
        z,
    };
    if known(position.page())
        && let Some(page) = world.page(position.page())
    {
        work.known_reads += 1;
        let light = page.light(position.local_index());
        PackedLight::new(light.sky, light.block, light.rgb).expect("world light validates 0..15")
    } else {
        work.unknown_reads += 1;
        PackedLight::default()
    }
}

use core_types::{ChannelMask, SpaceId, Tick};
use render::{
    light_volume::{LightUploadError, LightVolumeGpu},
    shadows::{ShadowConfig, ShadowError, ShadowGpu, ShadowWork, WorldBounds},
    terrain_gpu::{PageHandle, TerrainError, TerrainGpu, UploadBudget},
};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::Arc,
};
use world::{
    CursorError, CursorId, WorldChange,
    lighting::{
        LightBudget, LightIngestBudget, LightPriority, LightingEngine, LightingError,
        LightingLimits, SkyAccess,
    },
};

/// This is an explicit authority statement, never inferred from missing pages.
#[derive(Clone, Copy, Debug)]
pub enum SkyPolicy {
    UnknownIsDark,
    /// The standalone generator certifies air above its finite profile. Every
    /// later opacity edit above this height is retained, including across unload.
    GeneratedProfile {
        space: SpaceId,
        top_y: i32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightSource {
    LocalDerived,
    Authoritative,
}
#[derive(Clone, Copy, Debug)]
pub struct ClientLightingConfig {
    pub source: LightSource,
    pub shadows: ShadowConfig,
    pub sky: SkyPolicy,
    pub cpu_limits: LightingLimits,
    pub cpu_budget: LightBudget,
    pub ingest_budget: LightIngestBudget,
    pub max_journal_entries: usize,
    pub max_gpu_pages_per_pump: usize,
    pub max_pending_light_pages: usize,
    pub max_bootstrap_pages: usize,
    pub max_sky_blockers: usize,
}
impl Default for ClientLightingConfig {
    fn default() -> Self {
        Self {
            source: LightSource::LocalDerived,
            shadows: ShadowConfig::default(),
            sky: SkyPolicy::UnknownIsDark,
            cpu_limits: LightingLimits::default(),
            cpu_budget: LightBudget::default(),
            ingest_budget: LightIngestBudget::default(),
            max_journal_entries: 2048,
            max_gpu_pages_per_pump: 4,
            max_pending_light_pages: 1024,
            max_bootstrap_pages: 1024,
            max_sky_blockers: 65536,
        }
    }
}
impl ClientLightingConfig {
    /// Replica mode never creates or runs a CPU solver and never certifies sky.
    pub fn with_authoritative_source(mut self) -> Self {
        self.source = LightSource::Authoritative;
        self.sky = SkyPolicy::UnknownIsDark;
        self
    }
    pub fn standalone() -> Self {
        Self {
            sky: SkyPolicy::GeneratedProfile {
                space: SpaceId(0),
                top_y: terrain_gen::PROFILE_HEIGHT - 1,
            },
            ..Self::default()
        }
    }
}
#[derive(Debug)]
pub enum ClientLightingError {
    InvalidConfig,
    Cursor(CursorError),
    Cpu(LightingError),
    Shadow(ShadowError),
    Terrain(TerrainError),
    Upload(LightUploadError),
    Backpressure,
    CoordinateOverflow,
    SkyBlockerCapacity,
}
impl std::fmt::Display for ClientLightingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client lighting: {self:?}")
    }
}
impl std::error::Error for ClientLightingError {}
impl From<CursorError> for ClientLightingError {
    fn from(e: CursorError) -> Self {
        Self::Cursor(e)
    }
}
impl From<LightingError> for ClientLightingError {
    fn from(e: LightingError) -> Self {
        Self::Cpu(e)
    }
}
impl From<ShadowError> for ClientLightingError {
    fn from(e: ShadowError) -> Self {
        Self::Shadow(e)
    }
}
impl From<TerrainError> for ClientLightingError {
    fn from(e: TerrainError) -> Self {
        Self::Terrain(e)
    }
}
impl From<LightUploadError> for ClientLightingError {
    fn from(e: LightUploadError) -> Self {
        Self::Upload(e)
    }
}
impl From<LightBackpressure> for ClientLightingError {
    fn from(_: LightBackpressure) -> Self {
        Self::Backpressure
    }
}

struct SkyProof {
    policy: SkyPolicy,
    blockers: BTreeMap<(SpaceId, i32, i32), BTreeSet<i32>>,
    count: usize,
    max_blockers: usize,
}
impl SkyProof {
    fn observe(&mut self, position: BlockPos, opacity: u8) -> Result<(), ClientLightingError> {
        let SkyPolicy::GeneratedProfile { space, top_y } = self.policy else {
            return Ok(());
        };
        if position.space != space || position.y <= top_y {
            return Ok(());
        }
        let key = (space, position.x, position.z);
        if opacity > 0 {
            if self
                .blockers
                .get(&key)
                .is_some_and(|ys| ys.contains(&position.y))
            {
                return Ok(());
            }
            if self.count >= self.max_blockers {
                return Err(ClientLightingError::SkyBlockerCapacity);
            }
            self.blockers.entry(key).or_default().insert(position.y);
            self.count += 1;
        } else if let Some(ys) = self.blockers.get_mut(&key) {
            if ys.remove(&position.y) {
                self.count -= 1;
            }
            if ys.is_empty() {
                self.blockers.remove(&key);
            }
        }
        Ok(())
    }
    fn requires_page(&self, page: PageId) -> bool {
        matches!(self.policy,SkyPolicy::GeneratedProfile{space,top_y} if page.space==space&&i64::from(page.y)*16+15>i64::from(top_y))
    }
}
impl SkyAccess for SkyProof {
    fn incoming_from_unknown_above(&self, known: BlockPos) -> Option<u8> {
        let SkyPolicy::GeneratedProfile { space, top_y } = self.policy else {
            return None;
        };
        if known.space != space {
            return None;
        }
        let ceiling = self
            .blockers
            .get(&(space, known.x, known.z))
            .and_then(|ys| ys.last().copied())
            .unwrap_or(top_y);
        (known.y >= ceiling).then_some(15)
    }
}
struct ResidentLight {
    handle: PageHandle,
    bounds: WorldBounds,
    upload_revision: u64,
    bindings: [Option<u64>; 27],
}
struct OverlayLight {
    page: PageId,
    handle: PageHandle,
    bound_source: Option<PageHandle>,
}
struct InstanceLight {
    handle: PageHandle,
    position: BlockPos,
    bounds: WorldBounds,
    last: Option<PackedLight>,
    revision: u64,
    dirty: bool,
}
struct PendingRecord {
    change: Arc<WorldChange>,
    next: u64,
    page: usize,
    initial_index: usize,
    cell: usize,
    light: usize,
    page_started: bool,
    full_lights: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ClientLightingWork {
    pub journal_entries: usize,
    pub cpu_cells: u64,
    pub cpu_neighbors: u64,
    pub light_updates: u64,
    pub upload_bytes: u64,
    pub gpu_pages: usize,
    pub sample_reads: u64,
    pub pending_light_cells: usize,
    pub pending_seed_pages: usize,
    pub pending_gpu_pages: usize,
    pub backpressure: bool,
    pub ready: bool,
    pub oldest_light_age_ticks: u64,
    pub light_overdue: bool,
    pub confirmed_source_pages: usize,
    pub unknown_source_pages: usize,
    pub provenance_page_reads: u64,
    pub coalesced_light_changes: u64,
    pub retired_dirty_pages: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightingReadyDiagnostics {
    pub authoritative: bool,
    pub bootstrap_pages: usize,
    pub pending_solver_cells: usize,
    pub journal_next: u64,
    pub journal_consumed: u64,
    pub journal_lag_records: u64,
    pub partial_record: bool,
    pub partial_light_remaining: usize,
    pub pending_gpu_pages: usize,
    pub dirty_residents: usize,
    pub resident_pages: usize,
    pub unknown_source_pages: usize,
    pub unbound_overlays: usize,
    pub dirty_instances: usize,
    pub sky_columns: usize,
    pub pending_shadow_cascades: u32,
}
pub struct ClientLighting {
    config: ClientLightingConfig,
    engine: Option<LightingEngine>,
    sky: SkyProof,
    lights: LightVolumeGpu,
    shadows: ShadowGpu,
    cursor: CursorId,
    next_sequence: u64,
    record: Option<PendingRecord>,
    bootstrap: VecDeque<(PageId, usize)>,
    bootstrap_sky_complete: bool,
    dirty: LightDirtyTracker,
    dirty_retire_cursor: Option<(u64, PageId)>,
    scratch: LightUploadScratch,
    residents: BTreeMap<PageId, ResidentLight>,
    gpu_capacity: usize,
    overlays: BTreeMap<u32, OverlayLight>,
    instances: BTreeMap<u32, InstanceLight>,
    known_columns: BTreeMap<(SpaceId, i32, i32), BTreeSet<i32>>,
    sky_dirty: BTreeMap<(SpaceId, i32, i32), Option<i32>>,
}
impl ClientLighting {
    /// Loading-only. The caller must invoke this before the first terrain frame.
    /// Existing pages are queued for bounded bootstrap rather than scanned here.
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        world: &mut World,
        config: ClientLightingConfig,
        center: [f32; 3],
        sun: [f32; 3],
    ) -> Result<Self, ClientLightingError> {
        if config.max_journal_entries == 0
            || config.max_gpu_pages_per_pump == 0
            || config.max_pending_light_pages < 27
            || (config.source == LightSource::LocalDerived
                && (config.max_bootstrap_pages < world.loaded_pages()
                    || config.max_sky_blockers == 0))
        {
            return Err(ClientLightingError::InvalidConfig);
        }
        let geometry = terrain.geometry();
        let max_pages = geometry.max_pages;
        let lights = LightVolumeGpu::new(device, max_pages)?;
        let shadows = ShadowGpu::new(device, queue, geometry, config.shadows, center, sun)?;
        terrain.attach_lighting(device, &shadows, &lights)?;
        let derived = config.source == LightSource::LocalDerived;
        let engine = if derived {
            Some(LightingEngine::attach(world, config.cpu_limits)?)
        } else {
            None
        };
        let next_sequence = world.journal_next();
        let cursor = world.register_cursor();
        let bootstrap = if derived {
            world.pages().map(|(page, _)| (page, 0)).collect()
        } else {
            VecDeque::new()
        };
        let mut known_columns: BTreeMap<(SpaceId, i32, i32), BTreeSet<i32>> = BTreeMap::new();
        if derived {
            for (page, _) in world.pages() {
                known_columns
                    .entry((page.space, page.x, page.z))
                    .or_default()
                    .insert(page.y);
            }
        }
        Ok(Self {
            config,
            engine,
            sky: SkyProof {
                policy: if derived {
                    config.sky
                } else {
                    SkyPolicy::UnknownIsDark
                },
                blockers: BTreeMap::new(),
                count: 0,
                max_blockers: config.max_sky_blockers,
            },
            lights,
            shadows,
            cursor,
            next_sequence,
            record: None,
            bootstrap,
            bootstrap_sky_complete: !derived,
            dirty: LightDirtyTracker::new(config.max_pending_light_pages),
            dirty_retire_cursor: None,
            scratch: LightUploadScratch::default(),
            residents: BTreeMap::new(),
            gpu_capacity: max_pages as usize,
            overlays: BTreeMap::new(),
            instances: BTreeMap::new(),
            known_columns,
            sky_dirty: BTreeMap::new(),
        })
    }
    pub fn prepare_scene(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        scene: &mut crate::ClientScene,
        config: ClientLightingConfig,
        center: [f32; 3],
        sun: [f32; 3],
    ) -> Result<Self, ClientLightingError> {
        Self::prepare(
            device,
            queue,
            terrain,
            scene.derived_world_mut(),
            config,
            center,
            sun,
        )
    }
    pub fn register_page(
        &mut self,
        page: PageId,
        handle: PageHandle,
        bounds: WorldBounds,
    ) -> Result<(), ClientLightingError> {
        if !self.residents.contains_key(&page) && self.residents.len() >= self.gpu_capacity {
            return Err(ClientLightingError::Backpressure);
        }
        if self
            .residents
            .get(&page)
            .is_some_and(|old| old.handle == handle)
        {
            self.shadows.invalidate_bounds(bounds);
            return Ok(());
        }
        self.dirty.mark_full(page, Version(0))?;
        if let Some(old) = self.residents.insert(
            page,
            ResidentLight {
                handle,
                bounds,
                upload_revision: 0,
                bindings: [None; 27],
            },
        ) {
            self.shadows.invalidate_bounds(old.bounds);
        }
        self.shadows.invalidate_bounds(bounds);
        Ok(())
    }
    /// Register a H1 shell without replacing its page's base-volume mapping.
    /// The alias follows all later base RGB/skylight patches automatically.
    pub fn register_overlay(
        &mut self,
        queue: &wgpu::Queue,
        page: PageId,
        handle: PageHandle,
        budget: &mut UploadBudget,
    ) -> Result<(), ClientLightingError> {
        let base = self
            .residents
            .get(&page)
            .ok_or(ClientLightingError::Backpressure)?
            .handle;
        if self
            .overlays
            .get(&handle.slot)
            .is_some_and(|old| old.handle == handle && old.bound_source == Some(base))
        {
            return Ok(());
        }
        if self.overlays.len() + self.instances.len() + self.residents.len() >= self.gpu_capacity
            && !self.overlays.contains_key(&handle.slot)
        {
            return Err(ClientLightingError::Backpressure);
        }
        let bound_source = match self.lights.alias_page(queue, handle, base, budget) {
            Ok(_) => Some(base),
            Err(LightUploadError::StaleHandle | LightUploadError::UploadBudget) => None,
            Err(error) => return Err(error.into()),
        };
        self.overlays.insert(
            handle.slot,
            OverlayLight {
                page,
                handle,
                bound_source,
            },
        );
        if let Some(resident) = self.residents.get(&page) {
            self.shadows.invalidate_bounds(resident.bounds);
        }
        Ok(())
    }
    /// Rigid player/model parts upload only one uniform light sample (four bytes)
    /// when their sampled world lighting changes; their mesh is never rebuilt.
    pub fn register_instance(
        &mut self,
        handle: PageHandle,
        position: BlockPos,
        bounds: WorldBounds,
    ) -> Result<(), ClientLightingError> {
        if self.instances.len() + self.overlays.len() + self.residents.len() >= self.gpu_capacity
            && !self.instances.contains_key(&handle.slot)
        {
            return Err(ClientLightingError::Backpressure);
        }
        if self
            .instances
            .get(&handle.slot)
            .is_some_and(|old| old.handle == handle)
        {
            self.update_instance(handle, position, bounds)?;
            return Ok(());
        }
        self.instances.insert(
            handle.slot,
            InstanceLight {
                handle,
                position,
                bounds,
                last: None,
                revision: 0,
                dirty: true,
            },
        );
        self.shadows.invalidate_bounds(bounds);
        Ok(())
    }
    pub fn update_instance(
        &mut self,
        handle: PageHandle,
        position: BlockPos,
        bounds: WorldBounds,
    ) -> Result<(), ClientLightingError> {
        let instance = self
            .instances
            .get_mut(&handle.slot)
            .filter(|old| old.handle == handle)
            .ok_or(ClientLightingError::Backpressure)?;
        if instance.bounds != bounds {
            self.shadows.invalidate_bounds(instance.bounds);
            self.shadows.invalidate_bounds(bounds);
            instance.bounds = bounds;
        }
        if instance.position != position {
            instance.position = position;
            instance.dirty = true;
        }
        Ok(())
    }
    /// The caller retires the corresponding terrain geometry in the same epoch.
    pub fn unregister_handle(&mut self, handle: PageHandle) {
        if let Some(page) = self
            .residents
            .iter()
            .find(|(_, resident)| resident.handle == handle)
            .map(|(page, _)| *page)
        {
            self.unregister_page(page);
        }
        if self
            .overlays
            .get(&handle.slot)
            .is_some_and(|old| old.handle == handle)
            && let Some(old) = self.overlays.remove(&handle.slot)
            && let Some(resident) = self.residents.get(&old.page)
        {
            self.shadows.invalidate_bounds(resident.bounds);
        }
        if self
            .instances
            .get(&handle.slot)
            .is_some_and(|old| old.handle == handle)
            && let Some(old) = self.instances.remove(&handle.slot)
        {
            self.shadows.invalidate_bounds(old.bounds);
        }
    }
    pub fn unregister_page(&mut self, page: PageId) {
        if let Some(old) = self.residents.remove(&page) {
            self.shadows.invalidate_bounds(old.bounds);
            self.overlays.retain(|_, overlay| overlay.page != page);
        }
    }
    /// Call for a transformed page after notifying both old and new bounds.
    pub fn update_page_bounds(&mut self, page: PageId, bounds: WorldBounds) {
        if let Some(resident) = self.residents.get_mut(&page) {
            self.shadows.invalidate_bounds(resident.bounds);
            self.shadows.invalidate_bounds(bounds);
            resident.bounds = bounds;
        }
    }
    pub fn invalidate_bounds(&mut self, bounds: WorldBounds) {
        self.shadows.invalidate_bounds(bounds);
    }
    pub fn needs_work(&self, world: &World) -> bool {
        self.pending_light_work(world) || self.shadows.needs_work()
    }
    /// Source certificates remain the Scene caller's separate guard. This
    /// permits only coherent Sun-only refresh, preserving every data/journal
    /// condition and initial/recenter/structural shadow coverage requirement.
    pub fn presentation_ready(&self, world: &World) -> bool {
        !self.pending_light_work(world) && self.shadows.presentation_ready()
    }
    fn pending_light_work(&self, world: &World) -> bool {
        !self.bootstrap.is_empty()
            || self.engine.as_ref().is_some_and(|engine| !engine.is_idle())
            || self.next_sequence != world.journal_next()
            || self.record.is_some()
            || self.dirty.pending_pages() != 0
            || self
                .overlays
                .values()
                .any(|overlay| overlay.bound_source.is_none())
            || self.instances.values().any(|instance| instance.dirty)
            || !self.sky_dirty.is_empty()
    }
    /// Bounded CPU journal work can run while an older GPU frame is in flight.
    /// No queue, GPU resource, uniform update or solver is reachable here.
    pub fn ingest_scene_cpu(
        &mut self,
        scene: &mut crate::ClientScene,
    ) -> Result<ClientLightingWork, ClientLightingError> {
        if self.config.source != LightSource::Authoritative {
            return Err(ClientLightingError::InvalidConfig);
        }
        let mut work = ClientLightingWork::default();
        self.ingest(scene.derived_world_mut(), &mut work)?;
        let residents = &self.residents;
        work.retired_dirty_pages = self.dirty.retire_nonresident(
            &mut self.dirty_retire_cursor,
            self.config.max_gpu_pages_per_pump.saturating_mul(27),
            |page| residents.contains_key(&page),
        );
        work.pending_gpu_pages = self.dirty.pending_pages();
        Ok(work)
    }
    pub fn ready_diagnostics(&self, scene: &crate::ClientScene) -> LightingReadyDiagnostics {
        let journal_next = scene.world().journal_next();
        LightingReadyDiagnostics {
            authoritative: self.config.source == LightSource::Authoritative,
            bootstrap_pages: self.bootstrap.len(),
            pending_solver_cells: self
                .engine
                .as_ref()
                .map_or(0, LightingEngine::pending_cells),
            journal_next,
            journal_consumed: self.next_sequence,
            journal_lag_records: journal_next.saturating_sub(self.next_sequence),
            partial_record: self.record.is_some(),
            partial_light_remaining: self.record.as_ref().map_or(0, |record| {
                record.change.lights.len().saturating_sub(record.light)
            }),
            pending_gpu_pages: self.dirty.pending_pages(),
            dirty_residents: self
                .residents
                .keys()
                .filter(|page| self.dirty.pending_version(**page).is_some())
                .count(),
            resident_pages: self.residents.len(),
            unknown_source_pages: self
                .residents
                .keys()
                .filter(|page| scene.authoritative_light_version(**page).is_none())
                .count(),
            unbound_overlays: self
                .overlays
                .values()
                .filter(|overlay| overlay.bound_source.is_none())
                .count(),
            dirty_instances: self
                .instances
                .values()
                .filter(|instance| instance.dirty)
                .count(),
            sky_columns: self.sky_dirty.len(),
            pending_shadow_cascades: self.shadows.cache().pending(),
        }
    }
    pub fn set_cpu_budget(&mut self, budget: LightBudget) -> Result<(), ClientLightingError> {
        if budget.max_cells == 0
            || budget.max_neighbor_reads < 6
            || budget.max_seed_cells == 0
            || budget.max_updates == 0
        {
            return Err(ClientLightingError::InvalidConfig);
        }
        self.config.cpu_budget = budget;
        Ok(())
    }
    pub fn take_upload_bytes(&mut self) -> u64 {
        self.lights.take_upload_bytes()
    }
    pub fn light_volumes(&self) -> &LightVolumeGpu {
        &self.lights
    }
    pub fn set_shadow_visibility(
        &mut self,
        enabled: bool,
        cascades: u32,
    ) -> Result<bool, ClientLightingError> {
        Ok(self.shadows.set_visibility(enabled, cascades)?)
    }
    pub fn shadows(&self) -> &ShadowGpu {
        &self.shadows
    }
    pub fn pump_scene(
        &mut self,
        queue: &wgpu::Queue,
        scene: &mut crate::ClientScene,
        tick: Tick,
        budget: &mut UploadBudget,
    ) -> Result<ClientLightingWork, ClientLightingError> {
        if self.config.source == LightSource::LocalDerived {
            return self.pump(queue, scene.derived_world_mut(), tick, budget);
        }
        let mut work = ClientLightingWork::default();
        self.ingest(scene.derived_world_mut(), &mut work)?;
        for (&page, resident) in &mut self.residents {
            let mut bindings = [None; 27];
            let mut i = 0;
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        if let (Some(x), Some(y), Some(z)) = (
                            page.x.checked_add(dx),
                            page.y.checked_add(dy),
                            page.z.checked_add(dz),
                        ) {
                            work.provenance_page_reads += 1;
                            bindings[i] = scene
                                .authoritative_light_version(PageId {
                                    space: page.space,
                                    x,
                                    y,
                                    z,
                                })
                                .map(|stamp| stamp.cell_view);
                        }
                        i += 1;
                    }
                }
            }
            if bindings != resident.bindings {
                if self
                    .dirty
                    .mark_full(page, scene.world().page_version(page))
                    .is_err()
                {
                    work.backpressure = true;
                    continue;
                }
                let revoked = resident
                    .bindings
                    .iter()
                    .zip(bindings)
                    .any(|(old, new)| old.is_some() && old != &new);
                if revoked {
                    if budget.remaining_bytes < 8 {
                        work.backpressure = true;
                        continue;
                    }
                    let version = resident
                        .upload_revision
                        .checked_add(1)
                        .ok_or(ClientLightingError::CoordinateOverflow)?;
                    work.upload_bytes += self.lights.upload_uniform(
                        queue,
                        resident.handle,
                        version,
                        PackedLight::default(),
                        budget,
                    )?;
                    resident.upload_revision = version;
                }
                resident.bindings = bindings;
            }
        }
        self.upload_filtered(queue, scene.world(), budget, &mut work, |page| {
            scene.authoritative_light_version(page).is_some()
        })?;
        self.upload_instances_filtered(queue, scene.world(), budget, &mut work, |page| {
            scene.authoritative_light_version(page).is_some()
        })?;
        for page in self.residents.keys() {
            if scene.authoritative_light_version(*page).is_some() {
                work.confirmed_source_pages += 1;
            } else {
                work.unknown_source_pages += 1;
            }
        }
        work.pending_gpu_pages = self.dirty.pending_pages();
        work.ready = work.unknown_source_pages == 0
            && !work.backpressure
            && work.pending_gpu_pages == 0
            && self
                .overlays
                .values()
                .all(|overlay| overlay.bound_source.is_some())
            && self.instances.values().all(|instance| !instance.dirty)
            && self.next_sequence == scene.world().journal_next()
            && self.record.is_none();
        Ok(work)
    }
    pub fn pump(
        &mut self,
        queue: &wgpu::Queue,
        world: &mut World,
        tick: Tick,
        budget: &mut UploadBudget,
    ) -> Result<ClientLightingWork, ClientLightingError> {
        let tick = tick.max(world.tick());
        let mut work = ClientLightingWork::default();
        self.pump_bootstrap(world, tick, &mut work)?;
        self.ingest(world, &mut work)?;
        if self.engine.is_some() {
            self.pump_sky_seeds(tick, &mut work)?;
            let progress = self.engine.as_mut().expect("derived owner").ingest(
                world,
                tick,
                self.config.ingest_budget,
            )?;
            work.backpressure |= progress.backpressure;
            work.oldest_light_age_ticks = progress.oldest_age_ticks;
            work.light_overdue = progress.overdue;
            // Sky proof must include all pending authority edits before the solver
            // can use it. A bounded cursor lag delays light rather than leaking sky.
            if self.bootstrap_sky_complete
                && self.next_sequence == world.journal_next()
                && self.record.is_none()
            {
                let progress = self.engine.as_mut().expect("derived owner").step(
                    world,
                    tick,
                    self.config.cpu_budget,
                    &self.sky,
                )?;
                work.cpu_cells = progress.work.cells_examined;
                work.cpu_neighbors = progress.work.neighbor_reads;
                work.light_updates = progress.work.light_updates;
                work.oldest_light_age_ticks = progress.oldest_age_ticks;
                work.light_overdue = progress.overdue;
                work.backpressure |= progress.backpressure;
                self.ingest(world, &mut work)?;
            }
        }
        self.upload(queue, world, budget, &mut work)?;
        self.upload_instances(queue, world, budget, &mut work)?;
        work.pending_light_cells = self
            .engine
            .as_ref()
            .map_or(0, LightingEngine::pending_cells);
        work.pending_seed_pages = self
            .engine
            .as_ref()
            .map_or(0, LightingEngine::pending_seed_pages);
        work.pending_gpu_pages = self.dirty.pending_pages();
        work.ready = self
            .overlays
            .values()
            .all(|overlay| overlay.bound_source.is_some())
            && self.instances.values().all(|instance| !instance.dirty)
            && self.sky_dirty.is_empty()
            && self.bootstrap.is_empty()
            && self.engine.as_ref().is_none_or(LightingEngine::is_idle)
            && work.pending_gpu_pages == 0
            && self.next_sequence == world.journal_next()
            && self.record.is_none();
        Ok(work)
    }
    /// Enable ShadowConfig.dynamic_casters during loading, then use this route
    /// for animated model casters. Static terrain cache remains independent.
    #[allow(clippy::too_many_arguments)] // One frame owner supplies its prepared terrain/caster pair.
    pub fn encode_lighting_dynamic(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        center: [f32; 3],
        sun: [f32; 3],
        caster: &mut impl render::shadows::DynamicShadowCaster,
    ) -> Result<ShadowWork, ClientLightingError> {
        self.shadows.update_view(center, sun)?;
        let work = self.shadows.encode_dynamic(queue, encoder, caster)?;
        if work.cascade_updates > 0
            || work.dynamic_copy_bytes > 0
            || work.dynamic_restore_pixels > 0
            || work.gpu_restore_mask != 0
            || work.uniform_upload_bytes != 0
        {
            terrain.invalidate();
        }
        Ok(work)
    }
    pub fn encode_lighting(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        center: [f32; 3],
        sun: [f32; 3],
    ) -> Result<ShadowWork, ClientLightingError> {
        self.shadows.update_view(center, sun)?;
        let work = self.shadows.encode(queue, encoder);
        if work.cascade_updates > 0
            || work.dynamic_copy_bytes > 0
            || work.dynamic_restore_pixels > 0
            || work.gpu_restore_mask != 0
            || work.uniform_upload_bytes != 0
        {
            terrain.invalidate();
        }
        Ok(work)
    }
    pub fn detach(self, world: &mut World) {
        world.remove_cursor(self.cursor);
        if let Some(engine) = self.engine {
            engine.detach(world);
        }
    }
    fn observe_sky(&mut self, position: BlockPos, opacity: u8) -> Result<(), ClientLightingError> {
        observe_sky(&mut self.sky, &mut self.sky_dirty, position, opacity)
    }
    fn pump_sky_seeds(
        &mut self,
        tick: Tick,
        work: &mut ClientLightingWork,
    ) -> Result<(), ClientLightingError> {
        while work.journal_entries < self.config.max_journal_entries {
            let Some((column, last)) = self
                .sky_dirty
                .first_key_value()
                .map(|(key, last)| (*key, *last))
            else {
                break;
            };
            let key = (column.0, column.1.div_euclid(16), column.2.div_euclid(16));
            let next = self.known_columns.get(&key).and_then(|ys| match last {
                Some(y) => ys
                    .range((std::ops::Bound::Excluded(y), std::ops::Bound::Unbounded))
                    .next()
                    .copied(),
                None => ys.first().copied(),
            });
            let Some(y) = next else {
                self.sky_dirty.remove(&column);
                continue;
            };
            let Ok(world_y) = i32::try_from(i64::from(y) * 16 + 15) else {
                return Err(ClientLightingError::CoordinateOverflow);
            };
            let position = BlockPos {
                space: column.0,
                x: column.1,
                y: world_y,
                z: column.2,
            };
            if self
                .engine
                .as_mut()
                .expect("derived sky seed")
                .queue_cell(position, tick, LightPriority::Interactive)
                .is_err()
            {
                work.backpressure = true;
                break;
            }
            self.sky_dirty.insert(column, Some(y));
            work.journal_entries += 1;
        }
        Ok(())
    }
    fn pump_bootstrap(
        &mut self,
        world: &World,
        tick: Tick,
        work: &mut ClientLightingWork,
    ) -> Result<(), ClientLightingError> {
        if !self.bootstrap_sky_complete {
            for (page, index) in &mut self.bootstrap {
                if work.journal_entries >= self.config.max_journal_entries {
                    break;
                }
                if *index >= 4096 {
                    continue;
                }
                if self.sky.requires_page(*page) {
                    if let Some(source) = world.page(*page) {
                        while *index < 4096
                            && work.journal_entries < self.config.max_journal_entries
                        {
                            let local = LocalIdx::new(*index as u16).expect("page index");
                            observe_sky(
                                &mut self.sky,
                                &mut self.sky_dirty,
                                position(*page, local)?,
                                rules::properties(source.cell(local)).light_opacity,
                            )?;
                            *index += 1;
                            work.journal_entries += 1;
                        }
                    } else {
                        *index = 4096;
                    }
                } else {
                    *index = 4096;
                    work.journal_entries += 1;
                }
            }
            self.bootstrap_sky_complete = self.bootstrap.iter().all(|(_, index)| *index >= 4096);
        }
        if self.bootstrap_sky_complete {
            while work.journal_entries < self.config.max_journal_entries {
                let Some((page, _)) = self.bootstrap.front() else {
                    break;
                };
                if self
                    .engine
                    .as_mut()
                    .expect("derived bootstrap")
                    .queue_page(*page, tick, LightPriority::Background)
                    .is_err()
                {
                    work.backpressure = true;
                    break;
                }
                self.bootstrap.pop_front();
                work.journal_entries += 1;
            }
        }
        Ok(())
    }
    fn ingest(
        &mut self,
        world: &mut World,
        work: &mut ClientLightingWork,
    ) -> Result<(), ClientLightingError> {
        while work.journal_entries < self.config.max_journal_entries {
            let mut record = if let Some(record) = self.record.take() {
                record
            } else {
                if self.next_sequence == world.journal_next() {
                    break;
                }
                let batch = world.read_cursor(self.cursor, 1)?;
                let Some(change) = batch.changes.into_iter().next() else {
                    break;
                };
                PendingRecord {
                    full_lights: self.engine.is_none() && change.lights.len() > 1024,
                    change,
                    next: batch.next,
                    page: 0,
                    initial_index: 0,
                    cell: 0,
                    light: 0,
                    page_started: false,
                }
            };
            let done = self.ingest_record(&mut record, work)?;
            if !done {
                self.record = Some(record);
                break;
            }
            world.acknowledge_cursor(self.cursor, record.next)?;
            self.next_sequence = record.next;
        }
        Ok(())
    }
    fn ingest_record(
        &mut self,
        record: &mut PendingRecord,
        work: &mut ClientLightingWork,
    ) -> Result<bool, ClientLightingError> {
        while record.page < record.change.pages.len() {
            let change = &record.change.pages[record.page];
            if !record.page_started {
                if work.journal_entries >= self.config.max_journal_entries {
                    return Ok(false);
                }
                if (change.created
                    || change.removed
                    || (record.full_lights && change.channels.intersects(ChannelMask::LIGHTING)))
                    && self
                        .dirty
                        .mark_page_boundary(change.page, change.after)
                        .is_err()
                {
                    work.backpressure = true;
                    return Ok(false);
                }
                if (change.created
                    || change.removed
                    || change.channels.intersects(
                        ChannelMask::SHAPE | ChannelMask::OPACITY | ChannelMask::TRANSFORM,
                    ))
                    && let Some(resident) = self.residents.get(&change.page)
                {
                    self.shadows.invalidate_bounds(resident.bounds);
                }
                if self.engine.is_some() && change.created {
                    self.known_columns
                        .entry((change.page.space, change.page.x, change.page.z))
                        .or_default()
                        .insert(change.page.y);
                }
                if self.engine.is_some() && change.removed {
                    let key = (change.page.space, change.page.x, change.page.z);
                    if let Some(ys) = self.known_columns.get_mut(&key) {
                        ys.remove(&change.page.y);
                        if ys.is_empty() {
                            self.known_columns.remove(&key);
                        }
                    }
                }
                record.page_started = true;
                work.journal_entries += 1;
            }
            if self.sky.requires_page(change.page)
                && let Some(initial) = &change.initial_cells
            {
                while record.initial_index < 4096
                    && work.journal_entries < self.config.max_journal_entries
                {
                    let local = LocalIdx::new(record.initial_index as u16).expect("page index");
                    self.observe_sky(
                        position(change.page, local)?,
                        rules::properties(initial[record.initial_index]).light_opacity,
                    )?;
                    record.initial_index += 1;
                    work.journal_entries += 1;
                }
                if record.initial_index < 4096 {
                    return Ok(false);
                }
            }
            record.page += 1;
            record.initial_index = 0;
            record.page_started = false;
        }
        if self.engine.is_none() {
            record.cell = record.change.cells.len();
        }
        while record.cell < record.change.cells.len() {
            if work.journal_entries >= self.config.max_journal_entries {
                return Ok(false);
            }
            let change = &record.change.cells[record.cell];
            self.observe_sky(
                position(change.page, change.index)?,
                rules::properties(change.new).light_opacity,
            )?;
            record.cell += 1;
            work.journal_entries += 1;
        }
        if record.full_lights {
            // Page metadata already covered every changed light and its halo.
            // GPU publication samples the current complete World volume later.
            work.coalesced_light_changes += (record.change.lights.len() - record.light) as u64;
            record.light = record.change.lights.len();
        }
        while record.light < record.change.lights.len() {
            if work.journal_entries >= self.config.max_journal_entries {
                return Ok(false);
            }
            if self
                .dirty
                .mark_light(
                    &record.change.lights[record.light],
                    record
                        .change
                        .pages
                        .first()
                        .map_or(Version(0), |page| page.after),
                )
                .is_err()
            {
                work.backpressure = true;
                return Ok(false);
            }
            record.light += 1;
            work.journal_entries += 1;
        }
        Ok(true)
    }
    fn upload_instances(
        &mut self,
        queue: &wgpu::Queue,
        world: &World,
        budget: &mut UploadBudget,
        work: &mut ClientLightingWork,
    ) -> Result<(), ClientLightingError> {
        self.upload_instances_filtered(queue, world, budget, work, |_| true)
    }
    fn upload_instances_filtered(
        &mut self,
        queue: &wgpu::Queue,
        world: &World,
        budget: &mut UploadBudget,
        work: &mut ClientLightingWork,
        known: impl Fn(PageId) -> bool,
    ) -> Result<(), ClientLightingError> {
        for overlay in self.overlays.values_mut() {
            let Some(base) = self
                .residents
                .get(&overlay.page)
                .map(|resident| resident.handle)
            else {
                continue;
            };
            if overlay.bound_source == Some(base) {
                continue;
            }
            match self.lights.alias_page(queue, overlay.handle, base, budget) {
                Ok(bytes) => {
                    overlay.bound_source = Some(base);
                    work.upload_bytes += bytes;
                }
                Err(LightUploadError::UploadBudget | LightUploadError::StaleHandle) => {
                    work.backpressure = true;
                }
                Err(error) => return Err(error.into()),
            }
        }
        for instance in self.instances.values_mut() {
            work.sample_reads += 1;
            let value = if known(instance.position.page())
                && let Some(page) = world.page(instance.position.page())
            {
                let light = page.light(instance.position.local_index());
                PackedLight::new(light.sky, light.block, light.rgb).expect("valid world light")
            } else {
                PackedLight::default()
            };
            if instance.last == Some(value) {
                instance.dirty = false;
                continue;
            }
            let revision = instance
                .revision
                .checked_add(1)
                .ok_or(ClientLightingError::CoordinateOverflow)?;
            match self
                .lights
                .upload_uniform(queue, instance.handle, revision, value, budget)
            {
                Ok(bytes) => {
                    instance.revision = revision;
                    instance.last = Some(value);
                    instance.dirty = false;
                    work.upload_bytes += bytes;
                }
                Err(LightUploadError::UploadBudget) => {
                    instance.dirty = true;
                    work.backpressure = true;
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn upload(
        &mut self,
        queue: &wgpu::Queue,
        world: &World,
        budget: &mut UploadBudget,
        work: &mut ClientLightingWork,
    ) -> Result<(), ClientLightingError> {
        self.upload_filtered(queue, world, budget, work, |_| true)
    }
    fn upload_filtered(
        &mut self,
        queue: &wgpu::Queue,
        world: &World,
        budget: &mut UploadBudget,
        work: &mut ClientLightingWork,
        known: impl Fn(PageId) -> bool,
    ) -> Result<(), ClientLightingError> {
        let mut considered = 0;
        let mut inspected = 0;
        let metadata_limit = self.config.max_gpu_pages_per_pump.saturating_mul(27);
        while considered < self.config.max_gpu_pages_per_pump && inspected < metadata_limit {
            let Some(page) = self.dirty.first_page() else {
                break;
            };
            inspected += 1;
            if !self.residents.contains_key(&page) {
                let version = self.dirty.pending_version(page).expect("dirty page");
                self.dirty.acknowledge(page, version);
                continue;
            }
            considered += 1;
            let prepared = self
                .dirty
                .prepare_filtered(world, page, &mut self.scratch, &known)
                .expect("dirty page");
            let version = match prepared {
                PreparedLight::Full { version, .. } | PreparedLight::Patch { version, .. } => {
                    version
                }
            };
            let Some(resident) = self.residents.get(&page) else {
                self.dirty.acknowledge(page, version);
                continue;
            };
            let gpu_version = resident
                .upload_revision
                .checked_add(1)
                .ok_or(ClientLightingError::CoordinateOverflow)?;
            let uploaded = match prepared {
                PreparedLight::Full {
                    samples,
                    work: sample_work,
                    ..
                } => {
                    work.sample_reads += sample_work.samples;
                    self.lights
                        .upload_page(queue, resident.handle, gpu_version, samples, budget)
                }
                PreparedLight::Patch {
                    updates,
                    work: sample_work,
                    ..
                } => {
                    work.sample_reads += sample_work.samples;
                    self.lights
                        .upload_patch(queue, resident.handle, gpu_version, updates, budget)
                }
            };
            match uploaded {
                Ok(bytes) => {
                    if bytes > 0 {
                        self.residents
                            .get_mut(&page)
                            .expect("resident upload")
                            .upload_revision = gpu_version;
                    }
                    work.upload_bytes += bytes;
                    work.gpu_pages += usize::from(bytes > 0);
                    self.dirty.acknowledge(page, version);
                }
                Err(LightUploadError::UploadBudget) => {
                    work.backpressure = true;
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
fn observe_sky(
    sky: &mut SkyProof,
    dirty: &mut BTreeMap<(SpaceId, i32, i32), Option<i32>>,
    position: BlockPos,
    opacity: u8,
) -> Result<(), ClientLightingError> {
    let SkyPolicy::GeneratedProfile { space, top_y } = sky.policy else {
        return Ok(());
    };
    if position.space != space || position.y <= top_y {
        return Ok(());
    }
    let key = (space, position.x, position.z);
    if !dirty.contains_key(&key) && dirty.len() >= sky.max_blockers {
        return Err(ClientLightingError::SkyBlockerCapacity);
    }
    let old = sky.blockers.get(&key).and_then(|ys| ys.last().copied());
    sky.observe(position, opacity)?;
    let new = sky.blockers.get(&key).and_then(|ys| ys.last().copied());
    if old != new {
        dirty.insert(key, None);
    }
    Ok(())
}

fn position(page: PageId, local: LocalIdx) -> Result<BlockPos, ClientLightingError> {
    let (x, y, z) = local.xyz();
    let coordinates = [
        i64::from(page.x) * 16 + i64::from(x),
        i64::from(page.y) * 16 + i64::from(y),
        i64::from(page.z) * 16 + i64::from(z),
    ];
    let (Ok(x), Ok(y), Ok(z)) = (
        i32::try_from(coordinates[0]),
        i32::try_from(coordinates[1]),
        i32::try_from(coordinates[2]),
    ) else {
        return Err(ClientLightingError::CoordinateOverflow);
    };
    Ok(BlockPos {
        space: page.space,
        x,
        y,
        z,
    })
}
