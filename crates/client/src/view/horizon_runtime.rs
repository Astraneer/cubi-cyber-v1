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

#[derive(Clone, Copy, Debug, Default)]
pub struct HorizonJobStats {
    pub queued: usize,
    pub running: usize,
    pub retained_bytes: usize,
    pub contexts: usize,
    pub capture_pending: bool,
    pub result_pending: bool,
    pub plans_completed: u64,
    pub plans_stale: u64,
    pub plans_failed: u64,
    /// 0 none,1 capacity,2 plan bounds,3 other worker plan error,4 transport.
    pub last_plan_failure: u8,
    pub proxy_results: u64,
    pub worker_failures: u64,
    pub jobs: jobs::JobStats,
}

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
    stats: HorizonJobStats,
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
            stats: HorizonJobStats::default(),
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
            self.enqueue(horizon::BackgroundTask::Plan(Box::new(task)), tick);
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
        let id = completion.id;
        let outcome = self.scheduler.finish(completion, |_| None);
        // An unsolicited completion for a queued job must not consume its
        // immutable owner context. Scheduler validates running ownership first.
        if matches!(outcome, Finish::UnknownJob) {
            return Ok(());
        }
        let context = self.contexts.remove(&id);
        match outcome {
            Finish::Published(horizon::BackgroundResult::Plan(Ok(plan))) => {
                if self
                    .owner
                    .complete_plan(plan)
                    .map_err(|error| error.to_string())?
                {
                    self.stats.plans_completed += 1;
                    self.stats.last_plan_failure = 0;
                } else {
                    self.stats.plans_stale += 1;
                }
            }
            Finish::Published(horizon::BackgroundResult::Plan(Err(error))) => {
                self.stats.plans_failed += 1;
                self.stats.last_plan_failure = match error {
                    horizon::Error::Capacity => 1,
                    horizon::Error::Plan => 2,
                    _ => 3,
                };
                if let Some(context) = context {
                    context.source.fail(&mut self.owner);
                }
            }
            Finish::Published(horizon::BackgroundResult::Proxy(Ok(proxy))) => {
                self.stats.proxy_results += 1;
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
            outcome => {
                if matches!(outcome, Finish::Failed(_)) {
                    self.stats.worker_failures += 1;
                    if context
                        .as_ref()
                        .is_some_and(|ctx| matches!(ctx.source, horizon::TaskContext::Plan { .. }))
                    {
                        self.stats.last_plan_failure = 4;
                    }
                }
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
    pub fn horizon_job_stats(&self) -> Option<HorizonJobStats> {
        let horizon = self.horizon.as_ref()?;
        Some(HorizonJobStats {
            queued: horizon.scheduler.pending_len(),
            running: horizon.scheduler.running_len(),
            retained_bytes: horizon.scheduler.retained_bytes(),
            contexts: horizon.contexts.len(),
            capture_pending: horizon.pending_task.is_some(),
            result_pending: horizon.pending_result.is_some(),
            jobs: horizon.scheduler.stats(),
            ..horizon.stats
        })
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
    pub fn horizon_wanted_exact_height(&self) -> impl Iterator<Item = net::height::HeightKey> + '_ {
        self.horizon
            .iter()
            .flat_map(|horizon| horizon.owner.wanted_exact_height())
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
        let parameters = self.camera_parameters();
        let eye = parameters.eye;
        let [width, height] = self.renderer.extent();
        // A whole camera cell and a ten-degree FOV interval certify every
        // interpolated/bobbed pose until a bin changes, instead of replanning
        // continuously during a sprint's FOV interpolation.
        let eye_min = eye.map(|v| (f64::from(v) / 16.).floor() * 16.);
        let eye_max = eye_min.map(|v| v + 16.);
        let lower_fov = (f64::from(parameters.fov) / 10.).floor().max(0.1) * 10.;
        let upper_fov = (lower_fov + 10.).min(179.);
        let focal_pixels =
            f64::from(height.max(1)) / (2. * libm::tan(lower_fov.to_radians() * 0.5));
        let aspect = f64::from(width.max(1)) / f64::from(height.max(1));
        let diagonal_tangent =
            libm::tan(upper_fov.to_radians() * 0.5) * libm::sqrt(1. + aspect * aspect);
        let camera_page = [
            (f64::from(eye[0]) / 16.).floor() as i64,
            (f64::from(eye[2]) / 16.).floor() as i64,
        ];
        let look: [f64; 3] =
            std::array::from_fn(|i| f64::from(parameters.target[i]) - f64::from(eye[i]));
        let yaw = (libm::atan2(look[0], look[2]).to_degrees() / 45.).floor() * 45. + 22.5;
        let pitch = (libm::atan2(look[1], libm::sqrt(look[0] * look[0] + look[2] * look[2]))
            .to_degrees()
            / 30.)
            .floor()
            * 30.
            + 15.;
        let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
        let view_direction = [
            libm::sin(yaw) * libm::cos(pitch),
            libm::sin(pitch),
            libm::cos(yaw) * libm::cos(pitch),
        ];
        let view_cosine = libm::cos(
            (libm::atan(diagonal_tangent) + 37.5f64.to_radians()).min(std::f64::consts::PI),
        );
        let Some(horizon) = &mut self.horizon else {
            return Ok(());
        };
        horizon
            .owner
            .set_projection(horizon::Projection {
                eye_min,
                eye_max,
                focal_pixels,
                diagonal_tangent,
                target_pixels: 32.,
                hysteresis_pixels: 8.,
                view_direction,
                view_cosine,
            })
            .map_err(|error| error.to_string())?;
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
