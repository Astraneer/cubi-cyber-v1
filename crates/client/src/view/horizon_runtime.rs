//! Bounded owner-thread scheduling and publication for the public horizon.
//! Plan refinement and proxy construction run in immutable worker tasks.
use super::{ClientView, horizon};
use core_types::{PageId, Tick};
use jobs::{
    Completion, Dispatch, Finish, JobId, JobKey, JobOutcome, JobSpec, Priority, Scheduler, Take,
};
use render::terrain_gpu::UploadBudget;
use std::collections::BTreeMap;

const RETAINED_JOB_BYTES: usize = 2 * 1024 * 1024;

struct Context {
    source: horizon::TaskContext,
    key: JobKey,
    running: bool,
}
pub(super) struct PreparedHorizon {
    pub owner: horizon::ViewHorizon,
    pub far_plane: f32,
    scheduler: Scheduler<horizon::BackgroundTask>,
    contexts: BTreeMap<JobId, Context>,
    pending_task: Option<horizon::BackgroundTask>,
    pending_result: Option<Box<horizon::ProxyBuilt>>,
    #[cfg(not(target_arch = "wasm32"))]
    native: jobs::NativePool<horizon::BackgroundTask, horizon::BackgroundResult>,
}
impl PreparedHorizon {
    fn new(owner: horizon::ViewHorizon, far_plane: f32) -> Result<Self, String> {
        Ok(Self {
            owner,
            far_plane,
            scheduler: Scheduler::new(jobs::Limits {
                max_jobs: 8,
                max_bytes: 8 * RETAINED_JOB_BYTES,
                max_age_ticks: 80,
                reserved_interactive_jobs: 0,
                reserved_interactive_bytes: 0,
            })
            .map_err(|_| "invalid horizon job limits".to_owned())?,
            contexts: BTreeMap::new(),
            pending_task: None,
            pending_result: None,
            #[cfg(not(target_arch = "wasm32"))]
            native: horizon::native_pool(1, 4)
                .map_err(|error| format!("horizon worker: {error:?}"))?,
        })
    }
    fn enqueue(&mut self, task: horizon::BackgroundTask, tick: Tick) {
        let context = Context {
            source: task.context(),
            key: task.job_key(),
            running: false,
        };
        let spec = JobSpec::derived(
            context.key,
            Priority::Background,
            task.cost(),
            RETAINED_JOB_BYTES,
            tick,
        );
        match self.scheduler.enqueue(spec, task) {
            Ok(id) => {
                // Scheduler coalesces queued derived work without a completion.
                // Keep contexts only for actual queued/running jobs.
                self.contexts
                    .retain(|_, old| old.running || old.key != context.key);
                self.contexts.insert(id, context);
            }
            Err(rejected) => self.pending_task = Some(rejected.payload),
        }
    }
    fn phase(&mut self, tick: Tick, space: u32, camera: [i64; 2]) -> Result<(), String> {
        if let Some(result) = self.pending_result.take()
            && let Err(result) = self.owner.complete_proxy(*result)
        {
            self.pending_result = Some(result);
            return Ok(());
        }
        if let Some(task) = self.pending_task.take() {
            self.enqueue(task, tick);
        }
        if self.pending_task.is_some() {
            return Ok(());
        }
        if let Some(task) = self
            .owner
            .plan_task(space, camera)
            .map_err(|error| error.to_string())?
        {
            self.enqueue(horizon::BackgroundTask::Plan(task), tick);
        }
        for _ in 0..2 {
            if self.pending_task.is_some() {
                break;
            }
            let Some(task) = self.owner.take_proxy_task() else {
                break;
            };
            self.enqueue(horizon::BackgroundTask::Proxy(task), tick);
        }
        Ok(())
    }
    fn take(&mut self, tick: Tick) -> Option<Dispatch<horizon::BackgroundTask>> {
        let mut budget = jobs::WorkBudget::unrestricted(jobs::WorkCost {
            cells: 131072,
            bytes: 4 * 1024 * 1024,
            ..jobs::WorkCost::ZERO
        });
        loop {
            match self.scheduler.take_next(tick, &mut budget, |_| None) {
                Take::Ready(dispatch) => {
                    if let Some(context) = self.contexts.get_mut(&dispatch.id) {
                        context.running = true;
                    }
                    return Some(dispatch);
                }
                Take::Expired(expired) => {
                    self.contexts.remove(&expired.id);
                    // Expiry returns ownership; the public source remains current
                    // only if its own plan/source guard passes at completion.
                    self.enqueue(expired.payload, tick);
                }
                Take::BudgetBlocked | Take::Empty => return None,
            }
        }
    }
    fn complete(
        &mut self,
        completion: Completion<horizon::BackgroundResult>,
    ) -> Result<(), String> {
        let context = self.contexts.remove(&completion.id);
        match self.scheduler.finish(completion, |_| None) {
            Finish::Published(horizon::BackgroundResult::Plan(Ok(plan))) => {
                self.owner
                    .complete_plan(plan)
                    .map_err(|error| error.to_string())?;
            }
            Finish::Published(horizon::BackgroundResult::Proxy(Ok(proxy))) => {
                match self.owner.complete_proxy(proxy) {
                    Ok(_) => {}
                    Err(proxy) => {
                        if self.pending_result.is_some() {
                            if let Some(context) = context {
                                context.source.fail(&mut self.owner);
                            }
                        } else {
                            self.pending_result = Some(proxy);
                        }
                    }
                }
            }
            _ => {
                if let Some(context) = context {
                    context.source.fail(&mut self.owner);
                }
            }
        }
        Ok(())
    }
    fn requeue(&mut self, dispatch: Dispatch<horizon::BackgroundTask>) -> Result<(), String> {
        if let Some(context) = self.contexts.get_mut(&dispatch.id) {
            context.running = false;
        }
        if let Err(dispatch) = self.scheduler.requeue(dispatch) {
            self.complete(Completion {
                id: dispatch.id,
                outcome: JobOutcome::Cancelled,
            })?;
        }
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn pump_native(&mut self, tick: Tick) -> Result<(), String> {
        for _ in 0..4 {
            let Some(completion) = self.native.try_recv() else {
                break;
            };
            self.complete(completion)?;
        }
        for _ in 0..2 {
            let Some(dispatch) = self.take(tick) else {
                break;
            };
            match self.native.try_submit(dispatch) {
                Ok(()) => {}
                Err(jobs::SubmitError::Full(dispatch)) => {
                    self.requeue(dispatch)?;
                    break;
                }
                Err(jobs::SubmitError::Closed(dispatch)) => {
                    self.complete(Completion {
                        id: dispatch.id,
                        outcome: JobOutcome::Failed(jobs::JobFailure::WorkerDisconnected),
                    })?;
                    return Err("native horizon transport closed".into());
                }
            }
        }
        Ok(())
    }
}

impl ClientView {
    pub fn prepare_horizon(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: horizon::Config,
    ) -> Result<(), String> {
        if self.horizon.is_some() || self.terrain_frames != 0 || !self.resident.is_empty() {
            return Err("horizon must be prepared once during loading".into());
        }
        let mut owner = horizon::ViewHorizon::prepare_with_environment(
            device,
            queue,
            &self.renderer,
            self.lighting
                .as_ref()
                .map(|lighting| lighting.owner.shadows()),
            config,
            self.environment
                .as_ref()
                .map(|environment| environment.owner.gpu.atmosphere()),
        )
        .map_err(|error| error.to_string())?;
        owner
            .reserve_near_cutout(u32::from(self.scene.config().horizontal_radius) + 1)
            .map_err(|error| error.to_string())?;
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(u64::from(owner.stats().pipeline_compilations));
        let far_plane = ((1_u32 << config.horizon_level) * 16) as f32 * 3.;
        self.horizon = Some(Box::new(PreparedHorizon::new(owner, far_plane.max(2048.))?));
        self.invalidate();
        Ok(())
    }
    pub fn horizon_stats(&self) -> Option<horizon::Stats> {
        self.horizon.as_ref().map(|horizon| horizon.owner.stats())
    }
    pub fn horizon_mut(&mut self) -> Option<&mut horizon::ViewHorizon> {
        self.horizon
            .as_deref_mut()
            .map(|horizon| &mut horizon.owner)
    }
    pub fn horizon_wanted_height(&self) -> impl Iterator<Item = net::height::HeightKey> + '_ {
        self.horizon
            .iter()
            .flat_map(|horizon| horizon.owner.wanted_height())
    }
    pub fn ingest_remote_height(
        &mut self,
        tile: Box<net::height::HeightTile>,
    ) -> Result<bool, String> {
        self.horizon.as_mut().map_or(Ok(false), |horizon| {
            horizon
                .owner
                .ingest_height(tile)
                .map_err(|error| error.to_string())
        })
    }
    pub fn ingest_remote_geometry(
        &mut self,
        geometry: &net::PublicGeometry,
    ) -> Result<bool, String> {
        self.horizon.as_mut().map_or(Ok(false), |horizon| {
            horizon
                .owner
                .ingest_geometry(geometry)
                .map_err(|error| error.to_string())
        })
    }
    pub fn remove_remote_geometry(&mut self, page: PageId) -> Result<(), String> {
        if let Some(horizon) = &mut self.horizon {
            horizon
                .owner
                .remove_geometry(page)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    /// Browser transport takes immutable work and returns ownership on refusal.
    pub fn take_horizon_task(&mut self) -> Option<Dispatch<horizon::BackgroundTask>> {
        self.horizon.as_mut()?.take(self.tick)
    }
    pub fn requeue_horizon_task(
        &mut self,
        dispatch: Dispatch<horizon::BackgroundTask>,
    ) -> Result<(), String> {
        self.horizon
            .as_mut()
            .ok_or("horizon is not prepared")?
            .requeue(dispatch)
    }
    pub fn complete_horizon_task(
        &mut self,
        completion: Completion<horizon::BackgroundResult>,
    ) -> Result<(), String> {
        self.horizon
            .as_mut()
            .ok_or("horizon is not prepared")?
            .complete(completion)
    }
    pub fn complete_horizon_bytes(&mut self, id: JobId, bytes: &[u8]) -> Result<(), String> {
        match horizon::decode_background_result(bytes) {
            Ok(result) => self.complete_horizon_task(Completion {
                id,
                outcome: JobOutcome::Success(result),
            }),
            Err(error) => {
                self.fail_horizon_task(id)?;
                Err(error.to_string())
            }
        }
    }
    pub fn fail_horizon_task(&mut self, id: JobId) -> Result<(), String> {
        self.complete_horizon_task(Completion {
            id,
            outcome: JobOutcome::Failed(jobs::JobFailure::WorkerDisconnected),
        })
    }
    pub(super) fn pump_horizon(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        if self.horizon.is_none() {
            return Ok(());
        }
        let eye = self.camera_parameters().eye;
        let camera_page = [
            (f64::from(eye[0]) / 16.).floor() as i64,
            (f64::from(eye[2]) / 16.).floor() as i64,
        ];
        let Some(horizon) = &mut self.horizon else {
            return Ok(());
        };
        horizon.phase(self.tick, self.scene.config().center.space.0, camera_page)?;
        #[cfg(not(target_arch = "wasm32"))]
        horizon.pump_native(self.tick)?;
        let before = budget.remaining_bytes;
        let guarded = horizon
            .owner
            .guard_near(
                queue,
                self.scene.config().center.space.0,
                camera_page,
                budget,
            )
            .map_err(|error| error.to_string())?;
        if guarded.is_some() {
            horizon
                .owner
                .pump(queue, budget, 4, 2)
                .map_err(|error| error.to_string())?;
        }
        let bytes = before - budget.remaining_bytes;
        // Count source/header writes through the shared budget once. Only new
        // camera writes remain for the encode phase's upload accounting.
        horizon.owner.height.take_work();
        horizon.owner.proxies.take_work();
        self.retained_foreground_work.record(telemetry::WorkCounts {
            upload_bytes: bytes,
            ..telemetry::WorkCounts::ZERO
        });
        if bytes != 0 || guarded.is_none() {
            self.renderer.invalidate();
            self.invalidate();
        }
        Ok(())
    }
}
