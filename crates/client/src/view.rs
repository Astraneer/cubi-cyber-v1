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
mod entities;
mod lighting;
mod remote;
pub use entities::{EntityStats, ViewEntities};
pub use lighting::{LightingStats, ViewLighting};
pub use remote::RemoteInput;

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
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewStats {
    pub resident_pages: usize,
    pub resident_quads: usize,
    pub terrain_frames: u64,
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

pub struct ClientView {
    pub scene: ClientScene,
    renderer: TerrainGpu,
    lighting: Option<Box<ViewLighting>>,
    entities: Option<Box<ViewEntities>>,
    player: Player,
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
    keys: [bool; 12],
    remote_controls: VecDeque<InputKey>,
    remote_look: Option<[f32; 2]>,
    remote_signature: Option<u64>,
    retained_gpu_work: telemetry::WorkCounters,
    retained_foreground_work: telemetry::WorkCounters,
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
            player: Player::new([
                f64::from(config.center.x) + 0.5,
                f64::from(config.center.y),
                f64::from(config.center.z) + 0.5,
            ]),
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
            keys: [false; 12],
            remote_controls: VecDeque::new(),
            remote_look: None,
            remote_signature: None,
            retained_gpu_work: telemetry::WorkCounters::new(),
            retained_foreground_work: telemetry::WorkCounters::new(),
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
    pub fn renderer_mut(&mut self) -> &mut TerrainGpu {
        &mut self.renderer
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
            || self.entities.is_some()
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
        self.lighting.as_ref().map(|lighting| lighting.stats)
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
        let entities = ViewEntities::prepare(
            device,
            queue,
            &self.renderer,
            self.lighting
                .as_ref()
                .map(|lighting| lighting.owner.shadows()),
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
            .set_remote_frame(source, self.scene.world(), space, origin)
            .map_err(|error| format!("public entity frame: {error:?}"))?;
        self.retained_foreground_work.record(telemetry::WorkCounts {
            cells_scanned: work.light_reads,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(true)
    }
    pub fn apply_settings(&mut self, settings: &ui::settings::Settings) -> SettingsApplication {
        use ui::settings::{Section, Setting};
        let fov = settings.get(Setting::Fov) as f32;
        let bob = settings.get(Setting::Bob) != 0;
        if self.field_of_view != fov || self.view_bob != bob {
            self.field_of_view = fov;
            self.view_bob = bob;
            self.invalidate();
        }
        let mut pending_render = 0;
        for setting in ui::settings::SCHEMA {
            if setting.section == Section::Graphics && setting.id != Setting::Fov {
                pending_render |= 1 << setting.id as u32;
            }
        }
        SettingsApplication {
            applied: (1 << Setting::Fov as u32) | (1 << Setting::Bob as u32),
            pending_render,
        }
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
        let rising = down && !self.keys[key as usize];
        self.keys[key as usize] = down;
        if down {
            self.activate_player();
        }
        if !rising {
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
                self.player.tick(
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
                );
                let attack = self.keys[InputKey::Attack as usize];
                let place = self.keys[InputKey::Place as usize];
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
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) -> Result<(), String> {
        self.invalidate();
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.renderer
            .resize(device, width, height)
            .map_err(|e| e.to_string())
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
                self.scene.world(),
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
        self.upload_bytes_total = self
            .upload_bytes_total
            .saturating_add(before - budget.remaining_bytes);
        Ok(())
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
    fn camera(&self, width: u32, height: u32) -> Camera {
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
                (camera.eye, camera.look, [0., 1., 0.])
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
        Camera::perspective(
            eye,
            target,
            up,
            fov.to_radians(),
            width.max(1) as f32 / height.max(1) as f32,
            0.05,
            2048.0,
        )
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
    pub fn rendered_camera_signature(&self) -> u64 {
        self.cached_camera.map_or(0, camera_signature)
    }
    /// Call before acquiring the surface or creating an encoder. A clean static
    /// iteration keeps the presented image and allocates no GPU dispatch handles.
    pub fn needs_redraw(&self, width: u32, height: u32) -> bool {
        self.force_redraw
            || self.page_links_dirty
            || self.cached_camera != Some(self.camera(width, height))
            || self.cached_world_version != self.scene.world().version()
            || self.cached_upload_bytes != self.upload_bytes_total
            || self
                .lighting
                .as_ref()
                .is_some_and(|lighting| lighting.owner.needs_work(self.scene.world()))
    }
    pub fn invalidate(&mut self) {
        self.force_redraw = true;
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Result<TerrainFrameWork, String> {
        let camera = self.camera(width, height);
        let center = self.lighting_center();
        if let Some(lighting) = &mut self.lighting {
            let work = if let Some(entities) = &mut self.entities {
                lighting.encode_models(
                    queue,
                    encoder,
                    &mut self.renderer,
                    center,
                    &mut entities.gpu,
                )
            } else {
                lighting.encode(queue, encoder, &mut self.renderer, center)
            }
            .map_err(|error| error.to_string())?;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: work.uniform_upload_bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        if let Some(entities) = &mut self.entities {
            let work = entities
                .prepare_camera(queue, encoder, camera)
                .map_err(|error| error.to_string())?;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: work.camera_upload_bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        let entities = self.entities.as_ref();
        let work = self
            .renderer
            .encode_frame_with_extra(
                queue,
                encoder,
                output,
                &camera,
                CullingMode::FrustumAndHiZ,
                |pass| {
                    entities
                        .filter(|entities| entities.stats.active)
                        .map_or(0, |entities| entities.draw(pass))
                },
            )
            .map_err(|e| e.to_string())?;
        self.self_model_rendered = self.entities.as_ref().map_or_else(
            || {
                self.self_uploaded_pose.is_some_and(|pose| pose.visible)
                    && self.self_handles.iter().all(Option::is_some)
            },
            |entities| entities.stats.primary_visible,
        );
        if self.edit_shell.dirty_pages().next().is_none()
            && self.pending_patches.is_empty()
            && self.is_ready()
            && let Some(since) = self.edit_pending_since.take()
        {
            self.rendered_edit_revision = self.edit_revision;
            self.edit_max_visible_frames = self
                .edit_max_visible_frames
                .max(self.terrain_frames.saturating_add(1).saturating_sub(since));
        }
        self.cached_camera = Some(camera);
        self.cached_world_version = self.scene.world().version();
        self.cached_upload_bytes = self.upload_bytes_total;
        self.force_redraw = false;
        if self.resident.values().any(|p| p.quads != 0) {
            self.terrain_frames += 1;
        }
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
        let workers = telemetry::WorkCounts {
            cells_scanned: scene
                .mesh_cells_scanned
                .saturating_sub(self.last_reported_mesh_cells),
            quads_emitted: scene
                .mesh_quads
                .saturating_sub(self.last_reported_mesh_quads),
            ..telemetry::WorkCounts::ZERO
        };
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
    pub fn is_ready(&self) -> bool {
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
            && self.lighting.as_ref().is_none_or(|lighting| {
                lighting.stats.ready && lighting.owner.shadows().cache().pending() == 0
            })
            && self.resident.values().any(|p| p.quads > 0)
            && self.terrain_frames > 0
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
    use super::camera_bob;

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
