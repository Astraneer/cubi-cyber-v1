//! Shared presentation/controller owner; platform shells transport tasks and present.
//! GPU uploads and simulation catch-up are bounded independently of frame rate.
use crate::{
    ClientScene, SceneConfig,
    demo_interaction::{DemoInteractions, PreparedDemoEdit},
    edit_shell::{EditOwnerUpdates, EditShell, ShellError},
    self_model::{PART_COUNT, PART_EXTENTS, SelfModel, SelfPose},
};
use core_types::{BlockPos, PageId, SpaceId, Tick, Version};
use render::terrain_gpu::{
    Camera, CullingMode, PageHandle, TerrainError, TerrainFrameWork, TerrainGpu, UploadBudget,
    translation,
};
use sim::{CameraMode, Keys, Permissions, Player, WorldQuery};
use std::collections::{BTreeMap, VecDeque};
use world::CellRead;
mod clock;
pub mod distant_shadows;
mod entities;
pub mod environment;
pub mod horizon;
mod horizon_runtime;
pub mod model_motion;
pub mod recipe_boards;
mod settings;
pub mod temporal;
pub use horizon_runtime::HorizonJobStats;
mod lighting;
pub mod props;
mod remote;
pub mod water;
pub mod water_model;
pub use entities::{EntityStats, ViewEntities};
pub use environment::{EnvironmentProfile, EnvironmentStats, ViewEnvironment};
pub use lighting::{LightingStats, ViewLighting};
pub use props::{PropsStats, ViewProps};
pub use remote::RemoteInput;

/// Source identity retained by an encoded terrain Scene. GPU completion and
/// presentation validation are separate from this read-only encoding receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodedSceneStamp {
    pub camera: u64,
    pub world: Version,
    pub visual: u64,
    pub uploads: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum InputKey {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Sprint,
    Sneak,
    Flight,
    Noclip,
    Camera,
    Attack,
    Place,
    ToolMode,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewStats {
    pub resident_pages: usize,
    pub resident_quads: usize,
    pub terrain_frames: u64,
    pub props_frames: u64,
    pub loaded_pages: usize,
    pub queued_jobs: usize,
    pub running_jobs: usize,
    pub meshes_published: u64,
    pub upload_bytes_total: u64,
    pub ready: bool,
    pub player_spawned: bool,
    pub player_pos: [f64; 3],
    pub flying: bool,
    pub noclip: bool,
    pub on_ground: bool,
    pub simulation_ticks: u64,
    pub camera_mode: u8,
    pub self_model_rendered: bool,
    pub pipeline_compilations: u64,
    pub frame_pipeline_compilations: u64,
    pub dropped_time_seconds: f64,
    pub denied_debug_requests: u64,
    pub blocks_broken: u64,
    pub blocks_placed: u64,
    pub edits_rolled_back: u64,
    pub edit_denied: u64,
    pub mining_progress: f32,
    pub last_edit_position: Option<BlockPos>,
    pub target_position: Option<[i32; 3]>,
    pub target_cell: u16,
    pub edit_shell_pages: usize,
    pub edit_shell_owners: usize,
    pub edit_overlay_uploads: u64,
    pub edit_max_visible_frames: u64,
    pub demo_mesh_delay_ticks: u64,
    pub full_mesh_ready: bool,
    pub base_rebases: u64,
    pub overlay_quads: usize,
    pub edit_revision: u64,
    pub rendered_edit_revision: u64,
}

/// Known logical work for one consumed interval. Worker counters are completed
/// reports (including stale results); unreported partial cancellations and
/// physics/ray query timings are deliberately not inferred.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameWorkDetails {
    pub renderer: telemetry::WorkCounts,
    pub workers: telemetry::WorkCounts,
    pub foreground: telemetry::WorkCounts,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsApplication {
    /// Bits use ui::settings::Setting discriminants.
    pub applied: u32,
    /// Requested graphics controls whose backend is not attached yet. They must
    /// not be shown as effective quality choices merely because they are saved.
    pub pending_render: u32,
    /// Resource/pipeline choices needing the existing asynchronous loading path.
    pub reload_required: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NearRasterization {
    /// Greedy 16-byte storage; exact unit faces are expanded in GPU visibility.
    #[default]
    CompactUnit,
    /// Explicit negative control for the old greedy/patch coverage mismatch.
    LegacyGreedy,
}

#[derive(Clone, Copy)]
struct Resident {
    handle: PageHandle,
    quads: usize,
    version: Version,
    inputs: [world::SampledPageInput; crate::INPUT_PAGES],
}

struct Overlay {
    handle: PageHandle,
    inputs: Vec<world::SampledPageInput>,
    mask: crate::edit_shell::OwnerMask,
    quads: usize,
    owner_faces: Box<[u8; 4096]>,
}

#[derive(Clone, Copy)]
struct CameraParameters {
    eye: [f32; 3],
    target: [f32; 3],
    up: [f32; 3],
    fov: f32,
    far: f32,
}
struct PreparedEnvironment {
    owner: ViewEnvironment,
    queue: wgpu::Queue,
    width: u32,
    height: u32,
    cloud_quality: i32,
    gi_quality: i32,
    reflection_quality: i32,
}
impl CameraParameters {
    fn projection(self, width: u32, height: u32) -> Camera {
        Camera::perspective(
            self.eye,
            self.target,
            self.up,
            self.fov.to_radians(),
            width.max(1) as f32 / height.max(1) as f32,
            0.05,
            self.far,
        )
    }
}

pub struct ClientView {
    pub scene: ClientScene,
    renderer: TerrainGpu,
    lighting: Option<Box<ViewLighting>>,
    entities: Option<Box<ViewEntities>>,
    props: Option<Box<ViewProps>>,
    water: Option<Box<water::ViewWater>>,
    environment: Option<Box<PreparedEnvironment>>,
    horizon: Option<Box<horizon_runtime::PreparedHorizon>>,
    distant_shadows: Option<Box<distant_shadows::ViewDistantShadows>>,
    recipe_boards: Option<Box<recipe_boards::ViewRecipeBoards>>,
    temporal: Option<Box<temporal::ViewTemporal>>,
    frame_upload_remaining: u64,
    frame_delta_seconds: f32,
    clock: clock::ClockState,
    requested_settings: Option<ui::settings::Settings>,
    render_scale: u32,
    player: Player,
    movement_replay: Box<sim::replay::Recorder>,
    replay_time_ns: u64,
    replay_external_clock: bool,
    interactions: DemoInteractions,
    edit_shell: EditShell,
    pending_patches: VecDeque<EditOwnerUpdates>,
    overlays: BTreeMap<PageId, Overlay>,
    self_model: SelfModel,
    self_handles: [Option<PageHandle>; PART_COUNT],
    self_uploaded_pose: Option<SelfPose>,
    self_model_rendered: bool,
    edit_overlay_uploads: u64,
    edit_max_visible_frames: u64,
    edit_pending_since: Option<u64>,
    demo_mesh_delay_ticks: u64,
    mesh_hold_until: Tick,
    demo_mesh_publication_paused: bool,
    demo_unit_mesh: bool,
    demo_gpu_unit_mesh: bool,
    gpu_revision: u64,
    base_rebases: u64,
    edit_revision: u64,
    rendered_edit_revision: u64,
    keys: [bool; 13],
    place_pressed: bool,
    place_sneaking: bool,
    remote_controls: VecDeque<InputKey>,
    remote_tool_modes: VecDeque<bool>,
    remote_look: Option<[f32; 2]>,
    remote_signature: Option<u64>,
    retained_gpu_work: telemetry::WorkCounters,
    retained_foreground_work: telemetry::WorkCounters,
    retained_worker_work: telemetry::WorkCounters,
    last_reported_mesh_cells: u64,
    last_reported_mesh_quads: u64,
    last_frame_work: telemetry::WorkCounts,
    last_frame_work_details: FrameWorkDetails,
    active: bool,
    activation_requested: bool,
    camera_mode: CameraMode,
    accumulator: f64,
    tick: Tick,
    resident: BTreeMap<PageId, Resident>,
    page_links_dirty: bool,
    terrain_frames: u64,
    props_frames: u64,
    props_content_encoded: bool,
    upload_bytes_total: u64,
    pipeline_compilations: u64,
    frame_pipeline_compilations: u64,
    dropped_time_seconds: f64,
    denied_debug_requests: u64,
    edit_rejected: u64,
    cached_camera: Option<Camera>,
    field_of_view: f32,
    view_bob: bool,
    cached_world_version: Version,
    cached_upload_bytes: u64,
    cached_visual_revision: u64,
    cached_props_epoch: u64,
    observed_world_version: Version,
    observed_upload_bytes: u64,
    encoded_shadows: bool,
    encoded_terrain: bool,
    cached_presentation_available: bool,
    force_redraw: bool,
}

impl ClientView {
    pub fn new(renderer: TerrainGpu, config: SceneConfig) -> Result<Self, String> {
        Self::new_with_rasterization(renderer, config, NearRasterization::CompactUnit)
    }
    pub fn new_with_rasterization(
        mut renderer: TerrainGpu,
        mut config: SceneConfig,
        rasterization: NearRasterization,
    ) -> Result<Self, String> {
        let compact_unit = rasterization == NearRasterization::CompactUnit;
        if compact_unit {
            renderer
                .prepare_unit_rasterization()
                .map_err(|error| error.to_string())?;
        }
        let visible = (usize::from(config.horizontal_radius) * 2 + 1).pow(2)
            * (usize::from(config.vertical_radius) * 2 + 1);
        if visible + PART_COUNT > renderer.geometry().max_pages as usize {
            return Err(
                "renderer profile cannot retain all requested pages and avatar objects".into(),
            );
        }
        config.coherent_rebasing = true;
        let setup = renderer.take_work();
        Ok(Self {
            scene: ClientScene::new(config).map_err(|e| e.to_string())?,
            renderer,
            lighting: None,
            entities: None,
            props: None,
            water: None,
            environment: None,
            temporal: None,
            horizon: None,
            distant_shadows: None,
            recipe_boards: None,
            frame_upload_remaining: 0,
            frame_delta_seconds: 1. / 60.,
            clock: Default::default(),
            requested_settings: None,
            render_scale: 100,
            player: Player::new([
                f64::from(config.center.x) + 0.5,
                f64::from(config.center.y),
                f64::from(config.center.z) + 0.5,
            ]),
            movement_replay: Box::new(
                sim::replay::Recorder::new(sim::replay::DEFAULT_STEPS).map_err(str::to_owned)?,
            ),
            replay_time_ns: 0,
            replay_external_clock: false,
            interactions: DemoInteractions::new(config.seed),
            edit_shell: EditShell::new(
                crate::edit_shell::DEFAULT_SHELL_PAGES,
                crate::edit_shell::DEFAULT_SHELL_PAGES * 4096,
            ),
            pending_patches: VecDeque::with_capacity(crate::edit_shell::DEFAULT_SHELL_PAGES),
            overlays: BTreeMap::new(),
            self_model: SelfModel::new(),
            self_handles: [None; PART_COUNT],
            self_uploaded_pose: None,
            self_model_rendered: false,
            edit_overlay_uploads: 0,
            edit_max_visible_frames: 0,
            edit_pending_since: None,
            demo_mesh_delay_ticks: 0,
            mesh_hold_until: Tick(0),
            demo_mesh_publication_paused: false,
            demo_unit_mesh: false,
            demo_gpu_unit_mesh: compact_unit,
            gpu_revision: 0,
            base_rebases: 0,
            edit_revision: 0,
            rendered_edit_revision: 0,
            keys: [false; 13],
            place_pressed: false,
            place_sneaking: false,
            remote_controls: VecDeque::with_capacity(32),
            remote_tool_modes: VecDeque::with_capacity(32),
            remote_look: None,
            remote_signature: None,
            retained_gpu_work: telemetry::WorkCounters::new(),
            retained_foreground_work: telemetry::WorkCounters::new(),
            retained_worker_work: telemetry::WorkCounters::new(),
            last_reported_mesh_cells: 0,
            last_reported_mesh_quads: 0,
            last_frame_work: telemetry::WorkCounts::ZERO,
            last_frame_work_details: FrameWorkDetails::default(),
            active: false,
            activation_requested: false,
            camera_mode: CameraMode::FirstPerson,
            accumulator: 0.0,
            tick: Tick(0),
            resident: BTreeMap::new(),
            page_links_dirty: false,
            terrain_frames: 0,
            props_frames: 0,
            props_content_encoded: false,
            upload_bytes_total: 0,
            pipeline_compilations: setup.pipeline_compilations,
            frame_pipeline_compilations: 0,
            dropped_time_seconds: 0.0,
            denied_debug_requests: 0,
            edit_rejected: 0,
            cached_camera: None,
            field_of_view: 70.,
            view_bob: false,
            cached_world_version: Version(0),
            cached_upload_bytes: 0,
            cached_visual_revision: 0,
            cached_props_epoch: 0,
            observed_world_version: Version(0),
            observed_upload_bytes: 0,
            encoded_shadows: false,
            encoded_terrain: false,
            cached_presentation_available: false,
            force_redraw: true,
        })
    }
    pub const fn tick(&self) -> Tick {
        self.tick
    }
    pub fn player(&self) -> &Player {
        &self.player
    }
    pub fn player_mut(&mut self) -> &mut Player {
        &mut self.player
    }
    pub fn movement_replay(&self) -> &sim::replay::Recorder {
        &self.movement_replay
    }
    /// Platform clocks keep replay expiry tied to real time even in a fixed-step
    /// benchmark. Once selected, this clock is supplied before every advance.
    pub fn set_movement_replay_time(&mut self, monotonic_ns: u64) -> Result<(), String> {
        self.movement_replay
            .set_time(monotonic_ns)
            .map_err(str::to_owned)?;
        self.replay_time_ns = monotonic_ns;
        self.replay_external_clock = true;
        Ok(())
    }
    pub fn renderer_mut(&mut self) -> &mut TerrainGpu {
        &mut self.renderer
    }
    pub fn render_sample_count(&self) -> u32 {
        self.renderer.geometry().sample_count
    }
    pub fn exposure_compensation(&self) -> Option<f32> {
        self.environment
            .as_ref()
            .map(|environment| environment.owner.gpu.post().compensation())
    }
    pub fn exposure_automatic(&self) -> Option<bool> {
        self.environment
            .as_ref()
            .map(|environment| environment.owner.gpu.post().automatic())
    }
    pub fn celestial_styles(&self) -> Option<[u32; 3]> {
        let config = self.environment.as_ref()?.owner.gpu.celestials()?.config();
        Some([config.sun_style, config.moon_style, config.star_style])
    }
    pub fn gi_prepared(&self) -> bool {
        self.environment
            .as_ref()
            .is_some_and(|environment| environment.owner.gpu.gi().is_some())
    }
    pub fn replace_material_albedo(
        &mut self,
        atlas: &render::terrain_gpu::AtlasImage,
    ) -> Result<(), String> {
        if let Some(environment) = &mut self.environment {
            environment
                .owner
                .replace_albedo(atlas)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    /// Attach the CPU light owner and all GPU pipelines during loading. Callers
    /// choose the standalone sky proof or the remote Unknown-is-dark policy.
    pub fn prepare_lighting(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: crate::lighting::ClientLightingConfig,
    ) -> Result<(), String> {
        if self.lighting.is_some()
            || self.recipe_boards.is_some()
            || self.entities.is_some()
            || self.props.is_some()
            || self.environment.is_some()
            || self.horizon.is_some()
            || self.terrain_frames != 0
            || !self.resident.is_empty()
            || self.self_handles.iter().any(Option::is_some)
        {
            return Err("lighting must be prepared once before uploading scene geometry".into());
        }
        let center = self.lighting_center();
        let lighting = ViewLighting::prepare(
            device,
            queue,
            &mut self.renderer,
            &mut self.scene,
            config,
            center,
        )
        .map_err(|error| error.to_string())?;
        let setup = self.renderer.take_work();
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(setup.pipeline_compilations)
            .saturating_add(u64::from(
                lighting.owner.shadows().startup_pipeline_compilations(),
            ));
        self.lighting = Some(Box::new(lighting));
        self.invalidate();
        Ok(())
    }
    pub fn lighting_stats(&self) -> Option<LightingStats> {
        self.lighting
            .as_ref()
            .map(|lighting| lighting.snapshot(&self.scene))
    }
    pub fn lighting_mut(&mut self) -> Option<&mut ViewLighting> {
        self.lighting.as_deref_mut()
    }
    /// Immutable catalogs and both color/shadow variants are prepared once.
    /// When lighting is attached it must have dynamic casters enabled first.
    pub fn prepare_entities(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(), String> {
        if self.entities.is_some()
            || self.terrain_frames != 0
            || !self.resident.is_empty()
            || self.self_handles.iter().any(Option::is_some)
        {
            return Err(
                "entity pipelines must be prepared once before uploading scene geometry".into(),
            );
        }
        if self
            .lighting
            .as_ref()
            .is_some_and(|lighting| !lighting.owner.shadows().cache().config().dynamic_casters)
        {
            return Err("prepare lighting with dynamic model casters before entities".into());
        }
        let entities = ViewEntities::prepare_with_source_and_gi(
            device,
            queue,
            &self.renderer,
            self.lighting
                .as_ref()
                .map(|lighting| lighting.owner.shadows()),
            self.environment
                .as_ref()
                .map(|environment| environment.owner.gpu.atmosphere()),
            self.environment
                .as_ref()
                .and_then(|environment| environment.owner.gpu.gi()),
            self.lighting
                .as_ref()
                .is_some_and(|lighting| lighting.stats.authoritative_source),
        )
        .map_err(|error| error.to_string())?;
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(u64::from(entities.stats.pipeline_compilations));
        self.entities = Some(Box::new(entities));
        self.invalidate();
        Ok(())
    }
    pub fn entity_stats(&self) -> Option<EntityStats> {
        self.entities.as_ref().map(|entities| entities.stats)
    }
    pub fn entities_mut(&mut self) -> Option<&mut ViewEntities> {
        self.entities.as_deref_mut()
    }
    /// Diagnostic borrow of the last uploaded entity bank. It does not prove
    /// that a frame using these records was submitted, completed or presented.
    pub fn factory_uploaded_entities(&self) -> Option<&[render::entity_gpu::EntityInstance]> {
        self.entities
            .as_ref()
            .map(|owner| owner.gpu.instance_records())
    }
    pub fn prepare_props(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        limits: crate::props_source::PropsLimits,
    ) -> Result<(), String> {
        if self.props.is_some() || self.terrain_frames != 0 || !self.resident.is_empty() {
            return Err("block model pipelines must be prepared once during loading".into());
        }
        if self
            .lighting
            .as_ref()
            .is_some_and(|lighting| !lighting.owner.shadows().cache().config().dynamic_casters)
        {
            return Err("prepare dynamic shadow casters before block models".into());
        }
        let props = ViewProps::prepare(
            device,
            queue,
            &self.renderer,
            &mut self.scene,
            self.lighting.as_deref(),
            self.environment
                .as_ref()
                .map(|environment| &environment.owner),
            limits,
        )?;
        self.pipeline_compilations += u64::from(props.stats.pipeline_compilations);
        self.props = Some(Box::new(props));
        self.invalidate();
        Ok(())
    }
    /// Persistent equality token for the actually drawable block-model family.
    /// A late completion cannot qualify HDR captured under an older token.
    pub fn props_source_epoch(&self) -> u64 {
        self.props.as_ref().map_or(0, |p| p.gpu.source_epoch())
    }
    pub fn props_sources_valid(&self) -> bool {
        self.props.as_ref().is_none_or(|p| p.gpu.source_valid())
    }
    fn observe_props_source_epoch(&mut self, previous: u64) {
        if previous != self.props_source_epoch() {
            self.cached_presentation_available = false;
            self.renderer.invalidate();
            self.force_redraw = true;
        }
    }
    pub fn props_source(&self) -> Option<&crate::props_source::PropsSource> {
        self.props.as_ref().map(|props| &props.source)
    }
    /// Attach the bounded source only after the optional physical-water GPU
    /// resources have been prepared, and before the first scene upload.
    pub fn prepare_water(&mut self, config: water::Config) -> Result<(), String> {
        if self.water.is_some() || self.terrain_frames != 0 || !self.resident.is_empty() {
            return Err("water source must be prepared once during loading".into());
        }
        let gpu = self
            .environment
            .as_mut()
            .and_then(|environment| environment.owner.gpu.water_mut())
            .ok_or("prepare the optional environment water GPU before its source")?;
        self.water = Some(Box::new(
            water::ViewWater::prepare(&mut self.scene, gpu, config)
                .map_err(|error| error.to_string())?,
        ));
        Ok(())
    }
    pub fn water_stats(&self) -> Option<water::Stats> {
        self.water.as_ref().map(|water| water.stats())
    }
    pub fn take_water_task(
        &mut self,
    ) -> Result<Option<jobs::Dispatch<crate::water_source::WaterJob>>, String> {
        self.water.as_mut().map_or(Ok(None), |water| {
            water
                .take_task(&self.scene, self.tick)
                .map_err(|error| error.to_string())
        })
    }
    pub fn encode_water_task(
        &mut self,
        dispatch: &jobs::Dispatch<crate::water_source::WaterJob>,
    ) -> Result<Vec<u8>, String> {
        let water = self.water.as_mut().ok_or("water source not prepared")?;
        let before = water.stats().wire_halo_reads;
        let bytes = water
            .encode_task(dispatch)
            .map_err(|error| error.to_string())?;
        self.retained_foreground_work.record(telemetry::WorkCounts {
            cells_scanned: water.stats().wire_halo_reads - before,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(bytes)
    }
    pub fn requeue_water_task(
        &mut self,
        dispatch: jobs::Dispatch<crate::water_source::WaterJob>,
    ) -> Result<(), String> {
        self.water
            .as_mut()
            .ok_or("water source not prepared")?
            .requeue(&self.scene, dispatch)
            .map_err(|error| error.to_string())
    }
    pub fn complete_water_bytes(&mut self, id: jobs::JobId, bytes: &[u8]) -> Result<(), String> {
        self.water
            .as_mut()
            .ok_or("water source not prepared")?
            .complete_bytes(&self.scene, id, bytes)
            .map_err(|error| error.to_string())
    }
    pub fn fail_water_task(&mut self, id: jobs::JobId) -> Result<(), String> {
        self.water
            .as_mut()
            .ok_or("water source not prepared")?
            .fail_task(&self.scene, id)
            .map_err(|error| error.to_string())
    }
    pub fn request_water_model_update(&mut self) {
        if let Some(water) = &mut self.water {
            water.request_model_update();
        }
    }
    pub fn invalidate_water_tints(&mut self) -> Result<(), String> {
        if let Some(water) = &mut self.water {
            water
                .invalidate_tints(&self.scene)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub fn props_stats(&self) -> Option<PropsStats> {
        self.props.as_ref().map(|props| props.stats)
    }
    pub fn publish_remote_props(
        &mut self,
        frame: std::sync::Arc<net::props::Frame>,
    ) -> Result<bool, String> {
        self.props.as_mut().map_or(Ok(false), |props| {
            props.publish_public_frame(&self.scene, frame)
        })
    }
    pub fn remove_remote_props(&mut self, page: PageId) -> Result<bool, String> {
        self.props.as_mut().map_or(Ok(false), |props| {
            props
                .source
                .revoke_public_frame(page, self.tick)
                .map_err(|error| error.to_string())
        })
    }
    pub fn take_props_task(&mut self) -> Option<jobs::Dispatch<crate::props_source::PropsTask>> {
        self.props.as_mut()?.take_task(&self.scene, self.tick)
    }
    pub fn requeue_props_task(
        &mut self,
        task: jobs::Dispatch<crate::props_source::PropsTask>,
    ) -> Result<(), String> {
        self.props
            .as_mut()
            .ok_or("block models not prepared")?
            .requeue_task(task)
    }
    pub fn complete_props_bytes(&mut self, id: jobs::JobId, bytes: &[u8]) -> Result<(), String> {
        self.props
            .as_mut()
            .ok_or("block models not prepared")?
            .complete_bytes(&self.scene, id, bytes)
    }
    pub fn fail_props_task(&mut self, id: jobs::JobId) -> Result<(), String> {
        self.props
            .as_mut()
            .ok_or("block models not prepared")?
            .fail(&self.scene, id)
    }
    fn record_props_work(&mut self, work: crate::props_source::PropsWork) {
        self.retained_worker_work.record(telemetry::WorkCounts {
            cells_scanned: work.worker_cells,
            ..telemetry::WorkCounts::ZERO
        });
        self.retained_foreground_work.record(telemetry::WorkCounts {
            cells_scanned: work.owner_light_reads,
            upload_bytes: work.upload_bytes,
            ..telemetry::WorkCounts::ZERO
        });
    }
    #[allow(clippy::too_many_arguments)] // Explicit loading resource/profile context.
    pub fn prepare_environment(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        transfer: render::OutputTransfer,
        profile: EnvironmentProfile,
        atlas: &render::terrain_gpu::AtlasImage,
    ) -> Result<(), String> {
        if self.environment.is_some()
            || self.recipe_boards.is_some()
            || self.entities.is_some()
            || self.props.is_some()
            || self.horizon.is_some()
            || self.terrain_frames != 0
            || !self.resident.is_empty()
            || self.self_handles.iter().any(Option::is_some)
        {
            return Err("environment must be prepared once during loading".into());
        }
        let cloud_quality = profile
            .graphics
            .clouds
            .map_or(0, |cloud| match cloud.quality {
                render::cloud_gpu::CloudQuality::LayerA => 1,
                render::cloud_gpu::CloudQuality::VolumeB => 2,
            });
        let gi_quality = profile
            .graphics
            .gi
            .map_or(0, |gi| if gi.rays_per_probe == 64 { 2 } else { 1 });
        let reflection_quality =
            profile
                .graphics
                .water
                .map_or(0, |water| match water.reflection_steps {
                    0 => 0,
                    32 => 1,
                    _ => 2,
                });
        let eye = self.camera_parameters().eye;
        let mut owner = ViewEnvironment::prepare(
            device,
            queue,
            &mut self.renderer,
            &mut self.scene,
            self.lighting.as_deref(),
            atlas,
            width,
            height,
            transfer,
            profile,
            eye,
            lighting::SUN,
        )
        .map_err(|error| error.to_string())?;
        let setup = self.renderer.take_work();
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(setup.pipeline_compilations)
            .saturating_add(u64::from(owner.gpu.take_startup_pipeline_compilations()));
        self.environment = Some(Box::new(PreparedEnvironment {
            owner,
            queue: queue.clone(),
            width,
            height,
            cloud_quality,
            gi_quality,
            reflection_quality,
        }));
        self.invalidate();
        Ok(())
    }
    pub fn environment_stats(&self) -> Option<EnvironmentStats> {
        self.environment
            .as_ref()
            .map(|environment| environment.owner.stats)
    }
    pub fn environment_mut(&mut self) -> Option<&mut ViewEnvironment> {
        self.environment
            .as_deref_mut()
            .map(|environment| &mut environment.owner)
    }
    pub fn take_gi_source_task(
        &mut self,
    ) -> Result<Option<jobs::Dispatch<crate::gi_source::GiBakeTask>>, String> {
        self.environment.as_mut().map_or(Ok(None), |environment| {
            environment
                .owner
                .take_source_task(self.tick)
                .map_err(|error| error.to_string())
        })
    }
    pub fn encode_gi_source_task(
        &mut self,
        task: &jobs::Dispatch<crate::gi_source::GiBakeTask>,
    ) -> Result<Vec<u8>, String> {
        let bytes = self
            .environment
            .as_mut()
            .ok_or("GI environment not prepared")?
            .owner
            .encode_source_task(task)
            .map_err(|error| error.to_string())?;
        self.retained_foreground_work.record(telemetry::WorkCounts {
            cells_scanned: task.payload.wire_samples(),
            ..telemetry::WorkCounts::ZERO
        });
        Ok(bytes)
    }
    pub fn requeue_gi_source_task(
        &mut self,
        task: jobs::Dispatch<crate::gi_source::GiBakeTask>,
    ) -> Result<(), String> {
        self.environment
            .as_mut()
            .ok_or("GI environment not prepared")?
            .owner
            .requeue_source_task(task)
            .map_err(|error| error.to_string())
    }
    pub fn complete_gi_source_bytes(
        &mut self,
        id: jobs::JobId,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.environment
            .as_mut()
            .ok_or("GI environment not prepared")?
            .owner
            .complete_source_bytes(id, bytes)
            .map_err(|error| error.to_string())
    }
    pub fn fail_gi_source_task(&mut self, id: jobs::JobId) -> Result<(), String> {
        self.environment
            .as_mut()
            .ok_or("GI environment not prepared")?
            .owner
            .fail_source_task(id)
            .map_err(|error| error.to_string())
    }
    /// Uses only the timing nodes encoded for this submitted image. In
    /// particular, a post-only image never copies a previous terrain query.
    pub fn encode_gpu_capture(
        &self,
        capture: &mut crate::capture::FrameGpuCapture,
        encoder: &mut wgpu::CommandEncoder,
        frame: u64,
    ) -> Option<crate::capture::CaptureTicket> {
        let shadows = self
            .encoded_shadows
            .then(|| {
                self.lighting
                    .as_ref()
                    .and_then(|lighting| lighting.owner.shadows().timing_source())
            })
            .flatten();
        if let Some(environment) = &self.environment {
            capture.encode_sources(
                environment
                    .owner
                    .gpu
                    .timing_sources(&self.renderer)
                    .into_iter()
                    .flatten()
                    .chain(shadows)
                    .chain(
                        self.temporal
                            .as_ref()
                            .map(|t| t.timing_sources())
                            .into_iter()
                            .flatten()
                            .flatten(),
                    ),
                encoder,
                frame,
            )
        } else if self.encoded_terrain {
            capture.encode_terrain_and(&self.renderer, shadows.as_slice(), encoder, frame)
        } else {
            capture.encode_sources(shadows, encoder, frame)
        }
    }
    fn environment_state(&self, eye: [f32; 3]) -> render::environment_gpu::EnvironmentState {
        let clock = self.sun_clock();
        let mut state = ViewEnvironment::state(
            eye,
            clock.map_or(
                (self.tick.0 as f64 * sim::TICK_SECONDS + self.accumulator) as f32,
                |clock| clock.animation_seconds,
            ),
            self.frame_delta_seconds,
            self.tick,
            clock.map_or(lighting::SUN, |clock| clock.sun_direction),
            self.player.eye_water,
        );
        if let Some(clock) = clock {
            state.sun_intensity = clock.sun_intensity;
        }
        state
    }
    /// Install public interpolated records without scanning the world. Returns
    /// false while this loading profile has no entity renderer attached.
    pub fn set_remote_entities(
        &mut self,
        source: &[rules::entities::mob::Instance],
        space: SpaceId,
        origin: [i64; 3],
    ) -> Result<bool, String> {
        let Some(entities) = &mut self.entities else {
            return Ok(false);
        };
        let work = entities
            .set_remote_frame(source, &self.scene, space, origin)
            .map_err(|error| format!("public entity frame: {error:?}"))?;
        self.retained_foreground_work.record(telemetry::WorkCounts {
            cells_scanned: work.light_reads,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(true)
    }
    pub fn temporal_entity_identities_prepared(&self) -> bool {
        self.entities
            .as_ref()
            .is_some_and(|entities| entities.published_temporal_identities().is_some())
    }
    pub fn set_remote_entities_with_identities(
        &mut self,
        source: &[rules::entities::mob::Instance],
        identities: &[render::presented_motion::ObjectKey],
        space: SpaceId,
        origin: [i64; 3],
    ) -> Result<bool, String> {
        let Some(entities) = &mut self.entities else {
            return Ok(false);
        };
        let work = entities
            .set_remote_frame_with_identities(source, identities, &self.scene, space, origin)
            .map_err(|error| format!("public temporal entity frame: {error:?}"))?;
        self.retained_foreground_work.record(telemetry::WorkCounts {
            cells_scanned: work.light_reads,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(true)
    }
    fn lighting_center(&self) -> [f32; 3] {
        if self.active {
            self.player.pos.map(|value| value as f32)
        } else {
            let center = self.scene.config().center;
            [center.x as f32, center.y as f32, center.z as f32]
        }
    }

    pub fn set_key(&mut self, key: InputKey, down: bool) {
        self.set_key_inner(key, down, None);
    }
    /// Capture the actual pointer modifier before keyup/transport sampling.
    pub fn set_place_key(&mut self, down: bool, sneaking: bool) {
        let rising = down && !self.keys[InputKey::Place as usize];
        self.set_key(InputKey::Place, down);
        if rising {
            self.place_sneaking = sneaking;
        }
    }
    /// Physical Shift is independent of the player's rebindable Sneak action.
    pub fn set_tool_mode_key(&mut self, down: bool, back: bool) {
        self.set_key_inner(InputKey::ToolMode, down, Some(back));
    }
    fn set_key_inner(&mut self, key: InputKey, down: bool, tool_back: Option<bool>) {
        let rising = down && !self.keys[key as usize];
        if key == InputKey::Place && self.recipe_board_place(down, rising) {
            self.keys[key as usize] = false;
            self.place_pressed = false;
            return;
        }
        self.keys[key as usize] = down;
        if down {
            self.activate_player();
        }
        if !rising {
            return;
        }
        if key == InputKey::Place {
            self.place_pressed = true;
            self.place_sneaking = self.keys[InputKey::Sneak as usize];
        }
        if self.authoritative_remote() && key == InputKey::ToolMode {
            if self.active && self.remote_tool_modes.len() < 32 {
                self.remote_tool_modes
                    .push_back(tool_back.unwrap_or(self.keys[InputKey::Sneak as usize]));
            }
            return;
        }
        if self.authoritative_remote() && matches!(key, InputKey::Flight | InputKey::Noclip) {
            if self.active && self.remote_controls.len() < 32 {
                self.remote_controls.push_back(key);
            }
            return;
        }
        match key {
            InputKey::Flight => {
                let _ = self.player.set_flight(!self.player.flying);
            }
            InputKey::Noclip => {
                if self
                    .player
                    .set_debug_noclip(
                        !self.player.noclip,
                        Permissions {
                            server_debug: false,
                        },
                    )
                    .is_err()
                {
                    self.denied_debug_requests = self.denied_debug_requests.saturating_add(1);
                }
            }
            InputKey::Camera => {
                self.camera_mode = match self.camera_mode {
                    CameraMode::FirstPerson => CameraMode::RightShoulder,
                    CameraMode::RightShoulder => CameraMode::FirstPerson,
                }
            }
            _ => {}
        }
    }
    pub fn clear_keys(&mut self) {
        self.keys.fill(false);
        self.recipe_board_place(false, false);
        self.place_pressed = false;
        self.place_sneaking = false;
    }
    pub fn input_buttons(&self) -> u32 {
        self.keys
            .iter()
            .enumerate()
            .fold(0, |bits, (index, pressed)| {
                bits | (u32::from(*pressed) << index)
            })
    }
    /// Explicit standalone benchmark setup. Reset the interpolation phase and
    /// actor motion after warmup so pipeline/worker startup timing cannot shift
    /// the deterministic input route by part of a simulation tick.
    pub fn start_benchmark_route(&mut self) -> Result<(), String> {
        if self.authoritative_remote() || self.edit_revision != 0 || !self.is_ready() {
            return Err("benchmark requires an unedited, ready standalone fixture".into());
        }
        self.activate_player();
        if !self.active {
            return Err("benchmark fixture has no known safe spawn".into());
        }
        self.player = Player::new(self.player.pos);
        self.player.yaw = 135.;
        self.accumulator = 0.;
        self.camera_mode = CameraMode::FirstPerson;
        self.clear_keys();
        self.invalidate();
        Ok(())
    }
    pub fn mouse_delta(&mut self, dx: f64, dy: f64) {
        self.activate_player();
        if self.authoritative_remote() && !self.active {
            return;
        }
        self.player.turn(dx, dy, 0.5);
        if self.authoritative_remote() {
            self.player.yaw = self.player.yaw.rem_euclid(360.0);
            self.remote_look = Some([self.player.yaw, self.player.pitch]);
        }
    }
    pub fn activate_player(&mut self) {
        self.activation_requested = true;
        if self.authoritative_remote() {
            return;
        }
        if self.active || !self.is_ready() {
            return;
        }
        let config = self.scene.config();
        let center = config.center;
        let min_y = (center.page().y - i32::from(config.vertical_radius)) * 16;
        let max_y = (center.page().y + i32::from(config.vertical_radius) + 1) * 16 - 3;
        // Spawn search is a one-time loaded-data query, never procedural generation.
        for radius in 0_i32..=4 {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if dx.abs().max(dz.abs()) != radius {
                        continue;
                    }
                    let (x, z) = (center.x + dx, center.z + dz);
                    for y in (min_y..=max_y).rev() {
                        let at = |y| {
                            self.scene.world().read(BlockPos {
                                space: center.space,
                                x,
                                y,
                                z,
                            })
                        };
                        let CellRead::Known(ground) = at(y) else {
                            continue;
                        };
                        if rules::blocks::is_leaves(ground.id())
                            || !rules::properties(ground).collidable
                        {
                            continue;
                        }
                        if !matches!(at(y+1), CellRead::Known(cell) if !rules::properties(cell).collidable)
                            || !matches!(at(y+2), CellRead::Known(cell) if !rules::properties(cell).collidable)
                        {
                            continue;
                        }
                        if self
                            .player
                            .teleport([f64::from(x) + 0.5, f64::from(y) + 1.0, f64::from(z) + 0.5])
                            .is_ok()
                        {
                            self.player.yaw = 135.0;
                            self.active = true;
                            return;
                        }
                    }
                }
            }
        }
    }
    pub fn advance_input(&mut self, elapsed_seconds: f64) -> Result<(), String> {
        if !elapsed_seconds.is_finite() || elapsed_seconds < 0.0 {
            return Err("invalid frame duration".into());
        }
        self.frame_delta_seconds = elapsed_seconds.min(0.1) as f32;
        if !self.replay_external_clock {
            self.replay_time_ns = self
                .replay_time_ns
                .saturating_add((elapsed_seconds * 1_000_000_000.).round() as u64);
            self.movement_replay
                .set_time(self.replay_time_ns)
                .map_err(str::to_owned)?;
        }
        let elapsed = elapsed_seconds.min(0.25);
        self.dropped_time_seconds += elapsed_seconds - elapsed;
        self.accumulator += elapsed;
        if self.activation_requested && !self.active {
            self.activate_player();
        }
        while self.accumulator >= sim::TICK_SECONDS {
            self.accumulator -= sim::TICK_SECONDS;
            self.tick.0 = self
                .tick
                .0
                .checked_add(1)
                .ok_or("simulation tick exhausted")?;
            if self.authoritative_remote() {
                continue;
            }
            if self.active {
                let query = WorldQuery::new(self.scene.world(), SpaceId(0));
                self.movement_replay.tick(
                    &mut self.player,
                    &query,
                    Keys {
                        fwd: self.keys[InputKey::Forward as usize],
                        back: self.keys[InputKey::Back as usize],
                        left: self.keys[InputKey::Left as usize],
                        right: self.keys[InputKey::Right as usize],
                        jump: self.keys[InputKey::Jump as usize],
                        sneak: self.keys[InputKey::Sneak as usize],
                        sprint: self.keys[InputKey::Sprint as usize],
                    },
                    false,
                );
                let attack = self.keys[InputKey::Attack as usize];
                let place_pressed = std::mem::take(&mut self.place_pressed);
                let place = self.keys[InputKey::Place as usize] || place_pressed;
                if attack || place {
                    self.player.swing_hand();
                }
                if let Ok(Some(edit)) = self.interactions.prepare_tick(
                    self.scene.world(),
                    &self.player,
                    self.tick,
                    attack,
                    place,
                ) {
                    self.apply_demo_edit(edit)?;
                }
                let position = self.player.pos.map(|v| v.floor() as i32);
                let center = BlockPos {
                    space: SpaceId(0),
                    x: position[0],
                    y: position[1],
                    z: position[2],
                };
                if center.page() != self.scene.config().center.page() {
                    self.scene.set_center(center).map_err(|e| e.to_string())?;
                }
            }
        }
        if let Some(lighting) = &mut self.lighting {
            lighting
                .advance_cpu(&mut self.scene)
                .map_err(|error| format!("client lighting CPU: {error}"))?;
        }
        let props_epoch = self.props_source_epoch();
        if let Some(props) = &mut self.props {
            props.set_visual_alpha(self.accumulator / sim::TICK_SECONDS);
            let work = props
                .advance_cpu(&mut self.scene, self.tick)
                .map_err(|error| format!("block models CPU: {error}"))?;
            self.record_props_work(work);
        }
        self.observe_props_source_epoch(props_epoch);
        if let Some(water) = &mut self.water {
            #[cfg(not(target_arch = "wasm32"))]
            let stats = water.pump_native(&mut self.scene, self.tick);
            #[cfg(target_arch = "wasm32")]
            let stats = water.phase(&mut self.scene, self.tick);
            let stats = stats.map_err(|error| format!("physical water CPU: {error}"))?;
            self.retained_worker_work.record(telemetry::WorkCounts {
                cells_scanned: stats.work.worker_cells + stats.work.worker_halo_samples,
                quads_emitted: stats.work.worker_quads,
                ..telemetry::WorkCounts::ZERO
            });
        }
        self.advance_horizon_cpu()?;
        let source_camera = self
            .environment
            .as_ref()
            .filter(|environment| environment.owner.gpu.gi().is_some())
            .map(|environment| {
                let parameters = self.camera_parameters();
                (
                    parameters.eye,
                    parameters.projection(environment.width, environment.height),
                )
            });
        if let (Some(environment), Some((eye, camera))) = (&mut self.environment, source_camera) {
            let stats = environment
                .owner
                .phase(&mut self.scene, eye, &camera, self.tick)
                .map_err(|error| error.to_string())?;
            self.record_environment_work(stats, 0);
        }
        Ok(())
    }
    fn preflight_edit(&self, position: BlockPos) -> bool {
        if !self.resident.contains_key(&position.page()) {
            return false;
        }
        if self.edit_shell.can_mark_cells(&[position]).is_err() {
            return false;
        }
        // AO of the radius-one owner stencil reads one additional cell rim.
        // No generation is permitted in this bounded preflight.
        for dy in -2..=2 {
            for dz in -2..=2 {
                for dx in -2..=2 {
                    let Some(x) = position.x.checked_add(dx) else {
                        return false;
                    };
                    let Some(y) = position.y.checked_add(dy) else {
                        return false;
                    };
                    let Some(z) = position.z.checked_add(dz) else {
                        return false;
                    };
                    if self.scene.world().read(BlockPos {
                        space: position.space,
                        x,
                        y,
                        z,
                    }) == CellRead::Unknown
                    {
                        return false;
                    }
                }
            }
        }
        true
    }
    pub const fn edit_revision(&self) -> u64 {
        self.edit_revision
    }
    pub const fn rendered_edit_revision(&self) -> u64 {
        self.rendered_edit_revision
    }
    fn note_edit_change(&mut self, change: &world::WorldChange) -> Result<(), String> {
        if change.cells.is_empty()
            || !change
                .channels
                .intersects(core_types::ChannelMask::SHAPE | core_types::ChannelMask::OPACITY)
        {
            return Ok(());
        }
        self.edit_revision = self
            .edit_revision
            .checked_add(1)
            .ok_or("edit revision exhausted")?;
        self.edit_shell
            .mark_changes(change)
            .map_err(|error| error.to_string())?;
        if let Some(lighting) = &mut self.lighting {
            for page in &change.pages {
                lighting
                    .dirty(page.page)
                    .map_err(|error| error.to_string())?;
            }
        }
        self.mesh_hold_until = Tick(self.tick.0.saturating_add(self.demo_mesh_delay_ticks));
        self.edit_pending_since.get_or_insert(self.terrain_frames);
        self.invalidate();
        Ok(())
    }
    fn apply_demo_edit(&mut self, edit: PreparedDemoEdit) -> Result<(), String> {
        if !self.preflight_edit(edit.position) {
            self.edit_rejected = self.edit_rejected.saturating_add(1);
            return Ok(());
        }
        let change = match self.scene.apply_transaction(edit.transaction) {
            Ok(change) => change,
            Err(
                crate::SceneError::OverrideLimit
                | crate::SceneError::PageLimit
                | crate::SceneError::World(
                    world::CommitError::ConflictingVersion { .. }
                    | world::CommitError::UnknownPage(_)
                    | world::CommitError::ChangeTooLarge,
                ),
            ) => {
                self.edit_rejected = self.edit_rejected.saturating_add(1);
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        self.note_edit_change(&change)?;
        self.interactions.published(&change);
        Ok(())
    }
    /// Explicit diagnostics delay final greedy publication without blocking a
    /// worker or foreground thread; one tick is50ms. Production default is zero.
    pub fn set_demo_mesh_delay_ticks(&mut self, ticks: u64) {
        self.demo_mesh_delay_ticks = ticks.min(200);
    }
    /// Diagnostic barrier isolates the fast image oracle from worker/GPU timing.
    /// The bounded ready queue retains final results; no thread blocks.
    pub fn set_demo_mesh_publication_paused(&mut self, paused: bool) {
        self.demo_mesh_publication_paused = paused;
    }
    /// Raster diagnostic only: use identical unit tessellation for both the
    /// initial base and every final replacement. Production retains greedy mesh.
    pub fn set_demo_unit_mesh(&mut self, enabled: bool) -> Result<(), String> {
        if self.terrain_frames != 0 || !self.resident.is_empty() {
            return Err("unit-mesh diagnostic must be selected during loading".into());
        }
        if enabled && self.demo_gpu_unit_mesh {
            return Err("CPU and GPU unit-mesh controls must be tested separately".into());
        }
        self.demo_unit_mesh = enabled;
        Ok(())
    }
    pub const fn demo_unit_mesh(&self) -> bool {
        self.demo_unit_mesh
    }
    pub fn prepare_gpu_unit_mesh(&mut self) -> Result<u32, String> {
        if self.demo_unit_mesh {
            return Err("CPU and GPU unit-mesh controls must be tested separately".into());
        }
        if self.terrain_frames != 0
            || !self.resident.is_empty()
            || self.self_handles.iter().any(Option::is_some)
        {
            return Err("GPU unit rasterization must be prepared during loading".into());
        }
        if self.demo_gpu_unit_mesh {
            return Ok(self.renderer.geometry().max_pages);
        }
        let geometry = self.renderer.geometry();
        let limit = geometry
            .max_pages
            .min(geometry.max_quads / render::terrain_mesh::MAX_PAGE_QUADS as u32);
        if self.scene.visible_pages().count() + PART_COUNT > limit as usize {
            return Err("GPU unit raster profile cannot retain all requested pages".into());
        }
        let limit = self
            .renderer
            .prepare_unit_rasterization()
            .map_err(|error| error.to_string())?;
        self.demo_gpu_unit_mesh = true;
        Ok(limit)
    }
    pub const fn demo_gpu_unit_mesh(&self) -> bool {
        self.demo_gpu_unit_mesh
    }
    pub fn gpu_object_limit(&self) -> u32 {
        self.renderer.geometry().max_pages
    }
    /// Standalone demo reconciliation only; multiplayer uses server changes.
    pub fn rollback_last_demo_edit(&mut self) -> Result<bool, String> {
        let Some(edit) = self
            .interactions
            .prepare_rollback(self.scene.world(), self.tick)
            .map_err(|error| format!("demo rollback: {error:?}"))?
        else {
            return Ok(false);
        };
        if !self.preflight_edit(edit.position) {
            return Ok(false);
        }
        let change = self
            .scene
            .apply_transaction(edit.transaction)
            .map_err(|error| error.to_string())?;
        self.note_edit_change(&change)?;
        self.interactions.rolled_back();
        Ok(true)
    }
    pub fn render_scale(&self) -> u32 {
        self.render_scale
    }
    pub fn render_extent(&self) -> [u32; 2] {
        self.renderer.extent()
    }
    fn desired_render_extent(&self, width: u32, height: u32) -> Result<(u32, [u32; 2]), String> {
        let scale = if self.environment.is_some() {
            self.requested_settings.as_ref().map_or(100, |settings| {
                settings.get(ui::settings::Setting::Scale) as u32
            })
        } else {
            100
        };
        if !(85..=100).contains(&scale) {
            return Err("render scale must be85..100 percent".into());
        }
        let scaled = |axis: u32| (u64::from(axis) * u64::from(scale)).div_ceil(100) as u32;
        Ok((scale, [scaled(width), scaled(height)]))
    }
    /// Read-only resource preflight for the requested physical extent. The
    /// window shells call this before changing their surface/canvas, so a
    /// temporal allocation rejection retains the current View and session.
    pub fn validate_resize(
        &self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let (_, extent) = self.desired_render_extent(width, height)?;
        self.preflight_temporal_resize(device, extent)
    }
    /// Surface/UI dimensions stay native. Prepared HDR/depth/environment targets
    /// use the selected scale; existing final Post expands them to the surface.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let (scale, [render_width, render_height]) = self.desired_render_extent(width, height)?;
        self.preflight_temporal_resize(device, [render_width, render_height])?;
        self.invalidate();
        if let Some(environment) = &mut self.environment {
            if environment.width != render_width || environment.height != render_height {
                self.cached_presentation_available = false;
            }
            let mut budget = UploadBudget::new(16 * 1024 * 1024);
            let before = budget.remaining_bytes;
            environment
                .owner
                .resize(
                    device,
                    &environment.queue,
                    &mut self.renderer,
                    render_width,
                    render_height,
                    &mut budget,
                )
                .map_err(|error| error.to_string())?;
            environment
                .owner
                .gpu
                .post_mut()
                .set_output_extent_with_device(device, width, height)
                .map_err(|error| error.to_string())?;
            environment.width = render_width;
            environment.height = render_height;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: before - budget.remaining_bytes,
                ..telemetry::WorkCounts::ZERO
            });
            self.render_scale = scale;
            self.resize_temporal(device)?;
            Ok(())
        } else {
            self.renderer
                .resize(device, width, height)
                .map_err(|error| error.to_string())?;
            self.render_scale = 100;
            Ok(())
        }
    }
    fn refresh_patches(&mut self) -> Result<(), String> {
        if self.edit_shell.dirty_pages().next().is_none() {
            return Ok(());
        }
        let scene = &self.scene;
        let patches = match self
            .edit_shell
            .rebuild_owner_updates(scene.world(), |page, x, z| {
                scene.biome_tint(page, x, z).unwrap_or(u16::MAX)
            }) {
            Ok(patches) => patches,
            Err(ShellError::UnknownPage(_)) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        for patch in patches {
            self.retained_foreground_work.record(patch.work.work);
            self.pending_patches.retain(|old| old.page != patch.page);
            self.pending_patches.push_back(patch);
        }
        Ok(())
    }
    fn upload_model(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        if let Some(entities) = &mut self.entities {
            let reads_before = entities.stats.light_reads;
            let update = match entities.upload_local(
                queue,
                self.active.then_some(&self.player),
                self.accumulator / sim::TICK_SECONDS,
                self.camera_mode,
                &self.scene,
                SpaceId(0),
                [0; 3],
                budget,
            ) {
                Ok(update) => update,
                Err(render::entity_gpu::EntityError::UploadBudget) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            };
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: update.work.upload_bytes,
                cells_scanned: entities.stats.light_reads - reads_before,
                ..telemetry::WorkCounts::ZERO
            });
            if update.changed {
                self.renderer.invalidate();
                self.invalidate();
            }
            return Ok(());
        }
        let mut pose = SelfModel::poses(
            &self.player,
            self.accumulator / sim::TICK_SECONDS,
            self.camera_mode,
        );
        pose.visible &= self.active;
        // Hidden animation cannot dirty a static first-person frame.
        if !pose.visible && self.self_uploaded_pose.is_some_and(|old| !old.visible) {
            return Ok(());
        }
        if self.self_uploaded_pose == Some(pose) {
            return Ok(());
        }
        // Worst case:6uploads(224B each)+6enabledflags(128B) or12objectupdates.
        if budget.remaining_bytes < 3 * 1024 {
            return Ok(());
        }
        for (index, model) in pose.transforms.iter().enumerate() {
            let handle = if let Some(handle) = self.self_handles[index] {
                self.renderer
                    .update_transform(queue, handle, *model, budget)
                    .map_err(|error| error.to_string())?;
                handle
            } else {
                let handle = self
                    .renderer
                    .upload_page(
                        queue,
                        None,
                        1,
                        *model,
                        &self.self_model.meshes()[index],
                        budget,
                    )
                    .map_err(|error| error.to_string())?;
                self.self_handles[index] = Some(handle);
                handle
            };
            self.renderer
                .set_enabled(queue, handle, pose.visible, budget)
                .map_err(|error| error.to_string())?;
            if pose.visible
                && let Some(lighting) = &mut self.lighting
            {
                let bounds = lighting::rigid_bounds(*model, PART_EXTENTS[index].map(f32::from))
                    .map_err(|error| error.to_string())?;
                let center = std::array::from_fn::<_, 3, _>(|axis| {
                    ((bounds.min[axis] + bounds.max[axis]) * 0.5).floor() as i32
                });
                lighting
                    .register_rigid(
                        handle,
                        BlockPos {
                            space: SpaceId(0),
                            x: center[0],
                            y: center[1],
                            z: center[2],
                        },
                        bounds,
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        self.self_uploaded_pose = Some(pose);
        Ok(())
    }
    pub fn upload_ready(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        let before = budget.remaining_bytes;
        self.flush_post_settings(queue, budget)?;
        // Keep one allowance for the complete frame. Small environment/camera
        // uniforms cannot be starved behind a ready terrain replacement.
        let environment_reserve = budget.remaining_bytes.min(4096);
        budget.remaining_bytes -= environment_reserve;
        while budget.remaining_bytes >= 256 {
            let Some(page) = self.scene.take_hidden_page() else {
                break;
            };
            if let Some(overlay) = self.overlays.remove(&page) {
                self.renderer
                    .release_page(queue, overlay.handle, budget)
                    .map_err(|error| error.to_string())?;
                if let Some(lighting) = &mut self.lighting {
                    lighting.unregister(overlay.handle);
                }
            }
            if let Some(resident) = self.resident.remove(&page) {
                self.renderer
                    .release_page(queue, resident.handle, budget)
                    .map_err(|error| error.to_string())?;
                if let Some(lighting) = &mut self.lighting {
                    lighting.unregister(resident.handle);
                }
                self.page_links_dirty = true;
            }
            self.edit_shell.clear_page(page);
            self.pending_patches.retain(|patch| patch.page != page);
        }
        self.refresh_patches()?;
        while let Some(patch) = self.pending_patches.pop_front() {
            if !patch.is_current(self.scene.world()) {
                continue;
            }
            let Some(base) = self.resident.get(&patch.page) else {
                // No old image exists to mask. Scene's independent candidate
                // certificate will initialize any post-capture owners atomically
                // with this page's first base; do not rebuild it every idle frame.
                self.edit_shell.clear_page(patch.page);
                continue;
            };
            let origin =
                [patch.page.x, patch.page.y, patch.page.z].map(|v| (f64::from(v) * 16.0) as f32);
            let revision = self
                .gpu_revision
                .checked_add(1)
                .ok_or("GPU representation revision exhausted")?;
            let alias_reserve = if self.lighting.is_some() { 4 } else { 0 };
            if budget.remaining_bytes < alias_reserve {
                self.pending_patches.push_front(patch);
                break;
            }
            budget.remaining_bytes -= alias_reserve;
            let result = self.renderer.publish_owner_patches(
                queue,
                base.handle,
                self.overlays.get(&patch.page).map(|o| o.handle),
                revision,
                translation(origin),
                patch.mask.words(),
                &patch.updates,
                budget,
            );
            budget.remaining_bytes += alias_reserve;
            match result {
                Ok(handle) => {
                    self.gpu_revision = revision;
                    self.register_lit_page(queue, patch.page, base.handle, Some(handle), budget)?;
                    if !self
                        .edit_shell
                        .ack_owner_updates(&patch, self.scene.world())
                    {
                        return Err(
                            "owner update certificate changed during atomic publication".into()
                        );
                    }
                    let previous = self.overlays.remove(&patch.page);
                    let (mut owner_faces, mut quads, old_mask) = previous.map_or_else(
                        || {
                            (
                                Box::new([0; 4096]),
                                0,
                                crate::edit_shell::OwnerMask::default(),
                            )
                        },
                        |old| (old.owner_faces, old.quads, old.mask),
                    );
                    for update in &patch.updates {
                        let index = usize::from(update.owner);
                        if old_mask.contains(
                            core_types::LocalIdx::new(update.owner).expect("validated owner"),
                        ) {
                            quads = quads.saturating_sub(usize::from(owner_faces[index]));
                        }
                        owner_faces[index] = update
                            .classes
                            .iter()
                            .filter(|class| **class != render::terrain_mesh::MeshClass::Empty)
                            .count() as u8;
                        quads += usize::from(owner_faces[index]);
                    }
                    self.overlays.insert(
                        patch.page,
                        Overlay {
                            handle,
                            inputs: patch.inputs,
                            mask: patch.mask,
                            quads,
                            owner_faces,
                        },
                    );
                    self.edit_overlay_uploads = self.edit_overlay_uploads.saturating_add(1);
                }
                Err(TerrainError::UploadBudget | TerrainError::Capacity) => {
                    self.pending_patches.push_front(patch);
                    break;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        self.upload_model(queue, budget)?;
        while !self.demo_mesh_publication_paused && self.tick.0 >= self.mesh_hold_until.0 {
            let Some(mut composite) = self.scene.take_composite() else {
                break;
            };
            if self.demo_unit_mesh
                && [
                    &composite.base.mesh.opaque,
                    &composite.base.mesh.cutout,
                    &composite.base.mesh.water,
                ]
                .into_iter()
                .flatten()
                .any(|quad| quad.size() != [1, 1])
            {
                let mut unit_mesh = render::terrain_mesh::PageMesh::default();
                let expanded = render::terrain_unit::expand(&composite.base.mesh, &mut unit_mesh)
                    .map_err(|error| format!("unit-mesh diagnostic: {error:?}"))?;
                self.retained_foreground_work.record(telemetry::WorkCounts {
                    quads_emitted: expanded.emitted_quads,
                    ..telemetry::WorkCounts::ZERO
                });
                composite.base.mesh = unit_mesh;
            }
            let page = composite.base.page;
            if self
                .edit_shell
                .can_replace_page_mask(page, &composite.changed_owners)
                .is_err()
            {
                self.scene
                    .return_composite(composite)
                    .map_err(|_| "cannot retain quota-blocked composite".to_string())?;
                break;
            }
            let reusable = self.overlays.get(&page).is_some_and(|old| {
                self.scene.world().matches_inputs(&old.inputs)
                    && composite
                        .changed_owners
                        .words()
                        .iter()
                        .zip(old.mask.words())
                        .all(|(new, old)| new & !old == 0)
            });
            if !reusable
                && self.overlays.contains_key(&page)
                && self.edit_shell.dirty_pages().any(|dirty| dirty == page)
            {
                self.scene
                    .return_composite(composite)
                    .map_err(|_| "cannot retain owner-blocked composite".to_string())?;
                break;
            }
            let patch = if composite.changed_owners.is_empty() || reusable {
                None
            } else {
                let scene = &self.scene;
                match crate::edit_shell::build_owner_updates(
                    page,
                    &composite.changed_owners,
                    &composite.changed_owners,
                    scene.world(),
                    &|page, x, z| scene.biome_tint(page, x, z).unwrap_or(u16::MAX),
                ) {
                    Ok(patch) => {
                        self.retained_foreground_work.record(patch.work.work);
                        Some(patch)
                    }
                    Err(ShellError::UnknownPage(_)) => {
                        self.scene
                            .return_composite(composite)
                            .map_err(|_| "cannot retain unknown composite".to_string())?;
                        break;
                    }
                    Err(error) => {
                        self.scene
                            .return_composite(composite)
                            .map_err(|_| "cannot retain failed composite".to_string())?;
                        return Err(error.to_string());
                    }
                }
            };
            if !self.scene.composite_is_current(&composite)
                || patch
                    .as_ref()
                    .is_some_and(|patch| !patch.is_current(self.scene.world()))
            {
                self.scene
                    .return_composite(composite)
                    .map_err(|_| "cannot retain changed composite".to_string())?;
                break;
            }
            let prior = self.resident.get(&page).copied();
            let origin = [page.x, page.y, page.z].map(|v| (f64::from(v) * 16.0) as f32);
            let revision = self
                .gpu_revision
                .checked_add(1)
                .ok_or("GPU representation revision exhausted")?;
            let alias_reserve = if self.lighting.is_some() { 4 } else { 0 };
            if budget.remaining_bytes < alias_reserve {
                self.scene
                    .return_composite(composite)
                    .map_err(|_| "cannot retain lighting-budget-blocked composite".to_string())?;
                break;
            }
            budget.remaining_bytes -= alias_reserve;
            let result = self.renderer.publish_owner_composite(
                queue,
                prior.map(|resident| resident.handle),
                revision,
                translation(origin),
                &composite.base.mesh,
                self.overlays.get(&page).map(|overlay| overlay.handle),
                composite.changed_owners.words(),
                patch.as_ref().map_or(&[], |patch| patch.updates.as_slice()),
                budget,
            );
            budget.remaining_bytes += alias_reserve;
            match result {
                Ok((handle, overlay)) => {
                    // No world mutation can interleave this single foreground
                    // call. The certificate is acknowledged only after the
                    // atomic GPU operation succeeded.
                    if !self.scene.ack_composite(&composite) {
                        return Err(
                            "composite certificate changed during atomic publication".into()
                        );
                    }
                    self.gpu_revision = revision;
                    if prior.map(|resident| resident.handle) != Some(handle) {
                        self.page_links_dirty = true;
                    }
                    self.resident.insert(
                        page,
                        Resident {
                            handle,
                            quads: composite.base.mesh.quad_count(),
                            version: composite.base.version,
                            inputs: composite.base.inputs,
                        },
                    );
                    self.edit_shell
                        .replace_page_mask(page, composite.changed_owners)
                        .map_err(|error| error.to_string())?;
                    let previous = self.overlays.remove(&page);
                    if let Some(previous) = &previous
                        && Some(previous.handle) != overlay
                        && let Some(lighting) = &mut self.lighting
                    {
                        lighting.unregister(previous.handle);
                    }
                    self.register_lit_page(queue, page, handle, overlay, budget)?;
                    if let Some(handle) = overlay {
                        let mut state = if let Some(old) = previous {
                            old
                        } else {
                            Overlay {
                                handle,
                                inputs: Vec::new(),
                                mask: crate::edit_shell::OwnerMask::default(),
                                quads: 0,
                                owner_faces: Box::new([0; 4096]),
                            }
                        };
                        if let Some(patch) = patch {
                            for update in &patch.updates {
                                state.owner_faces[usize::from(update.owner)] = update
                                    .classes
                                    .iter()
                                    .filter(|class| {
                                        **class != render::terrain_mesh::MeshClass::Empty
                                    })
                                    .count()
                                    as u8;
                            }
                            state.inputs = patch.inputs;
                        }
                        state.handle = handle;
                        state.mask = self
                            .edit_shell
                            .mask(page)
                            .expect("nonempty composite mask")
                            .clone();
                        state.quads = state
                            .mask
                            .cells()
                            .map(|[x, y, z]| {
                                usize::from(
                                    state.owner_faces[usize::from(x)
                                        + 16 * usize::from(z)
                                        + 256 * usize::from(y)],
                                )
                            })
                            .sum();
                        self.overlays.insert(page, state);
                        self.base_rebases = self.base_rebases.saturating_add(1);
                    }
                    self.pending_patches.retain(|patch| patch.page != page);
                }
                Err(TerrainError::UploadBudget | TerrainError::Capacity) => {
                    self.scene
                        .return_composite(composite)
                        .map_err(|_| "cannot retain budget-blocked composite".to_string())?;
                    break;
                }
                Err(error) => {
                    self.scene
                        .return_composite(composite)
                        .map_err(|_| "cannot retain failed composite".to_string())?;
                    return Err(error.to_string());
                }
            }
        }
        self.upload_page_links(queue, budget)?;
        self.pump_horizon(queue, budget)?;
        if let Some(lighting) = &mut self.lighting {
            let light_before = budget.remaining_bytes;
            let work = lighting
                .pump(
                    queue,
                    &mut self.renderer,
                    &mut self.scene,
                    self.tick,
                    budget,
                    !self.active,
                )
                .map_err(|error| error.to_string())?;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                cells_scanned: work
                    .cpu_cells
                    .saturating_add(work.cpu_neighbors)
                    .saturating_add(work.sample_reads),
                upload_bytes: light_before - budget.remaining_bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        let source_camera = self
            .environment
            .as_ref()
            .filter(|environment| environment.owner.gpu.gi().is_some())
            .map(|environment| {
                let parameters = self.camera_parameters();
                (
                    parameters.eye,
                    parameters.projection(environment.width, environment.height),
                )
            });
        let props_epoch = self.props_source_epoch();
        if let Some(props) = &mut self.props {
            let work = props.upload_ready(&self.scene, queue, budget, self.tick)?;
            if work.changed {
                self.renderer.invalidate();
            }
            self.record_props_work(work);
        }
        self.observe_props_source_epoch(props_epoch);
        let near_ready = self.near_meshes_ready();
        let far_water_ready = self.far_water_guard_ready();
        if let (Some(water), Some(environment)) = (&mut self.water, &mut self.environment) {
            let gpu = environment
                .owner
                .gpu
                .water_mut()
                .ok_or("water GPU detached from its source")?;
            let before = budget.remaining_bytes;
            let stats = water
                .flush(queue, gpu, &self.scene, budget, |page| {
                    water_model::page_model(self.scene.world(), page, [0.; 3])
                })
                .map_err(|error| error.to_string())?;
            let bytes = before - budget.remaining_bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: bytes,
                ..telemetry::WorkCounts::ZERO
            });
            if bytes > 0 {
                self.renderer.invalidate();
            }
            environment
                .owner
                .gpu
                .set_water_cover_ready(
                    &mut self.renderer,
                    stats.ready && near_ready && far_water_ready,
                )
                .map_err(|error| error.to_string())?;
        }
        if let (Some(environment), Some((eye, camera))) = (&mut self.environment, source_camera) {
            let before = budget.remaining_bytes;
            let work = environment
                .owner
                .pump(queue, &mut self.scene, eye, &camera, self.tick, budget)
                .map_err(|error| error.to_string())?;
            let bytes = before - budget.remaining_bytes;
            self.record_environment_work(work, bytes);
            if bytes != 0 {
                self.renderer.invalidate();
            }
        }
        budget.remaining_bytes += environment_reserve;
        self.frame_upload_remaining = budget.remaining_bytes;
        self.upload_bytes_total = self
            .upload_bytes_total
            .saturating_add(before - budget.remaining_bytes);
        Ok(())
    }
    fn record_environment_work(&mut self, stats: EnvironmentStats, upload_bytes: u64) {
        self.retained_worker_work.record(telemetry::WorkCounts {
            cells_scanned: stats.source_worker_reads,
            ..telemetry::WorkCounts::ZERO
        });
        self.retained_foreground_work.record(telemetry::WorkCounts {
            cells_scanned: stats.source_reads + u64::from(stats.probe_visibility_samples),
            upload_bytes,
            ..telemetry::WorkCounts::ZERO
        });
    }
    fn upload_page_links(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        if !self.page_links_dirty {
            return Ok(());
        }
        for (page, resident) in &self.resident {
            let neighbors = [
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
                [-1, 0, 0],
                [1, 0, 0],
            ]
            .map(|[dx, dy, dz]| {
                let page = PageId {
                    space: page.space,
                    x: page.x.checked_add(dx)?,
                    y: page.y.checked_add(dy)?,
                    z: page.z.checked_add(dz)?,
                };
                self.resident.get(&page).map(|resident| resident.handle)
            });
            match self
                .renderer
                .set_page_neighbors(queue, resident.handle, neighbors, budget)
            {
                Ok(_) => {}
                Err(TerrainError::UploadBudget) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
        }
        self.page_links_dirty = false;
        Ok(())
    }
    fn register_lit_page(
        &mut self,
        queue: &wgpu::Queue,
        page: PageId,
        base: PageHandle,
        overlay: Option<PageHandle>,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        if let Some(lighting) = &mut self.lighting {
            lighting
                .register_base(page, base)
                .map_err(|error| error.to_string())?;
            let before = budget.remaining_bytes;
            if let Some(overlay) = overlay {
                lighting
                    .register_overlay(queue, page, overlay, budget)
                    .map_err(|error| error.to_string())?;
            }
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: before - budget.remaining_bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        Ok(())
    }
    fn camera_parameters(&self) -> CameraParameters {
        let (eye, target, up, fov) = if self.active {
            let query = WorldQuery::new(self.scene.world(), SpaceId(0));
            let camera = self.player.camera(
                self.accumulator / sim::TICK_SECONDS,
                self.camera_mode,
                &query,
            );
            let (eye, look, up) = if self.view_bob && self.camera_mode == CameraMode::FirstPerson {
                camera_bob(
                    camera.eye,
                    camera.look,
                    self.player.yaw,
                    self.player.view(self.accumulator / sim::TICK_SECONDS),
                )
            } else {
                (
                    camera.eye,
                    camera.look,
                    pole_safe_up(camera.look, self.player.yaw),
                )
            };
            (
                eye.map(|v| v as f32),
                std::array::from_fn(|i| (eye[i] + look[i]) as f32),
                up.map(|v| v as f32),
                self.field_of_view * camera.fov_multiplier,
            )
        } else {
            let center = self.scene.config().center;
            (
                [
                    center.x as f32 + 24.0,
                    center.y as f32 + 12.0,
                    center.z as f32 + 24.0,
                ],
                [center.x as f32, center.y as f32 - 5.0, center.z as f32],
                [0., 1., 0.],
                self.field_of_view,
            )
        };
        CameraParameters {
            eye,
            target,
            up,
            fov,
            far: self
                .horizon
                .as_ref()
                .map_or(2048., |horizon| horizon.far_plane),
        }
    }
    fn camera(&self, width: u32, height: u32) -> Camera {
        self.camera_parameters().projection(width, height)
    }
    pub fn current_camera_signature(&self, width: u32, height: u32) -> u64 {
        camera_signature(self.camera(width, height))
    }
    pub fn rendered_upload_bytes(&self) -> u64 {
        self.cached_upload_bytes
    }
    pub fn rendered_world_version(&self) -> Version {
        self.cached_world_version
    }
    /// Last encoded scene revision. GPU completion is a separate frame token.
    pub fn rendered_visual_revision(&self) -> u64 {
        self.cached_visual_revision
    }
    pub fn rendered_camera_signature(&self) -> u64 {
        self.cached_camera.map_or(0, camera_signature)
    }
    /// Last encoded HDR camera, not a GPU-completion/presentation certificate.
    /// This View uploads terrain/models in absolute space (origin zero), as in
    /// upload_model and the shared RemoteSession entity conversion.
    pub fn factory_encoded_camera(&self) -> Option<(Camera, [i64; 3])> {
        self.cached_camera.map(|camera| (camera, [0; 3]))
    }
    /// Call before acquiring the surface or creating an encoder. A clean static
    /// iteration keeps the presented image and allocates no GPU dispatch handles.
    pub fn needs_redraw(&self, width: u32, height: u32) -> bool {
        let parameters = self.camera_parameters();
        let camera = parameters.projection(width, height);
        if self.temporal.is_some() {
            return self.temporal_needs_redraw(camera, self.environment_state(parameters.eye));
        }
        self.force_redraw
            || self.cached_props_epoch != self.props_source_epoch()
            || self.page_links_dirty
            || self.cached_camera != Some(camera)
            || self
                .renderer
                .needs_redraw(&camera, CullingMode::FrustumAndHiZ)
            || self.observed_world_version != self.scene.world().version()
            || self.observed_upload_bytes != self.upload_bytes_total
            || self
                .lighting
                .as_ref()
                .is_some_and(|lighting| lighting.owner.needs_work(self.scene.world()))
            || self.environment.as_ref().is_some_and(|environment| {
                environment.owner.needs_cpu_work(&self.scene)
                    || environment.owner.needs_redraw(
                        &self.renderer,
                        &camera,
                        CullingMode::FrustumAndHiZ,
                        self.environment_state(parameters.eye),
                    )
            })
    }
    pub fn invalidate(&mut self) {
        self.force_redraw = true;
    }
    /// A UI-only presentation reuses the last complete HDR image. Its original
    /// camera/world/edit stamps stay intact; pending world work is still dirty.
    pub fn encode_cached_present(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
    ) -> Result<bool, String> {
        self.encoded_shadows = false;
        self.encoded_terrain = false;
        if !self.cached_presentation_available
            || self.cached_props_epoch != self.props_source_epoch()
        {
            self.cached_presentation_available = false;
            return Ok(false);
        }
        let Some(environment) = &mut self.environment else {
            return Ok(false);
        };
        let encoded = environment
            .owner
            .encode_cached_present(encoder, output)
            .map_err(|error| error.to_string())?
            .is_some();
        if encoded {
            self.frame_pipeline_compilations = 0;
        }
        self.cached_presentation_available = encoded;
        Ok(encoded)
    }
    pub fn has_cached_present(&self) -> bool {
        self.cached_presentation_available
            && self.cached_props_epoch == self.props_source_epoch()
            && self.environment.is_some()
    }
    /// Menu-only changes use the same upload allowance and existing Post
    /// resources; world/source queues remain retained for the next scene frame.
    pub fn upload_cached_settings(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        self.flush_post_settings(queue, budget)?;
        self.frame_upload_remaining = budget.remaining_bytes;
        Ok(())
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Result<TerrainFrameWork, String> {
        if self.frame_upload_remaining < 4096 {
            return Err(
                "frame uniform upload allowance unavailable; run upload_ready first".into(),
            );
        }
        self.encoded_shadows = false;
        self.encoded_terrain = false;
        if self.cached_props_epoch != self.props_source_epoch() {
            // The environment also owns scene caches. Force an actual current
            // HDR graph instead of letting its Post reuse an older family.
            self.renderer.invalidate();
            self.force_redraw = true;
        }
        if !self.near_far_join_ready() || self.temporal_restore_needed() {
            if self.encode_cached_present(encoder, output)? {
                return Ok(TerrainFrameWork::default());
            }
            self.encode_unpresented_temporal_restore(encoder)?;
            // A resized/initial target has no prior HDR. A real clear keeps
            // Loading/UI valid without exposing a partially joined scene.
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pending near/far join Loading clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.025,
                            b: 0.03,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            return Ok(TerrainFrameWork::default());
        }
        let parameters = self.camera_parameters();
        let camera = parameters.projection(width, height);
        let sun = self
            .sun_clock()
            .map_or(lighting::SUN, |clock| clock.sun_direction);
        if let (Some(clock), Some(environment)) = (self.sun_clock(), &mut self.environment) {
            environment
                .owner
                .gpu
                .set_celestial_clock(clock.moon_phase, clock.rotation_radians)
                .map_err(|error| error.to_string())?;
        }
        if self.renderer.material_pack_revision().is_some() {
            let bytes = self
                .renderer
                .set_material_view(queue, parameters.eye, 0)
                .map_err(|error| error.to_string())?;
            self.frame_upload_remaining -= bytes;
        }
        let center = self.lighting_center();
        if let (Some(distant), Some(horizon)) = (&mut self.distant_shadows, &self.horizon)
            && distant
                .synchronize(&horizon.owner)
                .map_err(|error| error.to_string())?
        {
            self.renderer.invalidate();
        }
        if let Some(lighting) = &mut self.lighting {
            self.encoded_shadows = true;
            let work = if let Some(distant) = &mut self.distant_shadows {
                let mut casters = render::distant_shadows::SceneShadowCasters {
                    entities: self.entities.as_mut().map(|owner| {
                        &mut owner.gpu as &mut dyn render::shadows::DynamicShadowCaster
                    }),
                    props: self.props.as_mut().map(|owner| {
                        &mut owner.gpu as &mut dyn render::shadows::DynamicShadowCaster
                    }),
                    distant: &mut distant.gpu,
                };
                lighting.encode_models_with_sun(
                    queue,
                    encoder,
                    &mut self.renderer,
                    center,
                    sun,
                    &mut casters,
                )
            } else if let (Some(entities), Some(props)) = (&mut self.entities, &mut self.props) {
                let mut pair = render::shadows::ShadowCasterPair {
                    first: &mut entities.gpu,
                    second: &mut props.gpu,
                };
                lighting.encode_models_with_sun(
                    queue,
                    encoder,
                    &mut self.renderer,
                    center,
                    sun,
                    &mut pair,
                )
            } else if let Some(props) = &mut self.props {
                lighting.encode_models_with_sun(
                    queue,
                    encoder,
                    &mut self.renderer,
                    center,
                    sun,
                    &mut props.gpu,
                )
            } else if let Some(entities) = &mut self.entities {
                lighting.encode_models_with_sun(
                    queue,
                    encoder,
                    &mut self.renderer,
                    center,
                    sun,
                    &mut entities.gpu,
                )
            } else {
                lighting.encode_with_sun(queue, encoder, &mut self.renderer, center, sun)
            }
            .map_err(|error| error.to_string())?;
            self.frame_upload_remaining -= work.uniform_upload_bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: work.uniform_upload_bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        if let (Some(lighting), Some(environment)) = (&self.lighting, &mut self.environment) {
            environment.owner.gpu.set_shadow_revision(
                lighting
                    .stats
                    .shadow_draws
                    .wrapping_add(lighting.stats.shadow_updates),
            );
        }
        let state = self.environment_state(parameters.eye);
        if let Some(boards) = &mut self.recipe_boards {
            let mut upload = UploadBudget::new(self.frame_upload_remaining);
            if boards.prepare_frame(&self.scene, &self.resident, queue, &mut upload)? {
                self.renderer.invalidate();
            }
            let bytes = self.frame_upload_remaining - upload.remaining_bytes;
            self.frame_upload_remaining = upload.remaining_bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: bytes,
                ..telemetry::WorkCounts::ZERO
            });
            self.upload_bytes_total = self.upload_bytes_total.saturating_add(bytes);
        }
        if self.temporal.is_some() {
            return self.encode_temporal_scene(queue, encoder, output, camera, state);
        }
        let props_epoch = self.props_source_epoch();
        let scene_needed = self.cached_props_epoch != props_epoch
            || self.environment.as_ref().is_none_or(|environment| {
                environment.owner.scene_needed(
                    &self.renderer,
                    &camera,
                    CullingMode::FrustumAndHiZ,
                    state,
                )
            });
        if scene_needed && let Some(entities) = &mut self.entities {
            let work = entities
                .prepare_camera(queue, encoder, camera)
                .map_err(|error| error.to_string())?;
            self.frame_upload_remaining -= work.camera_upload_bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: work.camera_upload_bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        if scene_needed && let Some(horizon) = &mut self.horizon {
            horizon
                .owner
                .prepare_camera_extent(
                    queue,
                    encoder,
                    camera,
                    parameters.eye,
                    self.renderer.extent(),
                )
                .map_err(|error| error.to_string())?;
            let h = horizon.owner.height.take_work();
            let p = horizon.owner.proxies.take_work();
            let bytes = h.camera_upload_bytes + p.camera_upload_bytes;
            self.frame_upload_remaining -= bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        if scene_needed && let Some(props) = &mut self.props {
            let work = props.prepare_camera(queue, encoder, camera)?;
            self.frame_upload_remaining -= work.camera_upload_bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: work.camera_upload_bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        if scene_needed && let Some(boards) = &mut self.recipe_boards {
            let mut upload = UploadBudget::new(self.frame_upload_remaining);
            let bytes = boards.prepare_camera(queue, camera, &mut upload)?;
            self.frame_upload_remaining = upload.remaining_bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        let entities = self.entities.as_ref();
        let horizon = self.horizon.as_ref();
        let props = self.props.as_ref();
        let boards = self.recipe_boards.as_ref();
        let props_encoded = std::cell::Cell::new(false);
        let extra = |pass: &mut wgpu::RenderPass<'_>| {
            let entity_draws = entities
                .filter(|entities| entities.stats.primary_records != 0)
                .map_or(0, |entities| entities.draw(pass));
            let horizon_draws = horizon.map_or(0, |horizon| horizon.owner.draw(pass));
            let model_draws = props.map_or(0, |props| props.draw(pass));
            props_encoded.set(
                model_draws > 0 && props.is_some_and(|props| props.gpu.active_instances() > 0),
            );
            entity_draws
                + horizon_draws
                + model_draws
                + boards.map_or(0, |boards| boards.draw(pass))
        };
        let work = if let Some(environment) = &mut self.environment {
            // An acquired surface needs a complete background even when only
            // UI changed. Reuse the HDR scene, exposure and bloom in that case.
            environment.owner.gpu.request_present();
            let mut upload = UploadBudget::new(self.frame_upload_remaining);
            let work = environment
                .owner
                .encode(
                    queue,
                    encoder,
                    &mut self.renderer,
                    output,
                    &camera,
                    CullingMode::FrustumAndHiZ,
                    state,
                    &mut upload,
                    extra,
                )
                .map_err(|error| error.to_string())?;
            self.frame_upload_remaining = upload.remaining_bytes;
            let environment_bytes = work
                .upload_bytes
                .saturating_sub(work.terrain.camera_upload_bytes);
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: environment_bytes,
                ..telemetry::WorkCounts::ZERO
            });
            self.upload_bytes_total = self.upload_bytes_total.saturating_add(environment_bytes);
            work.terrain
        } else {
            let work = self
                .renderer
                .encode_frame_with_extra(
                    queue,
                    encoder,
                    output,
                    &camera,
                    CullingMode::FrustumAndHiZ,
                    extra,
                )
                .map_err(|error| error.to_string())?;
            self.frame_upload_remaining -= work.camera_upload_bytes;
            work
        };
        if scene_needed {
            self.props_content_encoded = props_encoded.get();
            self.props_frames += u64::from(self.props_content_encoded);
        }
        if scene_needed {
            self.self_model_rendered = self.entities.as_ref().map_or_else(
                || {
                    self.self_uploaded_pose.is_some_and(|pose| pose.visible)
                        && self.self_handles.iter().all(Option::is_some)
                },
                |entities| entities.stats.primary_visible,
            );
        }
        if scene_needed
            && self.edit_shell.dirty_pages().next().is_none()
            && self.pending_patches.is_empty()
            && self.is_ready()
            && let Some(since) = self.edit_pending_since.take()
        {
            self.rendered_edit_revision = self.edit_revision;
            self.edit_max_visible_frames = self
                .edit_max_visible_frames
                .max(self.terrain_frames.saturating_add(1).saturating_sub(since));
        }
        if scene_needed {
            self.encoded_terrain = work.passes != 0;
            self.cached_presentation_available = self.environment.is_some();
            if work.passes != 0 {
                self.cached_props_epoch = props_epoch;
            }
            self.cached_camera = Some(camera);
            self.cached_visual_revision = self.renderer.visual_revision();
            self.cached_world_version = self.scene.world().version();
            self.cached_upload_bytes = self.upload_bytes_total;
            if work.passes != 0 && self.resident.values().any(|p| p.quads != 0) {
                self.terrain_frames += 1;
            }
        }
        // A normal graph may conclude that an outside-near metadata change
        // needs only Post. Observe it for scheduling without relabelling the
        // retained HDR camera/world as newly rendered. A join/menu hold above
        // deliberately leaves both the scheduling and presentation dirtiness.
        self.observed_world_version = self.scene.world().version();
        self.observed_upload_bytes = self.upload_bytes_total;
        self.force_redraw = false;
        let measured = self.renderer.take_work();
        self.retained_gpu_work.record(measured);
        self.frame_pipeline_compilations = measured.pipeline_compilations;
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(measured.pipeline_compilations);
        Ok(work)
    }
    /// Consume known work since the previous submitted-frame boundary. GPU
    /// uploads include camera writes. Mesh emissions count each CPU kernel once;
    /// renderer descriptor writes remain available separately in the details.
    pub fn take_frame_work(&mut self) -> telemetry::WorkCounts {
        // Uploads may occur during a CPU iteration which retains the previous
        // image, so drain writes even when encode was skipped by surface pacing.
        let pending = self.renderer.take_work();
        self.retained_gpu_work.record(pending);
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(pending.pipeline_compilations);
        let renderer = self.retained_gpu_work.take();
        let foreground = self.retained_foreground_work.take();
        let scene = self.scene.stats();
        let mut workers = telemetry::WorkCounts {
            cells_scanned: scene
                .mesh_cells_scanned
                .saturating_sub(self.last_reported_mesh_cells),
            quads_emitted: scene
                .mesh_quads
                .saturating_sub(self.last_reported_mesh_quads),
            ..telemetry::WorkCounts::ZERO
        };
        workers.accumulate(self.retained_worker_work.take());
        self.last_reported_mesh_cells = scene.mesh_cells_scanned;
        self.last_reported_mesh_quads = scene.mesh_quads;
        let mut total = renderer;
        total.cells_scanned = total
            .cells_scanned
            .saturating_add(workers.cells_scanned)
            .saturating_add(foreground.cells_scanned);
        total.quads_emitted = workers
            .quads_emitted
            .saturating_add(foreground.quads_emitted);
        total.upload_bytes = total.upload_bytes.saturating_add(foreground.upload_bytes);
        total.pipeline_compilations = total
            .pipeline_compilations
            .saturating_add(foreground.pipeline_compilations);
        total.synchronous_generations = total
            .synchronous_generations
            .saturating_add(foreground.synchronous_generations);
        self.last_frame_work = total;
        self.last_frame_work_details = FrameWorkDetails {
            renderer,
            workers,
            foreground,
        };
        total
    }
    pub const fn last_frame_work(&self) -> telemetry::WorkCounts {
        self.last_frame_work
    }
    pub const fn last_frame_work_details(&self) -> FrameWorkDetails {
        self.last_frame_work_details
    }
    /// CPU-derived inputs and same-budget uploads required before the first
    /// expensive scene encode. Shadow/content presentation deliberately follows
    /// this gate, so an initial Loading clear cannot make the world ready.
    pub fn initial_scene_inputs_ready(&self) -> bool {
        self.near_far_join_ready()
            && self
                .horizon
                .as_ref()
                .is_none_or(|horizon| horizon.owner.initial_inputs_ready())
            && self.near_meshes_ready()
            && self.scene.is_ready()
            && self
                .lighting
                .as_ref()
                .is_none_or(|lighting| lighting.inputs_ready(&self.scene))
            && self.props.as_ref().is_none_or(|props| props.stats.ready)
    }
    /// A real cheap Loading frame, with no scene/content/timestamp publication.
    pub fn encode_loading_clear(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
    ) {
        self.encoded_shadows = false;
        self.encoded_terrain = false;
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("initial CPU inputs Loading clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.02,
                        g: 0.025,
                        b: 0.03,
                        a: 1.,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    pub fn is_ready(&self) -> bool {
        self.near_far_join_ready()
            && self.near_meshes_ready()
            && self
                .lighting
                .as_ref()
                .is_none_or(|lighting| lighting.is_ready(&self.scene))
            && self.props.as_ref().is_none_or(|props| props.stats.ready)
            && ((self.resident.values().any(|p| p.quads > 0) && self.terrain_frames > 0)
                || (self.props_content_encoded
                    && self
                        .props
                        .as_ref()
                        .is_some_and(|props| props.gpu.active_instances() > 0)))
    }
    fn near_meshes_ready(&self) -> bool {
        !self.page_links_dirty
            && self.scene.visible_pages().all(|p| {
                self.resident.get(&p).is_some_and(|resident| {
                    (resident.version == self.scene.world().input_version(p, crate::MESH_CHANNELS)
                        && self.scene.world().matches_inputs(&resident.inputs))
                        || self.overlays.get(&p).is_some_and(|overlay| {
                            self.scene.world().matches_inputs(&overlay.inputs)
                                && self.edit_shell.mask(p) == Some(&overlay.mask)
                        })
                })
            })
    }
    pub fn stats(&self) -> ViewStats {
        let scene = self.scene.stats();
        let edits = self.interactions.stats();
        let target = match self.interactions.target(self.scene.world(), &self.player) {
            sim::RayResult::Hit(hit) => Some(hit),
            _ => None,
        };
        ViewStats {
            resident_pages: self.resident.len(),
            resident_quads: self.resident.values().map(|p| p.quads).sum(),
            terrain_frames: self.terrain_frames,
            props_frames: self.props_frames,
            loaded_pages: scene.loaded_pages,
            queued_jobs: scene.queued_jobs,
            running_jobs: scene.running_jobs,
            meshes_published: scene.meshes_published,
            upload_bytes_total: self.upload_bytes_total,
            ready: self.is_ready(),
            player_spawned: self.active,
            player_pos: self.player.pos,
            flying: self.player.flying,
            noclip: self.player.noclip,
            on_ground: self.player.on_ground,
            simulation_ticks: self.tick.0,
            camera_mode: u8::from(self.camera_mode == CameraMode::RightShoulder),
            self_model_rendered: self.self_model_rendered,
            pipeline_compilations: self.pipeline_compilations,
            frame_pipeline_compilations: self.frame_pipeline_compilations,
            dropped_time_seconds: self.dropped_time_seconds,
            denied_debug_requests: self.denied_debug_requests,
            blocks_broken: edits.broken,
            blocks_placed: edits.placed,
            edits_rolled_back: edits.rolled_back,
            edit_denied: edits.denied.saturating_add(self.edit_rejected),
            mining_progress: self.interactions.mining_progress(),
            last_edit_position: edits.last_position,
            target_position: target.map(|hit| hit.cell),
            target_cell: target.map_or(0, |hit| hit.block.id()),
            edit_shell_pages: self.edit_shell.page_count(),
            edit_shell_owners: self.edit_shell.owner_cells(),
            edit_overlay_uploads: self.edit_overlay_uploads,
            edit_max_visible_frames: self.edit_max_visible_frames,
            demo_mesh_delay_ticks: self.demo_mesh_delay_ticks,
            base_rebases: self.base_rebases,
            edit_revision: self.edit_revision,
            rendered_edit_revision: self.rendered_edit_revision,
            overlay_quads: self.overlays.values().map(|overlay| overlay.quads).sum(),
            full_mesh_ready: self.is_ready()
                && self.scene.is_ready()
                && self.overlays.is_empty()
                && self.pending_patches.is_empty(),
        }
    }
}

/// Looking straight up/down must not make the view/up cross product zero.
fn pole_safe_up(look: [f64; 3], yaw: f32) -> [f64; 3] {
    if look[1].abs() < 0.9999 {
        return [0., 1., 0.];
    }
    let yaw = f64::from(yaw).to_radians();
    let right = [-libm::cos(yaw), 0., -libm::sin(yaw)];
    [
        right[1] * look[2] - right[2] * look[1],
        right[2] * look[0] - right[0] * look[2],
        right[0] * look[1] - right[1] * look[0],
    ]
}

/// Inverse of the Java-style local view bob transform. The simulation owns the
/// interpolated coefficients; this changes presentation without changing aim,
/// movement, interaction reach or the replicated player checkpoint.
fn camera_bob(
    eye: [f64; 3],
    look: [f64; 3],
    yaw: f32,
    view: [f64; 10],
) -> ([f64; 3], [f64; 3], [f64; 3]) {
    let yaw = f64::from(yaw).to_radians();
    let right = [-libm::cos(yaw), 0., -libm::sin(yaw)];
    let up = [
        right[1] * look[2] - right[2] * look[1],
        right[2] * look[0] - right[0] * look[2],
        right[0] * look[1] - right[1] * look[0],
    ];
    let eye = std::array::from_fn(|axis| eye[axis] - right[axis] * view[6] - up[axis] * view[7]);
    let pitch = view[9].to_radians();
    let look =
        std::array::from_fn(|axis| look[axis] * libm::cos(pitch) - up[axis] * libm::sin(pitch));
    let pitched_up: [f64; 3] = std::array::from_fn(|axis| {
        // The original forward can be recovered from orthonormal right/up.
        let forward = [
            up[1] * right[2] - up[2] * right[1],
            up[2] * right[0] - up[0] * right[2],
            up[0] * right[1] - up[1] * right[0],
        ];
        up[axis] * libm::cos(pitch) + forward[axis] * libm::sin(pitch)
    });
    let roll = view[8].to_radians();
    let up = std::array::from_fn(|axis| {
        pitched_up[axis] * libm::cos(roll) + right[axis] * libm::sin(roll)
    });
    (eye, look, up)
}

fn camera_signature(camera: Camera) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for value in camera.view_projection.iter().flatten() {
        for byte in value.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// Explicit near-world profile:64 fixed edit pages reserve24MiB of descriptors.
/// The arena also leaves room for bases and transient replacement allocations.
pub fn terrain_gpu_config() -> render::terrain_gpu::GpuConfig {
    render::terrain_gpu::GpuConfig {
        max_quads: 4_194_304,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{CameraParameters, camera_bob, pole_safe_up};

    #[test]
    fn vertical_look_with_bob_disabled_has_a_finite_projection() {
        for vertical in [-1., 1.] {
            let parameters = CameraParameters {
                eye: [96., 75., 96.],
                target: [96., 75. + vertical, 96.],
                up: pole_safe_up([0., f64::from(vertical), 0.], 135.).map(|v| v as f32),
                fov: 70.,
                far: 2048.,
            };
            let camera = parameters.projection(780, 437);
            assert!(
                camera
                    .view_projection
                    .into_iter()
                    .flatten()
                    .all(f32::is_finite)
            );
        }
    }

    #[test]
    fn view_bob_inverse_has_known_local_translation_and_rotation() {
        let mut coefficients = [0.; 10];
        coefficients[6] = 0.2;
        coefficients[7] = -0.1;
        let (eye, look, up) = camera_bob([1., 2., 3.], [0., 0., 1.], 0., coefficients);
        assert_eq!(eye, [1.2, 2.1, 3.]);
        assert_eq!(look, [0., 0., 1.]);
        assert_eq!(up, [0., 1., 0.]);
        coefficients[6] = 0.;
        coefficients[7] = 0.;
        coefficients[9] = 30.;
        let (_, look, up) = camera_bob([0.; 3], [0., 0., 1.], 0., coefficients);
        assert!((look[1] + 0.5).abs() < 1e-12);
        assert!((look[2] - 0.8660254037844386).abs() < 1e-12);
        assert!((up[1] - 0.8660254037844386).abs() < 1e-12);
        assert!((up[2] - 0.5).abs() < 1e-12);
    }
}
