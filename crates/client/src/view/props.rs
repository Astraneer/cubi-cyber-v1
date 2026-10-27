//! Loading-prepared static block catalog and bounded public model source.
//! The owner publishes 64-byte records; model vertices are immutable resources.
use super::{ViewEnvironment, ViewLighting};
use crate::{
    ClientScene,
    props_source::{
        PropsBuilt, PropsContext, PropsLimits, PropsPublication, PropsSource, PropsTask, PropsWork,
        VisibleRegion,
    },
};
use core_types::{PageId, Tick};
use jobs::{CancellationToken, Completion, Dispatch, JobId, JobOutcome, JobSpec, Priority};
use render::{
    block_models::{BlockConfig, BlockModelsGpu},
    terrain_gpu::{TerrainGpu, UploadBudget},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct PropsStats {
    pub startup_vertices: u64,
    pub startup_upload_bytes: u64,
    pub pipeline_compilations: u32,
    pub instances: usize,
    pub worker_cells: u64,
    pub worker_mask_words: u64,
    pub owner_light_reads: u64,
    pub upload_bytes: u64,
    pub cpu_vertices: u64,
    pub ready: bool,
    pub pending_jobs: usize,
    pub pending_records: usize,
    pub oldest_age_ticks: u64,
    pub overdue: bool,
}
struct Context {
    source: PropsContext,
    cancellation: CancellationToken,
    born: Tick,
}
pub struct ViewProps {
    pub gpu: BlockModelsGpu,
    pub source: PropsSource,
    pub stats: PropsStats,
    limits: PropsLimits,
    region: Option<VisibleRegion>,
    bindings: Vec<(PageId, Option<u64>)>,
    contexts: BTreeMap<JobId, Context>,
    pending: VecDeque<Box<PropsBuilt>>,
    retry: Option<Dispatch<PropsTask>>,
    alpha: f64,
    temporal_identities: Option<Vec<Option<PropsPublication>>>,
    quarantined: bool,
    #[cfg(not(target_arch = "wasm32"))]
    native: jobs::NativePool<PropsTask, Box<PropsBuilt>>,
}
impl ViewProps {
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        scene: &mut ClientScene,
        lighting: Option<&ViewLighting>,
        environment: Option<&ViewEnvironment>,
        limits: PropsLimits,
    ) -> Result<Self, String> {
        let config = BlockConfig {
            max_instances: u32::try_from(limits.max_instances)
                .map_err(|_| "model instance capacity")?,
            ..Default::default()
        };
        let shadows = lighting.map(|lighting| lighting.owner.shadows());
        let mut gpu = if let Some(environment) = environment {
            BlockModelsGpu::new_with_lighting(
                device,
                queue,
                terrain.geometry(),
                config,
                shadows,
                environment.gpu.atmosphere(),
                environment.gpu.gi(),
            )
        } else {
            BlockModelsGpu::new(device, queue, terrain.geometry(), config, shadows)
        }
        .map_err(|error| error.to_string())?;
        let mut loading = UploadBudget::new(16 * 1024 * 1024);
        gpu.reserve_slots(queue, config.max_instances, &mut loading)
            .map_err(|error| error.to_string())?;
        let work = gpu.take_work();
        let source = PropsSource::attach(scene.derived_world_mut(), limits)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            gpu,
            source,
            stats: PropsStats {
                startup_vertices: u64::from(work.startup_vertices),
                startup_upload_bytes: work.upload_bytes,
                pipeline_compilations: work.pipeline_compilations,
                ..PropsStats::default()
            },
            limits,
            region: None,
            bindings: Vec::with_capacity(limits.max_pages),
            contexts: BTreeMap::new(),
            pending: VecDeque::with_capacity(limits.max_running),
            retry: None,
            alpha: 0.,
            temporal_identities: None,
            quarantined: false,
            #[cfg(not(target_arch = "wasm32"))]
            native: jobs::NativePool::new(
                1,
                limits.max_running,
                |task: PropsTask, cancel: &CancellationToken| {
                    task.execute(|| cancel.is_cancelled())
                        .ok_or_else(|| "cancelled model snapshot".to_owned())
                },
            )
            .map_err(|error| format!("model worker: {error:?}"))?,
        })
    }
    /// Loading only. Default profiles allocate no temporal identity bank.
    pub fn prepare_temporal_raster(&mut self, device: &wgpu::Device) -> Result<u32, String> {
        if !self.gpu.temporal_raster_preparable() {
            return Err("prop temporal preparation requires loading resources".into());
        }
        self.source
            .prepare_temporal_publications()
            .map_err(|error| error.to_string())?;
        let work = self
            .gpu
            .prepare_temporal_raster(device)
            .map_err(|error| error.to_string())?;
        if self.temporal_identities.is_none() {
            self.temporal_identities = Some(vec![None; self.gpu.instance_capacity() as usize]);
        }
        self.stats.pipeline_compilations += work.pipeline_compilations;
        Ok(work.pipeline_compilations)
    }
    /// Paired with gpu.instance_records(): CPU-only changes cannot advance it.
    pub fn published_temporal_identities(&self) -> Option<&[Option<PropsPublication>]> {
        self.temporal_identities.as_deref()
    }
    fn configure_interest(&mut self, scene: &ClientScene, tick: Tick) -> Result<(), String> {
        let config = scene.config();
        let center = config.center.page();
        let h = i32::from(config.horizontal_radius);
        let v = i32::from(config.vertical_radius);
        let region = VisibleRegion {
            min: PageId {
                space: center.space,
                x: center.x.checked_sub(h).ok_or("model region bounds")?,
                y: center.y.checked_sub(v).ok_or("model region bounds")?,
                z: center.z.checked_sub(h).ok_or("model region bounds")?,
            },
            max: PageId {
                space: center.space,
                x: center.x.checked_add(h).ok_or("model region bounds")?,
                y: center.y.checked_add(v).ok_or("model region bounds")?,
                z: center.z.checked_add(h).ok_or("model region bounds")?,
            },
        };
        if self.region == Some(region) {
            return Ok(());
        }
        self.source
            .set_visible_region(Some(region), tick)
            .map_err(|error| error.to_string())?;
        self.bindings.clear();
        for page in scene.visible_pages() {
            if self.bindings.len() == self.limits.max_pages {
                return Err("visible model page capacity".into());
            }
            self.bindings.push((page, None));
            self.source
                .watch_page(page, tick)
                .map_err(|error| error.to_string())?;
        }
        self.region = Some(region);
        Ok(())
    }
    /// Journal traversal, certificate revocation, public animation and result
    /// installation continue while the GPU submission slot is occupied.
    pub fn advance_cpu(
        &mut self,
        scene: &mut ClientScene,
        tick: Tick,
    ) -> Result<PropsWork, String> {
        match self.advance_cpu_inner(scene, tick) {
            Ok(work) => Ok(work),
            Err(error) => {
                self.quarantine_source()
                    .map_err(|gate| format!("{error}; model quarantine: {gate}"))?;
                Err(error)
            }
        }
    }
    fn advance_cpu_inner(
        &mut self,
        scene: &mut ClientScene,
        tick: Tick,
    ) -> Result<PropsWork, String> {
        self.configure_interest(scene, tick)?;
        for (page, previous) in &mut self.bindings {
            let current = scene
                .authoritative_light_version(*page)
                .map(|stamp| stamp.cell_view);
            if current != *previous {
                self.source.mark_light_binding(*page, tick);
                *previous = current;
            }
            if self.source.public_frame(*page).is_some_and(|frame| {
                scene
                    .authoritative_version(*page)
                    .is_none_or(|stamp| stamp.view != frame.cell_view)
            }) {
                self.source
                    .revoke_public_frame(*page, tick)
                    .map_err(|error| error.to_string())?;
            }
        }
        for context in self.contexts.values() {
            if !self
                .region
                .is_some_and(|region| region.contains(context.source.page))
                || tick.0.saturating_sub(context.born.0) > self.limits.max_age_ticks
            {
                context.cancellation.cancel();
            }
        }
        {
            let (world, tints) = scene.derived_world_and_biome_tints();
            self.source
                .ingest(world, 2048, |position| {
                    tint(tints.get(&position.page()).map(|tints| {
                        tints[usize::from(position.local_index().xyz().0)
                            + 16 * usize::from(position.local_index().xyz().2)]
                    }))
                })
                .map_err(|error| error.to_string())?;
        }
        for _ in 0..self.limits.max_running {
            let Some(built) = self.pending.pop_front() else {
                break;
            };
            if let Err(built) = self.source.complete(scene.world(), built) {
                self.pending.push_front(built);
                break;
            }
        }
        self.source
            .pump_ready(scene.world(), 256)
            .map_err(|error| error.to_string())?;
        self.source.pump_public(tick, self.alpha, 256);
        self.source.animate_public(tick, self.alpha, 256);
        #[cfg(not(target_arch = "wasm32"))]
        self.pump_native(scene, tick)?;
        let work = self.source.take_work();
        self.record(scene, tick, work)?;
        Ok(work)
    }
    pub fn set_visual_alpha(&mut self, alpha: f64) {
        self.alpha = if alpha.is_finite() {
            alpha.clamp(0., 1.)
        } else {
            0.
        };
    }
    pub fn take_task(&mut self, scene: &ClientScene, tick: Tick) -> Option<Dispatch<PropsTask>> {
        if let Some(dispatch) = self.retry.take() {
            return Some(dispatch);
        }
        let task = self.source.take_job(scene.world(), |page| {
            Box::new(std::array::from_fn(|index| {
                tint(scene.biome_tint(page, (index % 16) as u8, (index / 16) as u8))
            }))
        })?;
        let id = JobId(task.id);
        let cancellation = CancellationToken::new();
        let spec = JobSpec::derived(
            task.job_key(),
            Priority::Background,
            task.cost(),
            task.retained_bytes(),
            tick,
        );
        self.contexts.insert(
            id,
            Context {
                source: task.context(),
                cancellation: cancellation.clone(),
                born: tick,
            },
        );
        Some(Dispatch {
            id,
            spec,
            payload: task,
            cancellation,
        })
    }
    pub fn requeue_task(&mut self, dispatch: Dispatch<PropsTask>) -> Result<(), String> {
        if self.retry.is_some() {
            return Err("model retry slot already occupied".into());
        }
        self.retry = Some(dispatch);
        Ok(())
    }
    pub fn complete(
        &mut self,
        scene: &ClientScene,
        completion: Completion<Box<PropsBuilt>>,
    ) -> Result<(), String> {
        let Some(context) = self.contexts.remove(&completion.id) else {
            return Ok(());
        };
        if let JobOutcome::Success(built) = completion.outcome
            && built.id == context.source.id
            && built.page == context.source.page
            && built.source == context.source.source
            && !context.cancellation.is_cancelled()
        {
            if let Err(built) = self.source.complete(scene.world(), built) {
                if self.pending.len() == self.limits.max_running {
                    return Err("model result capacity".into());
                }
                self.pending.push_back(built);
            }
            return Ok(());
        }
        self.source.fail_job(scene.world(), context.source.id);
        Ok(())
    }
    pub fn complete_bytes(
        &mut self,
        scene: &ClientScene,
        id: JobId,
        bytes: &[u8],
    ) -> Result<(), String> {
        match PropsBuilt::decode(bytes) {
            Ok(built) => self.complete(
                scene,
                Completion {
                    id,
                    outcome: JobOutcome::Success(built),
                },
            ),
            Err(error) => {
                self.fail(scene, id)?;
                Err(format!("model worker result: {error:?}"))
            }
        }
    }
    pub fn fail(&mut self, scene: &ClientScene, id: JobId) -> Result<(), String> {
        self.complete(
            scene,
            Completion {
                id,
                outcome: JobOutcome::Cancelled,
            },
        )
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn pump_native(&mut self, scene: &ClientScene, tick: Tick) -> Result<(), String> {
        for _ in 0..self.limits.max_running {
            let Some(completion) = self.native.try_recv() else {
                break;
            };
            self.complete(scene, completion)?;
        }
        for _ in 0..self.limits.max_running {
            let Some(dispatch) = self.take_task(scene, tick) else {
                break;
            };
            match self.native.try_submit(dispatch) {
                Ok(()) => {}
                Err(jobs::SubmitError::Full(dispatch)) => {
                    self.requeue_task(dispatch)?;
                    break;
                }
                Err(jobs::SubmitError::Closed(dispatch)) => {
                    self.fail(scene, dispatch.id)?;
                    return Err("model worker transport closed".into());
                }
            }
        }
        Ok(())
    }
    pub fn upload_ready(
        &mut self,
        scene: &ClientScene,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
        tick: Tick,
    ) -> Result<PropsWork, String> {
        match self.upload_ready_inner(scene, queue, budget, tick) {
            Ok(work) => Ok(work),
            Err(error) => {
                self.quarantine_source()
                    .map_err(|gate| format!("{error}; model quarantine: {gate}"))?;
                Err(error)
            }
        }
    }
    fn upload_ready_inner(
        &mut self,
        scene: &ClientScene,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
        tick: Tick,
    ) -> Result<PropsWork, String> {
        let light_at = |position: core_types::BlockPos| {
            if scene.config().authoritative_remote {
                scene.read_authoritative_light(position)
            } else {
                scene
                    .world()
                    .page(position.page())
                    .map(|page| page.light(position.local_index()))
            }
        };
        match self.temporal_identities.as_mut() {
            Some(identities) => self.source.flush_sampled_with_publications(
                queue,
                &mut self.gpu,
                [0; 3],
                budget,
                256,
                light_at,
                |publication| {
                    identities[publication.slot as usize] =
                        publication.position.map(|_| publication);
                },
            ),
            None => self
                .source
                .flush_sampled(queue, &mut self.gpu, [0; 3], budget, 256, light_at),
        }
        .map_err(|error| error.to_string())?;
        self.gpu.take_work();
        let work = self.source.take_work();
        self.record(scene, tick, work)?;
        Ok(work)
    }
    pub fn publish_public_frame(
        &mut self,
        scene: &ClientScene,
        frame: Arc<net::props::Frame>,
    ) -> Result<bool, String> {
        let Some(version) = scene.authoritative_version(frame.page) else {
            return Ok(false);
        };
        self.source
            .publish_public_frame(frame, version.view)
            .map_err(|error| error.to_string())
    }
    pub fn prepare_camera(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: render::terrain_gpu::Camera,
    ) -> Result<render::block_models::BlockWork, String> {
        self.gpu
            .prepare_frame(queue, encoder, camera)
            .map_err(|error| error.to_string())
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        self.gpu.draw(pass)
    }
    fn quarantine_source(&mut self) -> Result<(), String> {
        self.gpu
            .revoke_source()
            .map_err(|error| error.to_string())?;
        self.quarantined = true;
        self.stats.ready = false;
        Ok(())
    }
    fn record(&mut self, scene: &ClientScene, tick: Tick, work: PropsWork) -> Result<(), String> {
        if work.cursor_recoveries > 0 {
            self.quarantine_source()?;
        }
        self.stats.worker_cells += work.worker_cells;
        self.stats.worker_mask_words += work.worker_mask_words;
        self.stats.owner_light_reads += work.owner_light_reads;
        self.stats.upload_bytes += work.upload_bytes;
        self.stats.cpu_vertices += work.cpu_vertices;
        let source = self.source.stats(scene.world());
        let context_age = self
            .contexts
            .values()
            .map(|context| tick.0.saturating_sub(context.born.0))
            .max()
            .unwrap_or(0);
        self.stats.instances = source.models;
        self.stats.pending_jobs = self.contexts.len();
        self.stats.pending_records = source.queued;
        self.stats.oldest_age_ticks = source.oldest_age_ticks.max(context_age);
        self.stats.overdue = self.stats.oldest_age_ticks > self.limits.max_age_ticks;
        self.stats.ready = source.settled
            && self.pending.is_empty()
            && self.contexts.is_empty()
            && self.retry.is_none();
        if self.quarantined && self.stats.ready {
            self.gpu
                .set_source_valid(true)
                .map_err(|error| error.to_string())?;
            self.quarantined = false;
        }
        Ok(())
    }
}
fn tint(value: Option<u16>) -> u32 {
    value.map_or(u32::MAX, |color| {
        let r = u32::from((color >> 11) & 31) * 255 / 31;
        let g = u32::from((color >> 5) & 63) * 255 / 63;
        let b = u32::from(color & 31) * 255 / 31;
        r | (g << 8) | (b << 16) | 0xff000000
    })
}

#[cfg(test)]
#[path = "props_recovery_gpu_tests.rs"]
mod recovery_gpu_tests;
