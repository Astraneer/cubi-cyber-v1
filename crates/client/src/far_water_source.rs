//! Bounded owner for exact-column water. New objects stay inactive until the
//! caller publishes its entire height/opaque/water cover in one prepared plan.
use crate::far_water_worker::{
    BuiltFarWater, FarWaterTask, MAX_QUADS, MAX_RETAINED_RESULT_BYTES, Stamp,
};
use core_types::PageId;
use net::{
    height::GeometryStamp,
    water::{Request, WaterHalo},
};
use render::{
    terrain_gpu::UploadBudget,
    water_gpu::{WaterError, WaterGpu, WaterHandle},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
// Leases remain unique across owner teardown/recreation as well as page ABA.
static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_pages: usize,
    pub max_running: usize,
    pub max_ready_bytes: usize,
    pub max_total_quads: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pages: 512,
            max_running: 4,
            max_ready_bytes: 2 * 1024 * 1024,
            max_total_quads: 65536,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub epoch: u64,
    pub stamp: Stamp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    Capacity,
    Binding,
    NotReady,
    NotPrepared,
    Changed,
    Gpu(WaterError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<WaterError> for Error {
    fn from(e: WaterError) -> Self {
        Self::Gpu(e)
    }
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Work {
    pub staged: u64,
    pub published: u64,
    pub retired: u64,
    pub gpu_upload_bytes: u64,
    pub backpressure: bool,
    pub cpu_frame_vertices: u64,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stats {
    pub front_quads: usize,
    pub staged_quads: usize,
    pub ready_queue: usize,
    pub wanted: usize,
    pub records: usize,
    pub queued: usize,
    pub running: usize,
    pub reserved_bytes: usize,
    pub ready_bytes: usize,
    pub staged: usize,
    pub front: usize,
    pub retiring: usize,
    pub completed: u64,
    pub stale: u64,
    pub unknown_held: u64,
    pub failures: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    geometry: GeometryStamp,
    column: u64,
}
impl Binding {
    fn matches(self, stamp: Stamp) -> bool {
        self.geometry.page == stamp.page
            && self.geometry.source == stamp.geometry_source
            && self.geometry.view == stamp.geometry_view
            && self.column == stamp.column_source
    }
}
#[derive(Clone, Copy)]
struct Resident {
    quads: usize,
    stamp: Stamp,
    handle: Option<WaterHandle>,
    enabled: bool,
}
struct Entry {
    binding: Binding,
    registration: u64,
    wanted: bool,
    image: Option<Arc<WaterHalo>>,
    queued: bool,
    running: Option<u64>,
    parked: Option<Stamp>,
    failures: u8,
    ready: Option<Box<BuiltFarWater>>,
    pending_handle: Option<WaterHandle>,
    staged: Option<Resident>,
    front: Option<Resident>,
}
struct Running {
    context: Context,
    registration: u64,
}
struct Retired {
    handle: WaterHandle,
    has_quads: bool,
}
pub struct FarWaterSource {
    limits: Limits,
    entries: BTreeMap<PageId, Entry>,
    wanted: Vec<(GeometryStamp, u64)>,
    queue: VecDeque<PageId>,
    running: BTreeMap<u64, Running>,
    ready_order: VecDeque<PageId>,
    retired: VecDeque<Retired>,
    ready_bytes: usize,
    next_registration: u64,
    revision: u64,
    prepared: Option<u64>,
    flags: Vec<(WaterHandle, bool)>,
    visibility: Vec<(PageId, bool)>,
    guard_flags: Vec<(WaterHandle, bool)>,
    models: Vec<(WaterHandle, bool, [[f32; 4]; 4])>,
    guard_ready: bool,
    counters: Stats,
}
impl FarWaterSource {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        if limits.max_pages == 0
            || limits.max_pages > 4096
            || limits.max_running == 0
            || limits.max_running > 8
            || limits.max_ready_bytes < MAX_RETAINED_RESULT_BYTES
            || limits.max_ready_bytes > 32 * 1024 * 1024
            || limits.max_total_quads == 0
            || limits.max_total_quads > 65536
        {
            return Err(Error::InvalidLimits);
        }
        Ok(Self {
            limits,
            entries: BTreeMap::new(),
            wanted: Vec::with_capacity(limits.max_pages),
            queue: VecDeque::with_capacity(limits.max_pages),
            running: BTreeMap::new(),
            ready_order: VecDeque::with_capacity(limits.max_pages),
            retired: VecDeque::with_capacity(limits.max_pages * 2),
            ready_bytes: 0,
            next_registration: 1,
            revision: 1,
            prepared: None,
            flags: Vec::with_capacity(limits.max_pages * 2),
            visibility: Vec::with_capacity(limits.max_pages),
            guard_flags: Vec::with_capacity(limits.max_pages),
            models: Vec::with_capacity(limits.max_pages * 2),
            guard_ready: true,
            counters: Stats::default(),
        })
    }
    fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.prepared = None;
    }
    pub fn set_wanted(&mut self, wanted: &[(GeometryStamp, u64)]) -> Result<bool, Error> {
        if self.wanted == wanted {
            return Ok(false);
        }
        if wanted.len() > self.limits.max_pages {
            return Err(Error::Capacity);
        }
        let mut unique = BTreeMap::new();
        for &(geometry, column) in wanted {
            Request {
                page: geometry.page,
                geometry_source: geometry.source,
                geometry_view: geometry.view,
                column_source: column,
                acknowledged: None,
            }
            .validate()
            .map_err(|_| Error::Binding)?;
            if unique
                .insert(geometry.page, Binding { geometry, column })
                .is_some()
            {
                return Err(Error::Binding);
            }
        }
        self.next_registration
            .checked_add(wanted.len() as u64)
            .ok_or(Error::Capacity)?;
        for entry in self.entries.values_mut() {
            entry.wanted = false;
        }
        for (&page, &binding) in &unique {
            if let Some(entry) = self.entries.get_mut(&page) {
                entry.wanted = true;
                if entry.binding != binding {
                    clear_staging(entry, &mut self.ready_bytes, &mut self.retired);
                    entry.binding = binding;
                    entry.registration = self.next_registration;
                    self.next_registration += 1;
                    entry.image = None;
                    entry.running = None;
                    entry.parked = None;
                    entry.failures = 0;
                }
            } else {
                self.entries.insert(
                    page,
                    Entry {
                        binding,
                        registration: self.next_registration,
                        wanted: true,
                        image: None,
                        queued: false,
                        running: None,
                        parked: None,
                        failures: 0,
                        ready: None,
                        pending_handle: None,
                        staged: None,
                        front: None,
                    },
                );
                self.next_registration += 1;
            }
        }
        self.entries.retain(|_, entry| {
            if !entry.wanted {
                clear_staging(entry, &mut self.ready_bytes, &mut self.retired);
                entry.image = None;
                entry.running = None;
                entry.queued = false;
            }
            entry.wanted || entry.front.is_some()
        });
        self.wanted.clear();
        self.wanted.extend_from_slice(wanted);
        self.queue.clear();
        self.ready_order.clear();
        for (&page, entry) in &mut self.entries {
            entry.queued = false;
            if entry.wanted && entry.ready.is_some() {
                self.ready_order.push_back(page)
            }
            enqueue(&mut self.queue, page, entry);
        }
        self.changed();
        Ok(true)
    }
    pub fn admit(&mut self, image: Arc<WaterHalo>) -> Result<bool, Error> {
        let stamp = Stamp::from_halo(&image);
        let Some(entry) = self
            .entries
            .get_mut(&stamp.page)
            .filter(|e| e.wanted && e.binding.matches(stamp))
        else {
            return Ok(false);
        };
        if entry
            .image
            .as_ref()
            .is_some_and(|old| old.water_view >= image.water_view)
        {
            return Ok(false);
        }
        if image.water_view == 0 {
            return Err(Error::Binding);
        }
        clear_staging(entry, &mut self.ready_bytes, &mut self.retired);
        entry.image = Some(image);
        entry.parked = None;
        entry.failures = 0;
        enqueue(&mut self.queue, stamp.page, entry);
        self.changed();
        Ok(true)
    }
    pub fn take_task(&mut self) -> Option<(Context, FarWaterTask)> {
        if self.running.len() >= self.limits.max_running
            || self.ready_bytes
                + self.running.len() * MAX_RETAINED_RESULT_BYTES
                + MAX_RETAINED_RESULT_BYTES
                > self.limits.max_ready_bytes
        {
            return None;
        }
        for _ in 0..self.queue.len() {
            let page = self.queue.pop_front()?;
            let Some(entry) = self.entries.get_mut(&page) else {
                continue;
            };
            entry.queued = false;
            if !needs_task(entry) {
                continue;
            }
            let image = Arc::clone(entry.image.as_ref()?);
            let task = FarWaterTask::new(image, MAX_QUADS).ok()?;
            let epoch = NEXT_CONTEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
                .ok()?;
            let context = Context {
                epoch,
                stamp: task.stamp(),
            };
            entry.running = Some(epoch);
            self.running.insert(
                epoch,
                Running {
                    context,
                    registration: entry.registration,
                },
            );
            return Some((context, task));
        }
        None
    }
    pub fn complete(
        &mut self,
        context: Context,
        built: Box<BuiltFarWater>,
    ) -> Result<bool, Box<BuiltFarWater>> {
        let Some(running) = self
            .running
            .get(&context.epoch)
            .filter(|r| r.context == context)
        else {
            self.counters.stale += 1;
            return Ok(false);
        };
        let registration = running.registration;
        let current = self.entries.get(&context.stamp.page).is_some_and(|entry| {
            entry.registration == registration
                && entry.running == Some(context.epoch)
                && entry.wanted
                && built.stamp == context.stamp
                && entry.binding.matches(built.stamp)
                && entry.image.as_ref().is_some_and(|i| built.matches(i))
        });
        if !current {
            self.running.remove(&context.epoch);
            if let Some(entry) = self.entries.get_mut(&context.stamp.page) {
                if entry.running == Some(context.epoch) {
                    entry.running = None;
                }
                enqueue(&mut self.queue, context.stamp.page, entry);
            }
            self.counters.stale += 1;
            return Ok(false);
        }
        let bytes = retained(&built);
        if bytes > MAX_RETAINED_RESULT_BYTES
            || self.ready_bytes + bytes > self.limits.max_ready_bytes
        {
            return Err(built);
        }
        self.running.remove(&context.epoch);
        let Some(entry) = self.entries.get_mut(&context.stamp.page) else {
            self.counters.stale += 1;
            return Ok(false);
        };
        if entry.registration != registration || entry.running != Some(context.epoch) {
            self.counters.stale += 1;
            return Ok(false);
        }
        entry.running = None;
        if built.stamp != context.stamp
            || !entry.wanted
            || !entry.binding.matches(built.stamp)
            || entry.image.as_ref().is_none_or(|i| !built.matches(i))
        {
            self.counters.stale += 1;
            enqueue(&mut self.queue, context.stamp.page, entry);
            return Ok(false);
        }
        self.counters.completed += 1;
        if !built.complete() {
            entry.parked = Some(built.stamp);
            self.counters.unknown_held += 1;
            return Ok(false);
        }
        self.ready_bytes += bytes;
        entry.ready = Some(built);
        if !self.ready_order.contains(&context.stamp.page) {
            self.ready_order.push_back(context.stamp.page);
        }
        self.changed();
        Ok(true)
    }
    pub fn fail(&mut self, context: Context) -> bool {
        let Some(running) = self
            .running
            .get(&context.epoch)
            .filter(|r| r.context == context)
        else {
            return false;
        };
        let registration = running.registration;
        self.running.remove(&context.epoch);
        self.counters.failures += 1;
        if let Some(entry) = self
            .entries
            .get_mut(&context.stamp.page)
            .filter(|e| e.registration == registration && e.running == Some(context.epoch))
        {
            entry.running = None;
            entry.failures = entry.failures.saturating_add(1);
            if entry.failures < 3 {
                enqueue(&mut self.queue, context.stamp.page, entry)
            } else {
                entry.parked = Some(context.stamp)
            }
        }
        true
    }
    pub fn ready(&self, geometry: GeometryStamp, column_source: u64) -> bool {
        self.entries.get(&geometry.page).is_some_and(|entry| {
            entry.wanted
                && entry.binding
                    == Binding {
                        geometry,
                        column: column_source,
                    }
                && entry.image.as_ref().is_some_and(|image| {
                    entry
                        .staged
                        .as_ref()
                        .or(entry.front.as_ref())
                        .is_some_and(|r| r.stamp.matches(image))
                })
        })
    }
    pub fn all_ready(&self) -> bool {
        self.wanted.iter().all(|&(g, c)| self.ready(g, c))
    }
    pub fn flush_staging(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
        max_pages: usize,
        model: impl Fn(PageId) -> Option<[[f32; 4]; 4]>,
    ) -> Result<Work, Error> {
        let mut work = self.flush_retired(queue, gpu, budget, max_pages)?;
        if !self.retired.is_empty() {
            work.backpressure = true;
            return Ok(work);
        }
        let attempts = self.ready_order.len().min(max_pages);
        for _ in 0..attempts {
            let Some(page) = self.ready_order.pop_front() else {
                break;
            };
            let others: usize = self
                .entries
                .iter()
                .filter(|&(id, entry)| *id != page && entry.wanted)
                .map(|(_, entry)| entry.staged.or(entry.front).map_or(0, |r| r.quads))
                .sum();
            let Some(entry) = self.entries.get_mut(&page) else {
                continue;
            };
            let Some(built) = entry.ready.as_ref() else {
                continue;
            };
            let stamp = built.stamp;
            if !entry.wanted || entry.image.as_ref().is_none_or(|i| !built.matches(i)) {
                clear_staging(entry, &mut self.ready_bytes, &mut self.retired);
                continue;
            }
            if others + built.mesh.quads.len() > self.limits.max_total_quads {
                self.ready_order.push_back(page);
                work.backpressure = true;
                continue;
            }
            let quads = built.mesh.quads.len();
            let handle = if built.mesh.quads.is_empty() {
                None
            } else {
                let Some(model) = model(page) else {
                    self.ready_order.push_back(page);
                    work.backpressure = true;
                    continue;
                };
                let before = budget.remaining_bytes;
                let handle =
                    match gpu.upload_object_inactive(queue, entry.pending_handle, model, budget) {
                        Ok(h) => h,
                        Err(WaterError::Capacity | WaterError::UploadBudget) => {
                            self.ready_order.push_back(page);
                            work.backpressure = true;
                            break;
                        }
                        Err(e) => return Err(e.into()),
                    };
                entry.pending_handle = Some(handle);
                let result = gpu.upload_page_quads(queue, handle, &built.mesh.quads, budget);
                work.gpu_upload_bytes += before - budget.remaining_bytes;
                match result {
                    Ok(_) => Some(handle),
                    Err(WaterError::Capacity | WaterError::UploadBudget) => {
                        self.ready_order.push_back(page);
                        work.backpressure = true;
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            entry.pending_handle = None;
            entry.staged = Some(Resident {
                quads,
                stamp,
                handle,
                enabled: false,
            });
            self.ready_bytes -= retained(entry.ready.as_ref().expect("staged result"));
            entry.ready = None;
            work.staged += 1;
            self.changed();
        }
        Ok(work)
    }
    fn flush_retired(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
        max_pages: usize,
    ) -> Result<Work, Error> {
        let mut work = Work::default();
        for _ in 0..max_pages {
            let Some(old) = self.retired.front_mut() else {
                break;
            };
            let before = budget.remaining_bytes;
            if old.has_quads {
                match gpu.upload_page_quads(queue, old.handle, &[], budget) {
                    Ok(_) => old.has_quads = false,
                    Err(WaterError::UploadBudget) => {
                        work.backpressure = true;
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            match gpu.release_object(queue, old.handle, budget) {
                Ok(()) => {}
                Err(WaterError::UploadBudget) => {
                    work.gpu_upload_bytes += before - budget.remaining_bytes;
                    work.backpressure = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
            work.gpu_upload_bytes += before - budget.remaining_bytes;
            work.retired += 1;
            self.retired.pop_front();
        }
        Ok(work)
    }
    pub fn publication_bytes(
        &mut self,
        gpu: &mut WaterGpu,
        enabled: impl Fn(PageId) -> bool,
    ) -> Result<u64, Error> {
        if !self.all_ready() {
            return Err(Error::NotReady);
        }
        self.flags.clear();
        self.visibility.clear();
        for (&page, entry) in &self.entries {
            if entry.wanted {
                let visible = enabled(page);
                self.visibility.push((page, visible));
                if let Some(staged) = entry.staged {
                    if let Some(front) = entry.front.and_then(|r| r.handle) {
                        self.flags.push((front, false))
                    }
                    if let Some(handle) = staged.handle {
                        self.flags.push((handle, visible))
                    }
                } else if let Some(handle) = entry.front.and_then(|r| r.handle) {
                    self.flags.push((handle, visible))
                }
            } else if let Some(handle) = entry.front.and_then(|r| r.handle) {
                self.flags.push((handle, false))
            }
        }
        let bytes = gpu.enabled_batch_upload_bytes(&self.flags)?;
        self.prepared = Some(self.revision);
        Ok(bytes)
    }
    pub fn publish(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
        enabled: impl Fn(PageId) -> bool,
    ) -> Result<Work, Error> {
        if self.prepared != Some(self.revision) {
            return Err(Error::NotPrepared);
        }
        if !self.all_ready() {
            return Err(Error::NotReady);
        }
        if self
            .visibility
            .iter()
            .any(|&(page, value)| enabled(page) != value)
        {
            return Err(Error::Changed);
        }
        let bytes = gpu.set_enabled_batch(queue, &self.flags, budget)?;
        let mut work = Work {
            gpu_upload_bytes: bytes,
            ..Work::default()
        };
        self.entries.retain(|&page, entry| {
            if !entry.wanted {
                if let Some(old) = entry.front.take().and_then(|r| r.handle) {
                    self.retired.push_back(Retired {
                        handle: old,
                        has_quads: true,
                    })
                }
                return false;
            }
            if let Some(mut new) = entry.staged.take() {
                if let Some(old) = entry.front.take().and_then(|r| r.handle) {
                    self.retired.push_back(Retired {
                        handle: old,
                        has_quads: true,
                    })
                }
                new.enabled = enabled(page);
                entry.front = Some(new);
                work.published += 1;
            } else if let Some(front) = &mut entry.front {
                front.enabled = enabled(page)
            }
            true
        });
        self.guard_ready = true;
        self.changed();
        Ok(work)
    }
    pub fn guard_near(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
        allowed: impl Fn(PageId) -> bool,
    ) -> Result<u64, Error> {
        self.guard_flags.clear();
        for (&page, entry) in &self.entries {
            if let Some(handle) = entry.front.and_then(|r| r.handle) {
                self.guard_flags.push((handle, allowed(page)))
            }
        }
        self.guard_ready = false;
        let bytes = gpu.set_enabled_batch(queue, &self.guard_flags, budget)?;
        for (&page, entry) in &mut self.entries {
            if let Some(front) = &mut entry.front {
                front.enabled = allowed(page)
            }
        }
        self.guard_ready = true;
        Ok(bytes)
    }
    pub fn guard_ready(&self) -> bool {
        self.guard_ready
    }
    pub fn rebase_bytes(
        &mut self,
        gpu: &mut WaterGpu,
        model: impl Fn(PageId) -> Option<[[f32; 4]; 4]>,
    ) -> Result<u64, Error> {
        self.models.clear();
        self.guard_flags.clear();
        for (&page, entry) in &self.entries {
            let handles = [
                entry
                    .front
                    .and_then(|r| r.handle)
                    .map(|h| (h, false, entry.front.expect("front").enabled)),
                entry
                    .staged
                    .and_then(|r| r.handle)
                    .map(|h| (h, true, false)),
                entry.pending_handle.map(|h| (h, true, false)),
            ];
            if handles.iter().all(Option::is_none) {
                continue;
            }
            let matrix = model(page).ok_or(Error::NotReady)?;
            if matrix.iter().flatten().any(|v| !v.is_finite())
                || matrix[0][3] != 0.
                || matrix[1][3] != 0.
                || matrix[2][3] != 0.
                || matrix[3][3] != 1.
            {
                return Err(Error::Gpu(WaterError::InvalidModel));
            }
            for (handle, inactive, enabled) in handles.into_iter().flatten() {
                self.models.push((handle, inactive, matrix));
                self.guard_flags.push((handle, enabled));
            }
        }
        // Pure generation/duplicate validation for every handle before writes.
        gpu.enabled_batch_upload_bytes(&self.guard_flags)?;
        Ok(self.models.len() as u64 * 80)
    }
    pub fn rebase(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
        model: impl Fn(PageId) -> Option<[[f32; 4]; 4]>,
    ) -> Result<Work, Error> {
        let needed = self.rebase_bytes(gpu, model)?;
        if budget.remaining_bytes < needed {
            return Ok(Work {
                backpressure: true,
                ..Work::default()
            });
        }
        let before = budget.remaining_bytes;
        for &(handle, inactive, matrix) in &self.models {
            if inactive {
                gpu.upload_object_inactive(queue, Some(handle), matrix, budget)?;
            } else {
                gpu.upload_object(queue, Some(handle), matrix, budget)?;
            }
        }
        let uploaded = before - budget.remaining_bytes;
        if uploaded > 0 {
            self.changed();
        }
        Ok(Work {
            gpu_upload_bytes: uploaded,
            ..Work::default()
        })
    }
    pub fn teardown(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        self.set_wanted(&[])?;
        let needed = self.publication_bytes(gpu, |_| false)?;
        if budget.remaining_bytes < needed {
            return Ok(Work {
                backpressure: true,
                ..Work::default()
            });
        }
        let mut work = self.publish(queue, gpu, budget, |_| false)?;
        let retired = self.flush_retired(queue, gpu, budget, self.limits.max_pages * 2)?;
        work.retired += retired.retired;
        work.gpu_upload_bytes += retired.gpu_upload_bytes;
        work.backpressure = retired.backpressure;
        Ok(work)
    }
    pub fn stats(&self) -> Stats {
        let mut s = self.counters;
        s.wanted = self.wanted.len();
        s.records = self.entries.len();
        s.queued = self.queue.len();
        s.running = self.running.len();
        s.reserved_bytes = self.running.len() * MAX_RETAINED_RESULT_BYTES;
        s.ready_bytes = self.ready_bytes;
        s.ready_queue = self.ready_order.len();
        s.retiring = self.retired.len();
        for e in self.entries.values() {
            s.staged += usize::from(e.staged.is_some());
            s.front += usize::from(e.front.is_some());
            s.front_quads += e.front.map_or(0, |r| r.quads);
            s.staged_quads += e.staged.map_or(0, |r| r.quads);
        }
        s
    }
}
fn retained(built: &BuiltFarWater) -> usize {
    std::mem::size_of::<BuiltFarWater>() + built.mesh.quads.capacity() * 16
}
fn needs_task(entry: &Entry) -> bool {
    entry.wanted
        && entry.running.is_none()
        && entry.ready.is_none()
        && entry.image.as_ref().is_some_and(|image| {
            let stamp = Stamp::from_halo(image);
            entry.parked != Some(stamp)
                && entry
                    .staged
                    .as_ref()
                    .or(entry.front.as_ref())
                    .is_none_or(|r| r.stamp != stamp)
        })
}
fn enqueue(queue: &mut VecDeque<PageId>, page: PageId, entry: &mut Entry) {
    if !entry.queued && needs_task(entry) {
        entry.queued = true;
        queue.push_back(page)
    }
}
fn clear_staging(entry: &mut Entry, ready_bytes: &mut usize, retired: &mut VecDeque<Retired>) {
    if let Some(ready) = entry.ready.take() {
        *ready_bytes -= retained(&ready)
    }
    if let Some(handle) = entry.pending_handle.take() {
        retired.push_back(Retired {
            handle,
            has_quads: false,
        })
    }
    if let Some(handle) = entry.staged.take().and_then(|r| r.handle) {
        retired.push_back(Retired {
            handle,
            has_quads: true,
        })
    }
}
