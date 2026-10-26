//! Bounded journal owner for actual physical water. Foreground touches changed
//! pages and cached masks; immutable worker jobs read cells and build16B records.
use crate::water::{CHANNELS, CompletedWater, WaterMeshError, WaterTask};
use core_types::{PageId, SpaceId, Tick};
use render::{
    terrain_gpu::{UploadBudget, translation},
    water_gpu::{WaterError, WaterGpu, WaterHandle, WaterQuad},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use world::{CursorError, CursorId, World, WorldChange};
#[derive(Clone, Copy, Debug)]
pub struct WaterSourceLimits {
    pub max_pages: usize,
    pub max_running: usize,
    pub max_ready: usize,
    pub max_ready_bytes: usize,
    pub max_quads_per_page: usize,
    pub max_total_quads: usize,
    pub max_age_ticks: u64,
    pub max_task_probes: usize,
    pub max_retries: u8,
}
impl Default for WaterSourceLimits {
    fn default() -> Self {
        Self {
            max_pages: 512,
            max_running: 2,
            max_ready: 64,
            max_ready_bytes: 4 * 1024 * 1024,
            max_quads_per_page: 24576,
            max_total_quads: 65536,
            max_age_ticks: 40,
            max_task_probes: 16,
            max_retries: 3,
        }
    }
}
#[derive(Debug)]
pub enum WaterSourceError {
    InvalidLimits,
    Capacity,
    MissingTransform,
    Cursor(CursorError),
    Gpu(WaterError),
    Mesh(WaterMeshError),
}
impl std::fmt::Display for WaterSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WaterSourceError {}
impl From<CursorError> for WaterSourceError {
    fn from(e: CursorError) -> Self {
        Self::Cursor(e)
    }
}
impl From<WaterError> for WaterSourceError {
    fn from(e: WaterError) -> Self {
        Self::Gpu(e)
    }
}
impl From<WaterMeshError> for WaterSourceError {
    fn from(e: WaterMeshError) -> Self {
        Self::Mesh(e)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaterSourceWork {
    pub journal_entries: u64,
    pub mask_words: u64,
    pub dirty_pages: u64,
    pub task_probes: u64,
    pub jobs_submitted: u64,
    pub jobs_completed: u64,
    pub jobs_stale: u64,
    pub jobs_failed: u64,
    pub worker_cells: u64,
    pub worker_halo_samples: u64,
    pub gpu_upload_bytes: u64,
    pub pages_published: u64,
    pub pages_retired: u64,
    pub models_updated: u64,
    pub cpu_frame_vertices: u64,
    pub backpressure: bool,
    pub changed: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct WaterSourceStats {
    pub tracked_pages: usize,
    pub queued_pages: usize,
    pub running: usize,
    pub ready: usize,
    pub ready_bytes: usize,
    pub live_quads: usize,
    pub oldest_age_ticks: u64,
    pub overdue: bool,
    pub failed_pages: usize,
    pub caught_up: bool,
    pub settled: bool,
    pub models_dirty: bool,
}
struct PageState {
    known: bool,
    queued: bool,
    enqueued: bool,
    retire_enqueued: bool,
    running: bool,
    ready: bool,
    retire: bool,
    first_dirty: Option<Tick>,
    retries: u8,
    failed: bool,
    handle: Option<WaterHandle>,
    front: Vec<WaterQuad>,
    inputs: Option<[world::SampledPageInput; 27]>,
    front_tint: u64,
    model: Option<[[f32; 4]; 4]>,
}
impl PageState {
    fn new() -> Self {
        Self {
            known: true,
            queued: false,
            enqueued: false,
            retire_enqueued: false,
            running: false,
            ready: false,
            retire: false,
            first_dirty: None,
            retries: 0,
            failed: false,
            handle: None,
            front: Vec::new(),
            inputs: None,
            front_tint: 0,
            model: None,
        }
    }
}
struct Running {
    id: u64,
    page: PageId,
    inputs: [world::SampledPageInput; 27],
    tint: u64,
    first: Tick,
}
struct Ready {
    page: PageId,
    result: CompletedWater,
    tint: u64,
    bytes: usize,
}
struct Pending {
    change: Arc<WorldChange>,
    next: u64,
    page: usize,
    cell: usize,
}
pub struct WaterJob {
    pub id: u64,
    pub task: WaterTask,
    max_quads: usize,
}
pub struct BuiltWater {
    pub id: u64,
    pub result: Result<Box<CompletedWater>, WaterMeshError>,
}
impl WaterJob {
    pub fn execute(self, cancel: impl Fn() -> bool) -> BuiltWater {
        let mut result = self.task.execute(self.max_quads, cancel);
        if let Ok(b) = &mut result {
            b.mesh.quads.shrink_to_fit();
        }
        BuiltWater {
            id: self.id,
            result: result.map(Box::new),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterCompletion {
    Ready,
    Stale,
    Failed,
}
pub struct WaterBackpressure {
    pub built: BuiltWater,
}
pub struct WaterSource {
    limits: WaterSourceLimits,
    cursor: CursorId,
    next_sequence: u64,
    pending: Option<Pending>,
    pages: BTreeMap<PageId, PageState>,
    dirty: VecDeque<PageId>,
    retire: VecDeque<PageId>,
    running: Vec<Running>,
    ready: VecDeque<Ready>,
    ready_bytes: usize,
    total_quads: usize,
    next_job: u64,
    tint_revision: u64,
    models_dirty: bool,
    model_staging: Vec<(PageId, WaterHandle, [[f32; 4]; 4])>,
    tint_staging: Vec<PageId>,
    work: WaterSourceWork,
}
impl WaterSource {
    pub fn attach(world: &mut World, limits: WaterSourceLimits) -> Result<Self, WaterSourceError> {
        if limits.max_pages == 0
            || limits.max_running == 0
            || limits.max_ready == 0
            || limits.max_ready_bytes
                < std::mem::size_of::<CompletedWater>() + limits.max_quads_per_page * 16
            || limits.max_quads_per_page == 0
            || limits.max_quads_per_page > 24576
            || limits.max_total_quads == 0
            || limits.max_task_probes == 0
            || limits.max_retries == 0
            || world.loaded_pages() > limits.max_pages
        {
            return Err(WaterSourceError::InvalidLimits);
        }
        let next_sequence = world.journal_next();
        let cursor = world.register_cursor();
        let mut source = Self {
            limits,
            cursor,
            next_sequence,
            pending: None,
            pages: BTreeMap::new(),
            dirty: VecDeque::with_capacity(limits.max_pages * 2),
            retire: VecDeque::with_capacity(limits.max_pages * 2),
            running: Vec::with_capacity(limits.max_running),
            ready: VecDeque::with_capacity(limits.max_ready),
            ready_bytes: 0,
            total_quads: 0,
            next_job: 1,
            tint_revision: 1,
            models_dirty: false,
            model_staging: Vec::with_capacity(limits.max_pages * 2),
            tint_staging: Vec::with_capacity(limits.max_pages * 2),
            work: WaterSourceWork::default(),
        };
        for (page, _) in world.pages() {
            source.mark(world, page, world.tick())?;
        }
        Ok(source)
    }
    fn mark(&mut self, world: &World, page: PageId, tick: Tick) -> Result<(), WaterSourceError> {
        let Some(data) = world.page(page) else {
            self.forget_page(page);
            return Ok(());
        };
        if !self.pages.contains_key(&page) && self.pages.len() >= self.limits.max_pages * 2 {
            return Err(WaterSourceError::Capacity);
        }
        let p = self.pages.entry(page).or_insert_with(PageState::new);
        p.known = true;
        p.retire = false;
        self.work.mask_words += 64;
        if data.masks().fluid.count() == 0 && p.front.is_empty() && p.handle.is_none() {
            p.queued = false;
            p.first_dirty = None;
            p.failed = false;
            return Ok(());
        }
        if p.inputs
            .as_ref()
            .is_some_and(|inputs| world.matches_inputs(inputs))
            && p.front_tint == self.tint_revision
        {
            return Ok(());
        }
        if self.running.iter().any(|job| {
            job.page == page && job.tint == self.tint_revision && world.matches_inputs(&job.inputs)
        }) {
            return Ok(());
        }
        if self.ready.iter().any(|ready| {
            ready.page == page && ready.tint == self.tint_revision && ready.result.current(world)
        }) {
            return Ok(());
        }
        if !p.queued {
            if self.dirty.len() >= self.limits.max_pages * 2 {
                return Err(WaterSourceError::Capacity);
            }
            if !p.enqueued {
                self.dirty.push_back(page);
                p.enqueued = true;
            }
            p.queued = true;
            self.work.dirty_pages += 1;
        }
        p.first_dirty = Some(p.first_dirty.map_or(tick, |old| old.min(tick)));
        p.failed = false;
        Ok(())
    }
    pub fn watch_page(&mut self, world: &World, page: PageId) -> Result<(), WaterSourceError> {
        self.mark(world, page, world.tick())
    }
    pub fn forget_page(&mut self, page: PageId) {
        if let Some(p) = self.pages.get_mut(&page) {
            p.known = false;
            p.queued = false;
            p.first_dirty = None;
            p.retire = true;
            if !p.retire_enqueued {
                p.retire_enqueued = true;
                self.retire.push_back(page);
            }
        }
    }
    /// Limits count page/cell journal entries. No old/cold page enumeration occurs.
    pub fn ingest(
        &mut self,
        world: &mut World,
        max_entries: usize,
    ) -> Result<usize, WaterSourceError> {
        let mut entries = 0;
        while entries < max_entries {
            let mut pending = if let Some(p) = self.pending.take() {
                p
            } else {
                if self.next_sequence == world.journal_next() {
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
                }
            };
            if pending.page < pending.change.pages.len() {
                let page = &pending.change.pages[pending.page];
                if page.created || page.removed {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            for dx in -1..=1 {
                                if let Some(p) = neighbor(page.page, dx, dy, dz)
                                    && let Err(e) = self.mark(world, p, pending.change.tick)
                                {
                                    self.pending = Some(pending);
                                    return Err(e);
                                }
                            }
                        }
                    }
                } else if page.channels.intersects(CHANNELS)
                    && let Err(e) = self.mark(world, page.page, pending.change.tick)
                {
                    self.pending = Some(pending);
                    return Err(e);
                }
                pending.page += 1;
                entries += 1;
            } else if pending.cell < pending.change.cells.len() {
                let cell = &pending.change.cells[pending.cell];
                let a = rules::properties(cell.old);
                let b = rules::properties(cell.new);
                if a.shape != b.shape
                    || a.opaque != b.opaque
                    || a.fluid != b.fluid
                    || (a.fluid && cell.old != cell.new)
                {
                    let (x, y, z) = cell.index.xyz();
                    for &dx in offsets(x) {
                        for &dy in offsets(y) {
                            for &dz in offsets(z) {
                                if let Some(p) = neighbor(cell.page, dx, dy, dz)
                                    && let Err(e) = self.mark(world, p, pending.change.tick)
                                {
                                    self.pending = Some(pending);
                                    return Err(e);
                                }
                            }
                        }
                    }
                }
                pending.cell += 1;
                entries += 1;
            } else {
                self.models_dirty |= pending.change.transform.is_some();
                world.acknowledge_cursor(self.cursor, pending.next)?;
                self.next_sequence = pending.next;
                entries += 1;
                continue;
            }
            self.pending = Some(pending);
        }
        self.work.journal_entries += entries as u64;
        Ok(entries)
    }
    /// Callback supplies an existing immutable biome tint table, without generation.
    pub fn take_job(
        &mut self,
        world: &World,
        mut tints: impl FnMut(PageId) -> Box<[u32; 256]>,
    ) -> Result<Option<WaterJob>, WaterSourceError> {
        if self.running.len() >= self.limits.max_running
            || self.ready.len() >= self.limits.max_ready
        {
            return Ok(None);
        }
        for _ in 0..self.limits.max_task_probes {
            let Some(page) = self.dirty.pop_front() else {
                break;
            };
            self.work.task_probes += 1;
            let Some(p) = self.pages.get_mut(&page) else {
                continue;
            };
            p.enqueued = false;
            if !p.queued || !p.known {
                self.prune(page);
                continue;
            }
            if p.running || p.ready {
                self.dirty.push_back(page);
                p.enqueued = true;
                continue;
            }
            p.queued = false;
            if world.page(page).is_none() {
                self.forget_page(page);
                continue;
            }
            if self.next_job >= 1 << 63 {
                return Err(WaterSourceError::Capacity);
            }
            let task = WaterTask::capture(world, page, tints(page))?;
            let id = self.next_job;
            self.next_job = self
                .next_job
                .checked_add(1)
                .ok_or(WaterSourceError::Capacity)?;
            let p = self.pages.get_mut(&page).expect("tracked page");
            p.running = true;
            let first = p.first_dirty.unwrap_or(world.tick());
            self.running.push(Running {
                id,
                page,
                inputs: task.inputs,
                tint: self.tint_revision,
                first,
            });
            self.work.jobs_submitted += 1;
            return Ok(Some(WaterJob {
                id,
                task,
                max_quads: self.limits.max_quads_per_page,
            }));
        }
        Ok(None)
    }
    pub fn complete(
        &mut self,
        world: &World,
        built: BuiltWater,
    ) -> Result<WaterCompletion, WaterBackpressure> {
        let Some(index) = self.running.iter().position(|j| j.id == built.id) else {
            self.work.jobs_stale += 1;
            return Ok(WaterCompletion::Stale);
        };
        let meta = &self.running[index];
        let current = built.result.as_ref().is_ok_and(|r| {
            r.page == meta.page
                && r.inputs == meta.inputs
                && r.version == meta.inputs[13].version
                && r.current(world)
        }) && meta.tint == self.tint_revision
            && self.pages.get(&meta.page).is_some_and(|p| p.known);
        if current {
            let result = built.result.as_ref().expect("checked result");
            let bytes = std::mem::size_of::<CompletedWater>() + result.mesh.quads.capacity() * 16;
            if self.ready.len() >= self.limits.max_ready
                || self.ready_bytes + bytes > self.limits.max_ready_bytes
            {
                self.work.backpressure = true;
                return Err(WaterBackpressure { built });
            }
        }
        let meta = self.running.swap_remove(index);
        if let Some(p) = self.pages.get_mut(&meta.page) {
            p.running = false;
        }
        self.work.jobs_completed += 1;
        match built.result {
            Ok(result) => {
                self.work.worker_cells += result.mesh.work.cells_scanned;
                self.work.worker_halo_samples += result.mesh.work.halo_samples;
                if !current {
                    self.work.jobs_stale += 1;
                    let _ = self.mark(world, meta.page, meta.first);
                    return Ok(WaterCompletion::Stale);
                }
                let bytes =
                    std::mem::size_of::<CompletedWater>() + result.mesh.quads.capacity() * 16;
                self.ready_bytes += bytes;
                if let Some(p) = self.pages.get_mut(&meta.page) {
                    p.ready = true;
                    p.queued = false;
                }
                self.ready.push_back(Ready {
                    page: meta.page,
                    result: *result,
                    tint: meta.tint,
                    bytes,
                });
                Ok(WaterCompletion::Ready)
            }
            Err(_) => {
                self.work.jobs_failed += 1;
                let retry = if let Some(p) = self.pages.get_mut(&meta.page) {
                    p.retries += 1;
                    p.failed = p.retries >= self.limits.max_retries;
                    p.known && !p.failed
                } else {
                    false
                };
                if retry {
                    let _ = self.mark(world, meta.page, meta.first);
                }
                Ok(WaterCompletion::Failed)
            }
        }
    }
    /// Publishes bounded page ranges; old geometry remains until a current result
    /// passes the complete upload budget. Removed pages retire their range first.
    pub fn publish_ready(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        world: &World,
        origin: [i64; 3],
        budget: &mut UploadBudget,
        max_pages: usize,
    ) -> Result<usize, WaterSourceError> {
        self.publish_ready_with_model(queue, gpu, world, budget, max_pages, |page| {
            if page.space != SpaceId(0) {
                return None;
            }
            Some(translation([
                (i64::from(page.x) * 16 - origin[0]) as f32,
                (i64::from(page.y) * 16 - origin[1]) as f32,
                (i64::from(page.z) * 16 - origin[2]) as f32,
            ]))
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn publish_ready_with_model(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        world: &World,
        budget: &mut UploadBudget,
        max_pages: usize,
        mut model: impl FnMut(PageId) -> Option<[[f32; 4]; 4]>,
    ) -> Result<usize, WaterSourceError> {
        let mut count = 0;
        while count < max_pages {
            let Some(page) = self.retire.front().copied() else {
                break;
            };
            let Some(p) = self.pages.get_mut(&page) else {
                self.retire.pop_front();
                continue;
            };
            if p.known || !p.retire {
                p.retire_enqueued = false;
                self.retire.pop_front();
                self.prune(page);
                continue;
            }
            let needed = if p.handle.is_some() {
                4 + if p.front.is_empty() { 0 } else { 8 }
            } else {
                0
            };
            if budget.remaining_bytes < needed {
                self.work.backpressure = true;
                return Ok(count);
            }
            if let Some(handle) = p.handle {
                if !p.front.is_empty() {
                    let work = gpu.upload_page_quads(queue, handle, &[], budget)?;
                    self.work.gpu_upload_bytes += work.upload_bytes;
                }
                gpu.release_object(queue, handle, budget)?;
                self.work.gpu_upload_bytes += 4;
                self.total_quads -= p.front.len();
                p.handle = None;
                p.front.clear();
                p.inputs = None;
                p.model = None;
                self.work.changed = true;
            }
            p.retire = false;
            p.retire_enqueued = false;
            self.retire.pop_front();
            self.prune(page);
            self.work.pages_retired += 1;
            count += 1;
        }
        while count < max_pages {
            let Some(ready) = self.ready.front() else {
                break;
            };
            let page = ready.page;
            let current = ready.tint == self.tint_revision
                && ready.result.current(world)
                && self.pages.get(&page).is_some_and(|p| p.known);
            if !current {
                let ready = self.ready.pop_front().expect("ready item");
                self.ready_bytes -= ready.bytes;
                if let Some(p) = self.pages.get_mut(&page) {
                    p.ready = false;
                }
                self.work.jobs_stale += 1;
                self.mark(world, page, world.tick())?;
                continue;
            }
            let p = self.pages.get(&page).expect("ready page");
            let quads = &ready.result.mesh.quads;
            if self.total_quads - p.front.len() + quads.len() > self.limits.max_total_quads {
                self.work.backpressure = true;
                return Ok(count);
            }
            let transform = if quads.is_empty() {
                None
            } else {
                Some(model(page).ok_or(WaterSourceError::MissingTransform)?)
            };
            let different = p.front != *quads;
            let model_bytes = if transform.is_some() && p.model != transform {
                80
            } else {
                0
            };
            let needed = if quads.is_empty() {
                if p.handle.is_some() {
                    4 + if p.front.is_empty() { 0 } else { 8 }
                } else {
                    0
                }
            } else {
                model_bytes
                    + if different {
                        quads.len() as u64 * 16 + 8
                    } else {
                        0
                    }
            };
            if budget.remaining_bytes < needed {
                self.work.backpressure = true;
                return Ok(count);
            }
            let p = self.pages.get_mut(&page).expect("ready page");
            if let Some(transform) = transform {
                let before = budget.remaining_bytes;
                let handle = gpu.upload_object(queue, p.handle, transform, budget)?;
                self.work.gpu_upload_bytes += before - budget.remaining_bytes;
                self.work.changed |= before != budget.remaining_bytes;
                p.handle = Some(handle);
                p.model = Some(transform);
                if different {
                    let work = gpu.upload_page_quads(queue, handle, quads, budget)?;
                    self.work.gpu_upload_bytes += work.upload_bytes;
                    self.work.changed |= work.upload_bytes > 0;
                }
            } else if let Some(handle) = p.handle {
                if !p.front.is_empty() {
                    let work = gpu.upload_page_quads(queue, handle, &[], budget)?;
                    self.work.gpu_upload_bytes += work.upload_bytes;
                }
                gpu.release_object(queue, handle, budget)?;
                self.work.gpu_upload_bytes += 4;
                p.handle = None;
                p.model = None;
                self.work.changed = true;
            }
            let ready = self.ready.pop_front().expect("published ready");
            self.ready_bytes -= ready.bytes;
            self.total_quads = self.total_quads - p.front.len() + ready.result.mesh.quads.len();
            p.front = ready.result.mesh.quads;
            p.inputs = Some(ready.result.inputs);
            p.front_tint = ready.tint;
            p.ready = false;
            p.first_dirty = None;
            p.retries = 0;
            p.failed = false;
            self.work.pages_published += 1;
            count += 1;
        }
        Ok(count)
    }
    /// Call on a render-origin shift; committed Space transforms are detected by
    /// ingest. This changes only cached object matrices, without rebuilding water.
    pub fn request_model_update(&mut self) {
        self.models_dirty = true;
    }
    /// Returns None when the complete matrix batch does not fit. Validation and
    /// budget checks precede every write, so old coordinates remain coherent.
    pub fn refresh_models(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
        mut model: impl FnMut(PageId) -> Option<[[f32; 4]; 4]>,
    ) -> Result<Option<usize>, WaterSourceError> {
        if !self.models_dirty {
            return Ok(Some(0));
        }
        self.model_staging.clear();
        for (&page, p) in &self.pages {
            let Some(handle) = p.handle else { continue };
            let matrix = model(page).ok_or(WaterSourceError::MissingTransform)?;
            if matrix.iter().flatten().any(|v| !v.is_finite())
                || matrix[0][3] != 0.
                || matrix[1][3] != 0.
                || matrix[2][3] != 0.
                || matrix[3][3] != 1.
            {
                return Err(WaterError::InvalidModel.into());
            }
            if p.model != Some(matrix) {
                self.model_staging.push((page, handle, matrix));
            }
        }
        let needed = self.model_staging.len() as u64 * 80;
        if budget.remaining_bytes < needed {
            self.work.backpressure = true;
            return Ok(None);
        }
        for &(page, handle, matrix) in &self.model_staging {
            gpu.upload_object(queue, Some(handle), matrix, budget)?;
            self.pages
                .get_mut(&page)
                .expect("exclusive water handle")
                .model = Some(matrix);
        }
        let changed = self.model_staging.len();
        self.models_dirty = false;
        self.work.gpu_upload_bytes += needed;
        self.work.models_updated += changed as u64;
        self.work.changed |= changed != 0;
        Ok(Some(changed))
    }
    /// Loading/resource reload hook. The bounded metadata list is revisited;
    /// immutable worker tasks still obtain tints solely through take_job's cache.
    pub fn invalidate_tints(&mut self, world: &World) -> Result<(), WaterSourceError> {
        self.tint_revision = self
            .tint_revision
            .checked_add(1)
            .ok_or(WaterSourceError::Capacity)?;
        self.tint_staging.clear();
        self.tint_staging.extend(
            self.pages
                .iter()
                .filter_map(|(&page, p)| p.known.then_some(page)),
        );
        for index in 0..self.tint_staging.len() {
            self.mark(world, self.tint_staging[index], world.tick())?;
        }
        self.tint_staging.clear();
        Ok(())
    }
    /// Session teardown before detach: all owned ranges and handles are retired.
    /// None means no writes occurred because the full teardown did not fit.
    pub fn retire_all(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
    ) -> Result<Option<usize>, WaterSourceError> {
        let needed = self
            .pages
            .values()
            .filter(|p| p.handle.is_some())
            .map(|p| 4 + if p.front.is_empty() { 0 } else { 8 })
            .sum::<u64>();
        if budget.remaining_bytes < needed {
            self.work.backpressure = true;
            return Ok(None);
        }
        let mut count = 0;
        for p in self.pages.values_mut() {
            if let Some(handle) = p.handle.take() {
                if !p.front.is_empty() {
                    gpu.upload_page_quads(queue, handle, &[], budget)?;
                }
                gpu.release_object(queue, handle, budget)?;
                count += 1;
            }
        }
        self.pages.clear();
        self.dirty.clear();
        self.retire.clear();
        self.running.clear();
        self.ready.clear();
        self.ready_bytes = 0;
        self.total_quads = 0;
        self.models_dirty = false;
        self.work.gpu_upload_bytes += needed;
        self.work.pages_retired += count;
        self.work.changed |= count != 0;
        Ok(Some(count as usize))
    }
    fn prune(&mut self, page: PageId) {
        let remove = self.pages.get(&page).is_some_and(|p| {
            !p.known
                && !p.running
                && !p.ready
                && !p.enqueued
                && !p.retire_enqueued
                && p.handle.is_none()
        });
        if remove {
            self.pages.remove(&page);
        }
    }
    pub fn stats(&self, world: &World) -> WaterSourceStats {
        let oldest = self
            .pages
            .values()
            .filter_map(|p| p.first_dirty)
            .map(|tick| world.tick().0.saturating_sub(tick.0))
            .max()
            .unwrap_or(0);
        let failed = self.pages.values().filter(|p| p.failed).count();
        let caught = self.next_sequence == world.journal_next() && self.pending.is_none();
        WaterSourceStats {
            tracked_pages: self.pages.len(),
            queued_pages: self.pages.values().filter(|p| p.queued).count(),
            running: self.running.len(),
            ready: self.ready.len(),
            ready_bytes: self.ready_bytes,
            live_quads: self.total_quads,
            oldest_age_ticks: oldest,
            overdue: oldest > self.limits.max_age_ticks,
            failed_pages: failed,
            caught_up: caught,
            models_dirty: self.models_dirty,
            settled: caught
                && !self.models_dirty
                && self.dirty.is_empty()
                && self.running.is_empty()
                && self.ready.is_empty()
                && self.retire.is_empty()
                && failed == 0,
        }
    }
    pub fn take_work(&mut self) -> WaterSourceWork {
        std::mem::take(&mut self.work)
    }
    pub fn detach(self, world: &mut World) {
        world.remove_cursor(self.cursor);
    }
}
fn neighbor(p: PageId, dx: i32, dy: i32, dz: i32) -> Option<PageId> {
    Some(PageId {
        space: p.space,
        x: p.x.checked_add(dx)?,
        y: p.y.checked_add(dy)?,
        z: p.z.checked_add(dz)?,
    })
}

fn offsets(v: u8) -> &'static [i32] {
    match v {
        0 => &[0, -1],
        15 => &[0, 1],
        _ => &[0],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterWireError {
    Length,
    Magic,
    Inputs,
    Cell,
    Descriptor,
    Work,
}
impl std::fmt::Display for WaterWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WaterWireError {}
const REQUEST_BYTES: usize = 4 + 8 + 4 + 16 + 27 * 8 + 5832 + 256 * 4;
const MAX_RESULT_BYTES: usize = 512 + 24576 * 16;
impl WaterJob {
    /// Browser transport contains classified public water/air/opacity, never raw
    /// block IDs, entities or a generation seed. Native uses the original snapshots.
    pub fn encode(&self) -> Result<Vec<u8>, WaterWireError> {
        let mut bytes = Vec::with_capacity(REQUEST_BYTES);
        bytes.extend_from_slice(b"CWJ1");
        ww64(&mut bytes, self.id);
        ww32(&mut bytes, self.max_quads as u32);
        write_page(&mut bytes, self.task.page);
        for input in self.task.inputs {
            ww64(&mut bytes, input.version.0);
        }
        let halo = self.task.snapshot_halo();
        for y in -1..=16 {
            for z in -1..=16 {
                for x in -1..=16 {
                    let code = match halo.get(x, y, z) {
                        crate::water::SurfaceCell::Unknown => 255,
                        crate::water::SurfaceCell::Air => 0,
                        crate::water::SurfaceCell::Solid => 254,
                        crate::water::SurfaceCell::Water { amount, falling }
                            if (1..=8).contains(&amount) =>
                        {
                            amount | if falling { 16 } else { 0 }
                        }
                        _ => return Err(WaterWireError::Cell),
                    };
                    bytes.push(code);
                }
            }
        }
        for tint in self.task.tints() {
            ww32(&mut bytes, *tint)
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WaterWireError> {
        if bytes.len() != REQUEST_BYTES {
            return Err(WaterWireError::Length);
        }
        let mut r = WaterReader { bytes, at: 0 };
        if r.take::<4>()? != *b"CWJ1" {
            return Err(WaterWireError::Magic);
        }
        let id = r.u64()?;
        let max_quads = r.u32()? as usize;
        if id == 0 || id >= 1 << 63 || max_quads == 0 || max_quads > 24576 {
            return Err(WaterWireError::Length);
        }
        let page = r.page()?;
        let inputs = read_inputs(&mut r, page)?;
        let data = r
            .bytes
            .get(r.at..r.at + 5832)
            .ok_or(WaterWireError::Length)?;
        for code in data {
            if !matches!(*code,0|254|255|1..=8|17..=24) {
                return Err(WaterWireError::Cell);
            }
        }
        let mut i = 0;
        let halo = crate::water::WaterHalo::from_fn(|_, _, _| {
            let b = data[i];
            i += 1;
            match b {
                0 => crate::water::SurfaceCell::Air,
                254 => crate::water::SurfaceCell::Solid,
                255 => crate::water::SurfaceCell::Unknown,
                _ => crate::water::SurfaceCell::Water {
                    amount: b & 15,
                    falling: b & 16 != 0,
                },
            }
        });
        r.at += 5832;
        let mut tints = Box::new([0; 256]);
        for tint in tints.iter_mut() {
            *tint = r.u32()?;
        }
        let task =
            WaterTask::from_halo(page, inputs, tints, halo).map_err(|_| WaterWireError::Inputs)?;
        Ok(Self {
            id,
            task,
            max_quads,
        })
    }
    pub fn transport_id(&self) -> jobs::JobId {
        jobs::JobId(self.id | (1 << 63))
    }
}
impl BuiltWater {
    pub fn encode(&self) -> Result<Vec<u8>, WaterWireError> {
        let mut bytes =
            Vec::with_capacity(512 + self.result.as_ref().map_or(0, |r| r.mesh.quads.len() * 16));
        bytes.extend_from_slice(b"CWR1");
        ww64(&mut bytes, self.id);
        let result = match &self.result {
            Ok(result) => {
                bytes.push(0);
                result
            }
            Err(WaterMeshError::Cancelled) => {
                bytes.push(1);
                return Ok(bytes);
            }
            Err(WaterMeshError::Capacity) => {
                bytes.push(2);
                return Ok(bytes);
            }
            Err(_) => {
                bytes.push(3);
                return Ok(bytes);
            }
        };
        if result.mesh.quads.len() > 24576 {
            return Err(WaterWireError::Length);
        }
        write_page(&mut bytes, result.page);
        for input in result.inputs {
            ww64(&mut bytes, input.version.0);
        }
        for value in [
            result.mesh.work.snapshot_lookups,
            result.mesh.work.halo_samples,
            result.mesh.work.cells_scanned,
            result.mesh.work.quads,
            result.mesh.work.unknown_surface_cells,
            result.mesh.work.cpu_frame_vertices,
        ] {
            ww64(&mut bytes, value);
        }
        ww32(&mut bytes, result.mesh.quads.len() as u32);
        for quad in &result.mesh.quads {
            for word in quad.words {
                ww32(&mut bytes, word)
            }
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WaterWireError> {
        if bytes.len() > MAX_RESULT_BYTES || bytes.len() < 13 {
            return Err(WaterWireError::Length);
        }
        let mut r = WaterReader { bytes, at: 0 };
        if r.take::<4>()? != *b"CWR1" {
            return Err(WaterWireError::Magic);
        }
        let id = r.u64()?;
        if id == 0 || id >= 1 << 63 {
            return Err(WaterWireError::Inputs);
        }
        let status = r.take::<1>()?[0];
        if status > 0 {
            if bytes.len() != 13 {
                return Err(WaterWireError::Length);
            }
            let error = match status {
                1 => WaterMeshError::Cancelled,
                2 => WaterMeshError::Capacity,
                3 => WaterMeshError::Gpu(WaterError::InvalidQuad),
                _ => return Err(WaterWireError::Work),
            };
            return Ok(Self {
                id,
                result: Err(error),
            });
        }
        let page = r.page()?;
        let inputs = read_inputs(&mut r, page)?;
        let mut n = [0u64; 6];
        for v in &mut n {
            *v = r.u64()?;
        }
        let count = r.u32()? as usize;
        if count > 24576 || r.at + count * 16 != bytes.len() {
            return Err(WaterWireError::Length);
        }
        if !matches!(n[0], 0 | 27)
            || n[1] != 5832
            || n[2] != 4096
            || n[3] != count as u64
            || n[4] > 4096
            || n[5] != 0
        {
            return Err(WaterWireError::Work);
        }
        let mut quads = Vec::with_capacity(count);
        for _ in 0..count {
            let mut words = [0; 4];
            for v in &mut words {
                *v = r.u32()?
            }
            let b = words[0];
            let face = render::terrain_mesh::Face::from_index(((b >> 15) & 7) as u8)
                .ok_or(WaterWireError::Descriptor)?;
            if b >> 26 != 0
                || words[3] & 65535 != 0
                || render::terrain_mesh::QuadDescriptor::new(
                    [
                        (b & 31) as u8,
                        ((b >> 5) & 31) as u8,
                        ((b >> 10) & 31) as u8,
                    ],
                    face,
                    [(((b >> 18) & 15) + 1) as u8, (((b >> 22) & 15) + 1) as u8],
                    0,
                    [3; 4],
                    0,
                )
                .is_err()
            {
                return Err(WaterWireError::Descriptor);
            }
            quads.push(WaterQuad { words });
        }
        let result = CompletedWater {
            page,
            version: inputs[13].version,
            inputs,
            mesh: crate::water::WaterMesh {
                quads,
                work: crate::water::WaterMeshWork {
                    snapshot_lookups: n[0],
                    halo_samples: n[1],
                    cells_scanned: n[2],
                    quads: n[3],
                    unknown_surface_cells: n[4],
                    cpu_frame_vertices: n[5],
                },
            },
        };
        Ok(Self {
            id,
            result: Ok(Box::new(result)),
        })
    }
}
pub fn execute_water_request(
    bytes: &[u8],
    cancel: impl Fn() -> bool,
) -> Result<Vec<u8>, WaterWireError> {
    WaterJob::decode(bytes)?.execute(cancel).encode()
}
#[cfg(not(target_arch = "wasm32"))]
pub fn native_pool(
    workers: usize,
    capacity: usize,
) -> Result<jobs::NativePool<WaterJob, BuiltWater>, jobs::PoolError> {
    jobs::NativePool::new(
        workers,
        capacity,
        |job: WaterJob, cancel: &jobs::CancellationToken| Ok(job.execute(|| cancel.is_cancelled())),
    )
}
impl WaterSource {
    pub fn fail_job(&mut self, world: &World, id: u64) -> WaterCompletion {
        match self.complete(
            world,
            BuiltWater {
                id,
                result: Err(WaterMeshError::Cancelled),
            },
        ) {
            Ok(v) => v,
            Err(_) => unreachable!("failed job cannot occupy ready queue"),
        }
    }
}
fn write_page(bytes: &mut Vec<u8>, p: PageId) {
    ww32(bytes, p.space.0);
    for v in [p.x, p.y, p.z] {
        ww32(bytes, v as u32)
    }
}
fn ww32(bytes: &mut Vec<u8>, v: u32) {
    bytes.extend_from_slice(&v.to_le_bytes())
}
fn ww64(bytes: &mut Vec<u8>, v: u64) {
    bytes.extend_from_slice(&v.to_le_bytes())
}
struct WaterReader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl WaterReader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], WaterWireError> {
        let end = self.at.checked_add(N).ok_or(WaterWireError::Length)?;
        let out = self
            .bytes
            .get(self.at..end)
            .ok_or(WaterWireError::Length)?
            .try_into()
            .map_err(|_| WaterWireError::Length)?;
        self.at = end;
        Ok(out)
    }
    fn u32(&mut self) -> Result<u32, WaterWireError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, WaterWireError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn page(&mut self) -> Result<PageId, WaterWireError> {
        Ok(PageId {
            space: SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        })
    }
}
fn read_inputs(
    r: &mut WaterReader<'_>,
    page: PageId,
) -> Result<[world::SampledPageInput; 27], WaterWireError> {
    let mut out = [world::SampledPageInput {
        page,
        channels: CHANNELS,
        region: world::PageRegion::WHOLE,
        version: core_types::Version(0),
    }; 27];
    for dy in -1..=1 {
        for dz in -1..=1 {
            for dx in -1..=1 {
                let i = ((dx + 1) + 3 * (dz + 1) + 9 * (dy + 1)) as usize;
                let p = neighbor(page, dx, dy, dz).ok_or(WaterWireError::Inputs)?;
                out[i] = world::SampledPageInput {
                    page: p,
                    channels: CHANNELS,
                    region: world::PageRegion::new([-dx as i8, -dy as i8, -dz as i8])
                        .expect("halo region"),
                    version: core_types::Version(r.u64()?),
                };
            }
        }
    }
    Ok(out)
}
