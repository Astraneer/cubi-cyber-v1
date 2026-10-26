//! Bounded actual-world water ownership, native workers and browser dispatch.
//! Tint reads use the immutable existing cache; missing data retains plain water.
use crate::{
    ClientScene,
    water_source::{
        BuiltWater, WaterJob, WaterSource, WaterSourceError, WaterSourceLimits, WaterSourceStats,
        WaterSourceWork,
    },
};
use core_types::{PageId, Tick, Version};
use jobs::{
    Completion, Dispatch, Finish, JobId, JobKey, JobOutcome, JobSpec, Priority, Scheduler, Take,
};
use render::{terrain_gpu::UploadBudget, water_gpu::WaterGpu};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{AtomicU32, Ordering},
};
const RETAINED_JOB_BYTES: usize = 2 * 1024 * 1024;
const JOB_KIND: u16 = 0x5754;
const EPOCH_LIMIT: u32 = (1 << 21) - 1;
static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub source: WaterSourceLimits,
    pub journal_rows: usize,
    pub dispatches_per_phase: usize,
    pub publishes_per_flush: usize,
    pub scheduler_jobs: usize,
    pub workers: usize,
    pub queue_capacity: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            source: WaterSourceLimits::default(),
            journal_rows: 512,
            dispatches_per_phase: 2,
            publishes_per_flush: 2,
            scheduler_jobs: 4,
            workers: 2,
            queue_capacity: 2,
        }
    }
}
#[derive(Debug)]
pub enum Error {
    InvalidConfig,
    Source(WaterSourceError),
    Transport(String),
    Closed,
    IdExhausted,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<WaterSourceError> for Error {
    fn from(value: WaterSourceError) -> Self {
        Self::Source(value)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub source: WaterSourceStats,
    pub work: WaterSourceWork,
    pub scheduler: jobs::JobStats,
    pub pending_completions: usize,
    pub pending_dispatch: bool,
    pub missing_tints: bool,
    pub missing_transform: bool,
    pub tint_samples: u64,
    pub ready: bool,
    pub out_of_scope_completions: u64,
    pub bytes_encoded: u64,
    pub wire_halo_reads: u64,
}
struct Tint {
    source: [u16; 256],
    rgba: Box<[u32; 256]>,
}
pub struct ViewWater {
    config: Config,
    owner: WaterSource,
    scheduler: Scheduler<WaterJob>,
    contexts: BTreeMap<JobId, u64>,
    pending_job: Option<WaterJob>,
    pending_results: VecDeque<BuiltWater>,
    tints: BTreeMap<PageId, Tint>,
    interest: Vec<PageId>,
    interest_key: Option<(PageId, u8, u8)>,
    settled_world: Option<(Version, u64)>,
    epoch: u32,
    closed: bool,
    stats: Stats,
    #[cfg(not(target_arch = "wasm32"))]
    native: jobs::NativePool<WaterJob, BuiltWater>,
}
impl ViewWater {
    pub fn prepare(scene: &mut ClientScene, gpu: &WaterGpu, config: Config) -> Result<Self, Error> {
        let (objects, quads) = gpu.capacity();
        if config.source.max_running > config.scheduler_jobs
            || config.journal_rows == 0
            || config.dispatches_per_phase == 0
            || config.dispatches_per_phase > 8
            || config.publishes_per_flush == 0
            || config.scheduler_jobs == 0
            || config.scheduler_jobs > 16
            || config.workers == 0
            || config.workers > 4
            || config.queue_capacity == 0
            || config.queue_capacity > 16
            || config.source.max_pages > objects as usize
            || config.source.max_total_quads > quads as usize
        {
            return Err(Error::InvalidConfig);
        }
        let scheduler = Scheduler::new(jobs::Limits {
            max_jobs: config.scheduler_jobs,
            max_bytes: config.scheduler_jobs * RETAINED_JOB_BYTES,
            max_age_ticks: config.source.max_age_ticks,
            reserved_interactive_jobs: 0,
            reserved_interactive_bytes: 0,
        })
        .map_err(|_| Error::InvalidConfig)?;
        let epoch = NEXT_EPOCH
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < EPOCH_LIMIT).then_some(n + 1)
            })
            .map_err(|_| Error::IdExhausted)?;
        #[cfg(not(target_arch = "wasm32"))]
        let native = crate::water_source::native_pool(config.workers, config.queue_capacity)
            .map_err(|e| Error::Transport(format!("water workers: {e:?}")))?;
        let owner = WaterSource::attach(scene.derived_world_mut(), config.source)?;
        Ok(Self {
            config,
            owner,
            scheduler,
            contexts: BTreeMap::new(),
            pending_job: None,
            pending_results: VecDeque::with_capacity(config.source.max_running),
            tints: BTreeMap::new(),
            interest: Vec::with_capacity(config.source.max_pages),
            interest_key: None,
            settled_world: None,
            epoch,
            closed: false,
            stats: Stats::default(),
            #[cfg(not(target_arch = "wasm32"))]
            native,
        })
    }
    fn wire_id(&self, id: JobId) -> Result<JobId, Error> {
        if id.0 > u64::from(u32::MAX) {
            return Err(Error::IdExhausted);
        }
        Ok(JobId((u64::from(self.epoch) << 32) | id.0))
    }
    fn local_id(&self, id: JobId) -> Option<JobId> {
        (id.0 >> 32 == u64::from(self.epoch)).then_some(JobId(id.0 & u64::from(u32::MAX)))
    }
    fn enqueue(&mut self, job: WaterJob, tick: Tick) {
        let water_id = job.id;
        let spec = JobSpec::derived(
            JobKey {
                page: job.task.page,
                kind: JOB_KIND,
            },
            Priority::Interactive,
            jobs::WorkCost {
                cells: 9928,
                quads: self.config.source.max_quads_per_page as u64,
                bytes: RETAINED_JOB_BYTES as u64,
                ..jobs::WorkCost::ZERO
            },
            RETAINED_JOB_BYTES,
            tick,
        );
        match self.scheduler.enqueue(spec, job) {
            Ok(id) => {
                self.contexts.insert(id, water_id);
            }
            Err(rejected) => self.pending_job = Some(rejected.payload),
        }
    }
    fn tint(&mut self, scene: &ClientScene, page: PageId) -> Option<Box<[u32; 256]>> {
        let source = scene.cached_biome_tints(page)?;
        self.stats.tint_samples += 256;
        if self
            .tints
            .get(&page)
            .is_none_or(|tint| tint.source != *source)
        {
            let rgba = Box::new(std::array::from_fn(|i| {
                let c = source[i];
                let r = u32::from((c >> 11) & 31) * 255 / 31;
                let g = u32::from((c >> 5) & 63) * 255 / 63;
                let b = u32::from(c & 31) * 255 / 31;
                r | (g << 8) | (b << 16) | 0xff000000
            }));
            self.tints.insert(
                page,
                Tint {
                    source: *source,
                    rgba,
                },
            );
        }
        self.tints.get(&page).map(|tint| tint.rgba.clone())
    }
    fn configure_interest(&mut self, scene: &ClientScene, tick: Tick) -> Result<(), Error> {
        let config = scene.config();
        let key = (
            config.center.page(),
            config.horizontal_radius,
            config.vertical_radius,
        );
        if self.interest_key != Some(key) {
            self.interest.clear();
            for page in scene.visible_pages() {
                if self.interest.len() == self.config.source.max_pages {
                    return Err(Error::InvalidConfig);
                }
                self.interest.push(page);
            }
            self.interest.sort_unstable();
            self.owner
                .set_interest(scene.world(), &self.interest, tick)?;
            self.interest_key = Some(key);
            self.stats.ready = false;
            self.settled_world = None;
        }
        Ok(())
    }
    fn idle(&self, scene: &ClientScene) -> bool {
        self.stats.ready
            && self.settled_world == Some((scene.world().version(), scene.world().journal_next()))
    }
    /// Ingest only the configured journal budget and capture at most bounded jobs.
    /// This method never executes a mesher on its owner thread.
    pub fn phase(&mut self, scene: &mut ClientScene, tick: Tick) -> Result<Stats, Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.stats.work = WaterSourceWork::default();
        self.stats.tint_samples = 0;
        self.stats.missing_tints = false;
        self.stats.missing_transform = false;
        self.configure_interest(scene, tick)?;
        if self.idle(scene) {
            return Ok(self.stats);
        }
        self.owner
            .ingest(scene.derived_world_mut(), self.config.journal_rows)?;
        for _ in 0..self.config.source.max_running {
            let Some(result) = self.pending_results.pop_front() else {
                break;
            };
            match self.owner.complete(scene.world(), result) {
                Ok(_) => {}
                Err(blocked) => {
                    self.pending_results.push_front(blocked.built);
                    break;
                }
            }
        }
        self.tints
            .retain(|page, _| scene.world().page(*page).is_some());
        if let Some(job) = self.pending_job.take() {
            self.enqueue(job, tick);
        }
        for _ in 0..self.config.dispatches_per_phase {
            if self.pending_job.is_some() {
                break;
            }
            let Some(page) = self.owner.next_job_page(scene.world()) else {
                break;
            };
            let Some(tint) = self.tint(scene, page) else {
                self.stats.missing_tints = true;
                break;
            };
            let Some(job) = self.owner.take_job(scene.world(), |requested| {
                debug_assert_eq!(page, requested);
                tint.clone()
            })?
            else {
                break;
            };
            self.enqueue(job, tick);
        }
        self.update_stats(scene);
        Ok(self.stats)
    }
    fn update_stats(&mut self, scene: &ClientScene) {
        self.stats.source = self.owner.stats(scene.world());
        let work = self.owner.take_work();
        macro_rules! add {($($field:ident),*$(,)?)=>{$(self.stats.work.$field=self.stats.work.$field.saturating_add(work.$field);)*};}
        add!(
            journal_entries,
            mask_words,
            dirty_pages,
            task_probes,
            jobs_submitted,
            jobs_completed,
            jobs_stale,
            jobs_failed,
            worker_cells,
            worker_halo_samples,
            worker_quads,
            gpu_upload_bytes,
            pages_published,
            pages_retired,
            models_updated,
            cpu_frame_vertices
        );
        self.stats.work.backpressure |= work.backpressure;
        self.stats.work.changed |= work.changed;
        self.stats.scheduler = self.scheduler.stats();
        self.stats.pending_completions = self.pending_results.len();
        self.stats.pending_dispatch = self.pending_job.is_some();
        self.stats.ready = !self.closed
            && self.stats.source.settled
            && self.pending_job.is_none()
            && self.pending_results.is_empty()
            && self.contexts.is_empty()
            && !self.stats.missing_tints;
        self.settled_world = self
            .stats
            .ready
            .then_some((scene.world().version(), scene.world().journal_next()));
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    pub fn take_task(
        &mut self,
        scene: &ClientScene,
        tick: Tick,
    ) -> Result<Option<Dispatch<WaterJob>>, Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        let limit = self.config.dispatches_per_phase as u64;
        let mut budget = jobs::WorkBudget::unrestricted(jobs::WorkCost {
            cells: 9928 * limit,
            quads: self.config.source.max_quads_per_page as u64 * limit,
            bytes: RETAINED_JOB_BYTES as u64 * limit,
            ..jobs::WorkCost::ZERO
        });
        loop {
            match self.scheduler.take_next(tick, &mut budget, |_| None) {
                Take::Ready(mut dispatch) => {
                    dispatch.id = self.wire_id(dispatch.id)?;
                    return Ok(Some(dispatch));
                }
                Take::Expired(expired) => {
                    self.contexts.remove(&expired.id);
                    self.owner.fail_job(scene.world(), expired.payload.id);
                }
                Take::BudgetBlocked | Take::Empty => return Ok(None),
            }
        }
    }
    pub fn requeue(
        &mut self,
        scene: &ClientScene,
        mut dispatch: Dispatch<WaterJob>,
    ) -> Result<(), Error> {
        let Some(local) = self.local_id(dispatch.id) else {
            self.stats.out_of_scope_completions += 1;
            return Ok(());
        };
        dispatch.id = local;
        if let Err(dispatch) = self.scheduler.requeue(dispatch) {
            self.complete_local(
                scene,
                Completion {
                    id: dispatch.id,
                    outcome: JobOutcome::Cancelled,
                },
            )?;
        }
        Ok(())
    }
    fn complete_local(
        &mut self,
        scene: &ClientScene,
        completion: Completion<BuiltWater>,
    ) -> Result<(), Error> {
        let expected = self.contexts.remove(&completion.id);
        match self.scheduler.finish(completion, |_| None) {
            Finish::Published(built) => {
                if expected != Some(built.id) {
                    if let Some(id) = expected {
                        self.owner.fail_job(scene.world(), id);
                    }
                    return Err(Error::Transport(
                        "water result belongs to a different captured job".into(),
                    ));
                }
                match self.owner.complete(scene.world(), built) {
                    Ok(_) => {}
                    Err(blocked) => {
                        if self.pending_results.len() >= self.config.source.max_running {
                            return Err(Error::Transport("water result capacity invariant".into()));
                        }
                        self.pending_results.push_back(blocked.built);
                    }
                }
            }
            _ => {
                if let Some(id) = expected {
                    self.owner.fail_job(scene.world(), id);
                }
            }
        }
        Ok(())
    }
    pub fn complete(
        &mut self,
        scene: &ClientScene,
        mut completion: Completion<BuiltWater>,
    ) -> Result<(), Error> {
        if self.closed {
            self.stats.out_of_scope_completions += 1;
            return Ok(());
        }
        let Some(local) = self.local_id(completion.id) else {
            self.stats.out_of_scope_completions += 1;
            return Ok(());
        };
        completion.id = local;
        self.complete_local(scene, completion)
    }
    pub fn complete_bytes(
        &mut self,
        scene: &ClientScene,
        id: JobId,
        bytes: &[u8],
    ) -> Result<(), Error> {
        match BuiltWater::decode(bytes) {
            Ok(built) => self.complete(
                scene,
                Completion {
                    id,
                    outcome: JobOutcome::Success(built),
                },
            ),
            Err(error) => {
                self.fail_task(scene, id)?;
                Err(Error::Transport(format!("water response: {error}")))
            }
        }
    }
    pub fn fail_task(&mut self, scene: &ClientScene, id: JobId) -> Result<(), Error> {
        self.complete(
            scene,
            Completion {
                id,
                outcome: JobOutcome::Failed(jobs::JobFailure::WorkerDisconnected),
            },
        )
    }
    /// Encoding is foreground work: exactly one 5832-cell classified halo per task.
    pub fn encode_task(&mut self, dispatch: &Dispatch<WaterJob>) -> Result<Vec<u8>, Error> {
        if self.local_id(dispatch.id).is_none() {
            return Err(Error::Closed);
        }
        let bytes = dispatch
            .payload
            .encode()
            .map_err(|e| Error::Transport(e.to_string()))?;
        self.stats.bytes_encoded += bytes.len() as u64;
        self.stats.wire_halo_reads += 5832;
        Ok(bytes)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn pump_native(&mut self, scene: &mut ClientScene, tick: Tick) -> Result<Stats, Error> {
        for _ in 0..self.config.source.max_running {
            let Some(completion) = self.native.try_recv() else {
                break;
            };
            self.complete(scene, completion)?;
        }
        self.phase(scene, tick)?;
        if self.idle(scene) {
            return Ok(self.stats);
        }
        for _ in 0..self.config.dispatches_per_phase {
            let Some(dispatch) = self.take_task(scene, tick)? else {
                break;
            };
            match self.native.try_submit(dispatch) {
                Ok(()) => {}
                Err(jobs::SubmitError::Full(dispatch)) => {
                    self.requeue(scene, dispatch)?;
                    break;
                }
                Err(jobs::SubmitError::Closed(dispatch)) => {
                    self.fail_task(scene, dispatch.id)?;
                    return Err(Error::Transport("water worker pool closed".into()));
                }
            }
        }
        self.update_stats(scene);
        Ok(self.stats)
    }
    pub fn request_model_update(&mut self) {
        self.owner.request_model_update();
        self.stats.ready = false;
    }
    pub fn invalidate_tints(&mut self, scene: &ClientScene) -> Result<(), Error> {
        self.tints.clear();
        self.owner.invalidate_tints(scene.world())?;
        self.stats.ready = false;
        Ok(())
    }
    pub fn flush(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        scene: &ClientScene,
        budget: &mut UploadBudget,
        mut model: impl FnMut(PageId) -> Option<[[f32; 4]; 4]>,
    ) -> Result<Stats, Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.configure_interest(scene, scene.world().tick())?;
        if self.idle(scene) {
            return Ok(self.stats);
        }
        match self.owner.publish_ready_with_model(
            queue,
            gpu,
            scene.world(),
            budget,
            self.config.publishes_per_flush,
            &mut model,
        ) {
            Ok(_) => {}
            Err(WaterSourceError::MissingTransform) => {
                self.stats.missing_transform = true;
                self.stats.work.backpressure = true;
                self.update_stats(scene);
                return Ok(self.stats);
            }
            Err(e) => return Err(e.into()),
        }
        match self.owner.refresh_models(queue, gpu, budget, model) {
            Ok(_) => {}
            Err(WaterSourceError::MissingTransform) => {
                self.stats.missing_transform = true;
                self.stats.work.backpressure = true;
            }
            Err(e) => return Err(e.into()),
        }
        self.update_stats(scene);
        Ok(self.stats)
    }
    pub fn retire_all(
        &mut self,
        queue: &wgpu::Queue,
        gpu: &mut WaterGpu,
        budget: &mut UploadBudget,
    ) -> Result<bool, Error> {
        if self.owner.retire_all(queue, gpu, budget)?.is_none() {
            return Ok(false);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.native.cancel_all();
        self.closed = true;
        for &id in self.contexts.keys() {
            self.scheduler.cancel(id);
        }
        self.contexts.clear();
        self.pending_job = None;
        self.pending_results.clear();
        self.tints.clear();
        self.stats.ready = false;
        Ok(true)
    }
    pub fn detach(self, scene: &mut ClientScene) {
        #[cfg(not(target_arch = "wasm32"))]
        self.native.cancel_all();
        self.owner.detach(scene.derived_world_mut());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use core_types::{BlockPos, Cell, PAGE_VOLUME, SpaceId, Version};
    use net::{PageSnapshot, PageUpdate};
    use render::{terrain_gpu::translation, water_gpu::WaterConfig};
    use world::Transaction;

    fn point(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos {
            space: SpaceId(0),
            x,
            y,
            z,
        }
    }
    fn fixture() -> ClientScene {
        let mut scene = ClientScene::new(crate::SceneConfig {
            center: point(8, 8, 8),
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 27,
            authoritative_remote: true,
            ..crate::SceneConfig::forest()
        })
        .unwrap();
        let mut cells = Box::new([Cell::AIR; PAGE_VOLUME]);
        cells[point(8, 4, 8).local_index().get() as usize] = Cell::from_parts(rules::ids::WATER, 0);
        scene
            .import_authoritative_page(
                &PageUpdate::Snapshot(PageSnapshot {
                    page: point(8, 8, 8).page(),
                    version: 1,
                    source_version: Version(1),
                    applied_action: Default::default(),
                    cells,
                }),
                Tick(1),
            )
            .unwrap();
        assert!(scene.cached_biome_tints(point(8, 8, 8).page()).is_some());
        scene
    }
    fn config() -> Config {
        Config {
            source: WaterSourceLimits {
                max_pages: 16,
                max_ready: 4,
                max_total_quads: 1024,
                ..WaterSourceLimits::default()
            },
            ..Config::default()
        }
    }
    fn water(gpu: &render::Gpu) -> WaterGpu {
        let create = |format, samples| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ViewWater public-world GPU oracle input"),
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let color = create(wgpu::TextureFormat::Rgba16Float, 4).create_view(&Default::default());
        let depth = create(wgpu::TextureFormat::Depth32Float, 4).create_view(&Default::default());
        let sky = create(wgpu::TextureFormat::Rgba32Float, 1).create_view(&Default::default());
        WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            WaterConfig {
                max_objects: 16,
                max_quads: 1024,
                reflection_steps: 0,
                wave_amplitude: 0.,
                ..WaterConfig::default()
            },
            64,
            64,
            render::water_gpu::WaterInputs {
                color_msaa: &color,
                depth_msaa: &depth,
                sky: &sky,
                transmittance: &sky,
            },
            None,
        )
        .unwrap()
    }
    fn publish(
        owner: &mut ViewWater,
        gpu: &render::Gpu,
        water: &mut WaterGpu,
        scene: &ClientScene,
        bytes: u64,
    ) -> Stats {
        owner
            .flush(
                &gpu.queue,
                water,
                scene,
                &mut UploadBudget::new(bytes),
                |_| Some(translation([0.; 3])),
            )
            .unwrap()
    }
    #[test]
    fn browser_wire_requeue_stale_session_budget_static_and_teardown_are_bounded() {
        pollster::block_on(async {
            let gpu = render::Gpu::request(&render::instance(), None, true)
                .await
                .unwrap();
            let mut water = water(&gpu);
            let mut scene = fixture();
            let original_pages = scene.world().loaded_pages();
            let mut owner = ViewWater::prepare(&mut scene, &water, config()).unwrap();
            owner.phase(&mut scene, Tick(1)).unwrap();
            let dispatch = owner.take_task(&scene, Tick(1)).unwrap().unwrap();
            let first_id = dispatch.id;
            assert!(first_id.0 < 1u64 << 53);
            owner.requeue(&scene, dispatch).unwrap();
            let dispatch = owner.take_task(&scene, Tick(1)).unwrap().unwrap();
            assert_eq!(dispatch.id, first_id);
            let request = owner.encode_task(&dispatch).unwrap();
            assert_eq!(request.len(), 7104);
            assert_eq!(&request[..4], b"CWJ1");
            let old_response = WaterJob::decode(&request)
                .unwrap()
                .execute(|| false)
                .encode()
                .unwrap();
            assert_eq!(&old_response[..4], b"CWR1");
            let mut tx = Transaction::new(Tick(2), SpaceId(0));
            tx.set_cell(point(8, 4, 8), Cell::from_parts(rules::ids::WATER, 7));
            scene.derived_world_mut().commit(tx).unwrap();
            owner
                .complete_bytes(&scene, first_id, &old_response)
                .unwrap();
            owner.phase(&mut scene, Tick(2)).unwrap();
            let stale_work = owner.stats().work.jobs_stale;
            assert_eq!(stale_work, 1);
            assert!(!owner.stats().ready);
            let current = owner.take_task(&scene, Tick(2)).unwrap().unwrap();
            let response = WaterJob::decode(&owner.encode_task(&current).unwrap())
                .unwrap()
                .execute(|| false)
                .encode()
                .unwrap();

            // Another live view has the same local job number but a different session.
            let mut other = ViewWater::prepare(&mut scene, &water, config()).unwrap();
            other.phase(&mut scene, Tick(2)).unwrap();
            let other_job = other.take_task(&scene, Tick(2)).unwrap().unwrap();
            assert_ne!(other_job.id, first_id);
            other
                .complete_bytes(&scene, first_id, &old_response)
                .unwrap();
            assert_eq!(other.stats().out_of_scope_completions, 1);
            assert_eq!(other.contexts.len(), 1);
            other.fail_task(&scene, other_job.id).unwrap();
            other.detach(&mut scene);

            owner.complete_bytes(&scene, current.id, &response).unwrap();
            let held = publish(&mut owner, &gpu, &mut water, &scene, 0);
            assert!(!held.ready);
            assert_eq!(held.source.ready, 1);
            assert_eq!(water.active_quad_count(), 0);
            let ready = publish(&mut owner, &gpu, &mut water, &scene, 100000);
            assert!(ready.ready);
            assert_eq!(water.active_quad_count(), 6);
            assert_eq!(ready.work.cpu_frame_vertices, 0);
            assert_eq!(ready.wire_halo_reads, 2 * 5832);
            assert_eq!(ready.bytes_encoded, 2 * 7104);
            for _ in 0..1000 {
                owner.phase(&mut scene, Tick(3)).unwrap();
                let idle = publish(&mut owner, &gpu, &mut water, &scene, 100000);
                assert!(idle.ready);
                assert_eq!(idle.work, WaterSourceWork::default());
                assert_eq!(idle.tint_samples, 0);
            }
            assert_eq!(scene.world().loaded_pages(), original_pages);
            assert_eq!(scene.stats().generation_published, 0);
            assert!(
                !owner
                    .retire_all(&gpu.queue, &mut water, &mut UploadBudget::new(0))
                    .unwrap()
            );
            assert_eq!(water.active_quad_count(), 6);
            assert!(
                owner
                    .retire_all(&gpu.queue, &mut water, &mut UploadBudget::new(100000))
                    .unwrap()
            );
            assert_eq!(water.active_quad_count(), 0);
            owner.complete_bytes(&scene, current.id, &response).unwrap();
            assert_eq!(owner.stats().out_of_scope_completions, 1);
            owner.detach(&mut scene);
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
        });
    }
    #[test]
    fn native_workers_publish_public_cached_tints_without_owner_meshing() {
        pollster::block_on(async {
            let gpu = render::Gpu::request(&render::instance(), None, true)
                .await
                .unwrap();
            let mut water = water(&gpu);
            let mut scene = fixture();
            let mut owner = ViewWater::prepare(&mut scene, &water, config()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut submitted = 0;
            let mut worker_cells = 0;
            let mut uploads = 0;
            loop {
                owner.pump_native(&mut scene, Tick(1)).unwrap();
                let stats = publish(&mut owner, &gpu, &mut water, &scene, 100000);
                submitted += stats.work.jobs_submitted;
                worker_cells += stats.work.worker_cells;
                uploads += stats.work.gpu_upload_bytes;
                assert_eq!(stats.work.cpu_frame_vertices, 0);
                if stats.ready {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "water worker deadline: {stats:?}"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(submitted, 1);
            assert_eq!(worker_cells, 4096);
            assert!(uploads >= 6 * 16);
            assert_eq!(water.active_quad_count(), 6);
            assert_eq!(owner.stats().bytes_encoded, 0);
            assert_eq!(owner.stats().wire_halo_reads, 0);
            assert_eq!(scene.stats().generation_published, 0);
            assert!(
                owner
                    .retire_all(&gpu.queue, &mut water, &mut UploadBudget::new(100000))
                    .unwrap()
            );
            owner.detach(&mut scene);
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
        });
    }
}
