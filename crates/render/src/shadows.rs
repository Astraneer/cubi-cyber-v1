//! Stable world-space directional cascades and a bounded static depth cache.
use crate::terrain_gpu::{Camera, TerrainGeometry};
use bytemuck::{Pod, Zeroable};
#[path = "shadow_damage.rs"]
mod damage;
#[path = "shadow_footprint.rs"]
pub(crate) mod footprint;
pub(crate) use damage::ShadowDamageTracker;
#[path = "shadow_diagnostics.rs"]
mod diagnostics;
pub use diagnostics::{ShadowNode, ShadowNodeTimings};

pub const CASCADES: usize = 3;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
#[derive(Clone, Copy, Debug)]
pub struct ShadowConfig {
    pub resolution: u32,
    pub half_widths: [f32; CASCADES],
    pub depth_half_extent: f32,
    pub max_updates_per_frame: u32,
    pub sun_step_radians: f32,
    /// Separate combined target is allocated only when moving model shadows are enabled.
    pub dynamic_casters: bool,
    /// GPU old/new caster bounds restrict clears without changing projection.
    pub projected_bounds: bool,
    pub enabled: bool,
    pub active_cascades: u32,
}
impl Default for ShadowConfig {
    fn default() -> Self {
        Self {
            resolution: 4096,
            half_widths: [32., 128., 512.],
            depth_half_extent: 1024.,
            max_updates_per_frame: 1,
            sun_step_radians: std::f32::consts::PI / 720.,
            dynamic_casters: false,
            projected_bounds: false,
            enabled: true,
            active_cascades: 3,
        }
    }
}
impl ShadowConfig {
    pub fn with_resolution(mut self, resolution: u32) -> Result<Self, ShadowError> {
        if resolution == 0 || !resolution.is_power_of_two() {
            return Err(ShadowError::InvalidConfig);
        }
        self.resolution = resolution;
        Ok(self)
    }
    pub fn with_cascades(mut self, cascades: u32) -> Result<Self, ShadowError> {
        if !(1..=CASCADES as u32).contains(&cascades) {
            return Err(ShadowError::InvalidConfig);
        }
        self.active_cascades = cascades;
        Ok(self)
    }
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowError {
    InvalidConfig,
    InvalidBounds,
    DynamicDisabled,
    CasterUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}
impl WorldBounds {
    pub fn new(min: [f32; 3], max: [f32; 3]) -> Result<Self, ShadowError> {
        if (0..3).any(|i| !min[i].is_finite() || !max[i].is_finite() || min[i] > max[i]) {
            return Err(ShadowError::InvalidBounds);
        }
        Ok(Self { min, max })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl ShadowRect {
    pub const fn full(resolution: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: resolution,
            height: resolution,
        }
    }
    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
    pub const fn pixels(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}
const DIRTY_SUN: u8 = 1;
const DIRTY_VIEW: u8 = 2;
const DIRTY_GEOMETRY: u8 = 4;
const DIRTY_INITIAL: u8 = 8;
const DIRTY_FORCE_FULL: u8 = 16;
#[derive(Clone, Copy, Debug)]
struct Cascade {
    committed: Option<Camera>,
    desired: Camera,
    dirty: Option<ShadowRect>,
    dirty_causes: u8,
    age: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShadowWork {
    pub cascade_updates: u32,
    pub static_updated_mask: u32,
    pub dynamic_copy_bytes: u64,
    pub dynamic_draw_calls: u32,
    pub dynamic_cull_dispatches: u32,
    pub dynamic_restore_pixels: u64,
    pub dynamic_restore_draws: u32,
    pub dynamic_damage_bounds: u64,
    /// Area is GPU-reduced; read footprint metrics after completion for exact pixels.
    pub gpu_clear_mask: u32,
    pub gpu_restore_mask: u32,
    pub footprint_dispatches: u32,
    pub metadata_copy_bytes: u64,
    pub clear_pixels: u64,
    pub caster_dispatches: u32,
    pub draw_calls: u32,
    pub uniform_upload_bytes: u64,
    pub pending_cascades: u32,
    pub oldest_age_frames: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ShadowSampling {
    pub matrices: [[[f32; 4]; 4]; CASCADES],
    /// Current selection footprints, independent of deferred committed depth.
    pub desired_matrices: [[[f32; 4]; 4]; CASCADES],
    /// Direction to the sun, followed by reverse-depth receiver bias.
    pub sun: [f32; 4],
    /// Valid cascade bitmask, texture resolution, reserved, reserved.
    pub params: [u32; 4],
}
#[derive(Clone, Copy, Debug)]
pub struct CascadeUpdate {
    pub cascade: usize,
    pub camera: Camera,
    pub rect: ShadowRect,
    pub force_full: bool,
}

pub struct ShadowCache {
    config: ShadowConfig,
    cascades: [Cascade; CASCADES],
    sun: [f32; 3],
    frame: u64,
}
impl ShadowCache {
    pub fn new(config: ShadowConfig, center: [f32; 3], sun: [f32; 3]) -> Result<Self, ShadowError> {
        validate(config, center, sun)?;
        let sun = stepped_sun(sun, config.sun_step_radians);
        let cascades = std::array::from_fn(|index| Cascade {
            committed: None,
            desired: stable_camera(config, index, center, sun),
            dirty: Some(ShadowRect::full(config.resolution)),
            dirty_causes: DIRTY_INITIAL,
            age: 0,
        });
        Ok(Self {
            config,
            cascades,
            sun,
            frame: 0,
        })
    }
    pub fn config(&self) -> ShadowConfig {
        self.config
    }
    fn active(&self) -> &[Cascade] {
        &self.cascades[..if self.config.enabled {
            self.config.active_cascades as usize
        } else {
            0
        }]
    }
    pub fn set_visibility(&mut self, enabled: bool, cascades: u32) -> Result<bool, ShadowError> {
        if !(1..=CASCADES as u32).contains(&cascades) {
            return Err(ShadowError::InvalidConfig);
        }
        if self.config.enabled == enabled && self.config.active_cascades == cascades {
            return Ok(false);
        }
        let old = self.active().len();
        self.config.enabled = enabled;
        self.config.active_cascades = cascades;
        for index in old..self.active().len() {
            let layer = &mut self.cascades[index];
            layer.dirty = Some(ShadowRect::full(self.config.resolution));
            layer.dirty_causes |= DIRTY_GEOMETRY;
            layer.age = self.frame;
        }
        Ok(true)
    }
    pub fn sampling(&self) -> ShadowSampling {
        ShadowSampling {
            matrices: std::array::from_fn(|i| {
                self.cascades[i]
                    .committed
                    .unwrap_or(self.cascades[i].desired)
                    .view_projection
            }),
            desired_matrices: std::array::from_fn(|i| self.cascades[i].desired.view_projection),
            sun: [self.sun[0], self.sun[1], self.sun[2], 0.00005],
            params: [
                self.active().iter().enumerate().fold(0, |bits, (i, c)| {
                    bits | if c.committed.is_some() { 1 << i } else { 0 }
                }),
                self.config.resolution,
                0,
                0,
            ],
        }
    }
    /// View direction is deliberately absent. Rotating the player cannot rotate
    /// the shadow texel grid. Desired matrices become visible only after drawing.
    pub fn update_view(&mut self, center: [f32; 3], sun: [f32; 3]) -> Result<(), ShadowError> {
        validate(self.config, center, sun)?;
        self.frame = self.frame.saturating_add(1);
        let sun = stepped_sun(sun, self.config.sun_step_radians);
        let old_sun = self.sun;
        let sun_changed = sun != old_sun;
        self.sun = sun;
        for (i, cascade) in self.cascades.iter_mut().enumerate() {
            let view_changed = stable_camera(self.config, i, center, old_sun) != cascade.desired;
            let desired = stable_camera(self.config, i, center, sun);
            if cascade.desired != desired || sun_changed {
                if cascade.dirty.is_none() {
                    cascade.age = self.frame;
                }
                cascade.desired = desired;
                if sun_changed {
                    cascade.dirty_causes |= DIRTY_SUN;
                }
                if view_changed {
                    cascade.dirty_causes |= DIRTY_VIEW;
                }
                cascade.dirty = Some(ShadowRect::full(self.config.resolution));
            }
        }
        Ok(())
    }
    /// Old *and* new bounds must be submitted for moves/removal. Light-only and
    /// inventory changes must not call this: they do not change occlusion.
    pub fn invalidate_bounds(&mut self, bounds: WorldBounds) {
        for cascade in &mut self.cascades {
            if let Some(rect) = project_bounds(cascade.desired, bounds, self.config.resolution) {
                if cascade.dirty.is_none() {
                    cascade.age = self.frame;
                }
                cascade.dirty = Some(cascade.dirty.map_or(rect, |old| old.union(rect)));
                cascade.dirty_causes |= DIRTY_GEOMETRY;
            }
        }
    }
    pub fn invalidate_all(&mut self) {
        for c in &mut self.cascades {
            if c.dirty.is_none() {
                c.age = self.frame;
            }
            c.dirty = Some(ShadowRect::full(self.config.resolution));
            c.dirty_causes |= DIRTY_GEOMETRY | DIRTY_FORCE_FULL;
        }
    }
    /// Every layer has a coherent committed image. Ongoing Sun-only refresh
    /// does not revoke it; initial, recenter and geometry work still do.
    pub fn presentation_ready(&self) -> bool {
        self.active().iter().all(|c| {
            c.committed.is_some() && (c.dirty.is_none() || c.dirty_causes & !DIRTY_SUN == 0)
        })
    }
    pub fn pending(&self) -> u32 {
        self.active().iter().filter(|c| c.dirty.is_some()).count() as u32
    }
    pub fn oldest_age(&self) -> u64 {
        self.active()
            .iter()
            .filter(|c| c.dirty.is_some())
            .map(|c| self.frame.saturating_sub(c.age))
            .max()
            .unwrap_or(0)
    }
    /// Near cascade first on startup, then oldest dirty cascade to avoid starving
    /// distant shadows when an actor keeps changing the near cascade.
    pub fn next_update(&self) -> Option<CascadeUpdate> {
        let index = self
            .active()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.dirty.is_some())
            .min_by_key(|(i, c)| (c.committed.is_some(), c.age, *i))
            .map(|(i, _)| i)?;
        let c = self.cascades[index];
        Some(CascadeUpdate {
            cascade: index,
            camera: c.desired,
            rect: c.dirty?,
            force_full: c.dirty_causes & DIRTY_FORCE_FULL != 0,
        })
    }
    pub fn commit(&mut self, update: CascadeUpdate) {
        let c = &mut self.cascades[update.cascade];
        if c.desired == update.camera && c.dirty == Some(update.rect) {
            c.committed = Some(update.camera);
            c.dirty = None;
            c.dirty_causes = 0;
        }
    }
}
fn validate(config: ShadowConfig, center: [f32; 3], sun: [f32; 3]) -> Result<(), ShadowError> {
    if !(1..=CASCADES as u32).contains(&config.active_cascades)
        || config.resolution == 0
        || !config.resolution.is_power_of_two()
        || config.max_updates_per_frame == 0
        || config.max_updates_per_frame > 3
        || config
            .half_widths
            .iter()
            .any(|r| !r.is_finite() || *r <= 0.)
        || config.half_widths.windows(2).any(|r| r[0] >= r[1])
        || !config.depth_half_extent.is_finite()
        || config.depth_half_extent <= 0.
        || !config.sun_step_radians.is_finite()
        || config.sun_step_radians <= 0.
        || center.iter().chain(sun.iter()).any(|v| !v.is_finite())
        || dot(sun, sun) < 0.000001
    {
        return Err(ShadowError::InvalidConfig);
    }
    Ok(())
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalized(v: [f32; 3]) -> [f32; 3] {
    let n = dot(v, v).sqrt();
    v.map(|x| x / n)
}
fn stepped_sun(sun: [f32; 3], step: f32) -> [f32; 3] {
    let n = normalized(sun);
    let azimuth = (n[2].atan2(n[0]) / step).round() * step;
    let altitude = (n[1].asin() / step).round() * step;
    [
        azimuth.cos() * altitude.cos(),
        altitude.sin(),
        azimuth.sin() * altitude.cos(),
    ]
}
fn stable_camera(config: ShadowConfig, index: usize, center: [f32; 3], sun: [f32; 3]) -> Camera {
    let reference = if sun[1].abs() > 0.99 {
        [0., 0., 1.]
    } else {
        [0., 1., 0.]
    };
    let right = normalized(cross(reference, sun));
    let up = cross(sun, right);
    let radius = config.half_widths[index];
    let texel = radius * 2. / config.resolution as f32;
    let snap = |axis: [f32; 3]| (dot(center, axis) / texel).round() * texel;
    let x = snap(right);
    let y = snap(up);
    let z = snap(sun);
    let depth = 2. * config.depth_half_extent;
    Camera {
        view_projection: [
            [right[0] / radius, up[0] / radius, sun[0] / depth, 0.],
            [right[1] / radius, up[1] / radius, sun[1] / depth, 0.],
            [right[2] / radius, up[2] / radius, sun[2] / depth, 0.],
            [-x / radius, -y / radius, 0.5 - z / depth, 1.],
        ],
    }
}
fn project_bounds(camera: Camera, bounds: WorldBounds, resolution: u32) -> Option<ShadowRect> {
    let mut low = [f32::INFINITY; 2];
    let mut high = [f32::NEG_INFINITY; 2];
    let mut depth = [f32::INFINITY, f32::NEG_INFINITY];
    for i in 0..8 {
        let p = std::array::from_fn::<_, 3, _>(|a| {
            if i & (1 << a) == 0 {
                bounds.min[a]
            } else {
                bounds.max[a]
            }
        });
        let clip = std::array::from_fn::<_, 3, _>(|r| {
            camera.view_projection[3][r]
                + (0..3)
                    .map(|c| camera.view_projection[c][r] * p[c])
                    .sum::<f32>()
        });
        let uv = [clip[0] * 0.5 + 0.5, 0.5 - clip[1] * 0.5];
        for a in 0..2 {
            low[a] = low[a].min(uv[a]);
            high[a] = high[a].max(uv[a]);
        }
        depth[0] = depth[0].min(clip[2]);
        depth[1] = depth[1].max(clip[2]);
    }
    if high[0] < 0. || low[0] > 1. || high[1] < 0. || low[1] > 1. || depth[1] < 0. || depth[0] > 1.
    {
        return None;
    }
    // Two-pixel padding includes PCF taps and raster rounding at dirty edges.
    let first = low
        .map(|v| (v * resolution as f32).floor().max(0.) as u32)
        .map(|v| v.saturating_sub(2).min(resolution - 1));
    let last = high.map(|v| {
        ((v * resolution as f32).ceil().max(0.) as u32)
            .saturating_add(2)
            .min(resolution)
    });
    Some(ShadowRect {
        x: first[0],
        y: first[1],
        width: last[0].saturating_sub(first[0]).max(1),
        height: last[1].saturating_sub(first[1]).max(1),
    })
}

/// A prepared dynamic model pass uses independent visibility so it cannot
/// overwrite the camera's color list or the cached terrain visibility.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShadowPrepareWork {
    pub dispatches: u32,
    pub upload_bytes: u64,
    pub bounds_complete: bool,
}
pub trait DynamicShadowCaster {
    fn available(&self) -> bool;
    fn shadow_revision(&self) -> u64;
    /// Exact component versions; composites must preserve each counter separately.
    fn shadow_key(&self) -> [u64; 4] {
        [self.shadow_revision(), 0, 0, 0]
    }
    /// False requests a conservative full-layer restore when damage history is
    /// unavailable. Acknowledgement is valid only after every valid cascade.
    fn visit_shadow_damage(
        &self,
        _previous: [u64; 4],
        _visit: &mut dyn FnMut(WorldBounds),
    ) -> bool {
        false
    }
    fn commit_shadow_damage(&mut self) {}
    fn prepare_shadow_region(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        _rect: ShadowRect,
        _resolution: u32,
    ) -> ShadowPrepareWork {
        ShadowPrepareWork {
            dispatches: self.prepare_shadow(queue, encoder, cascade, camera),
            upload_bytes: 0,
            bounds_complete: false,
        }
    }
    /// Optional GPU footprint collection. Unsupported casters restore a full layer.
    fn prepare_shadow_sparse(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        resolution: u32,
    ) -> ShadowPrepareWork {
        self.prepare_shadow_region(
            queue,
            encoder,
            cascade,
            camera,
            ShadowRect::full(resolution),
            resolution,
        )
    }
    fn prepare_shadow(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
    ) -> u32;
    fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, cascade: usize) -> u32;
}
/// Borrow two prepared model backends for one combined depth pass. The exact
/// pair of counters avoids collisions and works when recreated each frame.
pub struct ShadowCasterPair<'a> {
    pub first: &'a mut dyn DynamicShadowCaster,
    pub second: &'a mut dyn DynamicShadowCaster,
}
impl DynamicShadowCaster for ShadowCasterPair<'_> {
    fn available(&self) -> bool {
        self.first.available() && self.second.available()
    }
    fn shadow_revision(&self) -> u64 {
        0
    }
    fn shadow_key(&self) -> [u64; 4] {
        [
            self.first.shadow_revision(),
            self.second.shadow_revision(),
            2,
            0,
        ]
    }
    fn visit_shadow_damage(&self, previous: [u64; 4], visit: &mut dyn FnMut(WorldBounds)) -> bool {
        if previous[2] != 2 {
            return false;
        }
        self.first
            .visit_shadow_damage([previous[0], 0, 0, 0], visit)
            && self
                .second
                .visit_shadow_damage([previous[1], 0, 0, 0], visit)
    }
    fn commit_shadow_damage(&mut self) {
        self.first.commit_shadow_damage();
        self.second.commit_shadow_damage();
    }
    fn prepare_shadow_region(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        rect: ShadowRect,
        resolution: u32,
    ) -> ShadowPrepareWork {
        let a = self
            .first
            .prepare_shadow_region(queue, encoder, cascade, camera, rect, resolution);
        let b = self
            .second
            .prepare_shadow_region(queue, encoder, cascade, camera, rect, resolution);
        ShadowPrepareWork {
            dispatches: a.dispatches + b.dispatches,
            upload_bytes: a.upload_bytes + b.upload_bytes,
            bounds_complete: a.bounds_complete && b.bounds_complete,
        }
    }
    fn prepare_shadow_sparse(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        resolution: u32,
    ) -> ShadowPrepareWork {
        let a = self
            .first
            .prepare_shadow_sparse(queue, encoder, cascade, camera, resolution);
        let b = self
            .second
            .prepare_shadow_sparse(queue, encoder, cascade, camera, resolution);
        ShadowPrepareWork {
            dispatches: a.dispatches + b.dispatches,
            upload_bytes: a.upload_bytes + b.upload_bytes,
            bounds_complete: a.bounds_complete && b.bounds_complete,
        }
    }
    fn prepare_shadow(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
    ) -> u32 {
        self.first.prepare_shadow(queue, encoder, cascade, camera)
            + self.second.prepare_shadow(queue, encoder, cascade, camera)
    }
    fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, cascade: usize) -> u32 {
        self.first.draw_shadow(pass, cascade) + self.second.draw_shadow(pass, cascade)
    }
}
struct NoModelCaster;
impl DynamicShadowCaster for NoModelCaster {
    fn available(&self) -> bool {
        true
    }
    fn shadow_revision(&self) -> u64 {
        0
    }
    fn prepare_shadow(
        &mut self,
        _: &wgpu::Queue,
        _: &mut wgpu::CommandEncoder,
        _: usize,
        _: Camera,
    ) -> u32 {
        0
    }
    fn prepare_shadow_sparse(
        &mut self,
        _: &wgpu::Queue,
        _: &mut wgpu::CommandEncoder,
        _: usize,
        _: Camera,
        _: u32,
    ) -> ShadowPrepareWork {
        ShadowPrepareWork {
            bounds_complete: true,
            ..Default::default()
        }
    }
    fn draw_shadow(&self, _: &mut wgpu::RenderPass<'_>, _: usize) -> u32 {
        0
    }
}
struct CombinedDepth {
    restore: wgpu::RenderPipeline,
    restore_binds: [wgpu::BindGroup; CASCADES],
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    layers: [wgpu::TextureView; CASCADES],
}
struct DynamicStamp {
    revision: [u64; 4],
    matrices: [[[f32; 4]; 4]; CASCADES],
    valid: u32,
}
pub struct ShadowGpu {
    cache: ShadowCache,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    layers: [wgpu::TextureView; CASCADES],
    sampler: wgpu::Sampler,
    sampling: wgpu::Buffer,
    cameras: [wgpu::Buffer; CASCADES],
    cull_cameras: [wgpu::Buffer; CASCADES],
    render_binds: [wgpu::BindGroup; CASCADES],
    cull_binds: [wgpu::BindGroup; CASCADES],
    _visible: wgpu::Buffer,
    indirect: wgpu::Buffer,
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    depth: wgpu::RenderPipeline,
    clear: wgpu::RenderPipeline,
    max_pages: u32,
    max_quads: u32,
    sampling_dirty: bool,
    visibility_dirty: bool,
    atlas_state: std::sync::Arc<crate::shadow_alpha::AtlasState>,
    atlas_revision: u64,
    atlas_refresh_mask: u32,
    combined: Option<CombinedDepth>,
    last_dynamic: Option<DynamicStamp>,
    timer: crate::gpu_time::GpuPassTimer<9>,
    total_timer: crate::gpu_time::GpuGraphTimer,
    last_static_rects: [Option<ShadowRect>; CASCADES],
    footprints: Option<footprint::Footprints>,
    clear_binds: Option<[wgpu::BindGroup; CASCADES]>,
    last_force_full_mask: u32,
}
impl ShadowGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: ShadowConfig,
        center: [f32; 3],
        sun: [f32; 3],
    ) -> Result<Self, ShadowError> {
        let cache = ShadowCache::new(config, center, sun)?;
        if config.resolution > device.limits().max_texture_dimension_2d {
            return Err(ShadowError::InvalidConfig);
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cached directional shadow cascades"),
            size: wgpu::Extent3d {
                width: config.resolution,
                height: config.resolution,
                depth_or_array_layers: 3,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layers = std::array::from_fn(|i| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: i as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("stable shadow bilinear PCF taps"),
            compare: Some(wgpu::CompareFunction::GreaterEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let make_buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let sampling = make_buffer(
            "committed shadow sampling matrices",
            std::mem::size_of::<ShadowSampling>() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        queue.write_buffer(&sampling, 0, bytemuck::bytes_of(&cache.sampling()));
        let cameras = std::array::from_fn(|_| {
            make_buffer(
                "shadow cascade camera",
                96,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            )
        });
        let cull_cameras = std::array::from_fn(|_| {
            make_buffer(
                "shadow dirty-rectangle cull camera",
                96,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            )
        });
        let footprints = config.projected_bounds.then(|| {
            footprint::Footprints::new(
                device,
                queue,
                geometry.objects,
                &cameras,
                geometry.max_pages,
                config.resolution,
            )
        });
        let visible = make_buffer(
            "shadow cascade compact descriptors",
            u64::from(geometry.max_quads) * 6 * 4,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = make_buffer(
            "shadow cascade six indirect buckets",
            96,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
        );
        let vertex = wgpu::ShaderStages::VERTEX;
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let compute = wgpu::ShaderStages::COMPUTE;
        let storage_entry = |binding, visibility, uniform, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: if uniform {
                    wgpu::BufferBindingType::Uniform
                } else {
                    wgpu::BufferBindingType::Storage { read_only }
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: fragment,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        };
        let render_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow shared terrain tables"),
            entries: &[
                storage_entry(0, vertex, true, true),
                storage_entry(1, vertex | fragment, false, true),
                storage_entry(2, vertex, false, true),
                storage_entry(3, vertex, false, true),
                storage_entry(4, fragment, false, true),
                texture_entry(5),
                texture_entry(6),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage_entry(8, fragment, false, true),
                storage_entry(16, fragment, false, true),
            ],
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("independent shadow cascade culling"),
            entries: &[
                storage_entry(0, compute, true, true),
                storage_entry(1, compute, false, true),
                storage_entry(2, compute, false, false),
                storage_entry(3, compute, false, false),
                storage_entry(5, compute, false, true),
                storage_entry(6, compute, false, true),
                storage_entry(7, compute, false, true),
            ],
        });

        let render_binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shadow terrain tables"),
                layout: &render_layout,
                entries: &[
                    buffer_entry(0, &cameras[i]),
                    buffer_entry(1, geometry.objects),
                    buffer_entry(2, geometry.quads),
                    buffer_entry(3, &visible),
                    buffer_entry(4, geometry.materials),
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(geometry.colors),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(geometry.tint_masks),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::Sampler(geometry.sampler),
                    },
                    buffer_entry(8, geometry.edit_masks),
                    buffer_entry(16, geometry.edit_neighbors),
                ],
            })
        });
        let cull_binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shadow compute tables"),
                layout: &cull_layout,
                entries: &[
                    buffer_entry(0, &cull_cameras[i]),
                    buffer_entry(1, geometry.objects),
                    buffer_entry(2, &visible),
                    buffer_entry(3, &indirect),
                    buffer_entry(5, geometry.edit_masks),
                    buffer_entry(6, geometry.patch_states),
                    buffer_entry(7, geometry.quads),
                ],
            })
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shared terrain shadow vertex + masked depth"),
            source: wgpu::ShaderSource::Wgsl(
                format!("{}\n{}", include_str!("terrain.wgsl"), SHADOW_FRAGMENT).into(),
            ),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow depth pipeline"),
            bind_group_layouts: &[&render_layout],
            push_constant_ranges: &[],
        });
        let depth = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("masked cutout reverse-Z cascade depth"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("shadow_fragment"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview: None,
            cache: None,
        });
        let cull_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow independent GPU cull"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cull.wgsl").into()),
        });
        let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow cull layout"),
            bind_group_layouts: &[&cull_layout],
            push_constant_ranges: &[],
        });
        let make_compute = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&compute_layout),
                module: &cull_shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let reset = make_compute("reset");
        let cull = make_compute("frustum");
        let footprint_entry = |binding| storage_entry(binding, vertex, binding == 0, true);
        let clear_layout = footprints.as_ref().map(|_| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("GPU footprint clear inputs"),
                entries: &[footprint_entry(0), footprint_entry(1)],
            })
        });
        let clear_binds = clear_layout.as_ref().map(|layout| {
            std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("GPU footprint clear rectangle"),
                    layout,
                    entries: &[
                        buffer_entry(0, &cameras[i]),
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: footprints.as_ref().unwrap().binding(i),
                        },
                    ],
                })
            })
        });
        let clear_source = if footprints.is_some() {
            format!("{}\n{}", footprint::COMMON, SPARSE_RECT_SHADER)
        } else {
            CLEAR_SHADER.to_owned()
        };
        let clear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scissored reverse-depth cache clear"),
            source: wgpu::ShaderSource::Wgsl(clear_source.into()),
        });
        let empty_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow clear layout"),
            bind_group_layouts: &clear_layout.iter().collect::<Vec<_>>(),
            push_constant_ranges: &[],
        });
        let clear = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dirty shadow rectangle clear"),
            layout: Some(&empty_layout),
            vertex: wgpu::VertexState {
                module: &clear_shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: None,
            multiview: None,
            cache: None,
        });
        let combined = config.dynamic_casters.then(|| {
            let mut entries = vec![wgpu::BindGroupLayoutEntry {
                binding: if footprints.is_some() { 2 } else { 0 },
                visibility: fragment,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }];
            if footprints.is_some() {
                entries.extend([footprint_entry(0), footprint_entry(1)]);
            }
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("localized combined-depth restore source"),
                entries: &entries,
            });
            let restore_binds = std::array::from_fn(|i| {
                let mut entries = vec![wgpu::BindGroupEntry {
                    binding: if footprints.is_some() { 2 } else { 0 },
                    resource: wgpu::BindingResource::TextureView(&layers[i]),
                }];
                if let Some(fp) = &footprints {
                    entries.extend([
                        buffer_entry(0, &cameras[i]),
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: fp.binding(i),
                        },
                    ]);
                }
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("static layer for combined restore"),
                    layout: &layout,
                    entries: &entries,
                })
            });
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("scissored static depth restore"),
                source: wgpu::ShaderSource::Wgsl(
                    if footprints.is_some() {
                        format!(
                            "{}\n{}\n{}",
                            footprint::COMMON,
                            SPARSE_RECT_SHADER,
                            RESTORE_SHADER.replace("binding(0)", "binding(2)")
                        )
                    } else {
                        format!("{CLEAR_SHADER}\n{RESTORE_SHADER}")
                    }
                    .into(),
                ),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("combined restore layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let restore = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("bounded fragment depth restore"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if footprints.is_some() {
                        "restore_vertex"
                    } else {
                        "vertex"
                    }),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Always,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("restore"),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                multiview: None,
                cache: None,
            });
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("static cache plus moving model shadow depth"),
                size: wgpu::Extent3d {
                    width: config.resolution,
                    height: config.resolution,
                    depth_or_array_layers: 3,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            let layers = std::array::from_fn(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: i as u32,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            });
            CombinedDepth {
                restore,
                restore_binds,
                texture,
                view,
                layers,
            }
        });
        Ok(Self {
            cache,
            texture,
            view,
            layers,
            sampler,
            sampling,
            cameras,
            cull_cameras,
            render_binds,
            cull_binds,
            _visible: visible,
            indirect,
            reset,
            cull,
            depth,
            clear,
            max_pages: geometry.max_pages,
            max_quads: geometry.max_quads,
            sampling_dirty: false,
            visibility_dirty: false,
            atlas_state: std::sync::Arc::clone(geometry.shadow_alpha_state),
            atlas_revision: geometry.shadow_alpha_state.revision(),
            atlas_refresh_mask: 0,
            combined,
            last_dynamic: None,
            last_static_rects: [None; CASCADES],
            footprints,
            clear_binds,
            last_force_full_mask: 0,
            total_timer: crate::gpu_time::GpuGraphTimer::new(
                device,
                queue,
                "total directional shadow graph",
            ),
            timer: crate::gpu_time::GpuPassTimer::new(
                device,
                queue,
                "cached/dynamic shadow pass timestamps",
            ),
        })
    }
    /// Depth, clear, reset and cull plus an optional combined restore pipeline,
    /// all created during loading.
    pub const fn startup_pipeline_compilations(&self) -> u32 {
        4 + self.combined.is_some() as u32 + if self.footprints.is_some() { 5 } else { 0 }
    }
    pub(crate) fn footprint_binding(&self, cascade: usize) -> Option<wgpu::BindingResource<'_>> {
        self.footprints.as_ref().map(|fp| fp.binding(cascade))
    }
    /// GPU records contain exact clear/restore areas at byte96 of each256-byte cascade.
    pub fn footprint_metrics_buffer(&self) -> Option<&wgpu::Buffer> {
        self.footprints.as_ref().map(|fp| &fp.buffer)
    }
    pub(crate) fn uses_atlas_state(
        &self,
        state: &std::sync::Arc<crate::shadow_alpha::AtlasState>,
    ) -> bool {
        std::sync::Arc::ptr_eq(&self.atlas_state, state)
    }
    pub fn cache(&self) -> &ShadowCache {
        &self.cache
    }
    /// Hot metadata change; shaders/pipelines/textures remain prepared. Encoding
    /// publishes the mask and restores newly enabled layers before readiness.
    pub fn set_visibility(&mut self, enabled: bool, cascades: u32) -> Result<bool, ShadowError> {
        let changed = self.cache.set_visibility(enabled, cascades)?;
        self.sampling_dirty |= changed;
        self.visibility_dirty |= changed;
        Ok(changed)
    }
    pub fn presentation_ready(&self) -> bool {
        !self.visibility_dirty
            && (self.active_mask() == 0 || self.atlas_revision == self.atlas_state.revision())
            && self.atlas_refresh_mask & self.active_mask() == 0
            && self.cache.presentation_ready()
    }
    pub fn needs_work(&self) -> bool {
        self.sampling_dirty
            || self.cache.pending() != 0
            || self.atlas_revision != self.atlas_state.revision()
            || self.atlas_refresh_mask & self.active_mask() != 0
    }
    fn active_mask(&self) -> u32 {
        if self.cache.config.enabled {
            (1 << self.cache.config.active_cascades) - 1
        } else {
            0
        }
    }
    /// Albedo imports mutate the shared color array without geometry/Sun changes.
    /// Keep the exact epoch separate from caster keys; both static and combined
    /// depth must be refreshed, including inactive layers before later reuse.
    fn synchronize_atlas(&mut self) {
        let revision = self.atlas_state.revision();
        if self.atlas_revision != revision {
            self.cache.invalidate_all();
            self.last_dynamic = None;
            self.sampling_dirty = true;
            self.visibility_dirty = true;
            self.atlas_refresh_mask = (1 << CASCADES) - 1;
            self.atlas_revision = revision;
        }
    }
    pub fn enabled(&self) -> bool {
        self.cache.config.enabled
    }
    pub fn active_cascades(&self) -> u32 {
        self.cache.config.active_cascades
    }
    pub fn sampling_buffer(&self) -> &wgpu::Buffer {
        &self.sampling
    }
    pub fn depth_view(&self) -> &wgpu::TextureView {
        self.combined
            .as_ref()
            .map_or(&self.view, |combined| &combined.view)
    }
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }
    /// Detailed pass kinds1=static frustum,2=static depth/clear,3=dynamic restore/depth.
    /// timing_source() reports a separate whole-graph scope including copies and caster compute.
    pub fn graph(&self) -> crate::gpu_time::PassGraph<9> {
        self.timer.graph()
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.timer.timestamp_buffer()
    }
    pub fn copy_timestamps_into(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::Buffer,
        offset: u64,
    ) {
        self.timer.copy_into(encoder, destination, offset);
    }
    pub fn clear_timing(&self) {
        self.timer.begin();
        self.total_timer.begin_frame();
    }
    pub fn timing_source(&self) -> Option<crate::gpu_time::TimingSource<'_>> {
        self.total_timer
            .timestamp_buffer()
            .map(|(buffer, period)| crate::gpu_time::TimingSource {
                buffer,
                period,
                kinds: [4; 32],
                count: 1,
            })
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
    pub fn update_view(&mut self, center: [f32; 3], sun: [f32; 3]) -> Result<(), ShadowError> {
        let old = self.cache.sun;
        let desired = self.cache.cascades.map(|c| c.desired);
        self.cache.update_view(center, sun)?;
        self.sampling_dirty |= old != self.cache.sun
            || desired
                .into_iter()
                .zip(self.cache.cascades)
                .any(|(old, new)| old != new.desired);
        Ok(())
    }
    pub fn invalidate_bounds(&mut self, bounds: WorldBounds) {
        self.cache.invalidate_bounds(bounds);
    }
    pub fn invalidate_all(&mut self) {
        self.cache.invalidate_all();
    }
    pub fn sampled_texture(&self) -> &wgpu::Texture {
        self.combined
            .as_ref()
            .map_or(&self.texture, |combined| &combined.texture)
    }
    /// Restore only old/new model damage from static depth, then draw current
    /// intersecting casters. Incompatible matrices/history fall back to a full
    /// restore. No static terrain is re-rendered solely for model animation.
    pub fn encode_dynamic(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        caster: &mut impl DynamicShadowCaster,
    ) -> Result<ShadowWork, ShadowError> {
        self.encode_dynamic_with_diagnostics(queue, encoder, caster, None)
    }
    /// Optional native timestamp scopes preserve the existing passes. Ordinary
    /// callers use `encode_dynamic`, with no diagnostic resources or commands.
    pub fn encode_dynamic_with_diagnostics(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        caster: &mut impl DynamicShadowCaster,
        diagnostics: Option<&ShadowNodeTimings>,
    ) -> Result<ShadowWork, ShadowError> {
        self.timer.begin();
        self.total_timer.begin_frame();
        if self.combined.is_none() {
            return Err(ShadowError::DynamicDisabled);
        }
        if !caster.available() {
            return Err(ShadowError::CasterUnavailable);
        }
        self.synchronize_atlas();
        if self.cache.pending() != 0 {
            self.total_timer.start(encoder);
        }
        let mut work = self.encode_static(queue, encoder, diagnostics);
        if self.footprints.is_some() {
            self.encode_sparse_dynamic(queue, encoder, caster, &mut work, diagnostics);
            self.total_timer.finish(encoder);
            self.timer.finish(encoder);
            return Ok(work);
        }
        let sampling = self.cache.sampling();
        let revision = caster.shadow_key();
        let config = self.cache.config;
        let mut rectangles = [None; CASCADES];
        let previous = self.last_dynamic.as_ref().map(|old| old.revision);
        let mut damage_bounds = 0;
        let damage_known = previous.is_some_and(|previous| {
            caster.visit_shadow_damage(previous, &mut |bounds| {
                damage_bounds += 1;
                for (cascade, rect) in rectangles.iter_mut().enumerate() {
                    if let Some(next) = project_bounds(
                        Camera {
                            view_projection: sampling.matrices[cascade],
                        },
                        bounds,
                        config.resolution,
                    ) {
                        *rect = Some(rect.map_or(next, |old: ShadowRect| old.union(next)));
                    }
                }
            })
        });
        work.dynamic_damage_bounds = damage_bounds;
        let combined = self.combined.as_ref().expect("enabled combined target");
        for (cascade, mut rect) in rectangles.into_iter().enumerate() {
            let bit = 1u32 << cascade;
            if sampling.params[0] & bit == 0 {
                continue;
            }
            let incompatible = self.last_dynamic.as_ref().is_none_or(|old| {
                old.valid & bit == 0 || old.matrices[cascade] != sampling.matrices[cascade]
            });
            if incompatible || (!damage_known && previous != Some(revision)) {
                rect = Some(ShadowRect::full(config.resolution));
            }
            if let Some(static_rect) = self.last_static_rects[cascade] {
                rect = Some(rect.map_or(static_rect, |old| old.union(static_rect)));
            }
            let Some(rect) = rect else {
                continue;
            };
            self.total_timer.start(encoder);
            if let Some(timing) = diagnostics {
                timing.encoder_begin(encoder, cascade, ShadowNode::Cull);
            }
            let prepared = caster.prepare_shadow_region(
                queue,
                encoder,
                cascade,
                Camera {
                    view_projection: sampling.matrices[cascade],
                },
                rect,
                config.resolution,
            );
            work.dynamic_cull_dispatches += prepared.dispatches;
            work.uniform_upload_bytes += prepared.upload_bytes;
            if let Some(timing) = diagnostics {
                timing.encoder_end(encoder, cascade, ShadowNode::Cull);
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("localized combined restore and current model depth"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &combined.layers[cascade],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.timer.render(3),
                occlusion_query_set: None,
            });
            pass.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
            if let Some(timing) = diagnostics {
                timing.render_begin(&mut pass, cascade, ShadowNode::Restore);
            }
            pass.set_pipeline(&combined.restore);
            pass.set_bind_group(0, &combined.restore_binds[cascade], &[]);
            pass.draw(0..3, 0..1);
            if let Some(timing) = diagnostics {
                timing.render_end(&mut pass, cascade, ShadowNode::Restore);
            }
            work.dynamic_restore_pixels += rect.pixels();
            work.dynamic_restore_draws += 1;
            work.dynamic_draw_calls += caster.draw_shadow(&mut pass, cascade);
        }
        caster.commit_shadow_damage();
        self.last_dynamic = Some(DynamicStamp {
            revision,
            matrices: sampling.matrices,
            valid: sampling.params[0],
        });
        self.total_timer.finish(encoder);
        self.timer.finish(encoder);
        Ok(work)
    }
    fn encode_sparse_dynamic(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        caster: &mut impl DynamicShadowCaster,
        work: &mut ShadowWork,
        diagnostics: Option<&ShadowNodeTimings>,
    ) {
        let sampling = self.cache.sampling();
        let revision = caster.shadow_key();
        let fp = self.footprints.as_ref().expect("GPU footprint mode");
        let combined = self.combined.as_ref().expect("combined target");
        for cascade in 0..CASCADES {
            let bit = 1 << cascade;
            if sampling.params[0] & bit == 0 {
                continue;
            }
            let changed = self.last_dynamic.as_ref().is_none_or(|old| {
                old.valid & bit == 0
                    || old.revision != revision
                    || old.matrices[cascade] != sampling.matrices[cascade]
            });
            if !changed && work.static_updated_mask & bit == 0 {
                continue;
            }
            self.total_timer.start(encoder);
            if let Some(timing) = diagnostics {
                timing.encoder_begin(encoder, cascade, ShadowNode::Cull);
            }
            if work.static_updated_mask & bit == 0 {
                fp.seed_combined(encoder, cascade);
                work.footprint_dispatches += 1;
            }
            let prepared = caster.prepare_shadow_sparse(
                queue,
                encoder,
                cascade,
                Camera {
                    view_projection: sampling.matrices[cascade],
                },
                self.cache.config.resolution,
            );
            work.dynamic_cull_dispatches += prepared.dispatches;
            work.uniform_upload_bytes += prepared.upload_bytes;
            fp.combined_bounds(
                encoder,
                cascade,
                prepared.bounds_complete && self.last_force_full_mask & bit == 0,
            );
            work.footprint_dispatches += 1;
            if let Some(timing) = diagnostics {
                timing.encoder_end(encoder, cascade, ShadowNode::Cull);
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("GPU old/new footprint restore and current model depth"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &combined.layers[cascade],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.timer.render(3),
                occlusion_query_set: None,
            });
            if let Some(timing) = diagnostics {
                timing.render_begin(&mut pass, cascade, ShadowNode::Restore);
            }
            pass.set_pipeline(&combined.restore);
            pass.set_bind_group(0, &combined.restore_binds[cascade], &[]);
            pass.draw(0..6, 0..1);
            if let Some(timing) = diagnostics {
                timing.render_end(&mut pass, cascade, ShadowNode::Restore);
            }
            work.gpu_restore_mask |= bit;
            work.dynamic_restore_draws += 1;
            work.dynamic_draw_calls += caster.draw_shadow(&mut pass, cascade);
        }
        caster.commit_shadow_damage();
        self.last_dynamic = Some(DynamicStamp {
            revision,
            matrices: sampling.matrices,
            valid: sampling.params[0],
        });
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) -> ShadowWork {
        if self.footprints.is_some() && self.combined.is_some() {
            return self
                .encode_dynamic(queue, encoder, &mut NoModelCaster)
                .expect("prepared combined target");
        }
        self.timer.begin();
        self.total_timer.begin_frame();
        self.synchronize_atlas();
        if self.cache.pending() != 0 || self.last_dynamic.is_some() {
            self.total_timer.start(encoder);
        }
        let mut work = self.encode_static(queue, encoder, None);
        if let Some(combined) = &self.combined {
            let sampling = self.cache.sampling();
            let mask = if self.last_dynamic.take().is_some() {
                sampling.params[0]
            } else {
                work.static_updated_mask
            };
            for cascade in 0..CASCADES {
                if mask & (1 << cascade) == 0 {
                    continue;
                }
                let origin = wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: cascade as u32,
                };
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: 0,
                        origin,
                        aspect: wgpu::TextureAspect::DepthOnly,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &combined.texture,
                        mip_level: 0,
                        origin,
                        aspect: wgpu::TextureAspect::DepthOnly,
                    },
                    wgpu::Extent3d {
                        width: self.cache.config.resolution,
                        height: self.cache.config.resolution,
                        depth_or_array_layers: 1,
                    },
                );
                work.dynamic_copy_bytes += u64::from(self.cache.config.resolution).pow(2) * 4;
            }
        }
        self.total_timer.finish(encoder);
        self.timer.finish(encoder);
        work
    }
    fn encode_static(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        diagnostics: Option<&ShadowNodeTimings>,
    ) -> ShadowWork {
        let mut work = ShadowWork::default();
        self.last_static_rects.fill(None);
        self.last_force_full_mask = 0;
        for _ in 0..self.cache.config.max_updates_per_frame {
            let Some(update) = self.cache.next_update() else {
                break;
            };
            let config = self.cache.config;
            if let Some(timing) = diagnostics {
                timing.encoder_begin(encoder, update.cascade, ShadowNode::StaticCull);
            }
            #[repr(C)]
            #[derive(Clone, Copy, Pod, Zeroable)]
            struct CameraUniform {
                matrix: [[f32; 4]; 4],
                viewport: [f32; 4],
                params: [u32; 4],
            }
            let camera = CameraUniform {
                matrix: update.camera.view_projection,
                viewport: [config.resolution as f32, config.resolution as f32, 0., 0.],
                params: [self.max_pages, self.max_quads, 1, 0],
            };
            queue.write_buffer(
                &self.cameras[update.cascade],
                0,
                bytemuck::bytes_of(&camera),
            );
            if let Some(fp) = &self.footprints {
                if update.force_full {
                    fp.mark_static_full(encoder, update.cascade);
                    work.metadata_copy_bytes += 32;
                    self.last_force_full_mask |= 1 << update.cascade;
                }
                fp.static_bounds(encoder, update.cascade);
                work.footprint_dispatches += 3;
            }
            let cull_camera = CameraUniform {
                matrix: if self.footprints.is_some() {
                    update.camera
                } else {
                    crop_camera(update.camera, update.rect, config.resolution)
                }
                .view_projection,
                ..camera
            };
            queue.write_buffer(
                &self.cull_cameras[update.cascade],
                0,
                bytemuck::bytes_of(&cull_camera),
            );
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("shadow independent frustum compaction"),
                    timestamp_writes: self.timer.compute(1),
                });
                pass.set_bind_group(0, &self.cull_binds[update.cascade], &[]);
                pass.set_pipeline(&self.reset);
                pass.dispatch_workgroups(1, 1, 1);
                pass.set_pipeline(&self.cull);
                pass.dispatch_workgroups(self.max_pages.div_ceil(64), 1, 1);
            }
            if let Some(timing) = diagnostics {
                timing.encoder_end(encoder, update.cascade, ShadowNode::StaticCull);
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("cached directional depth dirty region"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.layers[update.cascade],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: self.timer.render(2),
                    occlusion_query_set: None,
                });
                let rect = if self.footprints.is_some() {
                    ShadowRect::full(config.resolution)
                } else {
                    update.rect
                };
                pass.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
                if let Some(timing) = diagnostics {
                    timing.render_begin(&mut pass, update.cascade, ShadowNode::Clear);
                }
                pass.set_pipeline(&self.clear);
                if let Some(binds) = &self.clear_binds {
                    pass.set_bind_group(0, &binds[update.cascade], &[]);
                }
                pass.draw(0..if self.footprints.is_some() { 6 } else { 3 }, 0..1);
                if let Some(timing) = diagnostics {
                    timing.render_end(&mut pass, update.cascade, ShadowNode::Clear);
                    timing.render_begin(&mut pass, update.cascade, ShadowNode::StaticDraw);
                }
                pass.set_pipeline(&self.depth);
                pass.set_bind_group(0, &self.render_binds[update.cascade], &[]);
                for bucket in [0, 1, 3, 4] {
                    pass.draw_indirect(&self.indirect, bucket * 16);
                }
                if let Some(timing) = diagnostics {
                    timing.render_end(&mut pass, update.cascade, ShadowNode::StaticDraw);
                }
            }
            self.last_static_rects[update.cascade] = Some(update.rect);
            self.cache.commit(update);
            self.sampling_dirty = true;
            work.cascade_updates += 1;
            work.static_updated_mask |= 1 << update.cascade;
            if self.footprints.is_some() {
                work.gpu_clear_mask |= 1 << update.cascade;
            } else {
                work.clear_pixels += update.rect.pixels();
            }
            work.caster_dispatches += 2;
            work.draw_calls += 5;
            work.uniform_upload_bytes += 192;
        }
        if self.sampling_dirty {
            queue.write_buffer(
                &self.sampling,
                0,
                bytemuck::bytes_of(&self.cache.sampling()),
            );
            work.uniform_upload_bytes += std::mem::size_of::<ShadowSampling>() as u64;
            self.sampling_dirty = false;
            self.visibility_dirty = false;
        }
        work.pending_cascades = self.cache.pending();
        work.oldest_age_frames = self.cache.oldest_age();
        self.atlas_refresh_mask &= !work.static_updated_mask;
        work
    }
}
const SHADOW_FRAGMENT: &str = r#"
@fragment fn shadow_fragment(input:VertexOut) {
    let du=dpdx(input.uv);let dv=dpdy(input.uv);
    if edited_owner(input) {discard;}
    if input.bucket==1u {let alpha=textureSampleGrad(colors,texels,input.uv,i32(materials[input.material].x),du,dv).a;if alpha<0.5 {discard;}}
}
"#;
const SPARSE_RECT_SHADER: &str = r#"
struct RectCamera {matrix:mat4x4<f32>,viewport:vec4<f32>,params:vec4<u32>};
@group(0)@binding(0)var<uniform>rect_camera:RectCamera;
@group(0)@binding(1)var<storage,read>rect_footprint:ShadowFootprintRead;
fn rect_vertex(r:vec4<u32>,i:u32)->vec4<f32>{
    let corners=array<vec2<u32>,6>(vec2(0u,0u),vec2(1u,0u),vec2(0u,1u),vec2(0u,1u),vec2(1u,0u),vec2(1u,1u));
    let pixel=select(r.xy,r.zw,corners[i]!=vec2(0u));
    let uv=vec2<f32>(pixel)/rect_camera.viewport.xy;return vec4(uv.x*2.-1.,1.-uv.y*2.,0.,1.);
}
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{return rect_vertex(rect_footprint.static_draw,i);}
@vertex fn restore_vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{return rect_vertex(rect_footprint.combined_draw,i);}
"#;
const RESTORE_SHADER: &str = r#"
@group(0)@binding(0)var static_depth:texture_depth_2d;
@fragment fn restore(@builtin(position)p:vec4<f32>)->@builtin(frag_depth)f32 {
    return textureLoad(static_depth,vec2<i32>(p.xy),0);
}
"#;
const CLEAR_SHADER: &str = r#"
@vertex fn vertex(@builtin(vertex_index) i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
"#;

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

pub(crate) fn crop_camera(camera: Camera, rect: ShadowRect, resolution: u32) -> Camera {
    let low = [
        rect.x as f32 / resolution as f32 * 2. - 1.,
        1. - (rect.y + rect.height) as f32 / resolution as f32 * 2.,
    ];
    let high = [
        (rect.x + rect.width) as f32 / resolution as f32 * 2. - 1.,
        1. - rect.y as f32 / resolution as f32 * 2.,
    ];
    let center = [(low[0] + high[0]) * 0.5, (low[1] + high[1]) * 0.5];
    let scale = [2. / (high[0] - low[0]), 2. / (high[1] - low[1])];
    let mut matrix = camera.view_projection;
    for column in &mut matrix {
        for axis in 0..2 {
            column[axis] = (column[axis] - center[axis] * column[3]) * scale[axis];
        }
    }
    Camera {
        view_projection: matrix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> ShadowConfig {
        ShadowConfig {
            resolution: 128,
            half_widths: [16., 64., 256.],
            ..ShadowConfig::default()
        }
    }
    fn drain(cache: &mut ShadowCache) {
        while let Some(update) = cache.next_update() {
            cache.commit(update);
        }
    }
    #[test]
    fn runtime_mask_controls_only_active_layers_and_reenable_requires_current_coverage() {
        let mut c = ShadowCache::new(config(), [0.; 3], [0.3, 0.8, 0.2]).unwrap();
        drain(&mut c);
        assert_eq!(c.sampling().params[0], 7);
        assert!(c.set_visibility(false, 3).unwrap());
        c.update_view([10.; 3], [0.4, 0.8, 0.2]).unwrap();
        assert_eq!(c.pending(), 0);
        assert_eq!(c.sampling().params[0], 0);
        assert!(c.presentation_ready());
        assert!(c.next_update().is_none());
        assert!(c.set_visibility(true, 1).unwrap());
        assert_eq!(c.pending(), 1);
        assert!(!c.presentation_ready());
        drain(&mut c);
        assert_eq!(c.sampling().params[0], 1);
        assert!(c.presentation_ready());
        assert!(c.set_visibility(true, 2).unwrap());
        assert_eq!(c.pending(), 1);
        assert!(!c.presentation_ready());
        drain(&mut c);
        assert_eq!(c.sampling().params[0], 3);
        assert!(c.set_visibility(true, 3).unwrap());
        drain(&mut c);
        assert_eq!(c.sampling().params[0], 7);
        let before = c.sampling().params;
        assert!(c.set_visibility(true, 0).is_err());
        assert!(c.set_visibility(true, 4).is_err());
        assert_eq!(c.sampling().params, before);
        assert!(!c.set_visibility(true, 3).unwrap());
        assert!(config().with_resolution(0).is_err());
        assert!(config().with_resolution(1000).is_err());
        assert!(config().with_cascades(4).is_err());
        assert_eq!(
            config()
                .with_resolution(512)
                .unwrap()
                .with_cascades(1)
                .unwrap()
                .with_enabled(false)
                .resolution,
            512
        );
    }
    #[test]
    fn presentation_allows_only_coherent_sun_refresh_not_initial_geometry_or_recenter() {
        let mut c = ShadowCache::new(config(), [0.; 3], [0.35, 0.8, 0.2]).unwrap();
        assert!(!c.presentation_ready());
        drain(&mut c);
        assert!(c.presentation_ready());
        c.update_view([0.; 3], [0.36, 0.8, 0.2]).unwrap();
        assert_eq!(c.pending(), 3);
        assert!(c.presentation_ready());
        c.invalidate_bounds(WorldBounds {
            min: [0.; 3],
            max: [1.; 3],
        });
        assert!(!c.presentation_ready());
        drain(&mut c);
        assert!(c.presentation_ready());
        c.update_view([1., 0., 0.], [0.36, 0.8, 0.2]).unwrap();
        assert!(!c.presentation_ready());
        drain(&mut c);
        assert!(c.presentation_ready());
        for frame in 0..32 {
            c.update_view([1., 0., 0.], [0.36 + frame as f32 * 0.002, 0.8, 0.2])
                .unwrap();
            assert!(c.presentation_ready());
            if let Some(u) = c.next_update() {
                c.commit(u);
            }
        }
    }
    #[test]
    fn static_cache_and_subtexel_camera_do_no_work() {
        let mut cache = ShadowCache::new(config(), [0.; 3], [0., 1., 0.]).unwrap();
        drain(&mut cache);
        let matrices = cache.sampling().matrices;
        for i in 0..1000 {
            cache
                .update_view([i as f32 * 0.00001, 0., 0.], [0., 1., 0.])
                .unwrap();
            assert_eq!(cache.pending(), 0);
            assert_eq!(matrices, cache.sampling().matrices);
        }
    }
    #[test]
    fn desired_camera_is_not_published_before_depth() {
        let mut cache = ShadowCache::new(config(), [0.; 3], [0., 1., 0.]).unwrap();
        drain(&mut cache);
        let old = cache.sampling();
        cache.update_view([10., 0., 0.], [0., 1., 0.]).unwrap();
        assert!(cache.pending() > 0);
        assert_eq!(old.matrices, cache.sampling().matrices);
        let update = cache.next_update().unwrap();
        cache.commit(update);
        assert_ne!(
            old.matrices[update.cascade],
            cache.sampling().matrices[update.cascade]
        );
    }
    #[test]
    fn page_invalidation_is_bounded_and_unrelated_pages_are_ignored() {
        let mut cache = ShadowCache::new(config(), [0.; 3], [0., 1., 0.]).unwrap();
        drain(&mut cache);
        cache.invalidate_bounds(WorldBounds::new([10000.; 3], [10016.; 3]).unwrap());
        assert_eq!(cache.pending(), 0);
        cache.invalidate_bounds(WorldBounds::new([1., 0., 1.], [2., 1., 2.]).unwrap());
        assert_eq!(cache.pending(), 3);
        let rect = cache.next_update().unwrap().rect;
        assert!(rect.pixels() < 128 * 128 / 16);
        drain(&mut cache);
        assert_eq!(cache.pending(), 0);
    }
    #[test]
    fn dirty_age_fairness_and_sun_steps_are_explicit() {
        let mut cache = ShadowCache::new(config(), [0.; 3], [0.3, 0.8, 0.2]).unwrap();
        let first = cache.next_update().unwrap();
        assert_eq!(first.cascade, 0);
        cache.commit(first);
        cache.invalidate_bounds(WorldBounds::new([-1.; 3], [1.; 3]).unwrap());
        assert_eq!(cache.next_update().unwrap().cascade, 1);
        drain(&mut cache);
        let before = cache.sampling().matrices;
        cache.update_view([0.; 3], [0.3, 0.800_001, 0.2]).unwrap();
        assert_eq!(cache.pending(), 0);
        assert_eq!(before, cache.sampling().matrices);
        cache.update_view([0.; 3], [0.4, 0.8, 0.2]).unwrap();
        assert_eq!(cache.pending(), 3);
    }
    #[test]
    fn continuous_sun_motion_cannot_starve_far_cascades() {
        let mut cache = ShadowCache::new(config(), [0.; 3], [0.3, 0.8, 0.2]).unwrap();
        drain(&mut cache);
        let mut visits = [0; 3];
        for frame in 1..=12 {
            cache
                .update_view(
                    [frame as f32, 0., 0.],
                    [0.3 + frame as f32 * 0.01, 0.8, 0.2],
                )
                .unwrap();
            let update = cache.next_update().unwrap();
            visits[update.cascade] += 1;
            cache.commit(update);
        }
        assert!(
            visits.iter().all(|n| *n >= 3),
            "far cascade starved under repeated invalidation: {visits:?}"
        );
        assert!(cache.oldest_age() <= 2);
    }
    #[test]
    fn invalid_config_is_rejected_before_gpu_creation() {
        let mut bad = config();
        bad.resolution = 63;
        assert!(matches!(
            ShadowCache::new(bad, [0.; 3], [0., 1., 0.]),
            Err(ShadowError::InvalidConfig)
        ));
        assert!(WorldBounds::new([f32::NAN; 3], [1.; 3]).is_err());
        assert!(WorldBounds::new([2.; 3], [1.; 3]).is_err());
    }
}
