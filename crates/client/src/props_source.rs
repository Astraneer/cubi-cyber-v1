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
    sync::Arc,
};
use world::{CursorError, CursorId, Page, World, WorldChange};
#[path = "props_wire.rs"]
mod wire;
pub use wire::{PropsWireError, execute_props_request};
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
    pub jobs_submitted: u64,
    pub jobs_stale: u64,
    pub worker_mask_words: u64,
    pub worker_cells: u64,
    pub worker_models: u64,
    pub owner_light_reads: u64,
    pub binding_members: u64,
    pub public_members: u64,
    pub animated_models: u64,
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
    pub oldest_age_ticks: u64,
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
pub struct PropsSource {
    limits: PropsLimits,
    cursor: CursorId,
    sequence: u64,
    pending: Option<Pending>,
    pages: HashMap<PageId, PageState>,
    positions: HashMap<BlockPos, u32>,
    slots: Vec<Slot>,
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
    work: PropsWork,
}
impl PropsSource {
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
            pages: HashMap::with_capacity(limits.max_pages * 2),
            positions: HashMap::with_capacity(limits.max_instances),
            slots: (0..limits.max_instances).map(|_| Slot::empty()).collect(),
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
            work: PropsWork::default(),
        };
        for (page, _) in world.pages() {
            if page.space == SpaceId(0) {
                source.watch_page(page, world.tick())?;
            }
        }
        Ok(source)
    }
    pub fn watch_page(&mut self, page: PageId, tick: Tick) -> Result<(), PropsError> {
        if page.space != SpaceId(0) {
            return Err(PropsError::Space);
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
    /// Entry credits include created/removed pages, individual cells/lights and ACK.
    pub fn ingest(
        &mut self,
        world: &mut World,
        max_entries: usize,
        mut tint: impl FnMut(BlockPos) -> u32,
    ) -> Result<usize, PropsError> {
        let mut used = 0;
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
        self.work.journal_entries += used as u64;
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
                return Ok(false);
            }
        } else {
            self.work.jobs_stale += 1;
            return Ok(false);
        }
        self.ready.push_back(Ready { built, next: 0 });
        Ok(true)
    }
    pub fn fail_job(&mut self, world: &World, id: u64) {
        if let Some((page, _)) = self.running.remove(&id) {
            if let Some(state) = self.pages.get_mut(&page) {
                state.running = false;
            }
            if world.page(page).is_some() {
                let _ = self.watch_page(page, world.tick());
            }
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
        self.flush_inner(queue, gpu, origin, budget, max_records, |sample| sample)
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
        mut light_at: impl FnMut(BlockPos) -> Option<world::LightSample>,
    ) -> Result<usize, PropsError> {
        let mut reads = 0;
        let result = self.flush_inner(queue, gpu, origin, budget, max_records, |mut sample| {
            reads += 1;
            sample.light = light_at(sample.position)
                .and_then(|light| PackedLight::new(light.sky, light.block, light.rgb))
                .unwrap_or_default()
                .raw();
            sample
        });
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
                        value.connections = ports;
                    } else {
                        value.reserved[3] = ports | (1 << 31);
                    }
                }
                value
            } else {
                BlockInstance::hidden()
            };
            let work = gpu.update(queue, index, value, budget)?;
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
        let caught = self.sequence == world.journal_next() && self.pending.is_none();
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
            oldest_age_ticks: oldest,
            overdue: oldest > self.limits.max_age_ticks,
            caught_up: caught,
            settled: caught
                && self.dirty.is_empty()
                && self.jobs.is_empty()
                && self.running.is_empty()
                && self.ready.is_empty()
                && self.light_jobs.is_empty()
                && self.public_jobs.is_empty(),
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
    rules::block(cell.id()).is_some_and(|d| {
        matches!(
            d.render,
            rules::blocks::Render::Model(_) | rules::blocks::Render::Cross
        )
    })
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
                && matches!(
                    definition.render,
                    rules::blocks::Render::Model(rules::Model::Mek)
                )
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
