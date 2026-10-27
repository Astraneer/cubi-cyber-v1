//! Bounded lifetime owner for explicit block models. Initial/new page cell reads
//! occur on immutable worker snapshots; journal cell/light changes touch only
//! indexed model records. Stable GPU slots are never compacted on removal.
use crate::block_models::{Animation, BlockSample, ModelError, instance};
use core_types::{BlockPos, Cell, ChannelMask, LocalIdx, PageId, SpaceId, Tick, Version};
use render::{
    block_models::{BlockError, BlockInstance, BlockModelsGpu},
    light_volume::PackedLight,
    terrain_gpu::UploadBudget,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use world::{CursorError, CursorId, Page, World, WorldChange};
#[path = "props_wire.rs"]
mod wire;
pub use wire::{PropsWireError, execute_props_request};
static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(1);
/// Exact GPU-slot identity, emitted only after a successful record update.
/// Consumers retain this beside the actual GPU bank, never infer it from
/// float positions or CPU slots which may already have been retired/reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropsPublication {
    pub slot: u32,
    pub position: Option<BlockPos>,
    pub registration: u64,
}
const CHANNELS: ChannelMask = ChannelMask(
    ChannelMask::SHAPE.0
        | ChannelMask::OPACITY.0
        | ChannelMask::EMISSION.0
        | ChannelMask::FLUID.0
        | ChannelMask::CONNECTIONS.0,
);
#[derive(Clone, Copy, Debug)]
pub struct PropsLimits {
    pub max_pages: usize,
    pub max_instances: usize,
    pub max_running: usize,
    pub max_ready: usize,
    pub max_age_ticks: u64,
}
/// Inclusive page bounds for near-model ownership. LIGHT halo pages can remain
/// in World without being duplicated over the public horizon renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleRegion {
    pub min: PageId,
    pub max: PageId,
}
impl VisibleRegion {
    pub fn contains(self, page: PageId) -> bool {
        page.space == self.min.space
            && page.x >= self.min.x
            && page.x <= self.max.x
            && page.y >= self.min.y
            && page.y <= self.max.y
            && page.z >= self.min.z
            && page.z <= self.max.z
    }
    fn valid(self) -> bool {
        self.min.space == self.max.space
            && self.min.space == SpaceId(0)
            && self.min.x <= self.max.x
            && self.min.y <= self.max.y
            && self.min.z <= self.max.z
    }
}
impl Default for PropsLimits {
    fn default() -> Self {
        Self {
            max_pages: 1024,
            max_instances: 16384,
            max_running: 2,
            max_ready: 4,
            max_age_ticks: 40,
        }
    }
}
#[derive(Debug)]
pub enum PropsError {
    InvalidLimits,
    Capacity,
    Space,
    Cursor(CursorError),
    Model(ModelError),
    Gpu(BlockError),
    PublicFrame,
    RevisionOverflow,
}
impl std::fmt::Display for PropsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PropsError {}
impl From<CursorError> for PropsError {
    fn from(e: CursorError) -> Self {
        Self::Cursor(e)
    }
}
impl From<ModelError> for PropsError {
    fn from(e: ModelError) -> Self {
        Self::Model(e)
    }
}
impl From<BlockError> for PropsError {
    fn from(e: BlockError) -> Self {
        Self::Gpu(e)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PropsWork {
    pub journal_entries: u64,
    pub cursor_recoveries: u64,
    pub recovery_headers: u64,
    pub recovery_slots: u64,
    pub recovery_pages: u64,
    pub jobs_submitted: u64,
    pub jobs_stale: u64,
    pub worker_mask_words: u64,
    pub worker_cells: u64,
    pub worker_models: u64,
    pub owner_light_reads: u64,
    pub binding_members: u64,
    pub public_members: u64,
    pub animated_models: u64,
    pub visibility_headers: u64,
    pub visibility_members: u64,
    pub records_uploaded: u64,
    pub upload_bytes: u64,
    pub cpu_vertices: u64,
    pub changed: bool,
    pub backpressure: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct PropsStats {
    pub pages: usize,
    pub models: usize,
    pub queued: usize,
    pub running: usize,
    pub ready: usize,
    pub queued_pages: usize,
    pub light_bindings: usize,
    pub public_pages: usize,
    pub retirements: usize,
    pub oldest_age_ticks: u64,
    pub recovering_cursor: bool,
    pub recovery_slots_remaining: usize,
    pub recovery_pages_remaining: usize,
    pub caught_up: bool,
    pub settled: bool,
    pub overdue: bool,
}
struct PageState {
    known: bool,
    queued: bool,
    running: bool,
    members: Vec<u32>,
    free_members: Vec<usize>,
    live: usize,
    light_next: Option<usize>,
    public_next: Option<usize>,
    first: Option<Tick>,
    retiring: bool,
}
struct Slot {
    position: Option<BlockPos>,
    sample: Option<BlockSample>,
    queued: bool,
    first: Tick,
    member: usize,
    ports: Option<(net::props::Kind, u32)>,
    wind: Option<(net::props::Hint, Tick)>,
    animated_member: Option<usize>,
}
impl Slot {
    fn empty() -> Self {
        Self {
            position: None,
            sample: None,
            queued: false,
            first: Tick(0),
            member: 0,
            ports: None,
            wind: None,
            animated_member: None,
        }
    }
}
struct Pending {
    change: Arc<WorldChange>,
    next: u64,
    page: usize,
    cell: usize,
    light: usize,
    removed: usize,
}
pub struct PropsTask {
    pub id: u64,
    pub page: PageId,
    pub source: Version,
    data: Arc<Page>,
    tints: Box<[u32; 256]>,
}
pub struct PropsBuilt {
    pub id: u64,
    pub page: PageId,
    pub source: Version,
    pub samples: Vec<BlockSample>,
    pub mask_words: u64,
    pub cells: u64,
    pub worker_thread: std::thread::ThreadId,
}
impl PropsTask {
    pub fn context(&self) -> PropsContext {
        PropsContext {
            id: self.id,
            page: self.page,
            source: self.source,
        }
    }
    pub fn job_key(&self) -> jobs::JobKey {
        jobs::JobKey {
            page: self.page,
            kind: 0x5052,
        }
    }
    /// Retained page snapshot plus worst-case 4096 model samples and wire buffers.
    pub fn retained_bytes(&self) -> usize {
        512 * 1024
    }
    pub fn cost(&self) -> jobs::WorkCost {
        jobs::WorkCost {
            cells: 8192,
            bytes: 512 * 1024,
            ..jobs::WorkCost::ZERO
        }
    }
    pub fn execute(self, cancel: impl Fn() -> bool) -> Option<Box<PropsBuilt>> {
        build_snapshot(
            self.context(),
            &self.tints,
            self.data.masks().occupancy.words(),
            |local| self.data.cell(local),
            cancel,
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropsContext {
    pub id: u64,
    pub page: PageId,
    pub source: Version,
}
fn build_snapshot(
    context: PropsContext,
    tints: &[u32; 256],
    occupancy: &[u64; 64],
    mut cell_at: impl FnMut(LocalIdx) -> Cell,
    cancel: impl Fn() -> bool,
) -> Option<Box<PropsBuilt>> {
    let PropsContext { id, page, source } = context;
    let mut samples = Vec::new();
    let mut cells = 0;
    for (word_index, word) in occupancy.iter().enumerate() {
        if cancel() {
            return None;
        }
        let mut bits = *word;
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let local = LocalIdx::new((word_index * 64 + bit) as u16)?;
            let cell = cell_at(local);
            cells += 1;
            if !eligible(cell) {
                continue;
            }
            let p = position(page, local)?;
            let (x, _, z) = local.xyz();
            samples.push(BlockSample {
                position: p,
                cell,
                // Geometry workers cannot certify the current public LIGHT view.
                // The owner samples it again immediately before GPU publication.
                light: 0,
                tint: tints[usize::from(x) + 16 * usize::from(z)],
                animation: Animation::default(),
            });
        }
    }
    Some(Box::new(PropsBuilt {
        id,
        page,
        source,
        samples,
        mask_words: 64,
        cells,
        worker_thread: std::thread::current().id(),
    }))
}

struct Ready {
    built: Box<PropsBuilt>,
    next: usize,
}
struct CursorRecovery {
    slot: usize,
    page: usize,
}
pub struct PropsSource {
    limits: PropsLimits,
    cursor: CursorId,
    sequence: u64,
    pending: Option<Pending>,
    recovery: Option<CursorRecovery>,
    recovery_pages: Vec<PageId>,
    pages: HashMap<PageId, PageState>,
    positions: HashMap<BlockPos, u32>,
    slots: Vec<Slot>,
    registrations: Option<Vec<u64>>,
    free: Vec<u32>,
    dirty: VecDeque<u32>,
    light_jobs: VecDeque<PageId>,
    public_jobs: VecDeque<PageId>,
    public_frames: BTreeMap<PageId, Arc<net::props::Frame>>,
    public_hint_count: usize,
    public_revision: u64,
    animated: Vec<u32>,
    animation_cursor: usize,
    visual_time: (Tick, f64),
    jobs: VecDeque<PageId>,
    running: HashMap<u64, (PageId, Version)>,
    ready: VecDeque<Ready>,
    next_job: u64,
    recovery_job_floor: u64,
    work: PropsWork,
    region: Option<VisibleRegion>,
    retiring: VecDeque<(PageId, usize)>,
}
impl PropsSource {
    fn in_region(&self, page: PageId) -> bool {
        self.region.is_none_or(|region| region.contains(page))
    }
    /// A region change visits indexed page headers once. Record retirement then
    /// consumes normal ingest credits, and hidden records consume upload credits.
    /// The caller explicitly watches newly included visible pages afterwards.
    pub fn set_visible_region(
        &mut self,
        region: Option<VisibleRegion>,
        tick: Tick,
    ) -> Result<bool, PropsError> {
        if region.is_some_and(|region| !region.valid()) {
            return Err(PropsError::Space);
        }
        if self.region == region {
            return Ok(false);
        }
        self.region = region;
        for (&page, state) in &mut self.pages {
            self.work.visibility_headers += 1;
            if region.is_some_and(|region| !region.contains(page)) {
                state.known = false;
                state.first = Some(state.first.map_or(tick, |old| old.min(tick)));
                if !state.retiring {
                    state.retiring = true;
                    self.retiring.push_back((page, 0));
                }
            }
        }
        Ok(true)
    }
    fn advance_retirement(&mut self, tick: Tick) -> Result<bool, PropsError> {
        let Some(&(page, next)) = self.retiring.front() else {
            return Ok(false);
        };
        if self.in_region(page) {
            if let Some(state) = self.pages.get_mut(&page) {
                state.retiring = false;
            }
            self.retiring.pop_front();
            return Ok(true);
        }
        let member = self
            .pages
            .get(&page)
            .and_then(|state| state.members.get(next))
            .copied();
        if let Some(index) = member {
            if index != u32::MAX
                && let Some(position) = self.slots[index as usize].position
            {
                self.remove(position, tick)?;
            }
            self.retiring.front_mut().expect("retirement cursor").1 += 1;
            self.work.visibility_members += 1;
        } else {
            self.retiring.pop_front();
            if let Some(state) = self.pages.get_mut(&page) {
                state.retiring = false;
            }
            if self
                .pages
                .get(&page)
                .is_some_and(|state| state.live == 0 && !state.running && !state.queued)
            {
                self.pages.remove(&page);
            }
        }
        Ok(true)
    }
    /// GPU reserve_slots(max_instances) is a separate loading operation and its
    /// actual bytes must be reported as loading, before this owner publishes.
    pub fn attach(world: &mut World, limits: PropsLimits) -> Result<Self, PropsError> {
        if limits.max_pages == 0
            || limits.max_instances == 0
            || limits.max_instances > 1_048_576
            || limits.max_pages > 16384
            || limits.max_running == 0
            || limits.max_ready == 0
            || world.loaded_pages() > limits.max_pages
        {
            return Err(PropsError::InvalidLimits);
        }
        let cursor = world.register_cursor();
        let sequence = world.journal_next();
        let mut source = Self {
            limits,
            cursor,
            sequence,
            pending: None,
            recovery: None,
            recovery_pages: Vec::with_capacity(limits.max_pages),
            pages: HashMap::with_capacity(limits.max_pages * 2),
            positions: HashMap::with_capacity(limits.max_instances),
            slots: (0..limits.max_instances).map(|_| Slot::empty()).collect(),
            registrations: None,
            free: (0..limits.max_instances as u32).rev().collect(),
            dirty: VecDeque::with_capacity(limits.max_instances),
            light_jobs: VecDeque::with_capacity(limits.max_pages * 2),
            public_jobs: VecDeque::with_capacity(limits.max_pages * 2),
            public_frames: BTreeMap::new(),
            public_hint_count: 0,
            public_revision: 0,
            animated: Vec::with_capacity(limits.max_instances),
            animation_cursor: 0,
            visual_time: (world.tick(), 0.),
            jobs: VecDeque::with_capacity(limits.max_pages * 2),
            running: HashMap::with_capacity(limits.max_running),
            ready: VecDeque::with_capacity(limits.max_ready),
            next_job: 1,
            recovery_job_floor: 0,
            work: PropsWork::default(),
            region: None,
            retiring: VecDeque::with_capacity(limits.max_pages * 2),
        };
        for (page, _) in world.pages() {
            if page.space == SpaceId(0) {
                source.watch_page(page, world.tick())?;
            }
        }
        Ok(source)
    }
    /// Loading-only optional lifetime bank. Ordinary profiles retain no array
    /// and do not allocate or advance temporal registration IDs.
    pub fn prepare_temporal_publications(&mut self) -> Result<bool, PropsError> {
        if self.registrations.is_some() {
            return Ok(false);
        }
        let count = self
            .slots
            .iter()
            .filter(|slot| slot.sample.is_some())
            .count() as u64;
        let first = NEXT_REGISTRATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(count)
            })
            .map_err(|_| PropsError::RevisionOverflow)?;
        let mut bank = vec![0; self.slots.len()];
        let mut next = first;
        for (slot, registration) in self.slots.iter().zip(&mut bank) {
            if slot.sample.is_some() {
                *registration = next;
                next += 1;
            }
        }
        self.registrations = Some(bank);
        Ok(true)
    }
    pub fn temporal_registration_bytes(&self) -> usize {
        self.registrations
            .as_ref()
            .map_or(0, |bank| bank.len() * std::mem::size_of::<u64>())
    }
    pub fn watch_page(&mut self, page: PageId, tick: Tick) -> Result<(), PropsError> {
        if page.space != SpaceId(0) {
            return Err(PropsError::Space);
        }
        if !self.in_region(page) {
            return Ok(());
        }
        if !self.pages.contains_key(&page) && self.pages.len() >= self.limits.max_pages * 2 {
            return Err(PropsError::Capacity);
        }
        let state = self.pages.entry(page).or_insert_with(|| PageState {
            known: true,
            queued: false,
            running: false,
            members: Vec::new(),
            free_members: Vec::new(),
            live: 0,
            light_next: None,
            public_next: None,
            first: Some(tick),
            retiring: false,
        });
        state.known = true;
        state.first = Some(state.first.map_or(tick, |old| old.min(tick)));
        if !state.queued {
            if self.jobs.len() >= self.limits.max_pages * 2 {
                return Err(PropsError::Capacity);
            }
            state.queued = true;
            self.jobs.push_back(page);
        }
        Ok(())
    }
    fn set(&mut self, sample: BlockSample, tick: Tick) -> Result<(), PropsError> {
        let selected = eligible(sample.cell);
        let registration = if self.registrations.is_some()
            && selected
            && self
                .positions
                .get(&sample.position)
                .is_none_or(|slot| self.slots[*slot as usize].sample.is_none())
        {
            Some(
                NEXT_REGISTRATION
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                        next.checked_add(1)
                    })
                    .map_err(|_| PropsError::RevisionOverflow)?,
            )
        } else {
            None
        };
        let slot = if let Some(&slot) = self.positions.get(&sample.position) {
            slot
        } else {
            if !selected {
                return Ok(());
            }
            let slot = *self.free.last().ok_or(PropsError::Capacity)?;
            let state = self
                .pages
                .get_mut(&sample.position.page())
                .ok_or(PropsError::Capacity)?;
            let member = if let Some(member) = state.free_members.pop() {
                state.members[member] = slot;
                member
            } else {
                let member = state.members.len();
                state.members.push(slot);
                member
            };
            state.live += 1;
            self.free.pop();
            self.positions.insert(sample.position, slot);
            self.slots[slot as usize] = Slot {
                position: Some(sample.position),
                member,
                ..Slot::empty()
            };
            slot
        };
        if let Some(registration) = registration {
            self.registrations
                .as_mut()
                .expect("prepared temporal registrations")[slot as usize] = registration;
        }
        let state = &mut self.slots[slot as usize];
        let mut sample = sample;
        if let Some(old) = state.sample
            && old.cell.id() == sample.cell.id()
        {
            sample.animation = old.animation;
        }
        let sample = selected.then_some(sample);
        if state.sample == sample {
            return Ok(());
        }
        state.sample = sample;
        if !state.queued {
            state.queued = true;
            state.first = tick;
            self.dirty.push_back(slot);
        }
        self.apply_public_slot(slot, tick, false);
        Ok(())
    }
    fn set_from_world(&mut self, world: &World, p: BlockPos, tint: u32) -> Result<(), PropsError> {
        if !self.in_region(p.page()) {
            return Ok(());
        }
        let Some(page) = world.page(p.page()) else {
            return self.remove(p, world.tick());
        };
        let light = page.light(p.local_index());
        self.work.owner_light_reads += 1;
        self.set(
            BlockSample {
                position: p,
                cell: page.cell(p.local_index()),
                light: PackedLight::new(light.sky, light.block, light.rgb)
                    .expect("valid world light")
                    .raw(),
                tint,
                animation: Animation::default(),
            },
            world.tick(),
        )
    }
    fn remove(&mut self, p: BlockPos, tick: Tick) -> Result<(), PropsError> {
        self.set(
            BlockSample {
                position: p,
                cell: Cell::AIR,
                light: 0,
                tint: u32::MAX,
                animation: Animation::default(),
            },
            tick,
        )
    }
    /// Immutable public visual state only. Never decode inventory/window payloads.
    pub fn set_animation(
        &mut self,
        p: BlockPos,
        animation: Animation,
        tick: Tick,
    ) -> Result<bool, PropsError> {
        if !animation.chest_open.is_finite()
            || !(0.0..=1.0).contains(&animation.chest_open)
            || !animation.rotor_radians.is_finite()
        {
            return Err(ModelError::InvalidAnimation.into());
        }
        let Some(&index) = self.positions.get(&p) else {
            return Ok(false);
        };
        let slot = &mut self.slots[index as usize];
        let Some(sample) = slot.sample.as_mut() else {
            return Ok(false);
        };
        if sample.animation == animation {
            return Ok(false);
        }
        sample.animation = animation;
        if !slot.queued {
            slot.queued = true;
            slot.first = tick;
            self.dirty.push_back(index);
        }
        Ok(true)
    }
    /// Exact raw cell-view binding is supplied by ClientScene, never inferred
    /// from a zero light value or the derived World page revision.
    pub fn publish_public_frame(
        &mut self,
        frame: Arc<net::props::Frame>,
        current_cell_view: u64,
    ) -> Result<bool, PropsError> {
        frame.validate().map_err(|_| PropsError::PublicFrame)?;
        if frame.page.space != SpaceId(0) {
            return Err(PropsError::Space);
        }
        if frame.cell_view != current_cell_view {
            return Ok(false);
        }
        if let Some(old) = self.public_frames.get(&frame.page)
            && old.cell_view == frame.cell_view
            && (old.view >= frame.view || old.source > frame.source)
        {
            return Ok(false);
        }
        if !self.public_frames.contains_key(&frame.page)
            && self.public_frames.len() >= self.limits.max_pages * 2
        {
            return Err(PropsError::Capacity);
        }
        let previous = self
            .public_frames
            .get(&frame.page)
            .map_or(0, |old| old.hints.len());
        let count = self.public_hint_count - previous + frame.hints.len();
        if count > self.limits.max_instances {
            return Err(PropsError::Capacity);
        }
        let revision = self
            .public_revision
            .checked_add(1)
            .ok_or(PropsError::RevisionOverflow)?;
        self.queue_public_page(frame.page)?;
        self.public_frames.insert(frame.page, frame);
        self.public_hint_count = count;
        self.public_revision = revision;
        Ok(true)
    }
    pub fn public_binding(&self, page: PageId) -> Option<(u64, u64)> {
        self.public_frames.get(&page).map(|f| (f.cell_view, f.view))
    }
    /// Public visual snapshots only. Consumers must compare cell_view with
    /// ClientScene again before using a cached audio emitter or visual hint.
    pub fn public_revision(&self) -> u64 {
        self.public_revision
    }
    pub fn public_frame(&self, page: PageId) -> Option<&net::props::Frame> {
        self.public_frames.get(&page).map(AsRef::as_ref)
    }
    pub fn public_frames_after(
        &self,
        after: Option<PageId>,
    ) -> impl Iterator<Item = &net::props::Frame> {
        use std::ops::Bound::{Excluded, Unbounded};
        self.public_frames
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .map(|(_, frame)| frame.as_ref())
    }
    pub fn public_frames(&self) -> impl Iterator<Item = &net::props::Frame> {
        self.public_frames.values().map(AsRef::as_ref)
    }
    pub fn revoke_public_frame(&mut self, page: PageId, tick: Tick) -> Result<bool, PropsError> {
        if !self.public_frames.contains_key(&page) {
            return Ok(false);
        }
        let revision = self
            .public_revision
            .checked_add(1)
            .ok_or(PropsError::RevisionOverflow)?;
        self.queue_public_page(page)?;
        if let Some(old) = self.public_frames.remove(&page) {
            self.public_hint_count -= old.hints.len();
        }
        self.visual_time.0 = tick;
        self.public_revision = revision;
        Ok(true)
    }
    fn queue_public_page(&mut self, page: PageId) -> Result<(), PropsError> {
        if let Some(state) = self.pages.get_mut(&page) {
            if state.public_next.is_none() {
                if self.public_jobs.len() >= self.limits.max_pages * 2 {
                    return Err(PropsError::Capacity);
                }
                self.public_jobs.push_back(page);
            }
            state.public_next = Some(0);
        }
        Ok(())
    }
    /// Applies a full public snapshot by visiting indexed members with explicit
    /// credits; absent hints reset previous visual state. No 4096-cell scan.
    pub fn pump_public(&mut self, now: Tick, alpha: f64, max_members: usize) -> usize {
        self.visual_time = (
            now,
            if alpha.is_finite() {
                alpha.clamp(0., 1.)
            } else {
                0.
            },
        );
        let mut used = 0;
        while used < max_members {
            let Some(&page) = self.public_jobs.front() else {
                break;
            };
            let Some(state) = self.pages.get_mut(&page) else {
                self.public_jobs.pop_front();
                used += 1;
                continue;
            };
            let next = state.public_next.unwrap_or(0);
            if let Some(&index) = state.members.get(next) {
                state.public_next = Some(next + 1);
                self.work.public_members += 1;
                if index != u32::MAX {
                    self.apply_public_slot(index, now, true);
                }
            } else {
                state.public_next = None;
                self.public_jobs.pop_front();
            }
            used += 1;
        }
        used
    }
    /// Only active rotors participate. Stationary/no-public-state frames do no
    /// model traversal or upload; interpolation does not need new server packets.
    pub fn animate_public(&mut self, now: Tick, alpha: f64, max_models: usize) -> usize {
        self.visual_time = (
            now,
            if alpha.is_finite() {
                alpha.clamp(0., 1.)
            } else {
                0.
            },
        );
        let count = max_models.min(self.animated.len());
        self.work.animated_models += count as u64;
        for _ in 0..count {
            self.animation_cursor %= self.animated.len();
            let index = self.animated[self.animation_cursor];
            self.animation_cursor += 1;
            let slot = &mut self.slots[index as usize];
            if let (Some((hint, epoch)), Some(sample)) = (slot.wind, slot.sample.as_mut()) {
                let angle = hint.rotor_at(epoch, now, self.visual_time.1);
                if sample.animation.rotor_radians != angle {
                    sample.animation.rotor_radians = angle;
                    if !slot.queued {
                        slot.queued = true;
                        slot.first = now;
                        self.dirty.push_back(index);
                    }
                }
            }
        }
        count
    }
    fn unanimate(&mut self, index: u32) {
        let Some(member) = self.slots[index as usize].animated_member.take() else {
            return;
        };
        self.animated.swap_remove(member);
        if let Some(&moved) = self.animated.get(member) {
            self.slots[moved as usize].animated_member = Some(member);
        }
    }
    fn apply_public_slot(&mut self, index: u32, tick: Tick, reset_without_frame: bool) {
        let slot = &self.slots[index as usize];
        let Some(sample) = slot.sample else {
            self.unanimate(index);
            return;
        };
        let frame = self.public_frames.get(&sample.position.page());
        if frame.is_none() && !reset_without_frame {
            return;
        }
        let hint = frame
            .and_then(|f| {
                f.hints
                    .binary_search_by_key(&sample.position.local_index(), |h| h.index)
                    .ok()
                    .map(|i| (f.hints[i], f.tick))
            })
            .filter(|(h, _)| hint_matches(h.kind, sample.cell));
        let mut animation = Animation::default();
        let mut ports = None;
        let mut wind = None;
        if let Some((hint, epoch)) = hint {
            animation.active = hint.flags & net::props::ACTIVE != 0;
            animation.chest_open = hint.lid_amount();
            if hint.flags & net::props::ROTOR != 0 {
                animation.rotor_radians =
                    hint.rotor_at(epoch, self.visual_time.0, self.visual_time.1);
                if hint.rotor_velocity != 0 {
                    wind = Some((hint, epoch));
                }
            }
            if hint.flags & net::props::PORTS != 0 {
                ports = Some((hint.kind, hint.ports));
            }
        }
        if wind.is_some() && self.slots[index as usize].animated_member.is_none() {
            self.slots[index as usize].animated_member = Some(self.animated.len());
            self.animated.push(index);
        } else if wind.is_none() {
            self.unanimate(index);
        }
        let slot = &mut self.slots[index as usize];
        slot.wind = wind;
        let sample = slot.sample.as_mut().expect("retained public model");
        if sample.animation != animation || slot.ports != ports {
            sample.animation = animation;
            slot.ports = ports;
            if !slot.queued {
                slot.queued = true;
                slot.first = tick;
                self.dirty.push_back(index);
            }
        }
    }
    /// A lagged journal cannot describe this derived cache any longer. Capture
    /// only current bounded page HEADERS, then reconcile existing indexed slots
    /// incrementally and rediscover new models on immutable worker snapshots.
    /// No gameplay consumer is advanced and no cell traversal runs here.
    fn recover_lagged_cursor(&mut self, world: &mut World) -> Result<(), PropsError> {
        let revision = self
            .public_revision
            .checked_add(1)
            .ok_or(PropsError::RevisionOverflow)?;
        if world.loaded_pages() > self.limits.max_pages {
            return Err(PropsError::Capacity);
        }
        self.recovery_pages.clear();
        let region = self.region;
        for (page, _) in world.pages() {
            self.work.recovery_headers += 1;
            if page.space == SpaceId(0) && region.is_none_or(|r| r.contains(page)) {
                self.recovery_pages.push(page);
            }
        }
        let boundary = world.journal_next();
        self.pending = None;
        // Keep old running reservations: ViewProps retains their contexts,
        // retry slots or pending terminal results until normal completion.
        self.recovery_job_floor = self.next_job;
        self.ready.clear();
        self.jobs.clear();
        self.light_jobs.clear();
        self.public_jobs.clear();
        // A discarded removal can leave a frame outside the current bindings.
        // Restore only frames the connection can rebind to current public Raw.
        self.work.recovery_headers += self.public_frames.len() as u64;
        self.public_frames.clear();
        self.public_hint_count = 0;
        self.public_revision = revision;
        self.retiring.clear();
        self.work.recovery_headers += self.pages.len() as u64;
        self.pages.retain(|_, page| {
            page.known = false;
            page.queued = false;
            // Existing running flags retain exactly their old reservations.
            page.light_next = None;
            page.public_next = None;
            page.retiring = false;
            page.first = Some(world.tick());
            page.live > 0 || page.running
        });
        self.recovery = Some(CursorRecovery { slot: 0, page: 0 });
        world.recover_cursor(self.cursor, boundary)?;
        self.sequence = boundary;
        self.work.cursor_recoveries += 1;
        Ok(())
    }
    fn advance_cursor_recovery(
        &mut self,
        world: &World,
        mut tint: impl FnMut(BlockPos) -> u32,
    ) -> Result<bool, PropsError> {
        let Some(recovery) = &self.recovery else {
            return Ok(false);
        };
        if recovery.slot < self.slots.len() {
            let next = recovery.slot;
            if let Some(position) = self.slots[next].position {
                // A missing journal can contain remove/recreate ABA. Revoke the
                // former optional temporal registration even for equal cells.
                self.remove(position, world.tick())?;
                self.slots[next].ports = None;
                self.slots[next].wind = None;
                self.set_from_world(world, position, tint(position))?;
            }
            self.recovery.as_mut().expect("cursor snapshot").slot += 1;
            self.work.recovery_slots += 1;
            return Ok(true);
        }
        if let Some(&page) = self.recovery_pages.get(recovery.page) {
            match self.watch_page(page, world.tick()) {
                Ok(()) => {}
                Err(PropsError::Capacity) => {
                    // Normal hidden-record GPU publication releases old slots
                    // and page headers. Retain this exact snapshot request.
                    self.work.backpressure = true;
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
            self.recovery.as_mut().expect("cursor snapshot").page += 1;
            self.work.recovery_pages += 1;
            return Ok(true);
        }
        self.recovery = None;
        self.recovery_pages.clear();
        Ok(false)
    }
    /// Entry credits include created/removed pages, individual cells/lights and ACK.
    pub fn ingest(
        &mut self,
        world: &mut World,
        max_entries: usize,
        mut tint: impl FnMut(BlockPos) -> u32,
    ) -> Result<usize, PropsError> {
        if max_entries == 0 {
            return Ok(0);
        }
        if self.sequence < world.journal_oldest() {
            self.recover_lagged_cursor(world)?;
        }
        let mut used = 0;
        while used < max_entries && self.recovery.is_some() {
            if !self.advance_cursor_recovery(world, &mut tint)? {
                if self.recovery.is_some() {
                    return Ok(used);
                }
                break;
            }
            used += 1;
        }
        if self.recovery.is_some() {
            return Ok(used);
        }
        while used < max_entries && self.advance_retirement(world.tick())? {
            used += 1;
        }
        let retirement_credits = used;
        while used < max_entries {
            let mut pending = if let Some(p) = self.pending.take() {
                p
            } else {
                if self.sequence == world.journal_next() {
                    break;
                }
                let batch = world.read_cursor(self.cursor, 1)?;
                let Some(change) = batch.changes.into_iter().next() else {
                    break;
                };
                Pending {
                    change,
                    next: batch.next,
                    page: 0,
                    cell: 0,
                    light: 0,
                    removed: 0,
                }
            };
            let result = (|| -> Result<bool, PropsError> {
                if pending.page < pending.change.pages.len() {
                    let change = &pending.change.pages[pending.page];
                    if change.page.space == SpaceId(0) {
                        if change.removed {
                            if self.public_frames.contains_key(&change.page) {
                                let revision = self
                                    .public_revision
                                    .checked_add(1)
                                    .ok_or(PropsError::RevisionOverflow)?;
                                if let Some(old) = self.public_frames.remove(&change.page) {
                                    self.public_hint_count -= old.hints.len();
                                }
                                self.public_revision = revision;
                            }
                            if let Some(state) = self.pages.get_mut(&change.page) {
                                state.known = false;
                                let member = state.members.get(pending.removed).copied();
                                if let Some(slot) = member {
                                    if slot == u32::MAX {
                                        pending.removed += 1;
                                        return Ok(false);
                                    }
                                    let p = self.slots[slot as usize]
                                        .position
                                        .expect("member position");
                                    self.remove(p, pending.change.tick)?;
                                    pending.removed += 1;
                                    return Ok(false);
                                }
                            }
                        } else if change.created {
                            self.watch_page(change.page, pending.change.tick)?;
                        }
                    }
                    pending.page += 1;
                    pending.removed = 0;
                } else if pending.cell < pending.change.cells.len() {
                    let cell = &pending.change.cells[pending.cell];
                    if cell.page.space == SpaceId(0)
                        && let Some(p) = position(cell.page, cell.index)
                    {
                        self.set_from_world(world, p, tint(p))?;
                    }
                    pending.cell += 1;
                } else if pending.light < pending.change.lights.len() {
                    let light = &pending.change.lights[pending.light];
                    if let Some(p) = position(light.page, light.index)
                        && self.positions.contains_key(&p)
                    {
                        self.set_from_world(world, p, tint(p))?;
                    }
                    pending.light += 1;
                } else {
                    world.acknowledge_cursor(self.cursor, pending.next)?;
                    self.sequence = pending.next;
                    return Ok(true);
                }
                Ok(false)
            })();
            used += 1;
            match result {
                Ok(true) => {}
                Ok(false) => self.pending = Some(pending),
                Err(e) => {
                    self.pending = Some(pending);
                    self.work.backpressure = true;
                    return Err(e);
                }
            }
        }
        self.work.journal_entries += (used - retirement_credits) as u64;
        Ok(used)
    }
    pub fn take_job(
        &mut self,
        world: &World,
        mut tints: impl FnMut(PageId) -> Box<[u32; 256]>,
    ) -> Option<PropsTask> {
        if self.running.len() >= self.limits.max_running
            || self.ready.len() >= self.limits.max_ready
        {
            return None;
        }
        let probes = self.jobs.len().min(16);
        for _ in 0..probes {
            let page = self.jobs.pop_front()?;
            let state = self.pages.get_mut(&page)?;
            state.queued = false;
            if !state.known {
                if state.live == 0 && !state.running {
                    self.pages.remove(&page);
                }
                continue;
            }
            if state.running {
                state.queued = true;
                self.jobs.push_back(page);
                continue;
            }
            let Some(data) = world.snapshot(page) else {
                state.known = false;
                continue;
            };
            let source = world.input_version(page, CHANNELS);
            let id = self.next_job;
            self.next_job = self.next_job.checked_add(1)?;
            state.running = true;
            self.running.insert(id, (page, source));
            self.work.jobs_submitted += 1;
            return Some(PropsTask {
                id,
                page,
                source,
                data,
                tints: tints(page),
            });
        }
        None
    }
    /// On a full ready queue the caller retains the owned result and retries.
    pub fn complete(
        &mut self,
        world: &World,
        built: Box<PropsBuilt>,
    ) -> Result<bool, Box<PropsBuilt>> {
        let mut seen = [0u64; 64];
        let valid = built.mask_words == 64
            && built.cells <= 4096
            && built.samples.len() <= built.cells as usize
            && built.samples.iter().all(|sample| {
                if sample.position.page() != built.page || !eligible(sample.cell) {
                    return false;
                }
                let index = usize::from(sample.position.local_index().get());
                let bit = 1u64 << (index % 64);
                if seen[index / 64] & bit != 0 {
                    return false;
                }
                seen[index / 64] |= bit;
                true
            });
        let current = valid
            && built.id >= self.recovery_job_floor
            && self
                .running
                .get(&built.id)
                .is_some_and(|&(page, source)| page == built.page && source == built.source)
            && self.pages.get(&built.page).is_some_and(|p| p.known)
            && world.page(built.page).is_some()
            && world.input_version(built.page, CHANNELS) == built.source;
        if current && self.ready.len() >= self.limits.max_ready {
            self.work.backpressure = true;
            return Err(built);
        }
        self.work.worker_mask_words += built.mask_words;
        self.work.worker_cells += built.cells;
        self.work.worker_models += built.samples.len() as u64;
        if let Some((page, _)) = self.running.remove(&built.id) {
            if let Some(state) = self.pages.get_mut(&page) {
                state.running = false;
            }
            if !current {
                self.work.jobs_stale += 1;
                if world.page(page).is_some() {
                    let _ = self.watch_page(page, world.tick());
                }
                self.prune_released_page(page);
                return Ok(false);
            }
        } else {
            self.work.jobs_stale += 1;
            return Ok(false);
        }
        self.ready.push_back(Ready { built, next: 0 });
        Ok(true)
    }
    fn prune_released_page(&mut self, page: PageId) {
        if self
            .pages
            .get(&page)
            .is_some_and(|p| !p.known && !p.queued && !p.running && p.live == 0)
        {
            self.pages.remove(&page);
        }
    }
    pub fn fail_job(&mut self, world: &World, id: u64) {
        if let Some((page, _)) = self.running.remove(&id) {
            if let Some(state) = self.pages.get_mut(&page) {
                state.running = false;
            }
            if world.page(page).is_some() {
                let _ = self.watch_page(page, world.tick());
            }
            self.prune_released_page(page);
        }
    }
    /// Copies only bounded model records; full-cell traversal stayed on worker.
    pub fn pump_ready(&mut self, world: &World, max_records: usize) -> Result<usize, PropsError> {
        let mut count = 0;
        while count < max_records {
            let Some(ready) = self.ready.front() else {
                break;
            };
            let page = ready.built.page;
            if world.page(page).is_none()
                || world.input_version(page, CHANNELS) != ready.built.source
                || !self.in_region(page)
                || self.pages.get(&page).is_none_or(|state| !state.known)
            {
                self.ready.pop_front();
                self.work.jobs_stale += 1;
                if world.page(page).is_some() {
                    self.watch_page(page, world.tick())?;
                }
                continue;
            }
            let Some(mut sample) = ready.built.samples.get(ready.next).copied() else {
                self.ready.pop_front();
                if let Some(state) = self.pages.get_mut(&page)
                    && !state.queued
                    && !state.running
                {
                    state.first = None;
                }
                continue;
            };
            let light = world
                .page(page)
                .expect("known ready page")
                .light(sample.position.local_index());
            self.work.owner_light_reads += 1;
            sample.light = PackedLight::new(light.sky, light.block, light.rgb)
                .expect("valid world light")
                .raw();
            self.set(sample, world.tick())?;
            self.ready.front_mut().expect("retained ready").next += 1;
            count += 1;
        }
        Ok(count)
    }
    /// Caller uses loading reserve_slots(max_instances). Insufficient budget keeps
    /// the exact pending record; a hidden slot is reclaimed only after publication.
    pub fn flush(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut BlockModelsGpu,
        origin: [i64; 3],
        budget: &mut UploadBudget,
        max_records: usize,
    ) -> Result<usize, PropsError> {
        self.flush_inner(
            queue,
            gpu,
            origin,
            budget,
            max_records,
            |sample| sample,
            |_| {},
        )
    }
    /// Rebind only indexed models after a public LIGHT certificate changes,
    /// including zero snapshots and raw-view revocations without a WorldChange.
    pub fn mark_light_binding(&mut self, page: PageId, tick: Tick) -> bool {
        let Some(state) = self.pages.get_mut(&page) else {
            return true;
        };
        if state.light_next.is_none() {
            if self.light_jobs.len() >= self.limits.max_pages * 2 {
                self.work.backpressure = true;
                return false;
            }
            self.light_jobs.push_back(page);
        }
        // A newer certificate can supersede a partially processed cursor.
        state.light_next = Some(0);
        state.first = Some(state.first.map_or(tick, |old| old.min(tick)));
        true
    }
    fn advance_light_binding(&mut self) -> bool {
        let Some(&page) = self.light_jobs.front() else {
            return false;
        };
        let Some(state) = self.pages.get_mut(&page) else {
            self.light_jobs.pop_front();
            return true;
        };
        let next = state.light_next.unwrap_or(0);
        if let Some(&index) = state.members.get(next) {
            state.light_next = Some(next + 1);
            self.work.binding_members += 1;
            if index != u32::MAX {
                let slot = &mut self.slots[index as usize];
                if slot.sample.is_some() && !slot.queued {
                    slot.queued = true;
                    slot.first = state.first.unwrap_or(Tick(0));
                    self.dirty.push_back(index);
                }
            }
        } else {
            state.light_next = None;
            self.light_jobs.pop_front();
            if !state.queued && !state.running {
                state.first = None;
            }
        }
        true
    }
    /// The sampler supplies current certified light; None publishes darkness.
    /// Member cursor visits and uploads share max_records credits.
    #[allow(clippy::too_many_arguments)] // Explicit GPU publication inputs plus a certificate sampler.
    pub fn flush_sampled(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut BlockModelsGpu,
        origin: [i64; 3],
        budget: &mut UploadBudget,
        max_records: usize,
        light_at: impl FnMut(BlockPos) -> Option<world::LightSample>,
    ) -> Result<usize, PropsError> {
        self.flush_sampled_with_publications(
            queue,
            gpu,
            origin,
            budget,
            max_records,
            light_at,
            |_| {},
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn flush_sampled_with_publications(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut BlockModelsGpu,
        origin: [i64; 3],
        budget: &mut UploadBudget,
        max_records: usize,
        mut light_at: impl FnMut(BlockPos) -> Option<world::LightSample>,
        published: impl FnMut(PropsPublication),
    ) -> Result<usize, PropsError> {
        let mut reads = 0;
        let result = self.flush_inner(
            queue,
            gpu,
            origin,
            budget,
            max_records,
            |mut sample| {
                reads += 1;
                sample.light = light_at(sample.position)
                    .and_then(|light| PackedLight::new(light.sky, light.block, light.rgb))
                    .unwrap_or_default()
                    .raw();
                sample
            },
            published,
        );
        self.work.owner_light_reads += reads;
        result
    }
    #[allow(clippy::too_many_arguments)] // Shared implementation of the two publication policies above.
    fn flush_inner(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut BlockModelsGpu,
        origin: [i64; 3],
        budget: &mut UploadBudget,
        max_records: usize,
        mut sample_at: impl FnMut(BlockSample) -> BlockSample,
        mut published: impl FnMut(PropsPublication),
    ) -> Result<usize, PropsError> {
        if gpu.reserved_slots() < self.limits.max_instances {
            return Err(PropsError::InvalidLimits);
        }
        let mut count = 0;
        let mut credits = 0;
        while credits < max_records {
            let Some(&index) = self.dirty.front() else {
                if self.advance_light_binding() {
                    credits += 1;
                    continue;
                }
                break;
            };
            if budget.remaining_bytes < 64 {
                self.work.backpressure = true;
                break;
            }
            let slot = &self.slots[index as usize];
            let value = if let Some(sample) = slot.sample {
                let mut value =
                    instance(sample_at(sample), origin)?.unwrap_or_else(BlockInstance::hidden);
                if let Some((kind, ports)) = slot.ports {
                    if kind == net::props::Kind::Connector {
                        value.connections = ports & 0xfff;
                        let color = ports >> 12;
                        value.reserved[3] = if color == 0 { 0 } else { (1 << 30) | color };
                    } else {
                        value.reserved[3] = ports | (1 << 31);
                    }
                }
                value
            } else {
                BlockInstance::hidden()
            };
            let work = gpu.update(queue, index, value, budget)?;
            published(PropsPublication {
                slot: index,
                position: slot.sample.map(|sample| sample.position),
                registration: self
                    .registrations
                    .as_ref()
                    .map_or(0, |bank| bank[index as usize]),
            });
            self.work.upload_bytes += work.upload_bytes;
            self.work.records_uploaded += u64::from(work.upload_bytes > 0);
            self.work.changed |= work.upload_bytes > 0;
            self.dirty.pop_front();
            if self.slots[index as usize].sample.is_none() {
                self.unanimate(index);
            }
            let slot = &mut self.slots[index as usize];
            slot.queued = false;
            if slot.sample.is_none() {
                let p = slot.position.take().expect("dirty member");
                let member = slot.member;
                self.positions.remove(&p);
                if let Some(page) = self.pages.get_mut(&p.page()) {
                    page.members[member] = u32::MAX;
                    page.free_members.push(member);
                    page.live -= 1;
                    if !page.known && !page.queued && !page.running && page.live == 0 {
                        self.pages.remove(&p.page());
                    }
                }
                self.free.push(index);
            }
            count += 1;
            credits += 1;
        }
        Ok(count)
    }
    /// Explicit origin changes revisit only indexed model records, never cells.
    pub fn invalidate_origin(&mut self, tick: Tick) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.sample.is_some() && !slot.queued {
                slot.queued = true;
                slot.first = tick;
                self.dirty.push_back(index as u32);
            }
        }
    }
    pub fn stats(&self, world: &World) -> PropsStats {
        let caught = self.sequence == world.journal_next()
            && self.pending.is_none()
            && self.recovery.is_none();
        let oldest = self
            .slots
            .iter()
            .filter(|s| s.queued)
            .map(|s| world.tick().0.saturating_sub(s.first.0))
            .chain(
                self.pages
                    .values()
                    .filter_map(|p| p.first)
                    .map(|first| world.tick().0.saturating_sub(first.0)),
            )
            .max()
            .unwrap_or(0);
        PropsStats {
            pages: self.pages.len(),
            models: self.positions.len(),
            queued: self.dirty.len(),
            running: self.running.len(),
            ready: self.ready.len(),
            queued_pages: self.jobs.len(),
            light_bindings: self.light_jobs.len(),
            public_pages: self.public_jobs.len(),
            retirements: self.retiring.len(),
            oldest_age_ticks: oldest,
            overdue: oldest > self.limits.max_age_ticks,
            recovering_cursor: self.recovery.is_some(),
            recovery_slots_remaining: self
                .recovery
                .as_ref()
                .map_or(0, |r| self.slots.len().saturating_sub(r.slot)),
            recovery_pages_remaining: self
                .recovery
                .as_ref()
                .map_or(0, |r| self.recovery_pages.len().saturating_sub(r.page)),
            caught_up: caught,
            settled: caught
                && self.dirty.is_empty()
                && self.jobs.is_empty()
                && self.running.is_empty()
                && self.ready.is_empty()
                && self.light_jobs.is_empty()
                && self.public_jobs.is_empty()
                && self.retiring.is_empty(),
        }
    }
    pub fn take_work(&mut self) -> PropsWork {
        std::mem::take(&mut self.work)
    }
    /// Already indexed model pages only; no World lookup or cell traversal.
    pub fn model_pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.pages
            .iter()
            .filter(|(_, page)| page.live > 0)
            .map(|(id, _)| *id)
    }
    /// Before detach, queue/flush every model removal if GPU owner is retained.
    pub fn clear(&mut self, tick: Tick) -> Result<(), PropsError> {
        self.recovery = None;
        self.recovery_pages.clear();
        self.retiring.clear();
        let revision = self
            .public_revision
            .checked_add(1)
            .ok_or(PropsError::RevisionOverflow)?;
        self.running.clear();
        self.ready.clear();
        self.jobs.clear();
        self.light_jobs.clear();
        self.public_jobs.clear();
        self.public_frames.clear();
        self.public_hint_count = 0;
        self.animated.clear();
        self.animation_cursor = 0;
        self.pages.retain(|_, page| {
            page.known = false;
            page.queued = false;
            page.running = false;
            page.first = None;
            page.light_next = None;
            page.public_next = None;
            page.live > 0
        });
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.position.is_some() {
                slot.sample = None;
                slot.wind = None;
                slot.ports = None;
                slot.animated_member = None;
                if !slot.queued {
                    slot.queued = true;
                    slot.first = tick;
                    self.dirty.push_back(index as u32);
                }
            }
        }
        self.public_revision = revision;
        Ok(())
    }
    pub fn detach(self, world: &mut World) {
        world.remove_cursor(self.cursor);
    }
}
fn eligible(cell: Cell) -> bool {
    crate::block_models::is_model_candidate(cell)
}
fn hint_matches(kind: net::props::Kind, cell: Cell) -> bool {
    let Some(definition) = rules::block(cell.id()) else {
        return false;
    };
    use net::props::Kind;
    match kind {
        Kind::None => true,
        Kind::Chest => matches!(
            definition.render,
            rules::blocks::Render::Model(rules::Model::Chest)
        ),
        Kind::Connector => matches!(
            definition.render,
            rules::blocks::Render::Model(rules::Model::Cable)
        ),
        Kind::Wind => cell.id() == rules::ids::WIND_GENERATOR,
        Kind::Machine => {
            cell.id() != rules::ids::WIND_GENERATOR
                && (crate::block_models::is_classic_machine(cell)
                    || matches!(
                        definition.render,
                        rules::blocks::Render::Model(rules::Model::Mek)
                    ))
        }
    }
}
fn position(page: PageId, index: LocalIdx) -> Option<BlockPos> {
    let (x, y, z) = index.xyz();
    Some(BlockPos {
        space: page.space,
        x: i32::try_from(i64::from(page.x) * 16 + i64::from(x)).ok()?,
        y: i32::try_from(i64::from(page.y) * 16 + i64::from(y)).ok()?,
        z: i32::try_from(i64::from(page.z) * 16 + i64::from(z)).ok()?,
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub fn native_pool(
    workers: usize,
    capacity: usize,
) -> Result<jobs::NativePool<PropsTask, Box<PropsBuilt>>, jobs::PoolError> {
    jobs::NativePool::new(
        workers,
        capacity,
        |task: PropsTask, cancel: &jobs::CancellationToken| {
            task.execute(|| cancel.is_cancelled())
                .ok_or_else(|| "cancelled model snapshot".into())
        },
    )
}

#[cfg(test)]
mod revision_tests {
    use super::*;
    #[test]
    fn public_revision_overflow_rejects_before_mutation_and_range_cursor_is_bounded() {
        let mut world = World::new(rules::properties, 32);
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        let mut tx = world::Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(page, vec![Cell::AIR; 4096]);
        world.commit(tx).unwrap();
        let mut owner = PropsSource::attach(
            &mut world,
            PropsLimits {
                max_pages: 2,
                max_instances: 16,
                ..PropsLimits::default()
            },
        )
        .unwrap();
        let make = |page, view| {
            Arc::new(net::props::Frame {
                page,
                cell_view: 10,
                view,
                source: Version(view),
                tick: Tick(1),
                hints: vec![],
            })
        };
        owner.publish_public_frame(make(page, 1), 10).unwrap();
        let next = PageId { x: 2, ..page };
        owner.publish_public_frame(make(next, 1), 10).unwrap();
        assert_eq!(owner.public_revision(), 2);
        assert_eq!(
            owner.public_frames_after(None).take(1).next().unwrap().page,
            page
        );
        assert_eq!(
            owner
                .public_frames_after(Some(page))
                .take(1)
                .next()
                .unwrap()
                .page,
            next
        );
        assert!(owner.public_frames_after(Some(next)).next().is_none());
        assert!(!owner.publish_public_frame(make(page, 1), 10).unwrap());
        assert_eq!(owner.public_revision(), 2);
        owner.public_revision = u64::MAX;
        assert!(matches!(
            owner.publish_public_frame(make(page, 2), 10),
            Err(PropsError::RevisionOverflow)
        ));
        assert!(matches!(
            owner.revoke_public_frame(page, Tick(2)),
            Err(PropsError::RevisionOverflow)
        ));
        assert!(matches!(
            owner.clear(Tick(2)),
            Err(PropsError::RevisionOverflow)
        ));
        assert_eq!(owner.public_frame(page).unwrap().view, 1);
        assert_eq!(owner.public_frames().count(), 2);
        assert_eq!(owner.stats(&world).queued, 0);
        owner.detach(&mut world);
    }
}

#[cfg(test)]
mod cursor_recovery_tests {
    use super::*;
    use world::{GameplayEvent, JournalLimits, Transaction};
    fn position(x: i32) -> BlockPos {
        BlockPos {
            space: SpaceId(0),
            x,
            y: 4,
            z: 8,
        }
    }
    fn fixture() -> (World, PropsSource) {
        let mut world = World::with_limits(
            rules::properties,
            JournalLimits {
                max_records: 2,
                ..JournalLimits::default()
            },
        );
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(position(0).page(), vec![Cell::AIR; 4096]);
        tx.set_cell(position(2), Cell::from_parts(rules::ids::CHEST, 0));
        world.commit(tx).unwrap();
        let mut owner = PropsSource::attach(
            &mut world,
            PropsLimits {
                max_pages: 4,
                max_instances: 16,
                max_running: 2,
                max_ready: 2,
                max_age_ticks: 40,
            },
        )
        .unwrap();
        finish(&mut owner, &world);
        (world, owner)
    }
    fn finish(owner: &mut PropsSource, world: &World) {
        while let Some(job) = owner.take_job(world, |_| Box::new([u32::MAX; 256])) {
            assert!(
                owner
                    .complete(world, job.execute(|| false).unwrap())
                    .map_err(|_| ())
                    .unwrap()
            );
            owner.pump_ready(world, 16).unwrap();
        }
    }
    fn event(world: &mut World, tick: u64) {
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.event(GameplayEvent {
            kind: 7,
            subject: None,
            position: Some(position(1)),
            payload: vec![tick as u8],
        });
        world.commit(tx).unwrap();
    }
    fn recover(owner: &mut PropsSource, world: &mut World) {
        for _ in 0..64 {
            assert!(owner.ingest(world, 1, |_| u32::MAX).unwrap() <= 1);
            if owner.stats(world).caught_up {
                return;
            }
        }
        panic!("bounded snapshot did not converge");
    }
    #[test]
    fn lagged_partial_record_installs_current_snapshot_with_one_slot_credit() {
        let (mut world, mut owner) = fixture();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(position(2), Cell::AIR);
        tx.set_cell(position(3), Cell::from_parts(rules::ids::STONE, 0));
        world.commit(tx).unwrap();
        owner.ingest(&mut world, 1, |_| u32::MAX).unwrap();
        assert!(owner.pending.is_some());
        event(&mut world, 3);
        event(&mut world, 4);
        event(&mut world, 5);
        assert!(owner.sequence < world.journal_oldest());
        owner.take_work();
        assert_eq!(owner.ingest(&mut world, 1, |_| u32::MAX).unwrap(), 1);
        assert!(owner.pending.is_none());
        assert!(owner.stats(&world).recovering_cursor);
        assert!(!owner.stats(&world).caught_up);
        let work = owner.take_work();
        assert_eq!(work.cursor_recoveries, 1);
        assert_eq!(work.recovery_slots, 1);
        assert!(work.recovery_headers <= (owner.limits.max_pages * 5) as u64);
        recover(&mut owner, &mut world);
        finish(&mut owner, &world);
        assert!(owner.slots.iter().all(|s| s.sample.is_none()));
    }
    #[test]
    fn vanished_models_and_new_current_models_reconcile_without_replaying_old_rows() {
        let (mut world, mut owner) = fixture();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(position(2), Cell::AIR);
        world.commit(tx).unwrap();
        let mut tx = Transaction::new(Tick(3), SpaceId(0));
        tx.set_cell(position(4), Cell::from_parts(rules::ids::WIND_GENERATOR, 0));
        world.commit(tx).unwrap();
        event(&mut world, 4);
        event(&mut world, 5);
        recover(&mut owner, &mut world);
        finish(&mut owner, &world);
        let alive: Vec<_> = owner
            .slots
            .iter()
            .filter_map(|s| s.sample.map(|s| (s.position, s.cell.id())))
            .collect();
        assert_eq!(alive, vec![(position(4), rules::ids::WIND_GENERATOR)]);
        assert!(
            owner
                .dirty
                .iter()
                .any(|&i| owner.slots[i as usize].position == Some(position(2))
                    && owner.slots[i as usize].sample.is_none()),
            "old GPU record remains queued for an actual hidden publication"
        );
    }
    #[test]
    fn old_worker_with_unchanged_world_source_is_rejected_after_snapshot_boundary() {
        let (mut world, mut owner) = fixture();
        owner.watch_page(position(2).page(), world.tick()).unwrap();
        let old = owner
            .take_job(&world, |_| Box::new([u32::MAX; 256]))
            .unwrap();
        let source = old.source;
        event(&mut world, 2);
        event(&mut world, 3);
        event(&mut world, 4);
        owner.ingest(&mut world, 1, |_| u32::MAX).unwrap();
        assert_eq!(source, world.input_version(position(2).page(), CHANNELS));
        assert!(
            !owner
                .complete(&world, old.execute(|| false).unwrap())
                .map_err(|_| ())
                .unwrap()
        );
    }
    #[test]
    fn snapshot_recovery_resets_optional_registration_even_for_equal_current_cells() {
        let (mut world, mut owner) = fixture();
        owner.prepare_temporal_publications().unwrap();
        let slot = owner.positions[&position(2)] as usize;
        let old = owner.registrations.as_ref().unwrap()[slot];
        event(&mut world, 2);
        event(&mut world, 3);
        event(&mut world, 4);
        recover(&mut owner, &mut world);
        assert!(owner.registrations.as_ref().unwrap()[slot] > old);
        assert_eq!(
            owner.slots[slot].sample.unwrap().cell.id(),
            rules::ids::CHEST
        );
    }
    #[test]
    fn own_recovery_never_advances_another_consumer_or_discards_live_gameplay_events() {
        let (mut world, mut owner) = fixture();
        let lagged = world.register_cursor();
        event(&mut world, 2);
        event(&mut world, 3);
        event(&mut world, 4);
        recover(&mut owner, &mut world);
        assert!(matches!(
            world.read_cursor(lagged, 1),
            Err(CursorError::Lagged { .. })
        ));
        let live = world.register_cursor();
        event(&mut world, 5);
        owner.ingest(&mut world, 16, |_| u32::MAX).unwrap();
        let batch = world.read_cursor(live, 1).unwrap();
        assert_eq!(batch.changes[0].events[0].kind, 7);
        assert_eq!(batch.changes[0].events[0].payload, vec![5]);
    }
    #[test]
    fn zero_credit_call_does_not_capture_or_recover_a_lagged_cursor() {
        let (mut world, mut owner) = fixture();
        event(&mut world, 2);
        event(&mut world, 3);
        event(&mut world, 4);
        let sequence = owner.sequence;
        owner.take_work();
        assert_eq!(owner.ingest(&mut world, 0, |_| u32::MAX).unwrap(), 0);
        assert_eq!(owner.sequence, sequence);
        assert!(owner.recovery.is_none());
        assert_eq!(owner.take_work(), PropsWork::default());
    }

    #[test]
    fn missed_page_removal_discards_its_public_frame_and_notifies_dependents() {
        let (mut world, mut owner) = fixture();
        let frame = Arc::new(net::props::Frame {
            page: position(2).page(),
            cell_view: 10,
            view: 1,
            source: Version(1),
            tick: Tick(1),
            hints: vec![
                net::props::Hint::from_visual(
                    position(2).local_index(),
                    net::props::Kind::Chest,
                    None,
                    false,
                    Some(1.),
                    None,
                )
                .unwrap(),
            ],
        });
        owner.publish_public_frame(frame, 10).unwrap();
        let old_revision = owner.public_revision();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.remove_page(position(2).page());
        world.commit(tx).unwrap();
        event(&mut world, 3);
        event(&mut world, 4);
        event(&mut world, 5);
        owner.ingest(&mut world, 1, |_| u32::MAX).unwrap();
        assert!(owner.public_frames.is_empty());
        assert_eq!(owner.public_hint_count, 0);
        assert_eq!(owner.public_revision(), old_revision + 1);
        recover(&mut owner, &mut world);
        assert!(owner.slots.iter().all(|s| s.sample.is_none()));
    }
    #[test]
    fn recovery_public_revision_overflow_rejects_before_mutating_owned_state() {
        let (mut world, mut owner) = fixture();
        owner.public_revision = u64::MAX;
        event(&mut world, 2);
        event(&mut world, 3);
        event(&mut world, 4);
        let sequence = owner.sequence;
        assert!(matches!(
            owner.ingest(&mut world, 1, |_| u32::MAX),
            Err(PropsError::RevisionOverflow)
        ));
        assert_eq!(owner.sequence, sequence);
        assert!(owner.recovery.is_none());
        assert_eq!(owner.slots.iter().filter(|s| s.sample.is_some()).count(), 1);
    }

    #[test]
    fn repeated_recovery_keeps_running_reservations_until_terminal_results() {
        let (mut world, mut owner) = fixture();
        let other = PageId {
            x: 1,
            ..position(2).page()
        };
        let mut cells = vec![Cell::AIR; 4096];
        cells[0] = Cell::from_parts(rules::ids::CHEST, 0);
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.create_page(other, cells);
        world.commit(tx).unwrap();
        owner.ingest(&mut world, 16, |_| u32::MAX).unwrap();
        owner.watch_page(position(2).page(), world.tick()).unwrap();
        let a = owner
            .take_job(&world, |_| Box::new([u32::MAX; 256]))
            .unwrap();
        let b = owner
            .take_job(&world, |_| Box::new([u32::MAX; 256]))
            .unwrap();
        assert_eq!(owner.running.len(), owner.limits.max_running);
        for base in [3, 6, 9] {
            for tick in base..base + 3 {
                event(&mut world, tick);
            }
            recover(&mut owner, &mut world);
            assert_eq!(owner.running.len(), owner.limits.max_running);
            assert!(
                owner
                    .take_job(&world, |_| Box::new([u32::MAX; 256]))
                    .is_none(),
                "old external contexts still reserve the existing admission capacity"
            );
        }
        assert!(
            !owner
                .complete(&world, a.execute(|| false).unwrap())
                .map_err(|_| ())
                .unwrap()
        );
        let fresh = owner
            .take_job(&world, |_| Box::new([u32::MAX; 256]))
            .unwrap();
        assert!(fresh.id >= owner.recovery_job_floor);
        assert_eq!(owner.running.len(), owner.limits.max_running);
        assert!(
            !owner
                .complete(&world, b.execute(|| false).unwrap())
                .map_err(|_| ())
                .unwrap()
        );
        assert!(owner.running.contains_key(&fresh.id));
    }
    #[test]
    fn old_terminal_result_releases_a_removed_empty_page_header() {
        for failed in [false, true] {
            let mut world = World::with_limits(
                rules::properties,
                JournalLimits {
                    max_records: 2,
                    ..JournalLimits::default()
                },
            );
            let page = position(0).page();
            let mut tx = Transaction::new(Tick(1), SpaceId(0));
            tx.create_page(page, vec![Cell::AIR; 4096]);
            world.commit(tx).unwrap();
            let mut owner = PropsSource::attach(
                &mut world,
                PropsLimits {
                    max_pages: 4,
                    max_instances: 16,
                    ..PropsLimits::default()
                },
            )
            .unwrap();
            let old = owner
                .take_job(&world, |_| Box::new([u32::MAX; 256]))
                .unwrap();
            let mut tx = Transaction::new(Tick(2), SpaceId(0));
            tx.remove_page(page);
            world.commit(tx).unwrap();
            event(&mut world, 3);
            event(&mut world, 4);
            event(&mut world, 5);
            recover(&mut owner, &mut world);
            assert!(owner.pages.contains_key(&page));
            if failed {
                owner.fail_job(&world, old.id)
            } else {
                assert!(
                    !owner
                        .complete(&world, old.execute(|| false).unwrap())
                        .map_err(|_| ())
                        .unwrap()
                );
            }
            assert!(owner.running.is_empty());
            assert!(
                !owner.pages.contains_key(&page),
                "the released reservation must not leak a stale empty page header"
            );
        }
    }

    #[test]
    fn running_ghost_headers_backpressure_exact_snapshot_page_until_release() {
        let mut world = World::with_limits(
            rules::properties,
            JournalLimits {
                max_records: 2,
                ..JournalLimits::default()
            },
        );
        let a = position(0).page();
        let b = PageId { x: 1, ..a };
        let c = PageId { x: 2, ..a };
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(a, vec![Cell::AIR; 4096]);
        world.commit(tx).unwrap();
        let mut owner = PropsSource::attach(
            &mut world,
            PropsLimits {
                max_pages: 1,
                max_instances: 16,
                max_running: 2,
                max_ready: 2,
                max_age_ticks: 40,
            },
        )
        .unwrap();
        let old_a = owner
            .take_job(&world, |_| Box::new([u32::MAX; 256]))
            .unwrap();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.remove_page(a);
        tx.create_page(b, vec![Cell::AIR; 4096]);
        world.commit(tx).unwrap();
        event(&mut world, 3);
        event(&mut world, 4);
        recover(&mut owner, &mut world);
        let old_b = owner
            .take_job(&world, |_| Box::new([u32::MAX; 256]))
            .unwrap();
        assert_eq!(owner.running.len(), 2);
        let mut cells = vec![Cell::AIR; 4096];
        cells[0] = Cell::from_parts(rules::ids::CHEST, 0);
        let mut tx = Transaction::new(Tick(5), SpaceId(0));
        tx.remove_page(b);
        tx.create_page(c, cells);
        world.commit(tx).unwrap();
        event(&mut world, 6);
        event(&mut world, 7);
        for _ in 0..32 {
            assert!(owner.ingest(&mut world, 1, |_| u32::MAX).unwrap() <= 1);
        }
        assert_eq!(owner.pages.len(), 2);
        assert_eq!(owner.running.len(), 2);
        assert!(owner.work.backpressure);
        assert!(!owner.pages.contains_key(&c));
        assert!(
            owner
                .take_job(&world, |_| Box::new([u32::MAX; 256]))
                .is_none()
        );
        let recovery = owner.recovery.as_ref().unwrap();
        assert_eq!(
            recovery.page, 0,
            "blocked C must retain its exact snapshot index"
        );
        assert_eq!(owner.recovery_pages, vec![c]);
        owner.fail_job(&world, old_a.id);
        assert!(!owner.pages.contains_key(&a));
        recover(&mut owner, &mut world);
        assert!(owner.pages.contains_key(&c));
        let current = owner
            .take_job(&world, |_| Box::new([u32::MAX; 256]))
            .unwrap();
        assert_eq!(current.page, c);
        assert_eq!(owner.running.len(), 2);
        assert!(
            !owner
                .complete(&world, old_b.execute(|| false).unwrap())
                .map_err(|_| ())
                .unwrap()
        );
        assert!(!owner.pages.contains_key(&b));
        assert!(
            owner
                .complete(&world, current.execute(|| false).unwrap())
                .map_err(|_| ())
                .unwrap()
        );
        owner.pump_ready(&world, 16).unwrap();
        assert!(owner.pages.len() <= owner.limits.max_pages * 2);
        let alive: Vec<_> = owner
            .slots
            .iter()
            .filter_map(|s| s.sample.map(|s| s.position.page()))
            .collect();
        assert_eq!(alive, vec![c]);
        drop(old_a);
    }
}
