use crate::task::{neighbor, neighbor_index};
use crate::{
    CompletedMesh, INPUT_PAGES, MESH_CHANNELS, MESH_RETAINED_BYTES, MeshTask, SceneOutput,
    SceneTask,
};
use core_types::{BlockPos, PageId, SpaceId, Tick, Version};
use jobs::{
    Completion, Dispatch, Finish, JobId, JobKey, JobOutcome, JobSpec, Priority, Scheduler, Take,
    WorkBudget, WorkCost,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use terrain_gen::{GenerationTask, TerrainSeed};
use world::{CursorId, PageRegion, SampledPageInput as PageInput, Transaction, World, WorldChange};

#[derive(Clone, Copy, Debug)]
pub struct SceneConfig {
    pub seed: TerrainSeed,
    pub center: BlockPos,
    pub horizontal_radius: u8,
    pub vertical_radius: u8,
    pub max_loaded_pages: usize,
    pub scheduler: jobs::Limits,
    pub max_ready_meshes: usize,
    pub max_ready_bytes: usize,
    pub max_retries: u8,
    pub max_new_jobs_per_phase: usize,
    pub overrides: crate::OverrideLimits,
    pub max_evictions_per_phase: usize,
    /// Permit publication only through `take_composite`: stale immutable bases
    /// must be paired atomically with the exact current post-capture overlay.
    pub coherent_rebasing: bool,
}
impl SceneConfig {
    pub fn forest() -> Self {
        Self {
            seed: TerrainSeed(1337),
            center: BlockPos {
                space: SpaceId(0),
                x: 96,
                y: 80,
                z: 96,
            },
            horizontal_radius: 1,
            vertical_radius: 1,
            max_loaded_pages: 512,
            scheduler: jobs::Limits {
                max_jobs: 64,
                max_bytes: 32 * 1024 * 1024,
                max_age_ticks: 80,
                reserved_interactive_jobs: 8,
                reserved_interactive_bytes: 4 * 1024 * 1024,
            },
            max_ready_meshes: 64,
            max_ready_bytes: 8 * 1024 * 1024,
            max_retries: 3,
            max_new_jobs_per_phase: 4,
            overrides: crate::OverrideLimits::default(),
            max_evictions_per_phase: 2,
            coherent_rebasing: false,
        }
    }
}
impl Default for SceneConfig {
    fn default() -> Self {
        Self::forest()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneError {
    InvalidConfig(&'static str),
    World(world::CommitError),
    Cursor(world::CursorError),
    UnexpectedCompletion,
    WrongOutput,
    WorkerFailed(PageId),
    PageLimit,
    OverrideLimit,
    HiddenPageLimit,
    Wire(crate::WireError),
}
impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client scene error: {self:?}")
    }
}
impl std::error::Error for SceneError {}
impl From<world::CommitError> for SceneError {
    fn from(value: world::CommitError) -> Self {
        Self::World(value)
    }
}
impl From<world::CursorError> for SceneError {
    fn from(value: world::CursorError) -> Self {
        Self::Cursor(value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneStats {
    pub desired_pages: usize,
    pub visible_pages: usize,
    pub loaded_pages: usize,
    pub queued_jobs: usize,
    pub running_jobs: usize,
    pub ready_meshes: usize,
    pub ready_bytes: usize,
    pub generation_published: u64,
    pub meshes_published: u64,
    pub invalidations: u64,
    pub stale_results: u64,
    pub failed_jobs: u64,
    pub backpressure: u64,
    /// Successful worker reports, including results later rejected as stale.
    /// Mid-kernel cancellations have no report and are tracked as jobs instead.
    pub generation_cells_copied: u64,
    pub mesh_cells_scanned: u64,
    pub mesh_quads: u64,
    pub override_pages: usize,
    pub override_cells: usize,
    pub override_bytes: usize,
    pub pending_evictions: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseWork {
    pub applied: usize,
    pub enqueued: usize,
    pub dispatched: usize,
    pub generation_pages: usize,
    pub mesh_results: usize,
    pub stale: usize,
    pub halo_waits: usize,
    pub backpressure: usize,
}
#[derive(Clone, Copy, Debug)]
enum RequestState {
    Pending,
    Job(JobId, Version),
    Loaded,
    Failed,
}
struct Request {
    state: RequestState,
    attempts: u8,
}
#[derive(Clone, Copy)]
struct Dirty {
    since: Tick,
    priority: Priority,
}
#[derive(Clone)]
enum Kind {
    Generation {
        expected: Version,
    },
    Mesh {
        inputs: Box<[PageInput; INPUT_PAGES]>,
    },
}
#[derive(Clone)]
struct Metadata {
    page: PageId,
    kind: Kind,
    dispatched: bool,
    cancel_notified: bool,
}

/// A base mesh is allowed to retain its original versions. `changed_owners`
/// covers all geometry/AO changes since capture; it is never a current-base tag.
#[derive(Debug)]
pub struct CompositeMesh {
    pub base: CompletedMesh,
    pub changed_owners: crate::edit_shell::OwnerMask,
    pub world_version: Version,
    job: Option<JobId>,
}

pub struct ClientScene {
    config: SceneConfig,
    world: World,
    cursor: CursorId,
    visible: BTreeSet<PageId>,
    requests: BTreeMap<PageId, Request>,
    pending_generation: VecDeque<PageId>,
    dirty: BTreeMap<PageId, Dirty>,
    candidates: Vec<PageId>,
    tints: BTreeMap<PageId, Box<[u16; 256]>>,
    scheduler: Scheduler<SceneTask>,
    metadata: BTreeMap<JobId, Metadata>,
    mesh_jobs: BTreeMap<PageId, JobId>,
    running_order: BTreeSet<JobId>,
    completions: BTreeMap<JobId, Completion<SceneOutput>>,
    cancellations: VecDeque<JobId>,
    ready: VecDeque<CompletedMesh>,
    ready_bytes: usize,
    meshed_versions: BTreeMap<PageId, Version>,
    mesh_failures: BTreeMap<PageId, u8>,
    stats: SceneStats,
    error: Option<SceneError>,
    overrides: crate::overrides::Overrides,
    hidden_pages: BTreeSet<PageId>,
    pending_evictions: VecDeque<PageId>,
    rebases: crate::rebase::RebaseTracker,
}
impl ClientScene {
    pub fn new(config: SceneConfig) -> Result<Self, SceneError> {
        if config.center.space != SpaceId(0)
            || config.max_retries == 0
            || config.max_new_jobs_per_phase == 0
            || config.max_evictions_per_phase == 0
            || config.max_ready_meshes == 0
            || config.max_ready_bytes < 1024 * 1024
            || config.max_loaded_pages > 8192
        {
            return Err(SceneError::InvalidConfig(
                "unsupported space or zero/oversized budgets",
            ));
        }
        let center = config.center.page();
        let horizontal = i32::from(config.horizontal_radius);
        let vertical = i32::from(config.vertical_radius);
        if horizontal > 8 || vertical > 8 {
            return Err(SceneError::InvalidConfig(
                "near-page radius exceeds bounded scene capacity",
            ));
        }
        let mut visible = BTreeSet::new();
        for dy in -vertical..=vertical {
            for dz in -horizontal..=horizontal {
                for dx in -horizontal..=horizontal {
                    visible.insert(
                        neighbor(center, dx, dy, dz)
                            .ok_or(SceneError::InvalidConfig("page coordinates overflow"))?,
                    );
                }
            }
        }
        let mut desired = Vec::new();
        for dy in -vertical - 1..=vertical + 1 {
            for dz in -horizontal - 1..=horizontal + 1 {
                for dx in -horizontal - 1..=horizontal + 1 {
                    desired.push(
                        neighbor(center, dx, dy, dz)
                            .ok_or(SceneError::InvalidConfig("halo coordinates overflow"))?,
                    );
                }
            }
        }
        if desired.len() > config.max_loaded_pages {
            return Err(SceneError::InvalidConfig(
                "halo pages exceed resident-page budget",
            ));
        }
        desired.sort_by_key(|page| {
            let dx = i64::from(page.x) - i64::from(center.x);
            let dz = i64::from(page.z) - i64::from(center.z);
            (
                (dx * dx + dz * dz) * 4 + (i64::from(page.y) - i64::from(center.y)).abs(),
                std::cmp::Reverse(page.y),
                page.x,
                page.z,
            )
        });
        let pending_generation = desired.iter().copied().collect();
        let requests = desired
            .into_iter()
            .map(|page| {
                (
                    page,
                    Request {
                        state: RequestState::Pending,
                        attempts: 0,
                    },
                )
            })
            .collect();
        let mut world = World::new(rules::properties, 4096);
        let cursor = world.register_cursor();
        let visible_count = visible.len();
        let scheduler = Scheduler::new(config.scheduler)
            .map_err(|_| SceneError::InvalidConfig("invalid scheduler bounds"))?;
        let overrides = crate::overrides::Overrides::new(config.overrides)?;
        Ok(Self {
            config,
            world,
            cursor,
            visible,
            requests,
            pending_generation,
            dirty: BTreeMap::new(),
            candidates: Vec::with_capacity(visible_count),
            tints: BTreeMap::new(),
            scheduler,
            metadata: BTreeMap::new(),
            mesh_jobs: BTreeMap::new(),
            running_order: BTreeSet::new(),
            completions: BTreeMap::new(),
            cancellations: VecDeque::with_capacity(config.scheduler.max_jobs),
            ready: VecDeque::with_capacity(config.max_ready_meshes),
            ready_bytes: 0,
            meshed_versions: BTreeMap::new(),
            mesh_failures: BTreeMap::new(),
            stats: SceneStats::default(),
            error: None,
            overrides,
            hidden_pages: BTreeSet::new(),
            pending_evictions: VecDeque::with_capacity(config.max_loaded_pages),
            rebases: crate::rebase::RebaseTracker::default(),
        })
    }
    /// Move the bounded near cache. Terrain is never generated here; obsolete
    /// pages are evicted in bounded owner phases, and user edits stay in overrides.
    pub fn set_center(&mut self, center: BlockPos) -> Result<(), SceneError> {
        if center.space != SpaceId(0) {
            return Err(SceneError::InvalidConfig("unsupported client space"));
        }
        if center.page() == self.config.center.page() {
            self.config.center = center;
            return Ok(());
        }
        let page = center.page();
        let horizontal = i32::from(self.config.horizontal_radius);
        let vertical = i32::from(self.config.vertical_radius);
        let mut visible = BTreeSet::new();
        for dy in -vertical..=vertical {
            for dz in -horizontal..=horizontal {
                for dx in -horizontal..=horizontal {
                    visible.insert(
                        neighbor(page, dx, dy, dz)
                            .ok_or(SceneError::InvalidConfig("streaming coordinate overflow"))?,
                    );
                }
            }
        }
        let mut desired = Vec::new();
        for dy in -vertical - 1..=vertical + 1 {
            for dz in -horizontal - 1..=horizontal + 1 {
                for dx in -horizontal - 1..=horizontal + 1 {
                    desired.push(
                        neighbor(page, dx, dy, dz)
                            .ok_or(SceneError::InvalidConfig("streaming halo overflow"))?,
                    );
                }
            }
        }
        let desired_set: BTreeSet<_> = desired.iter().copied().collect();
        let newly_hidden: Vec<_> = self.visible.difference(&visible).copied().collect();
        let hidden_count = self
            .hidden_pages
            .iter()
            .filter(|id| !visible.contains(id))
            .count()
            + newly_hidden
                .iter()
                .filter(|id| !self.hidden_pages.contains(id))
                .count();
        if hidden_count > self.config.max_loaded_pages {
            return Err(SceneError::HiddenPageLimit);
        }
        desired.sort_by_key(|id| {
            let dx = i64::from(id.x) - i64::from(page.x);
            let dz = i64::from(id.z) - i64::from(page.z);
            (
                (dx * dx + dz * dz) * 4 + (i64::from(id.y) - i64::from(page.y)).abs(),
                std::cmp::Reverse(id.y),
                id.x,
                id.z,
            )
        });
        let old_mesh_jobs: Vec<_> = self
            .mesh_jobs
            .iter()
            .filter(|(id, _)| !visible.contains(id))
            .map(|(page, id)| (*page, *id))
            .collect();
        for (page, id) in old_mesh_jobs {
            self.cancel_job(id);
            self.mesh_jobs.remove(&page);
        }
        let old_requests = std::mem::take(&mut self.requests);
        let mut old_requests = old_requests;
        let mut requests = BTreeMap::new();
        self.pending_generation.clear();
        for id in desired {
            let mut request = old_requests.remove(&id).unwrap_or(Request {
                state: RequestState::Pending,
                attempts: 0,
            });
            if self.world.page(id).is_some() {
                request.state = RequestState::Loaded;
            }
            if matches!(request.state, RequestState::Pending) {
                self.pending_generation.push_back(id);
            }
            requests.insert(id, request);
        }
        for request in old_requests.into_values() {
            if let RequestState::Job(id, _) = request.state {
                self.cancel_job(id);
            }
        }
        self.requests = requests;
        self.hidden_pages.retain(|page| !visible.contains(page));
        self.hidden_pages.extend(newly_hidden);
        self.visible = visible;
        self.rebases.retain_visible(&self.visible);
        self.dirty.retain(|page, _| self.visible.contains(page));
        self.mesh_failures
            .retain(|page, _| self.visible.contains(page));
        self.meshed_versions
            .retain(|page, _| self.visible.contains(page));
        let mut removed_bytes = 0;
        self.ready.retain(|mesh| {
            let keep = self.visible.contains(&mesh.page);
            if !keep {
                removed_bytes += mesh.retained_bytes();
            }
            keep
        });
        self.ready_bytes -= removed_bytes;
        self.pending_evictions.clear();
        self.pending_evictions.extend(
            self.world
                .pages()
                .filter_map(|(id, _)| (!desired_set.contains(&id)).then_some(id)),
        );
        self.config.center = center;
        self.candidates.clear();
        self.candidates.extend(self.visible.iter().copied());
        for index in 0..self.candidates.len() {
            let id = self.candidates[index];
            if !self.meshed_versions.contains_key(&id) {
                self.mark_dirty(id, self.world.tick(), Priority::Normal, true);
            }
        }
        Ok(())
    }
    /// GPU owner releases object-table slots for these pages before reusing them.
    pub fn take_hidden_page(&mut self) -> Option<PageId> {
        while let Some(page) = self.hidden_pages.pop_first() {
            if !self.visible.contains(&page) {
                return Some(page);
            }
        }
        None
    }
    fn evict_phase(&mut self) -> Result<(), SceneError> {
        for _ in 0..self.config.max_evictions_per_phase {
            let Some(page) = self.pending_evictions.pop_front() else {
                break;
            };
            if self.requests.contains_key(&page) {
                continue;
            }
            let Some(snapshot) = self.world.snapshot(page) else {
                continue;
            };
            let mut tx = Transaction::new(self.world.tick(), page.space);
            for (_, id) in snapshot.component_links() {
                tx.remove_component(id, core_types::ChannelMask::CONTENTS);
            }
            tx.remove_page(page);
            self.apply_internal(tx, false)?;
            self.tints.remove(&page);
        }
        Ok(())
    }
    pub fn world(&self) -> &World {
        &self.world
    }
    pub(crate) fn derived_world_mut(&mut self) -> &mut World {
        &mut self.world
    }
    /// Immutable generated biome tint for terrain and the immediate edit shell.
    pub fn biome_tint(&self, page: PageId, x: u8, z: u8) -> Option<u16> {
        if x >= 16 || z >= 16 {
            return None;
        }
        self.tints
            .get(&page)
            .map(|tints| tints[usize::from(x) + 16 * usize::from(z)])
    }
    pub fn config(&self) -> SceneConfig {
        self.config
    }
    pub fn visible_pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.visible.iter().copied()
    }
    pub fn requested_pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.requests.keys().copied()
    }
    pub fn is_visible(&self, page: PageId) -> bool {
        self.visible.contains(&page)
    }
    pub fn error(&self) -> Option<&SceneError> {
        self.error.as_ref()
    }
    pub fn is_ready(&self) -> bool {
        self.visible.iter().all(|page| {
            self.meshed_versions
                .get(page)
                .is_some_and(|v| *v == self.world.input_version(*page, MESH_CHANNELS))
        })
    }
    pub fn stats(&self) -> SceneStats {
        SceneStats {
            desired_pages: self.requests.len(),
            visible_pages: self.visible.len(),
            loaded_pages: self.world.loaded_pages(),
            queued_jobs: self.scheduler.pending_len(),
            running_jobs: self.scheduler.running_len(),
            ready_meshes: self.ready.len(),
            ready_bytes: self.ready_bytes,
            override_pages: self.overrides.stats().pages,
            override_cells: self.overrides.stats().cells,
            override_bytes: self.overrides.stats().bytes,
            pending_evictions: self.pending_evictions.len(),
            ..self.stats
        }
    }
    pub fn pending_meshes(&self) -> usize {
        self.dirty.len()
    }
    pub fn pending_generation(&self) -> usize {
        self.requests
            .values()
            .filter(|r| matches!(r.state, RequestState::Pending | RequestState::Job(..)))
            .count()
    }
    pub fn default_budget() -> WorkBudget {
        WorkBudget::new(
            WorkCost {
                cells: 250_000,
                quads: 100_000,
                bytes: 2 * 1024 * 1024,
                rays: 0,
            },
            WorkCost {
                cells: 16_000,
                quads: 25_000,
                bytes: 512 * 1024,
                rays: 0,
            },
        )
    }
    pub fn apply_transaction(&mut self, tx: Transaction) -> Result<WorldChange, SceneError> {
        self.apply_internal(tx, true)
    }
    fn apply_internal(
        &mut self,
        tx: Transaction,
        user_edit: bool,
    ) -> Result<WorldChange, SceneError> {
        let additions = tx
            .created_pages()
            .filter(|page| self.world.page(*page).is_none())
            .count();
        let removals = tx
            .removed_pages()
            .filter(|page| self.world.page(*page).is_some())
            .count();
        if self
            .world
            .loaded_pages()
            .saturating_add(additions)
            .saturating_sub(removals)
            > self.config.max_loaded_pages
        {
            return Err(SceneError::PageLimit);
        }
        let prepared = if user_edit {
            Some(self.overrides.prepare(&self.world, &tx)?)
        } else {
            None
        };
        let change = self.world.commit(tx)?;
        if let Some(prepared) = prepared {
            self.overrides.commit(prepared);
        }
        self.consume_changes()?;
        Ok(change)
    }
    fn cancel_job(&mut self, id: JobId) {
        if let Some(meta) = self.metadata.get(&id) {
            self.rebases.forget_job(meta.page, id);
        }
        if self.scheduler.cancel(id).is_some() {
            self.metadata.remove(&id);
            self.running_order.remove(&id);
            self.completions.remove(&id);
        } else if let Some(meta) = self.metadata.get_mut(&id)
            && meta.dispatched
            && !meta.cancel_notified
        {
            // Native workers share Scheduler's token. Browser transports must
            // forward this bounded command to their own shared/transfer control.
            meta.cancel_notified = true;
            self.cancellations.push_back(id);
        }
    }
    pub fn take_cancellation(&mut self) -> Option<JobId> {
        self.cancellations.pop_front()
    }
    fn mark_dirty(&mut self, page: PageId, tick: Tick, priority: Priority, reset_failure: bool) {
        if !self.visible.contains(&page) {
            return;
        }
        self.stats.invalidations = self.stats.invalidations.saturating_add(1);
        if reset_failure {
            self.mesh_failures.remove(&page);
        }
        let preserve = self.config.coherent_rebasing && self.rebases.preserves(page);
        if !preserve {
            self.rebases.forget(page);
        }
        if !preserve && let Some(id) = self.mesh_jobs.get(&page).copied() {
            self.cancel_job(id);
            if !self.metadata.contains_key(&id) {
                self.mesh_jobs.remove(&page);
            }
        }
        self.meshed_versions.remove(&page);
        let mut removed_bytes = 0;
        self.ready.retain(|mesh| {
            let keep = mesh.page != page || preserve;
            if !keep {
                removed_bytes += mesh.retained_bytes();
            }
            keep
        });
        self.ready_bytes -= removed_bytes;
        self.dirty
            .entry(page)
            .and_modify(|dirty| {
                dirty.priority = dirty.priority.min(priority);
                dirty.since = Tick(dirty.since.0.min(tick.0));
            })
            .or_insert(Dirty {
                since: tick,
                priority,
            });
    }
    pub fn invalidate_page(&mut self, page: PageId, tick: Tick) {
        self.rebases.forget(page);
        self.mark_dirty(page, tick, Priority::Interactive, true);
    }
    fn consume_changes(&mut self) -> Result<(), SceneError> {
        loop {
            let batch = match self.world.read_cursor(self.cursor, 32) {
                Ok(batch) => batch,
                Err(world::CursorError::Lagged { .. }) => {
                    self.rebases.invalidate_all();
                    self.candidates.clear();
                    self.candidates.extend(self.visible.iter().copied());
                    for index in 0..self.candidates.len() {
                        self.mark_dirty(
                            self.candidates[index],
                            self.world.tick(),
                            Priority::Interactive,
                            true,
                        );
                    }
                    self.world
                        .recover_cursor(self.cursor, self.world.journal_next())?;
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };
            if batch.changes.is_empty() {
                return Ok(());
            }
            for change in &batch.changes {
                self.rebases.observe(change);
                for page in &change.pages {
                    let old_job = self.requests.get(&page.page).and_then(|r| {
                        if let RequestState::Job(id, expected) = r.state {
                            (expected != self.world.page_version(page.page)).then_some(id)
                        } else {
                            None
                        }
                    });
                    if let Some(id) = old_job {
                        self.cancel_job(id);
                    }
                    if let Some(request) = self.requests.get_mut(&page.page) {
                        if self.world.page(page.page).is_some() {
                            request.state = RequestState::Loaded;
                        } else {
                            request.state = RequestState::Pending;
                            if !self.pending_generation.contains(&page.page) {
                                self.pending_generation.push_back(page.page);
                            }
                        }
                    }
                    if !page.channels.intersects(MESH_CHANNELS) {
                        continue;
                    }
                    let mut found_cell = false;
                    if !page.created && !page.removed {
                        for cell in &change.cells {
                            if cell.page != page.page {
                                continue;
                            }
                            found_cell = true;
                            let (x, y, z) = cell.index.xyz();
                            let axes = |v: u8| {
                                [
                                    Some(0),
                                    if v == 0 {
                                        Some(-1)
                                    } else if v == 15 {
                                        Some(1)
                                    } else {
                                        None
                                    },
                                ]
                            };
                            for dx in axes(x).into_iter().flatten() {
                                for dy in axes(y).into_iter().flatten() {
                                    for dz in axes(z).into_iter().flatten() {
                                        if let Some(id) = neighbor(page.page, dx, dy, dz) {
                                            self.mark_dirty(
                                                id,
                                                change.tick,
                                                Priority::Interactive,
                                                true,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !found_cell {
                        for dy in -1..=1 {
                            for dz in -1..=1 {
                                for dx in -1..=1 {
                                    if let Some(id) = neighbor(page.page, dx, dy, dz) {
                                        self.mark_dirty(id, change.tick, Priority::Normal, true);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            self.world.acknowledge_cursor(self.cursor, batch.next)?;
        }
    }
    fn mesh_task(&self, page: PageId) -> Option<MeshTask> {
        let mut snapshots: [Option<std::sync::Arc<world::Page>>; INPUT_PAGES] =
            std::array::from_fn(|_| None);
        let mut inputs = [PageInput {
            page,
            channels: MESH_CHANNELS,
            region: PageRegion::WHOLE,
            version: Version(0),
        }; INPUT_PAGES];
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let id = neighbor(page, dx, dy, dz)?;
                    let snapshot = self.world.snapshot(id)?; // Unknown is never an air halo.
                    let index = neighbor_index(dx, dy, dz);
                    let region = PageRegion::new([-dx as i8, -dy as i8, -dz as i8])?;
                    inputs[index] = PageInput {
                        page: id,
                        channels: MESH_CHANNELS,
                        region,
                        version: snapshot.sampled_version(MESH_CHANNELS, region),
                    };
                    snapshots[index] = Some(snapshot);
                }
            }
        }
        let tints = self
            .tints
            .get(&page)
            .cloned()
            .unwrap_or_else(|| Box::new([u16::MAX; 256]));
        Some(MeshTask::snapshots(
            page,
            inputs,
            snapshots.map(|p| p.expect("all 27 known")),
            tints,
        ))
    }
    fn enqueue_work(&mut self, tick: Tick, work: &mut PhaseWork) {
        self.candidates.clear();
        self.candidates.extend(self.dirty.keys().copied());
        for index in 0..self.candidates.len() {
            if work.enqueued >= self.config.max_new_jobs_per_phase {
                break;
            }
            let page = self.candidates[index];
            // A dispatched base is allowed to finish even during continuous
            // edits. A second task must not replace its certificate before the
            // owner atomically publishes or returns the complete composite.
            if self.config.coherent_rebasing && self.rebases.preserves(page) {
                continue;
            }
            let Some(dirty) = self.dirty.get(&page).copied() else {
                continue;
            };
            if self.world.page(page).is_none() {
                continue;
            }
            let Some(task) = self.mesh_task(page) else {
                work.halo_waits += 1;
                continue;
            };
            let spec = JobSpec::derived(
                JobKey { page, kind: 2 },
                dirty.priority,
                WorkCost {
                    cells: render::terrain_mesh::HALO_VOLUME as u64,
                    quads: render::terrain_mesh::MAX_PAGE_QUADS as u64,
                    bytes: (render::terrain_mesh::MAX_PAGE_QUADS * 16) as u64,
                    rays: 0,
                },
                MESH_RETAINED_BYTES,
                dirty.since,
            );
            // Channel-specific stamps live in MeshTask. Base page versions are
            // intentionally not used: chest/light changes must not cancel terrain.
            let inputs = task.inputs;
            match self
                .scheduler
                .enqueue(spec, SceneTask::Mesh(Box::new(task)))
            {
                Ok(id) => {
                    self.metadata.insert(
                        id,
                        Metadata {
                            page,
                            kind: Kind::Mesh {
                                inputs: Box::new(inputs),
                            },
                            dispatched: false,
                            cancel_notified: false,
                        },
                    );
                    self.mesh_jobs.insert(page, id);
                    self.dirty.remove(&page);
                    work.enqueued += 1;
                }
                Err(_) => {
                    self.stats.backpressure += 1;
                    work.backpressure += 1;
                    break;
                }
            }
        }
        while work.enqueued < self.config.max_new_jobs_per_phase {
            let Some(page) = self.pending_generation.front().copied() else {
                break;
            };
            if !self
                .requests
                .get(&page)
                .is_some_and(|r| matches!(r.state, RequestState::Pending))
            {
                self.pending_generation.pop_front();
                continue;
            }
            if self.world.page(page).is_some() {
                self.requests.get_mut(&page).expect("desired").state = RequestState::Loaded;
                self.pending_generation.pop_front();
                continue;
            }
            let expected = self.world.page_version(page);
            let task = GenerationTask {
                terrain_seed: self.config.seed,
                page,
                expected: Some(expected),
            };
            let spec = task.job_spec(Priority::Background, tick);
            match self.scheduler.enqueue(spec, SceneTask::Generation(task)) {
                Ok(id) => {
                    self.metadata.insert(
                        id,
                        Metadata {
                            page,
                            kind: Kind::Generation { expected },
                            dispatched: false,
                            cancel_notified: false,
                        },
                    );
                    self.requests.get_mut(&page).expect("desired").state =
                        RequestState::Job(id, expected);
                    self.pending_generation.pop_front();
                    work.enqueued += 1;
                }
                Err(_) => {
                    self.stats.backpressure += 1;
                    work.backpressure += 1;
                    break;
                }
            }
        }
    }
    /// Store arrivals; publication occurs only in `phase`, in dispatch-ID order.
    pub fn complete(&mut self, completion: Completion<SceneOutput>) -> Result<(), SceneError> {
        if !self.running_order.contains(&completion.id)
            || self.completions.contains_key(&completion.id)
        {
            return Err(SceneError::UnexpectedCompletion);
        }
        match &completion.outcome {
            JobOutcome::Success(SceneOutput::Generation(page)) => {
                self.stats.generation_cells_copied = self
                    .stats
                    .generation_cells_copied
                    .saturating_add(page.work.copied_cells);
            }
            JobOutcome::Success(SceneOutput::Mesh(mesh)) => {
                self.stats.mesh_cells_scanned = self
                    .stats
                    .mesh_cells_scanned
                    .saturating_add(mesh.work.work.cells_scanned);
                self.stats.mesh_quads = self
                    .stats
                    .mesh_quads
                    .saturating_add(mesh.work.work.quads_emitted);
            }
            _ => {}
        }
        self.completions.insert(completion.id, completion);
        Ok(())
    }
    pub fn complete_bytes(&mut self, id: JobId, bytes: &[u8]) -> Result<(), SceneError> {
        let outcome = match SceneOutput::decode(bytes) {
            Ok(value) => JobOutcome::Success(value),
            Err(error) => JobOutcome::Failed(jobs::JobFailure::Error(error.to_string())),
        };
        self.complete(Completion { id, outcome })
    }
    pub fn fail(&mut self, id: JobId, message: String) -> Result<(), SceneError> {
        self.complete(Completion {
            id,
            outcome: JobOutcome::Failed(jobs::JobFailure::Error(message)),
        })
    }
    pub fn cancelled(&mut self, id: JobId) -> Result<(), SceneError> {
        self.complete(Completion {
            id,
            outcome: JobOutcome::Cancelled,
        })
    }
    fn retry(&mut self, id: JobId, meta: Metadata, failed: bool, tick: Tick) {
        self.rebases.forget_job(meta.page, id);
        match meta.kind {
            Kind::Generation { .. } => {
                let Some(request) = self.requests.get_mut(&meta.page) else {
                    return;
                };
                if !matches!(request.state, RequestState::Job(current, _) if current == id) {
                    return;
                }
                if self.world.page(meta.page).is_some() {
                    request.state = RequestState::Loaded;
                    return;
                }
                if failed {
                    request.attempts = request.attempts.saturating_add(1);
                }
                if request.attempts >= self.config.max_retries {
                    request.state = RequestState::Failed;
                    self.error = Some(SceneError::WorkerFailed(meta.page));
                } else {
                    request.state = RequestState::Pending;
                    if !self.pending_generation.contains(&meta.page) {
                        self.pending_generation.push_back(meta.page);
                    }
                }
            }
            Kind::Mesh { .. } => {
                if self.mesh_jobs.get(&meta.page) == Some(&id) {
                    self.mesh_jobs.remove(&meta.page);
                }
                if !self.visible.contains(&meta.page) {
                    self.mesh_failures.remove(&meta.page);
                    return;
                }
                let count = self.mesh_failures.entry(meta.page).or_default();
                if failed {
                    *count = count.saturating_add(1);
                }
                if *count >= self.config.max_retries {
                    self.error = Some(SceneError::WorkerFailed(meta.page));
                } else {
                    self.mark_dirty(meta.page, tick, Priority::Interactive, false);
                }
            }
        }
    }
    pub fn phase(&mut self, tick: Tick, max_results: usize) -> Result<PhaseWork, SceneError> {
        if tick < self.world.tick() {
            return Err(SceneError::World(world::CommitError::TickWentBackwards));
        }
        self.evict_phase()?;
        self.consume_changes()?;
        let mut work = PhaseWork::default();
        while work.applied < max_results {
            let Some(id) = self.running_order.first().copied() else {
                break;
            };
            let Some(completion) = self.completions.get(&id) else {
                break;
            };
            if matches!(&completion.outcome, JobOutcome::Success(SceneOutput::Generation(page)) if self.world.page(page.page).is_none() && self.world.loaded_pages() >= self.config.max_loaded_pages)
            {
                work.backpressure += 1;
                self.stats.backpressure += 1;
                break;
            }
            if let JobOutcome::Success(SceneOutput::Mesh(mesh)) = &completion.outcome
                && (self.world.matches_inputs(&mesh.inputs)
                    || self.rebases.accepts(mesh.page, id, &mesh.inputs))
                && (self.ready.len() >= self.config.max_ready_meshes
                    || mesh.retained_bytes()
                        > self.config.max_ready_bytes.saturating_sub(self.ready_bytes))
            {
                work.backpressure += 1;
                self.stats.backpressure += 1;
                break;
            }
            let mut completion = self.completions.remove(&id).expect("checked arrival");
            self.running_order.remove(&id);
            let meta = self
                .metadata
                .remove(&id)
                .ok_or(SceneError::UnexpectedCompletion)?;
            self.cancellations.retain(|queued| *queued != id);
            let valid = match (&meta.kind, &completion.outcome) {
                (
                    Kind::Generation { expected },
                    JobOutcome::Success(SceneOutput::Generation(page)),
                ) => {
                    page.page == meta.page
                        && page.expected == Some(*expected)
                        && page.generator_version == terrain_gen::GENERATOR_VERSION
                }
                (Kind::Mesh { inputs }, JobOutcome::Success(SceneOutput::Mesh(mesh))) => {
                    mesh.page == meta.page
                        && mesh.inputs == **inputs
                        && mesh.version == inputs[13].version
                        && self.visible.contains(&mesh.page)
                        && (self.world.matches_inputs(inputs.as_ref())
                            || self.rebases.accepts(mesh.page, id, inputs.as_ref()))
                }
                (_, JobOutcome::Success(_)) => false,
                _ => true,
            };
            if !valid {
                completion.outcome = JobOutcome::Cancelled;
                self.stats.stale_results += 1;
                work.stale += 1;
            }
            let result = self
                .scheduler
                .finish(completion, |page| Some(self.world.page_version(page)));
            work.applied += 1;
            match result {
                Finish::Published(SceneOutput::Generation(page)) => {
                    if self.world.page(page.page).is_some() {
                        self.retry(id, meta, false, tick);
                        continue;
                    }
                    let mut tx = Transaction::new(tick, page.page.space);
                    tx.expect_page(page.page, page.expected.unwrap_or(Version(0)));
                    let mut cells = page.cells;
                    self.overrides.restore(page.page, cells.as_mut(), &mut tx);
                    let cells: Box<[core_types::Cell]> = cells;
                    tx.create_page(page.page, cells);
                    if let Err(error) = self.apply_internal(tx, false) {
                        self.retry(id, meta, false, tick);
                        return Err(error);
                    }
                    self.tints.insert(page.page, page.tints);
                    self.stats.generation_published += 1;
                    work.generation_pages += 1;
                }
                Finish::Published(SceneOutput::Mesh(mesh)) => {
                    if self.mesh_jobs.get(&mesh.page) == Some(&id) {
                        self.mesh_jobs.remove(&mesh.page);
                    }
                    self.meshed_versions.insert(mesh.page, mesh.version);
                    self.ready_bytes += mesh.retained_bytes();
                    self.stats.meshes_published += 1;
                    self.ready.push_back(*mesh);
                    work.mesh_results += 1;
                }
                Finish::Failed(_) => {
                    self.stats.failed_jobs += 1;
                    self.retry(id, meta, true, tick);
                }
                Finish::Cancelled | Finish::Stale => {
                    self.retry(id, meta, false, tick);
                }
                Finish::UnknownJob => return Err(SceneError::UnexpectedCompletion),
            }
        }
        self.enqueue_work(tick, &mut work);
        Ok(work)
    }
    pub fn take_task(
        &mut self,
        tick: Tick,
        budget: &mut WorkBudget,
    ) -> Option<Dispatch<SceneTask>> {
        loop {
            match self
                .scheduler
                .take_next(tick, budget, |page| Some(self.world.page_version(page)))
            {
                Take::Ready(dispatch) => {
                    let meta = self.metadata.get_mut(&dispatch.id)?;
                    if let Kind::Mesh { inputs } = &meta.kind
                        && !self.world.matches_inputs(inputs.as_ref())
                    {
                        let meta = meta.clone();
                        self.metadata.remove(&dispatch.id);
                        self.scheduler.finish::<SceneOutput>(
                            Completion {
                                id: dispatch.id,
                                outcome: JobOutcome::Cancelled,
                            },
                            |_| None,
                        );
                        self.retry(dispatch.id, meta, false, tick);
                        self.stats.stale_results += 1;
                        continue;
                    }
                    meta.dispatched = true;
                    if self.config.coherent_rebasing
                        && let Kind::Mesh { inputs } = &meta.kind
                    {
                        self.rebases.track(meta.page, dispatch.id, **inputs);
                    }
                    self.running_order.insert(dispatch.id);
                    return Some(dispatch);
                }
                Take::Expired(expired) => {
                    if let Some(meta) = self.metadata.remove(&expired.id) {
                        self.retry(expired.id, meta, true, tick);
                    }
                }
                Take::Empty | Take::BudgetBlocked => return None,
            }
        }
    }
    /// A saturated transport returns the untouched dispatch to its same queue.
    pub fn requeue(&mut self, dispatch: Dispatch<SceneTask>) {
        let id = dispatch.id;
        if let Some(meta) = self.metadata.get(&id) {
            self.rebases.forget_job(meta.page, id);
        }
        self.running_order.remove(&id);
        if let Some(meta) = self.metadata.get_mut(&id) {
            meta.dispatched = false;
        }
        if let Err(dispatch) = self.scheduler.requeue(dispatch) {
            if let Some(meta) = self.metadata.remove(&id) {
                self.cancellations.retain(|queued| *queued != id);
                self.scheduler.finish::<SceneOutput>(
                    Completion {
                        id,
                        outcome: JobOutcome::Cancelled,
                    },
                    |_| None,
                );
                self.retry(id, meta, false, self.world.tick());
            }
            drop(dispatch);
        }
    }
    pub fn peek_mesh(&mut self) -> Option<&CompletedMesh> {
        while self.ready.front().is_some_and(|mesh| {
            !self.world.matches_inputs(&mesh.inputs) || !self.visible.contains(&mesh.page)
        }) {
            let mesh = self.ready.pop_front().expect("checked front");
            self.rebases.forget(mesh.page);
            self.ready_bytes -= mesh.retained_bytes();
            self.mark_dirty(mesh.page, self.world.tick(), Priority::Interactive, false);
        }
        self.ready.front()
    }
    pub fn take_mesh(&mut self) -> Option<CompletedMesh> {
        self.peek_mesh()?;
        let mesh = self.ready.pop_front()?;
        self.rebases.forget(mesh.page);
        self.ready_bytes -= mesh.retained_bytes();
        Some(mesh)
    }
    /// Takes a current-base or certified base+delta candidate. Tracking remains
    /// live until `ack_composite`; budget backpressure must return this object.
    pub fn take_composite(&mut self) -> Option<CompositeMesh> {
        loop {
            let mesh = self.ready.front()?;
            if !self.visible.contains(&mesh.page)
                || (!self.world.matches_inputs(&mesh.inputs)
                    && self.rebases.certificate(mesh.page, &mesh.inputs).is_none())
            {
                let mesh = self.ready.pop_front().expect("checked front");
                self.ready_bytes -= mesh.retained_bytes();
                self.rebases.forget(mesh.page);
                self.mark_dirty(mesh.page, self.world.tick(), Priority::Interactive, false);
                continue;
            }
            let base = self.ready.pop_front().expect("checked front");
            self.ready_bytes -= base.retained_bytes();
            let (job, changed_owners) = self.rebases.certificate(base.page, &base.inputs).map_or(
                (None, crate::edit_shell::OwnerMask::default()),
                |(job, mask)| (Some(job), mask.clone()),
            );
            return Some(CompositeMesh {
                base,
                changed_owners,
                world_version: self.world.version(),
                job,
            });
        }
    }
    /// Validate immediately before the queue's atomic base+mask+patch upload.
    /// An intervening world mutation requires returning and taking a fresh token.
    pub fn composite_is_current(&self, mesh: &CompositeMesh) -> bool {
        if mesh.world_version != self.world.version() || !self.visible.contains(&mesh.base.page) {
            return false;
        }
        match mesh.job {
            Some(job) => self
                .rebases
                .certificate(mesh.base.page, &mesh.base.inputs)
                .is_some_and(|(expected, owners)| {
                    expected == job && *owners == mesh.changed_owners
                }),
            None => mesh.changed_owners.is_empty() && self.world.matches_inputs(&mesh.base.inputs),
        }
    }
    /// Acknowledges a successfully queued atomic composite, never an old base
    /// alone. The next pending task captures a fresh bounded delta window.
    pub fn ack_composite(&mut self, mesh: &CompositeMesh) -> bool {
        if !self.composite_is_current(mesh) {
            return false;
        }
        if let Some(job) = mesh.job {
            self.rebases.forget_job(mesh.base.page, job);
        }
        true
    }
    #[allow(clippy::result_large_err)]
    pub fn return_composite(&mut self, mesh: CompositeMesh) -> Result<(), CompositeMesh> {
        if self.ready.len() >= self.config.max_ready_meshes
            || mesh.base.retained_bytes()
                > self.config.max_ready_bytes.saturating_sub(self.ready_bytes)
        {
            return Err(mesh);
        }
        self.ready_bytes += mesh.base.retained_bytes();
        self.ready.push_front(mesh.base);
        Ok(())
    }
    // Backpressure returns the same owned buffers; boxing would allocate while
    // rejecting a full quota, and dropping that value would lose upload work.
    #[allow(clippy::result_large_err)]
    pub fn return_mesh(&mut self, mesh: CompletedMesh) -> Result<(), CompletedMesh> {
        if self.ready.len() >= self.config.max_ready_meshes
            || mesh.retained_bytes() > self.config.max_ready_bytes.saturating_sub(self.ready_bytes)
        {
            return Err(mesh);
        }
        self.ready_bytes += mesh.retained_bytes();
        self.ready.push_front(mesh);
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn pump_native(
        &mut self,
        pool: &jobs::NativePool<SceneTask, SceneOutput>,
        tick: Tick,
        budget: &mut WorkBudget,
        max_results: usize,
    ) -> Result<PhaseWork, SceneError> {
        for _ in 0..max_results {
            let Some(result) = pool.try_recv() else {
                break;
            };
            self.complete(result)?;
        }
        let mut work = self.phase(tick, max_results)?;
        for _ in 0..self.config.max_new_jobs_per_phase {
            let Some(dispatch) = self.take_task(tick, budget) else {
                break;
            };
            match pool.try_submit(dispatch) {
                Ok(()) => work.dispatched += 1,
                Err(jobs::SubmitError::Full(dispatch)) => {
                    self.requeue(dispatch);
                    work.backpressure += 1;
                    self.stats.backpressure += 1;
                    break;
                }
                Err(jobs::SubmitError::Closed(dispatch)) => {
                    let id = dispatch.id;
                    self.fail(id, "native worker transport closed".into())?;
                }
            }
        }
        Ok(work)
    }
}
