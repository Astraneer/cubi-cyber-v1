//! Prepared environment owner and bounded World→GI bridge for both shells.
//! No generation, network messages, or material bytes leave the local client.
use super::ViewLighting;
use crate::{
    ClientScene,
    gi_source::{GiAlbedo, GiSourceBuilder, GiSourceError, SourceBudget},
};
use core_types::{BlockPos, ChannelMask, LocalIdx, PageId, SpaceId, Tick};
use render::{
    OutputTransfer,
    aerial_gpu::AerialConfig,
    celestial::CelestialConfig,
    environment_gpu::{
        EnvironmentBudget, EnvironmentConfig, EnvironmentError, EnvironmentGpu, EnvironmentState,
        EnvironmentWork,
    },
    gi::{GiConfig, GiError, GiGpu},
    multiple_scattering::MultipleScatteringConfig,
    terrain_gpu::{AtlasImage, Camera, CullingMode, TerrainGpu, UploadBudget},
};
use std::sync::Arc;
use world::{CellRead, CursorError, CursorId, WorldChange};

#[derive(Clone, Copy, Debug)]
pub struct EnvironmentProfile {
    pub graphics: EnvironmentConfig,
    pub celestials: Option<CelestialConfig>,
    pub aerial: Option<AerialConfig>,
    pub multiple_scattering: Option<MultipleScatteringConfig>,
    pub journal_rows_per_pump: usize,
    pub source_pages_per_pump: usize,
    pub source_reads_per_pump: u64,
    pub gi_rays_per_tick: u32,
    pub atmosphere_steps: u64,
    pub aerial_steps: u64,
    pub aerial_samples: u64,
    pub cloud_density_samples: u64,
    pub water_reflection_steps: u64,
    pub post_pixels: u64,
}
impl Default for EnvironmentProfile {
    fn default() -> Self {
        Self {
            graphics: EnvironmentConfig::default(),
            celestials: Some(CelestialConfig::default()),
            aerial: Some(AerialConfig::default()),
            multiple_scattering: Some(MultipleScatteringConfig::default()),
            journal_rows_per_pump: 8192,
            source_pages_per_pump: 4,
            source_reads_per_pump: 131072,
            gi_rays_per_tick: 152,
            atmosphere_steps: 4_000_000,
            aerial_steps: 1_000_000,
            aerial_samples: 64_000_000,
            cloud_density_samples: 64_000_000,
            water_reflection_steps: 0,
            post_pixels: 16_000_000,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentStats {
    pub journal_rows: u64,
    pub journal_lag: u64,
    pub source_pages: u32,
    pub source_reads: u64,
    pub source_upload_bytes: u64,
    pub probe_visibility_samples: u32,
    pub pending_source_pages: usize,
    pub pending_probes: u32,
    pub source_ready: bool,
    pub recovered_cursor: bool,
    pub backpressure: bool,
    pub gpu: EnvironmentWork,
}
#[derive(Debug)]
pub enum ViewEnvironmentError {
    InvalidConfig,
    Environment(EnvironmentError),
    Source(GiSourceError),
    Gi(GiError),
    Cursor(CursorError),
}
impl std::fmt::Display for ViewEnvironmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ViewEnvironmentError {}
impl From<EnvironmentError> for ViewEnvironmentError {
    fn from(e: EnvironmentError) -> Self {
        Self::Environment(e)
    }
}
impl From<GiSourceError> for ViewEnvironmentError {
    fn from(e: GiSourceError) -> Self {
        Self::Source(e)
    }
}
impl From<GiError> for ViewEnvironmentError {
    fn from(e: GiError) -> Self {
        Self::Gi(e)
    }
}
impl From<CursorError> for ViewEnvironmentError {
    fn from(e: CursorError) -> Self {
        Self::Cursor(e)
    }
}

pub struct ViewEnvironment {
    pub gpu: EnvironmentGpu,
    pub stats: EnvironmentStats,
    profile: EnvironmentProfile,
    source: Option<SourceOwner>,
}
impl ViewEnvironment {
    /// Call from the existing native loading worker or browser loading task.
    /// No GI cursor/buffers/albedo table are created when the mode is disabled.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        scene: &mut ClientScene,
        lighting: Option<&ViewLighting>,
        atlas: &AtlasImage,
        width: u32,
        height: u32,
        transfer: OutputTransfer,
        mut profile: EnvironmentProfile,
        eye: [f32; 3],
        sun: [f32; 3],
    ) -> Result<Self, ViewEnvironmentError> {
        if profile.graphics.gi.is_some()
            && (profile.journal_rows_per_pump == 0
                || profile.source_pages_per_pump == 0
                || profile.source_reads_per_pump < 4096 * 7)
        {
            return Err(ViewEnvironmentError::InvalidConfig);
        }
        if let Some(config) = &mut profile.graphics.gi {
            let (origin, probe) = desired_grid(eye, *config)?;
            config.world_origin_page = origin;
            config.probe_origin = probe;
        }
        let initial = Self::state(eye, 0., 1. / 60., scene.world().tick(), sun, false);
        let mut budget = UploadBudget::new(16 * 1024 * 1024);
        let mut gpu = EnvironmentGpu::new(
            device,
            queue,
            terrain,
            width,
            height,
            transfer,
            profile.graphics,
            initial,
            lighting.map(|lighting| (lighting.owner.shadows(), lighting.owner.light_volumes())),
            &mut budget,
        )?;
        if let Some(config) = profile.multiple_scattering {
            gpu.prepare_multiple_scattering(device, queue, config, &mut budget)?;
        }
        if let Some(config) = profile.celestials {
            gpu.prepare_celestials(device, queue, config, &mut budget)?;
        }
        if let Some(config) = profile.aerial {
            gpu.prepare_aerial(device, queue, terrain, width, height, config, &mut budget)?;
        }
        let source = if let Some(config) = profile.graphics.gi {
            Some(SourceOwner::new(
                scene,
                GiAlbedo::from_atlas(atlas)?,
                config,
            )?)
        } else {
            None
        };
        Ok(Self {
            gpu,
            stats: EnvironmentStats {
                source_ready: source.is_none(),
                ..EnvironmentStats::default()
            },
            profile,
            source,
        })
    }
    pub fn state(
        eye: [f32; 3],
        time_seconds: f32,
        delta_seconds: f32,
        tick: Tick,
        sun: [f32; 3],
        underwater: bool,
    ) -> EnvironmentState {
        // A one-metre observer-height bin avoids rebuilding the sky LUT for
        // subpixel head motion. Perspective direction remains full precision.
        EnvironmentState {
            eye,
            sun,
            sun_intensity: 1.,
            altitude_km: (libm::roundf(eye[1]) * 0.001).clamp(0., 99.999),
            time_seconds,
            delta_seconds: delta_seconds.clamp(0., 0.1),
            tick: tick.0,
            underwater,
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn pump(
        &mut self,
        queue: &wgpu::Queue,
        scene: &mut ClientScene,
        eye: [f32; 3],
        camera: &Camera,
        tick: Tick,
        budget: &mut UploadBudget,
    ) -> Result<EnvironmentStats, ViewEnvironmentError> {
        let tick = tick.max(scene.world().tick());
        self.stats = EnvironmentStats {
            source_ready: self.source.is_none(),
            ..EnvironmentStats::default()
        };
        if let (Some(source), Some(gi)) = (&mut self.source, self.gpu.gi_mut()) {
            source.pump(
                queue,
                scene,
                gi,
                eye,
                camera,
                tick,
                self.profile,
                budget,
                &mut self.stats,
            )?;
        }
        Ok(self.stats)
    }
    pub fn needs_cpu_work(&self, scene: &ClientScene) -> bool {
        self.source.as_ref().is_some_and(|owner| {
            owner.pending.is_some()
                || owner.sequence != scene.world().journal_next()
                || owner.slots.iter().any(|slot| slot.dirty)
        })
    }
    pub fn needs_redraw(
        &self,
        terrain: &TerrainGpu,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
    ) -> bool {
        self.gpu.needs_redraw(terrain, camera, mode, state)
    }
    /// Use the remaining shared frame upload budget; this does not create a
    /// second independent 2-MiB allowance after terrain/source uploads.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        output: &wgpu::TextureView,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
        upload: &mut UploadBudget,
        extra: impl FnOnce(&mut wgpu::RenderPass<'_>) -> u32,
    ) -> Result<EnvironmentWork, ViewEnvironmentError> {
        let mut budget = EnvironmentBudget {
            aerial_steps: self.profile.aerial_steps,
            aerial_samples: self.profile.aerial_samples,
            upload_bytes: upload.remaining_bytes,
            atmosphere_steps: self.profile.atmosphere_steps,
            water_reflection_steps: self.profile.water_reflection_steps,
            cloud_density_samples: self.profile.cloud_density_samples,
            gi_rays: self.profile.gi_rays_per_tick,
            post_pixels: self.profile.post_pixels,
        };
        let work = self.gpu.encode(
            queue,
            encoder,
            terrain,
            output,
            camera,
            mode,
            state,
            &mut budget,
            extra,
        )?;
        upload.remaining_bytes = budget.upload_bytes;
        self.stats.gpu = work;
        self.stats.pending_probes = self.gpu.gi().map_or(0, GiGpu::pending);
        Ok(work)
    }
    pub fn replace_albedo(&mut self, atlas: &AtlasImage) -> Result<(), ViewEnvironmentError> {
        if let Some(source) = &mut self.source {
            source
                .builder
                .invalidate_materials(GiAlbedo::from_atlas(atlas)?);
            source.epoch = source
                .epoch
                .checked_add(1)
                .ok_or(ViewEnvironmentError::InvalidConfig)?;
            source.mark_all();
        }
        Ok(())
    }
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        width: u32,
        height: u32,
        budget: &mut UploadBudget,
    ) -> Result<(), ViewEnvironmentError> {
        self.gpu
            .resize(device, queue, terrain, width, height, budget)?;
        Ok(())
    }
    /// Needed when a profile is disabled while retaining the same World.
    pub fn detach(self, scene: &mut ClientScene) {
        if let Some(source) = self.source {
            scene.derived_world_mut().remove_cursor(source.cursor);
        }
    }
}

struct SourceSlot {
    page: PageId,
    dirty: bool,
    edited: bool,
    since: u64,
}
struct PendingRecord {
    change: Arc<WorldChange>,
    next: u64,
    phase: u8,
    index: usize,
}
struct SourceOwner {
    builder: GiSourceBuilder,
    config: GiConfig,
    slots: Vec<SourceSlot>,
    cursor: CursorId,
    sequence: u64,
    pending: Option<PendingRecord>,
    last_visibility: Option<(Camera, u32)>,
    epoch: u64,
}
impl SourceOwner {
    fn new(
        scene: &mut ClientScene,
        albedo: GiAlbedo,
        config: GiConfig,
    ) -> Result<Self, ViewEnvironmentError> {
        let count = config
            .world_pages
            .into_iter()
            .try_fold(1usize, |n, v| n.checked_mul(v as usize))
            .ok_or(ViewEnvironmentError::InvalidConfig)?;
        let mut builder = GiSourceBuilder::new(albedo, count)?;
        builder.set_sky_ambient([0.0; 3])?;
        let mut slots = Vec::with_capacity(count);
        for y in 0..config.world_pages[1] {
            for z in 0..config.world_pages[2] {
                for x in 0..config.world_pages[0] {
                    slots.push(SourceSlot {
                        page: PageId {
                            space: SpaceId(0),
                            x: config.world_origin_page[0] + x as i32,
                            y: config.world_origin_page[1] + y as i32,
                            z: config.world_origin_page[2] + z as i32,
                        },
                        dirty: true,
                        edited: false,
                        since: scene.world().tick().0,
                    });
                }
            }
        }
        let sequence = scene.world().journal_next();
        let cursor = scene.derived_world_mut().register_cursor();
        Ok(Self {
            builder,
            config,
            slots,
            cursor,
            sequence,
            pending: None,
            last_visibility: None,
            epoch: 0,
        })
    }
    fn mark_all(&mut self) {
        for slot in &mut self.slots {
            slot.dirty = true;
            slot.edited = false;
        }
        self.last_visibility = None;
    }
    fn mark_page(&mut self, page: PageId, tick: u64, edited: bool) {
        let p = [
            i64::from(page.x) - i64::from(self.config.world_origin_page[0]),
            i64::from(page.y) - i64::from(self.config.world_origin_page[1]),
            i64::from(page.z) - i64::from(self.config.world_origin_page[2]),
        ];
        if page.space != SpaceId(0)
            || (0..3).any(|axis| p[axis] < 0 || p[axis] >= i64::from(self.config.world_pages[axis]))
        {
            return;
        }
        let index = (p[0]
            + i64::from(self.config.world_pages[0])
                * (p[2] + i64::from(self.config.world_pages[2]) * p[1]))
            as usize;
        let slot = &mut self.slots[index];
        if !slot.dirty {
            slot.since = tick;
        }
        slot.dirty = true;
        slot.edited |= edited;
    }
    fn mark_neighbors(&mut self, page: PageId, tick: u64, edited: bool) {
        self.mark_page(page, tick, edited);
        for n in [
            [-1, 0, 0],
            [1, 0, 0],
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [0, 0, 1],
        ] {
            if let (Some(x), Some(y), Some(z)) = (
                page.x.checked_add(n[0]),
                page.y.checked_add(n[1]),
                page.z.checked_add(n[2]),
            ) {
                self.mark_page(
                    PageId {
                        space: page.space,
                        x,
                        y,
                        z,
                    },
                    tick,
                    edited,
                );
            }
        }
    }
    fn mark_cell(&mut self, page: PageId, index: LocalIdx, tick: u64) {
        let (x, y, z) = index.xyz();
        let Some(x) = page
            .x
            .checked_mul(16)
            .and_then(|v| v.checked_add(i32::from(x)))
        else {
            return;
        };
        let Some(y) = page
            .y
            .checked_mul(16)
            .and_then(|v| v.checked_add(i32::from(y)))
        else {
            return;
        };
        let Some(z) = page
            .z
            .checked_mul(16)
            .and_then(|v| v.checked_add(i32::from(z)))
        else {
            return;
        };
        let p = BlockPos {
            space: page.space,
            x,
            y,
            z,
        };
        self.mark_page(page, tick, true);
        for n in [
            [-1, 0, 0],
            [1, 0, 0],
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [0, 0, 1],
        ] {
            if let (Some(x), Some(y), Some(z)) = (
                p.x.checked_add(n[0]),
                p.y.checked_add(n[1]),
                p.z.checked_add(n[2]),
            ) {
                self.mark_page(
                    BlockPos {
                        space: p.space,
                        x,
                        y,
                        z,
                    }
                    .page(),
                    tick,
                    true,
                );
            }
        }
    }
    fn ingest(
        &mut self,
        scene: &mut ClientScene,
        gi: &mut GiGpu,
        tick: Tick,
        limit: usize,
        stats: &mut EnvironmentStats,
    ) -> Result<bool, ViewEnvironmentError> {
        let mut remaining = limit;
        while remaining > 0 {
            if self.pending.is_none() {
                if self.sequence == scene.world().journal_next() {
                    break;
                }
                match scene.world().read_cursor(self.cursor, 1) {
                    Ok(batch) => {
                        let Some(change) = batch.changes.into_iter().next() else {
                            break;
                        };
                        self.pending = Some(PendingRecord {
                            change,
                            next: batch.next,
                            phase: 0,
                            index: 0,
                        });
                        remaining -= 1;
                        stats.journal_rows += 1;
                    }
                    Err(CursorError::Lagged {
                        oldest_available, ..
                    }) => {
                        scene
                            .derived_world_mut()
                            .recover_cursor(self.cursor, oldest_available)?;
                        self.sequence = oldest_available;
                        self.builder.invalidate_sources();
                        self.mark_all();
                        gi.invalidate(tick.0);
                        stats.recovered_cursor = true;
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            let record = Arc::clone(&self.pending.as_ref().expect("pending record").change);
            let mut phase = self.pending.as_ref().unwrap().phase;
            let mut index = self.pending.as_ref().unwrap().index;
            while remaining > 0 && phase < 3 {
                let length = match phase {
                    0 => record.pages.len(),
                    1 => record.cells.len(),
                    _ => record.lights.len(),
                };
                if index == length {
                    phase += 1;
                    index = 0;
                    continue;
                }
                match phase {
                    0 => {
                        let changed = &record.pages[index];
                        self.builder.mark_page_change(changed);
                        if changed.created
                            || changed.removed
                            || changed.channels.intersects(
                                ChannelMask::SHAPE | ChannelMask::OPACITY | ChannelMask::EMISSION,
                            )
                        {
                            self.mark_neighbors(changed.page, tick.0, false);
                        }
                    }
                    1 => {
                        let changed = &record.cells[index];
                        self.builder.mark_cell_change(changed);
                        self.mark_cell(changed.page, changed.index, tick.0);
                    }
                    _ => {
                        let changed = &record.lights[index];
                        self.builder.mark_light_change(changed);
                        if changed.old.rgb != changed.new.rgb || changed.old.sky != changed.new.sky
                        {
                            self.mark_cell(changed.page, changed.index, tick.0);
                        }
                    }
                }
                index += 1;
                remaining -= 1;
                stats.journal_rows += 1;
            }
            while phase < 3 {
                let length = match phase {
                    0 => record.pages.len(),
                    1 => record.cells.len(),
                    _ => record.lights.len(),
                };
                if index != length {
                    break;
                }
                phase += 1;
                index = 0;
            }
            if phase == 3 {
                let next = self.pending.take().unwrap().next;
                scene
                    .derived_world_mut()
                    .acknowledge_cursor(self.cursor, next)?;
                self.sequence = next;
            } else {
                let pending = self.pending.as_mut().unwrap();
                pending.phase = phase;
                pending.index = index;
                break;
            }
        }
        stats.journal_lag = scene.world().journal_next().saturating_sub(self.sequence);
        Ok(self.pending.is_none() && stats.journal_lag == 0)
    }
    #[allow(clippy::too_many_arguments)]
    fn pump(
        &mut self,
        queue: &wgpu::Queue,
        scene: &mut ClientScene,
        gi: &mut GiGpu,
        eye: [f32; 3],
        camera: &Camera,
        tick: Tick,
        profile: EnvironmentProfile,
        budget: &mut UploadBudget,
        stats: &mut EnvironmentStats,
    ) -> Result<(), ViewEnvironmentError> {
        let (origin, probe) = desired_grid(eye, self.config)?;
        let shift = std::array::from_fn(|axis| {
            libm::roundf((probe[axis] - self.config.probe_origin[axis]) / self.config.probe_spacing)
                as i32
        });
        if origin != self.config.world_origin_page || shift != [0; 3] {
            match gi.recenter(queue, origin, shift, tick.0, budget) {
                Ok(work) => {
                    stats.source_upload_bytes += work.upload_bytes;
                    self.config.world_origin_page = origin;
                    self.config.probe_origin = probe;
                    self.builder.retain_aoi(origin, self.config.world_pages);
                    for (index, slot) in self.slots.iter_mut().enumerate() {
                        let x = index as u32 % self.config.world_pages[0];
                        let y = index as u32
                            / (self.config.world_pages[0] * self.config.world_pages[2]);
                        let z =
                            index as u32 / self.config.world_pages[0] % self.config.world_pages[2];
                        *slot = SourceSlot {
                            page: PageId {
                                space: SpaceId(0),
                                x: origin[0] + x as i32,
                                y: origin[1] + y as i32,
                                z: origin[2] + z as i32,
                            },
                            dirty: true,
                            edited: false,
                            since: tick.0,
                        };
                    }
                    self.last_visibility = None;
                }
                Err(GiError::UploadBudget) => {
                    stats.backpressure = true;
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
        }
        // A partial journal record must not certify an incomplete owner union.
        if self.ingest(scene, gi, tick, profile.journal_rows_per_pump, stats)? {
            let mut reads = SourceBudget {
                remaining_world_reads: profile.source_reads_per_pump,
            };
            for _ in 0..profile.source_pages_per_pump {
                let selected = self
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(_, slot)| slot.dirty)
                    .max_by_key(|(_, slot)| {
                        let visible = page_visible(slot.page, camera);
                        let age = tick.0.saturating_sub(slot.since);
                        (age >= 12, visible && slot.edited, visible, slot.edited, age)
                    })
                    .map(|(index, _)| index);
                let Some(index) = selected else {
                    break;
                };
                let page = self.slots[index].page;
                if scene.world().page(page).is_none()
                    && gi.source_page_version([page.x, page.y, page.z])?.is_none()
                {
                    self.slots[index].dirty = false;
                    self.builder.forget(page);
                    continue;
                }
                match self.builder.update_scene_page(
                    scene, page, self.epoch, queue, gi, tick.0, &mut reads, budget,
                ) {
                    Ok(work) => {
                        self.slots[index].dirty = false;
                        self.slots[index].edited = false;
                        stats.source_pages += 1;
                        stats.source_reads += work.world_reads;
                        stats.source_upload_bytes += work.upload_bytes;
                    }
                    Err(GiSourceError::ReadBudget | GiSourceError::Gi(GiError::UploadBudget)) => {
                        stats.backpressure = true;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        if self.last_visibility != Some((*camera, gi.source_revision())) {
            for index in 0..gi.probe_count() {
                let point = gi.probe_position(index).expect("bounded probe slot");
                let p = BlockPos {
                    space: SpaceId(0),
                    x: libm::floorf(point[0]) as i32,
                    y: libm::floorf(point[1]) as i32,
                    z: libm::floorf(point[2]) as i32,
                };
                let known_air = matches!(scene.world().read(p),CellRead::Known(cell) if rules::block(cell.id()).is_some()&&!rules::properties(cell).opaque);
                gi.set_visible(index, known_air && point_visible(point, camera))?;
                stats.probe_visibility_samples += 1;
            }
            self.last_visibility = Some((*camera, gi.source_revision()));
        }
        stats.pending_source_pages = self.slots.iter().filter(|slot| slot.dirty).count();
        stats.pending_probes = gi.pending();
        stats.source_ready = stats.pending_source_pages == 0
            && self.pending.is_none()
            && self.sequence == scene.world().journal_next();
        Ok(())
    }
}
fn desired_grid(
    eye: [f32; 3],
    config: GiConfig,
) -> Result<([i32; 3], [f32; 3]), ViewEnvironmentError> {
    if eye.iter().any(|v| !v.is_finite() || v.abs() > 999_000.) {
        return Err(ViewEnvironmentError::InvalidConfig);
    }
    let origin = std::array::from_fn(|axis| {
        libm::floorf(eye[axis] / 16.) as i32 - (config.world_pages[axis] / 2) as i32
    });
    let probe = std::array::from_fn(|axis| {
        libm::floorf(eye[axis] / config.probe_spacing) * config.probe_spacing
            - (config.probe_counts[axis] / 2) as f32 * config.probe_spacing
            + 0.5
    });
    Ok((origin, probe))
}
fn clip(point: [f32; 3], camera: &Camera) -> [f32; 4] {
    std::array::from_fn(|row| {
        camera.view_projection[0][row] * point[0]
            + camera.view_projection[1][row] * point[1]
            + camera.view_projection[2][row] * point[2]
            + camera.view_projection[3][row]
    })
}
fn point_visible(point: [f32; 3], camera: &Camera) -> bool {
    let p = clip(point, camera);
    p[3] > 0. && p[0].abs() <= p[3] * 1.2 && p[1].abs() <= p[3] * 1.2 && p[2] >= 0. && p[2] <= p[3]
}
fn page_visible(page: PageId, camera: &Camera) -> bool {
    let mut left = true;
    let mut right = true;
    let mut down = true;
    let mut up = true;
    let mut far = true;
    let mut near = true;
    for corner in 0..8 {
        let p = clip(
            [
                page.x as f32 * 16. + if corner & 1 == 0 { 0. } else { 16. },
                page.y as f32 * 16. + if corner & 2 == 0 { 0. } else { 16. },
                page.z as f32 * 16. + if corner & 4 == 0 { 0. } else { 16. },
            ],
            camera,
        );
        left &= p[0] < -p[3];
        right &= p[0] > p[3];
        down &= p[1] < -p[3];
        up &= p[1] > p[3];
        far &= p[2] < 0.;
        near &= p[2] > p[3];
    }
    !(left || right || down || up || far || near)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_snap_is_stable_and_uses_euclidean_negative_coordinates() {
        let config = GiConfig::default();
        let (a, p) = desired_grid([-0.5, 9.2, -16.1], config).unwrap();
        assert_eq!(a, [-3, -1, -4]);
        assert_eq!(p, [-19.5, 0.5, -35.5]);
        let (b, q) = desired_grid([0.5, 9.2, -16.1], config).unwrap();
        assert_eq!(b, [-2, -1, -4]);
        assert_eq!(q[0] - p[0], config.probe_spacing);
        assert_eq!(q[1..], p[1..]);
        assert!(desired_grid([f32::NAN, 0., 0.], config).is_err());
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn live_owner_bounds_journal_rows_publishes_complete_delta_and_never_generates_unknown() {
        use crate::{SceneConfig, lighting::ClientLightingConfig};
        use core_types::Cell;
        use render::{
            atmosphere::AtmosphereConfig,
            post::{Exposure, PostConfig},
            shadows::ShadowConfig,
            terrain_gpu::{AtlasMip, GpuConfig},
        };
        use world::{LightSample, Transaction};
        let gpu =
            pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
        let mut scene = ClientScene::new(SceneConfig {
            horizontal_radius: 0,
            vertical_radius: 0,
            ..SceneConfig::default()
        })
        .unwrap();
        let mut pages = [vec![Cell::AIR; 4096], vec![Cell::AIR; 4096]];
        let stone = BlockPos {
            space: SpaceId(0),
            x: 24,
            y: 7,
            z: 12,
        };
        pages[1][usize::from(stone.local_index().get())] = Cell::from_parts(rules::ids::STONE, 0);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        for (index, cells) in pages.into_iter().enumerate() {
            tx.create_page(
                PageId {
                    space: SpaceId(0),
                    x: index as i32,
                    y: 0,
                    z: 0,
                },
                cells,
            );
        }
        for face in render::terrain_mesh::Face::ALL {
            let n = face.normal();
            tx.set_light(
                BlockPos {
                    space: stone.space,
                    x: stone.x + i32::from(n[0]),
                    y: stone.y + i32::from(n[1]),
                    z: stone.z + i32::from(n[2]),
                },
                LightSample {
                    sky: 0,
                    block: 15,
                    rgb: [0, 0, 15],
                },
            );
        }
        scene.derived_world_mut().commit(tx).unwrap();
        let atlas = AtlasImage {
            tile_size: 1,
            layers: 186,
            mips: vec![AtlasMip {
                size: 1,
                rgba: vec![255; 186 * 4],
                tint_mask: vec![255; 186],
            }],
        };
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            96,
            64,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 4,
                max_quads: 65536,
                occlusion: true,
            },
            &atlas,
        )
        .unwrap();
        let eye = [24.5, 10.5, 14.5];
        let sun = [0.35, 0.8, 0.2];
        let lighting = ViewLighting::prepare(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut scene,
            ClientLightingConfig {
                shadows: ShadowConfig {
                    resolution: 32,
                    ..ShadowConfig::default()
                },
                ..ClientLightingConfig::default()
            },
            eye,
        )
        .unwrap();
        let profile = EnvironmentProfile {
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                gi: Some(GiConfig {
                    world_pages: [2, 1, 1],
                    probe_counts: [2, 1, 1],
                    max_jobs: 2,
                    ..GiConfig::default()
                }),
                post: PostConfig {
                    exposure: Exposure::Manual(1.),
                    bloom: None,
                    ..PostConfig::default()
                },
                ..EnvironmentConfig::default()
            },
            journal_rows_per_pump: 1,
            ..EnvironmentProfile::default()
        };
        let mut view = ViewEnvironment::prepare(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut scene,
            Some(&lighting),
            &atlas,
            96,
            64,
            OutputTransfer::ShaderSrgb,
            profile,
            eye,
            sun,
        )
        .unwrap();
        let camera = Camera::perspective(eye, [24.5, 8.5, 12.5], [0., 1., 0.], 1., 1.5, 0.1, 200.);
        let mut budget = UploadBudget::new(2 * 1024 * 1024);
        let bootstrap = view
            .pump(&gpu.queue, &mut scene, eye, &camera, Tick(1), &mut budget)
            .unwrap();
        assert!(bootstrap.source_ready);
        assert_eq!(bootstrap.source_pages, 2);
        assert_eq!(bootstrap.source_reads, 8198);
        let page = stone.page();
        let initial = view
            .gpu
            .gi()
            .unwrap()
            .source_page_version([page.x, page.y, page.z])
            .unwrap();
        let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("actual live environment owner output"),
            size: wgpu::Extent3d {
                width: 96,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let first = view
            .encode(
                &gpu.queue,
                &mut encoder,
                &mut terrain,
                &output.create_view(&Default::default()),
                &camera,
                CullingMode::FrustumAndHiZ,
                ViewEnvironment::state(eye, 0., 1. / 60., Tick(1), sun, false),
                &mut budget,
                |_| 0,
            )
            .unwrap();
        assert_eq!(first.gi.updated_probes, 2);
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        let above = BlockPos {
            y: stone.y + 1,
            ..stone
        };
        let mut tx = Transaction::new(Tick(2), stone.space);
        tx.set_light(
            above,
            LightSample {
                sky: 0,
                block: 15,
                rgb: [15, 0, 0],
            },
        );
        scene.derived_world_mut().commit(tx).unwrap();
        for _ in 0..2 {
            let partial = view
                .pump(&gpu.queue, &mut scene, eye, &camera, Tick(2), &mut budget)
                .unwrap();
            assert_eq!(partial.journal_rows, 1);
            assert!(!partial.source_ready);
            assert_eq!(partial.source_upload_bytes, 0);
            assert_eq!(
                view.gpu
                    .gi()
                    .unwrap()
                    .source_page_version([page.x, page.y, page.z])
                    .unwrap(),
                initial,
                "an incomplete journal record must never publish a partial owner union"
            );
        }
        let current = view
            .pump(&gpu.queue, &mut scene, eye, &camera, Tick(2), &mut budget)
            .unwrap();
        assert_eq!(current.journal_rows, 1);
        assert!(current.source_ready);
        assert_eq!(
            (current.source_reads, current.source_upload_bytes),
            (13, 124)
        );
        let known_pages = scene.world().loaded_pages();
        let moved = [1000.5, 10.5, 14.5];
        let deferred = view
            .pump(
                &gpu.queue,
                &mut scene,
                moved,
                &camera,
                Tick(3),
                &mut UploadBudget::new(0),
            )
            .unwrap();
        assert!(deferred.backpressure);
        let moved = view
            .pump(&gpu.queue, &mut scene, moved, &camera, Tick(3), &mut budget)
            .unwrap();
        assert!(moved.source_ready);
        assert_eq!(
            scene.world().loaded_pages(),
            known_pages,
            "GI AOI recenter may not synchronously generate absent terrain"
        );
        let cursor = view.source.as_ref().unwrap().cursor;
        view.detach(&mut scene);
        assert_eq!(
            scene.world().read_cursor(cursor, 1).unwrap_err(),
            CursorError::UnknownCursor
        );
        println!(
            "M10_VIEW_OWNER_GPU_OK bootstrap_pages=2 bootstrap_reads=8198 journal_limit=1 incomplete_publications=0 partial_reads=13 partial_upload_bytes=124 initial_gpu_probes=2 recenter_unknown_generation=0 detach_cursor_released=true"
        );
    }
}
