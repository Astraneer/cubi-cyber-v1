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
    near_residents: Vec<PageId>,
    scheduler: Scheduler<horizon::BackgroundTask>,
    contexts: BTreeMap<JobId, Context>,
    pending_task: Option<horizon::BackgroundTask>,
    pending_result: Option<Box<horizon::ProxyBuilt>>,
    pending_water_result: Option<(
        crate::far_water_source::Context,
        Box<crate::far_water_worker::BuiltFarWater>,
    )>,
    stats: HorizonJobStats,
    #[cfg(not(target_arch = "wasm32"))]
    native: jobs::NativePool<horizon::BackgroundTask, horizon::BackgroundResult>,
}
impl PreparedHorizon {
    fn new(owner: horizon::ViewHorizon, far_plane: f32) -> Result<Self, String> {
        Ok(Self {
            owner,
            far_plane,
            near_residents: Vec::with_capacity(512),
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
            pending_water_result: None,
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
        if let Some((context, result)) = self.pending_water_result.take()
            && let Err(result) = self.owner.complete_water(context, result)
        {
            self.pending_water_result = Some((context, result));
            return Ok(());
        }
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
        if self.pending_task.is_none()
            && let Some(task) = self.owner.take_water_task()
        {
            self.enqueue(task, tick);
        }
        Ok(())
    }
    fn take(&mut self, tick: Tick) -> Option<Dispatch<horizon::BackgroundTask>> {
        let mut budget = jobs::WorkBudget::unrestricted(jobs::WorkCost {
            cells: horizon::MAX_PROXY_WORK_CELLS,
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
        if let Finish::Published(result) = &outcome
            && result.context().is_some_and(|result| {
                context
                    .as_ref()
                    .is_none_or(|captured| captured.source != result)
            })
        {
            if let Some(context) = context {
                context.source.fail(&mut self.owner);
            }
            self.stats.worker_failures += 1;
            return Ok(());
        }
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
            Finish::Published(horizon::BackgroundResult::Water {
                context: water_context,
                result: Ok(water),
            }) => {
                if let Err(water) = self.owner.complete_water(water_context, water) {
                    if self.pending_water_result.is_some() {
                        return Err("far-water completion reservation violated".into());
                    }
                    self.pending_water_result = Some((water_context, water));
                }
            }
            Finish::Published(horizon::BackgroundResult::Water {
                context: water_context,
                result: Err(_),
            }) => {
                horizon::TaskContext::Water(water_context).fail(&mut self.owner);
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
    pub fn prepare_distant_shadows(&mut self, device: &wgpu::Device) -> Result<(), String> {
        if self.distant_shadows.is_some() || self.terrain_frames != 0 || !self.resident.is_empty() {
            return Err("distant shadows must be prepared once during loading".into());
        }
        let horizon = self
            .horizon
            .as_ref()
            .ok_or("distant shadows require a prepared horizon")?;
        let lighting = self
            .lighting
            .as_mut()
            .ok_or("distant shadows require prepared lighting")?;
        let owner = super::distant_shadows::ViewDistantShadows::prepare(
            device,
            &horizon.owner,
            lighting.owner.shadows(),
        )
        .map_err(|error| error.to_string())?;
        // Switching from the legacy pair to four exact leaf keys must refresh
        // an already rendered atlas once, even if numeric key values coincide.
        lighting.owner.invalidate_all_shadows();
        self.pipeline_compilations += u64::from(owner.stats().startup_pipelines);
        self.distant_shadows = Some(Box::new(owner));
        self.renderer.invalidate();
        self.invalidate();
        Ok(())
    }
    pub fn distant_shadow_stats(&self) -> Option<super::distant_shadows::Stats> {
        self.distant_shadows.as_ref().map(|owner| owner.stats())
    }
    pub fn prepare_far_water(
        &mut self,
        limits: crate::far_water_source::Limits,
    ) -> Result<(), String> {
        let gpu = self
            .environment
            .as_mut()
            .and_then(|environment| environment.owner.gpu.water_mut())
            .ok_or("far water requires the shared physical-water loading profile")?;
        let (objects, quads) = gpu.capacity();
        if limits
            .max_pages
            .checked_mul(3)
            .is_none_or(|needed| (objects as usize) < needed)
            || limits
                .max_total_quads
                .checked_mul(3)
                .is_none_or(|needed| (quads as usize) < needed)
        {
            return Err("shared near/far water arena cannot retain both fronts and staging".into());
        }
        self.horizon
            .as_mut()
            .ok_or("horizon must be prepared before far water")?
            .owner
            .enable_separate_water(limits)
            .map_err(|error| error.to_string())
    }
    pub fn horizon_water_stats(&self) -> Option<crate::far_water_source::Stats> {
        self.horizon.as_ref()?.owner.water_stats()
    }
    pub fn far_water_guard_ready(&self) -> bool {
        self.horizon
            .as_ref()
            .is_none_or(|horizon| horizon.owner.water_guard_ready())
    }
    pub fn horizon_wanted_water(&self) -> impl Iterator<Item = net::water::Request> + '_ {
        self.horizon
            .iter()
            .flat_map(|horizon| horizon.owner.wanted_water())
    }
    pub fn ingest_remote_water(
        &mut self,
        image: std::sync::Arc<net::water::WaterHalo>,
    ) -> Result<bool, String> {
        self.horizon.as_mut().map_or(Ok(false), |horizon| {
            horizon
                .owner
                .admit_water(image)
                .map_err(|error| error.to_string())
        })
    }
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
            .set_near_intent(intended_near_volume(&self.scene.config()))
            .map_err(|e| e.to_string())?;
        owner
            .reserve_near_cutout(u32::from(self.scene.config().horizontal_radius))
            .map_err(|error| error.to_string())?;
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(u64::from(owner.stats().pipeline_compilations));
        let far_plane = if let Some(settings) = &self.requested_settings {
            let pages = settings.get(ui::settings::Setting::Horizon) as u32;
            owner
                .set_selection(
                    pages,
                    f64::from(settings.get(ui::settings::Setting::LodError)),
                )
                .map_err(|e| e.to_string())?;
            (pages * 16) as f32
        } else {
            ((2_u32 << config.horizon_level) * 16) as f32
        };
        self.horizon = Some(Box::new(PreparedHorizon::new(owner, far_plane)?));
        self.invalidate();
        Ok(())
    }
    pub fn set_horizon_settings(
        &mut self,
        horizon_pages: u32,
        lod_error_pixels: f64,
    ) -> Result<bool, String> {
        let Some(horizon) = &mut self.horizon else {
            return Ok(false);
        };
        let changed = horizon
            .owner
            .set_selection(horizon_pages, lod_error_pixels)
            .map_err(|e| e.to_string())?;
        if changed {
            self.invalidate();
        }
        Ok(changed)
    }
    pub fn horizon_setting_applied(&self, horizon_pages: u32) -> bool {
        self.horizon
            .as_ref()
            .is_some_and(|h| h.owner.horizon_selection_applied(horizon_pages))
    }
    pub fn lod_error_setting_applied(&self, lod_error_pixels: f64) -> bool {
        self.horizon
            .as_ref()
            .is_some_and(|h| h.owner.error_selection_applied(lod_error_pixels))
    }
    pub fn near_far_join_ready(&self) -> bool {
        self.horizon
            .as_ref()
            .is_none_or(|h| h.owner.near_cover_ready())
    }
    pub fn has_committed_far_front(&self) -> bool {
        self.horizon
            .as_ref()
            .is_some_and(|h| h.owner.has_committed_front())
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
            result_pending: horizon.pending_result.is_some()
                || horizon.pending_water_result.is_some(),
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
    /// No queue access: worker capture, completion and plan scheduling continue
    /// while an earlier GPU submission is busy.
    pub(super) fn advance_horizon_cpu(&mut self) -> Result<(), String> {
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
        let actual_yaw = libm::atan2(look[0], look[2]).to_degrees();
        let actual_pitch =
            libm::atan2(look[1], libm::sqrt(look[0] * look[0] + look[2] * look[2])).to_degrees();
        let (view_direction, margin, max_abs_forward_y) =
            horizon::orientation_certificate(actual_yaw, actual_pitch, 45., 10.)
                .map_err(|e| e.to_string())?;
        let view_cosine =
            libm::cos((libm::atan(diagonal_tangent) + margin).min(std::f64::consts::PI));
        let Some(horizon) = &mut self.horizon else {
            return Ok(());
        };
        horizon
            .owner
            .set_display_space(self.scene.config().center.space.0);
        horizon
            .owner
            .set_near_intent(intended_near_volume(&self.scene.config()))
            .map_err(|e| e.to_string())?;
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
                max_abs_forward_y,
            })
            .map_err(|error| error.to_string())?;
        horizon.phase(self.tick, self.scene.config().center.space.0, camera_page)?;
        #[cfg(not(target_arch = "wasm32"))]
        horizon.pump_native(self.tick)?;
        Ok(())
    }
    pub(super) fn pump_horizon(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        self.advance_horizon_cpu()?;
        let Some(horizon) = &mut self.horizon else {
            return Ok(());
        };
        let before = budget.remaining_bytes;
        horizon.near_residents.clear();
        horizon.near_residents.extend(
            self.resident
                .keys()
                .copied()
                .filter(|page| page.space.0 == 0),
        );
        let guarded = if horizon.owner.separate_water_enabled() {
            let water = self
                .environment
                .as_mut()
                .and_then(|e| e.owner.gpu.water_mut())
                .ok_or("far water GPU was detached")?;
            horizon
                .owner
                .guard_near_cover(queue, Some(water), &horizon.near_residents, budget)
        } else {
            horizon
                .owner
                .guard_near_cover(queue, None, &horizon.near_residents, budget)
        }
        .map_err(|e| e.to_string())?;
        if guarded.is_some() {
            let initial = horizon.owner.stats();
            let (height_uploads, proxy_uploads) =
                if initial.height_front == 0 && initial.proxy_front == 0 {
                    (64, 16)
                } else {
                    (4, 2)
                };
            if horizon.owner.separate_water_enabled() {
                let water_gpu = self
                    .environment
                    .as_mut()
                    .and_then(|environment| environment.owner.gpu.water_mut())
                    .ok_or("far water GPU was detached")?;
                if horizon
                    .owner
                    .guard_water_near(queue, water_gpu, budget)
                    .map_err(|error| error.to_string())?
                {
                    horizon
                        .owner
                        .pump_with_water(queue, water_gpu, budget, height_uploads, proxy_uploads)
                        .map_err(|error| error.to_string())?;
                }
            } else {
                horizon
                    .owner
                    .pump(queue, budget, height_uploads, proxy_uploads)
                    .map_err(|error| error.to_string())?;
            }
        }
        let far_plane_changed = if let Some(pages) = horizon.owner.committed_horizon_pages() {
            let far_plane = (pages * 16) as f32;
            let changed = horizon.far_plane != far_plane;
            horizon.far_plane = far_plane;
            changed
        } else {
            false
        };
        let bytes = before - budget.remaining_bytes;
        // Count source/header writes through the shared budget once. Only new
        // camera writes remain for the encode phase's upload accounting.
        horizon.owner.height.take_work();
        horizon.owner.proxies.take_work();
        self.retained_foreground_work.record(telemetry::WorkCounts {
            upload_bytes: bytes,
            ..telemetry::WorkCounts::ZERO
        });
        if bytes != 0 || guarded.is_none() || far_plane_changed {
            self.renderer.invalidate();
            self.invalidate();
        }
        Ok(())
    }
}

fn intended_near_volume(config: &crate::SceneConfig) -> [[i32; 3]; 2] {
    let centre = config.center.page();
    let horizontal = i32::from(config.horizontal_radius);
    let vertical = i32::from(config.vertical_radius);
    [
        [
            centre.x - horizontal,
            centre.y - vertical,
            centre.z - horizontal,
        ],
        [
            centre.x + horizontal + 1,
            centre.y + vertical + 1,
            centre.z + horizontal + 1,
        ],
    ]
}
