//! Reflected-light probe cache with edited-visible scheduling and bounded GPU DDA.
use crate::gi_directional::{Directional, GiSurface, Sources, SurfaceUpdate};
use crate::terrain_gpu::UploadBudget;
use bytemuck::{Pod, Zeroable};

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct GiVoxel(u32);
pub const MAX_REFLECTED_RADIANCE: f32 = 32.0;
impl GiVoxel {
    pub const UNKNOWN: Self = Self(0);
    pub const EMPTY: Self = Self(0x8000_0000);
    pub fn packed(self) -> u32 {
        self.0
    }
    pub fn from_packed(value: u32) -> Option<Self> {
        (value == 0 || value == 0x8000_0000 || value & 0xc000_0000 == 0xc000_0000)
            .then_some(Self(value))
    }
    /// Reflected radiance only, after albedo/PI. Do not pass direct lamp emission
    /// or escaped skylight: those have separate terms in the M4 energy model.
    pub fn opaque_reflected(radiance: [f32; 3]) -> Option<Self> {
        if radiance
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=MAX_REFLECTED_RADIANCE).contains(v))
        {
            return None;
        }
        let rgb = radiance.map(|v| (v * (1023.0 / MAX_REFLECTED_RADIANCE)).round() as u32);
        Some(Self(0xc000_0000 | rgb[0] | (rgb[1] << 10) | (rgb[2] << 20)))
    }
    pub fn reflected(self) -> [f32; 3] {
        [self.0 & 1023, (self.0 >> 10) & 1023, (self.0 >> 20) & 1023]
            .map(|v| v as f32 * (MAX_REFLECTED_RADIANCE / 1023.0))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    EditedVisible,
    FifoControl,
}
#[derive(Clone, Copy, Debug)]
pub struct GiConfig {
    pub world_origin_page: [i32; 3],
    pub world_pages: [u32; 3],
    pub probe_origin: [f32; 3],
    pub probe_counts: [u32; 3],
    pub probe_spacing: f32,
    pub rays_per_probe: u32,
    pub max_steps: u32,
    pub max_distance: f32,
    pub max_jobs: u32,
    pub priority: Priority,
}
impl Default for GiConfig {
    fn default() -> Self {
        Self {
            world_origin_page: [-2, -1, -2],
            world_pages: [4, 3, 4],
            probe_origin: [-8.0, 0.5, -8.0],
            probe_counts: [8, 4, 8],
            probe_spacing: 4.0,
            rays_per_probe: 32,
            max_steps: 96,
            max_distance: 24.0,
            max_jobs: 32,
            priority: Priority::EditedVisible,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiError {
    InvalidConfig,
    InvalidPage,
    InvalidUpdates,
    StaleVersion,
    UploadBudget,
    Capacity,
    DirectionalRequired,
}
impl std::fmt::Display for GiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GiError {}
#[derive(Clone, Copy, Debug)]
pub struct CellUpdate {
    pub local: u16,
    pub value: GiVoxel,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct GiWork {
    pub updated_probes: u32,
    pub rays: u32,
    pub max_dda_steps: u64,
    pub upload_bytes: u64,
    pub candidates_scanned: u32,
    pub pending_probes: u32,
    pub passes: u32,
    pub max_shadow_samples: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GiScrollWork {
    pub recycled_probes: u32,
    pub retained_probes: u32,
    pub cleared_pages: u32,
    pub upload_bytes: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ProbeSample {
    pub position: [f32; 4],
    pub sh_r: [f32; 4],
    pub sh_g: [f32; 4],
    pub sh_b: [f32; 4],
    pub moments: [f32; 4],
    pub visibility_a: [f32; 4],
    pub visibility_b: [f32; 4],
    pub stamps: [u32; 4],
}
impl ProbeSample {
    pub fn irradiance(&self, normal: [f32; 3]) -> [f32; 3] {
        let basis = [
            0.2820948,
            0.48860252 * normal[1],
            0.48860252 * normal[2],
            0.48860252 * normal[0],
        ];
        [self.sh_r, self.sh_g, self.sh_b].map(|coefficient| {
            coefficient
                .into_iter()
                .zip(basis)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                .max(0.0)
        })
    }
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    origin_spacing: [f32; 4],
    probe_counts: [u32; 4],
    world_origin: [i32; 4],
    world_counts: [u32; 4],
    trace: [f32; 4],
    stamps: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Job {
    slot: u32,
    generation: u32,
    revision: u32,
    rays: u32,
}
struct ProbeMeta {
    position: [f32; 3],
    generation: u32,
    dirty: bool,
    visible: bool,
    edited: bool,
    dirty_since: u64,
}

pub struct GiGpu {
    config: GiConfig,
    parameters: Parameters,
    uniform: wgpu::Buffer,
    world: wgpu::Buffer,
    probes: wgpu::Buffer,
    job_buffer: wgpu::Buffer,
    statistics: wgpu::Buffer,
    rays: wgpu::Buffer,
    bind: wgpu::BindGroup,
    reset: wgpu::ComputePipeline,
    trace: wgpu::ComputePipeline,
    metadata: Vec<ProbeMeta>,
    page_versions: Vec<Option<u64>>,
    page_staging: Box<[u32; 4096]>,
    candidates: Vec<usize>,
    jobs: Vec<Job>,
    dirty: u32,
    timer: crate::gpu_time::GpuPassTimer<2>,
    directional: Option<Directional>,
    has_traced: bool,
}
fn grid_coordinate(index: usize, counts: [u32; 3]) -> [u32; 3] {
    let index = index as u32;
    [
        index % counts[0],
        index / (counts[0] * counts[2]),
        index / counts[0] % counts[2],
    ]
}
fn grid_index(coordinate: [u32; 3], counts: [u32; 3]) -> u32 {
    coordinate[0] + counts[0] * (coordinate[2] + counts[2] * coordinate[1])
}
fn ring_index(coordinate: [u32; 3], counts: [u32; 3], ring: u32) -> usize {
    let offset = grid_coordinate(ring as usize, counts);
    grid_index(
        std::array::from_fn(|axis| (coordinate[axis] + offset[axis]) % counts[axis]),
        counts,
    ) as usize
}
fn shift_ring(ring: u32, counts: [u32; 3], delta: [i64; 3]) -> u32 {
    let prior = grid_coordinate(ring as usize, counts);
    grid_index(
        std::array::from_fn(|axis| {
            (i64::from(prior[axis]) + delta[axis]).rem_euclid(i64::from(counts[axis])) as u32
        }),
        counts,
    )
}
fn overlaps(coordinate: [u32; 3], delta: [i64; 3], counts: [u32; 3]) -> bool {
    (0..3).all(|axis| {
        (0..i64::from(counts[axis])).contains(&(i64::from(coordinate[axis]) + delta[axis]))
    })
}

pub(crate) fn buffer_entry(
    binding: u32,
    uniform: bool,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
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
    }
}
pub(crate) fn binding(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

impl GiGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: GiConfig,
        budget: &mut UploadBudget,
    ) -> Result<Self, GiError> {
        let page_count = config
            .world_pages
            .into_iter()
            .try_fold(1u32, |count, value| count.checked_mul(value))
            .ok_or(GiError::InvalidConfig)?;
        let probe_count = config
            .probe_counts
            .into_iter()
            .try_fold(1u32, |count, value| count.checked_mul(value))
            .ok_or(GiError::InvalidConfig)?;
        if config.world_pages.contains(&0)
            || config.probe_counts.contains(&0)
            || probe_count > 4096
            || config
                .probe_origin
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1_000_000.0)
            || !config.probe_spacing.is_finite()
            || !(0.25..=32.0).contains(&config.probe_spacing)
            || !matches!(config.rays_per_probe, 16 | 32 | 64)
            || config.max_steps == 0
            || config.max_steps > 256
            || !config.max_distance.is_finite()
            || !(1.0..=128.0).contains(&config.max_distance)
            || config.max_jobs == 0
            || config.max_jobs > probe_count
        {
            return Err(GiError::InvalidConfig);
        }
        let world_bytes = u64::from(page_count) * 4096 * 4;
        if world_bytes > u64::from(device.limits().max_storage_buffer_binding_size)
            || world_bytes > device.limits().max_buffer_size
        {
            return Err(GiError::Capacity);
        }
        let upload = 96 + u64::from(probe_count) * 128 + 64 * 16;
        if budget.remaining_bytes < upload {
            return Err(GiError::UploadBudget);
        }
        let parameters = Parameters {
            origin_spacing: [
                config.probe_origin[0],
                config.probe_origin[1],
                config.probe_origin[2],
                config.probe_spacing,
            ],
            probe_counts: [
                config.probe_counts[0],
                config.probe_counts[1],
                config.probe_counts[2],
                0, // Physical ring origin, encoded as a linear grid index.
            ],
            world_origin: [
                config.world_origin_page[0],
                config.world_origin_page[1],
                config.world_origin_page[2],
                0,
            ],
            world_counts: [
                config.world_pages[0],
                config.world_pages[1],
                config.world_pages[2],
                0, // Physical brick-ring origin.
            ],
            trace: [config.max_distance, 0.0, 0.0, 0.0],
            stamps: [0, config.max_steps, config.max_jobs, 0],
        };
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let uniform = buffer(
            "GI brick lattice and probe grid",
            96,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let world = buffer(
            "GI known voxel bricks",
            world_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let probes = buffer(
            "reflected irradiance and visibility probes",
            u64::from(probe_count) * 128,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        );
        let job_buffer = buffer(
            "edited-visible GI jobs",
            u64::from(config.max_jobs) * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let statistics = buffer(
            "actual GI DDA counters",
            16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let rays = buffer(
            "fixed low discrepancy GI rays",
            64 * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let mut metadata = Vec::with_capacity(probe_count as usize);
        let mut initial = Vec::with_capacity(probe_count as usize);
        for y in 0..config.probe_counts[1] {
            for z in 0..config.probe_counts[2] {
                for x in 0..config.probe_counts[0] {
                    let position = std::array::from_fn(|axis| {
                        config.probe_origin[axis] + [x, y, z][axis] as f32 * config.probe_spacing
                    });
                    metadata.push(ProbeMeta {
                        position,
                        generation: 1,
                        dirty: true,
                        visible: false,
                        edited: false,
                        dirty_since: 0,
                    });
                    initial.push(ProbeSample {
                        position: [position[0], position[1], position[2], config.probe_spacing],
                        stamps: [1, 0, 0, 0],
                        ..ProbeSample::zeroed()
                    });
                }
            }
        }
        queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&parameters));
        queue.write_buffer(&probes, 0, bytemuck::cast_slice(&initial));
        queue.write_buffer(&rays, 0, bytemuck::cast_slice(&RAYS));
        budget.remaining_bytes -= upload;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("GI bounded DDA bindings"),
            entries: &[
                buffer_entry(0, true, true),
                buffer_entry(1, false, true),
                buffer_entry(2, false, false),
                buffer_entry(3, false, true),
                buffer_entry(4, false, true),
                buffer_entry(5, false, false),
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("GI source jobs and cached irradiance"),
            layout: &layout,
            entries: &[
                binding(0, &uniform),
                binding(1, &world),
                binding(2, &probes),
                binding(3, &job_buffer),
                binding(4, &rays),
                binding(5, &statistics),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GI reflected voxel DDA"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    include_str!("gi_flat.wgsl"),
                    include_str!("gi.wgsl")
                )
                .into(),
            ),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("GI tracing pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Ok(Self {
            config,
            parameters,
            uniform,
            world,
            probes,
            job_buffer,
            statistics,
            rays,
            bind,
            reset: pipeline("reset_statistics"),
            trace: pipeline("trace_probes"),
            metadata,
            page_versions: vec![None; page_count as usize],
            page_staging: Box::new([0; 4096]),
            candidates: Vec::with_capacity(probe_count as usize),
            jobs: Vec::with_capacity(config.max_jobs as usize),
            dirty: probe_count,
            timer: crate::gpu_time::GpuPassTimer::new(device, queue, "GI pass timings"),
            directional: None,
            has_traced: false,
        })
    }
    /// Loading-only optional first-bounce mode. No extra source memory exists
    /// for the original compact RGB-only mode.
    pub fn prepare_directional(
        &mut self,
        device: &wgpu::Device,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
        shadows: &crate::shadows::ShadowGpu,
    ) -> Result<(), GiError> {
        if self.directional.is_some()
            || self.has_traced
            || self.page_versions.iter().any(Option::is_some)
        {
            return Err(GiError::InvalidConfig);
        }
        self.directional = Some(Directional::new(
            device,
            self.page_versions.len(),
            Sources {
                uniform: &self.uniform,
                world: &self.world,
                probes: &self.probes,
                jobs: &self.job_buffer,
                rays: &self.rays,
                statistics: &self.statistics,
            },
            atmosphere,
            shadows,
        )?);
        Ok(())
    }
    pub fn directional_enabled(&self) -> bool {
        self.directional.is_some()
    }
    pub fn directional_source_bytes(&self) -> u64 {
        if self.directional.is_some() {
            self.page_versions.len() as u64 * 4096 * 48
        } else {
            0
        }
    }
    pub fn lighting_change_upload_bytes(&self) -> Result<u64, GiError> {
        if self.directional.is_none() {
            return Err(GiError::InvalidConfig);
        }
        self.parameters.stamps[0]
            .checked_add(1)
            .ok_or(GiError::Capacity)?;
        Ok(96)
    }
    /// Advance the GPU source certificate without re-uploading World pages.
    /// Previously encoded jobs cannot publish under a different sky/shadow state.
    pub fn invalidate_lighting(
        &mut self,
        queue: &wgpu::Queue,
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<u64, GiError> {
        let bytes = self.lighting_change_upload_bytes()?;
        if budget.remaining_bytes < bytes {
            return Err(GiError::UploadBudget);
        }
        self.invalidate_queued_jobs(tick);
        self.invalidate(tick);
        self.parameters.stamps[0] += 1;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.parameters));
        budget.remaining_bytes -= bytes;
        Ok(bytes)
    }
    pub fn probe_count(&self) -> usize {
        self.metadata.len()
    }
    pub fn probe_position(&self, slot: usize) -> Option<[f32; 3]> {
        self.metadata.get(slot).map(|probe| probe.position)
    }
    /// Logical grid coordinates map to stable physical slots until recycled.
    pub fn probe_slot(&self, coordinate: [u32; 3]) -> Option<usize> {
        coordinate
            .iter()
            .enumerate()
            .all(|(axis, &v)| v < self.config.probe_counts[axis])
            .then(|| {
                ring_index(
                    coordinate,
                    self.config.probe_counts,
                    self.parameters.probe_counts[3],
                )
            })
    }

    /// Scroll the source AOI and probe lattice without moving overlapping GPU data.
    /// `probe_shift` is an integer number of spacings, independent of page motion.
    /// Entering bricks become Unknown before any new traces; entering probes lose
    /// validity and advance generation. The complete upload is preflighted.
    pub fn recenter(
        &mut self,
        queue: &wgpu::Queue,
        world_origin_page: [i32; 3],
        probe_shift: [i32; 3],
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<GiScrollWork, GiError> {
        if world_origin_page == self.config.world_origin_page && probe_shift == [0; 3] {
            return Ok(GiScrollWork {
                retained_probes: self.metadata.len() as u32,
                ..GiScrollWork::default()
            });
        }
        let origin = std::array::from_fn::<_, 3, _>(|axis| {
            self.config.probe_origin[axis] + probe_shift[axis] as f32 * self.config.probe_spacing
        });
        if origin
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
        {
            return Err(GiError::InvalidConfig);
        }
        let revision = self.parameters.stamps[0]
            .checked_add(1)
            .ok_or(GiError::Capacity)?;
        let delta = std::array::from_fn::<_, 3, _>(|axis| {
            i64::from(world_origin_page[axis]) - i64::from(self.config.world_origin_page[axis])
        });
        let world_ring = shift_ring(
            self.parameters.world_counts[3],
            self.config.world_pages,
            delta,
        );
        let probe_ring = shift_ring(
            self.parameters.probe_counts[3],
            self.config.probe_counts,
            probe_shift.map(i64::from),
        );
        let mut work = GiScrollWork::default();
        for index in 0..self.metadata.len() {
            let coordinate = grid_coordinate(index, self.config.probe_counts);
            if !overlaps(
                coordinate,
                probe_shift.map(i64::from),
                self.config.probe_counts,
            ) {
                let slot = ring_index(coordinate, self.config.probe_counts, probe_ring);
                self.metadata[slot]
                    .generation
                    .checked_add(1)
                    .ok_or(GiError::Capacity)?;
                work.recycled_probes += 1;
            }
        }
        for index in 0..self.page_versions.len() {
            if !overlaps(
                grid_coordinate(index, self.config.world_pages),
                delta,
                self.config.world_pages,
            ) {
                work.cleared_pages += 1;
            }
        }
        work.retained_probes = self.metadata.len() as u32 - work.recycled_probes;
        work.upload_bytes =
            96 + u64::from(work.recycled_probes) * 128 + u64::from(work.cleared_pages) * 4096 * 4;
        if budget.remaining_bytes < work.upload_bytes {
            return Err(GiError::UploadBudget);
        }
        // A source revision can invalidate a previously encoded job even when
        // its probe lies outside the changed page's influence. Requeue it.
        self.invalidate_queued_jobs(tick);
        for index in 0..self.page_versions.len() {
            let coordinate = grid_coordinate(index, self.config.world_pages);
            if !overlaps(coordinate, delta, self.config.world_pages) {
                let slot = ring_index(coordinate, self.config.world_pages, world_ring);
                self.page_staging.fill(0);
                queue.write_buffer(
                    &self.world,
                    slot as u64 * 4096 * 4,
                    bytemuck::cast_slice(&*self.page_staging),
                );
                self.page_versions[slot] = None;
            }
        }
        for index in 0..self.metadata.len() {
            let coordinate = grid_coordinate(index, self.config.probe_counts);
            if !overlaps(
                coordinate,
                probe_shift.map(i64::from),
                self.config.probe_counts,
            ) {
                let slot = ring_index(coordinate, self.config.probe_counts, probe_ring);
                let prior = &self.metadata[slot];
                let generation = prior.generation + 1;
                let position = std::array::from_fn(|axis| {
                    origin[axis] + coordinate[axis] as f32 * self.config.probe_spacing
                });
                if !prior.dirty {
                    self.dirty += 1;
                }
                self.metadata[slot] = ProbeMeta {
                    position,
                    generation,
                    dirty: true,
                    visible: false,
                    edited: false,
                    dirty_since: tick,
                };
                let sample = ProbeSample {
                    position: [
                        position[0],
                        position[1],
                        position[2],
                        self.config.probe_spacing,
                    ],
                    stamps: [generation, revision, 0, 0],
                    ..ProbeSample::zeroed()
                };
                queue.write_buffer(&self.probes, slot as u64 * 128, bytemuck::bytes_of(&sample));
            }
        }
        // Cached probes may have seen the entering/leaving slices. Keep their
        // result until bounded recomputation; only affected probes become dirty.
        for index in 0..self.page_versions.len() {
            let coordinate = grid_coordinate(index, self.config.world_pages);
            if !overlaps(coordinate, delta, self.config.world_pages) {
                self.mark_page(
                    std::array::from_fn(|axis| {
                        (i64::from(world_origin_page[axis]) + i64::from(coordinate[axis])) as i32
                    }),
                    tick,
                );
            }
            if !overlaps(coordinate, delta.map(|v| -v), self.config.world_pages) {
                self.mark_page(
                    std::array::from_fn(|axis| {
                        (i64::from(self.config.world_origin_page[axis])
                            + i64::from(coordinate[axis])) as i32
                    }),
                    tick,
                );
            }
        }
        self.config.world_origin_page = world_origin_page;
        self.config.probe_origin = origin;
        self.parameters.world_origin[..3].copy_from_slice(&world_origin_page);
        self.parameters.origin_spacing[..3].copy_from_slice(&origin);
        self.parameters.world_counts[3] = world_ring;
        self.parameters.probe_counts[3] = probe_ring;
        self.parameters.stamps[0] = revision;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.parameters));
        budget.remaining_bytes -= work.upload_bytes;
        Ok(work)
    }
    fn invalidate_queued_jobs(&mut self, tick: u64) {
        for job in &self.jobs {
            let probe = &mut self.metadata[job.slot as usize];
            if job.generation == probe.generation && !probe.dirty {
                probe.dirty = true;
                probe.dirty_since = tick;
                self.dirty += 1;
            }
        }
        self.jobs.clear();
    }
    pub fn set_visible(&mut self, index: usize, visible: bool) -> Result<(), GiError> {
        self.metadata
            .get_mut(index)
            .ok_or(GiError::InvalidUpdates)?
            .visible = visible;
        Ok(())
    }
    pub fn pending(&self) -> u32 {
        self.dirty
    }
    pub fn config(&self) -> GiConfig {
        self.config
    }
    pub fn source_revision(&self) -> u32 {
        self.parameters.stamps[0]
    }
    pub fn sampling_buffer(&self) -> &wgpu::Buffer {
        &self.uniform
    }
    pub fn probe_buffer(&self) -> &wgpu::Buffer {
        &self.probes
    }
    /// Existing reflected-source voxels, used read-only to keep a model
    /// receiver in an Unknown cell from borrowing nearby probe radiance.
    pub fn voxel_buffer(&self) -> &wgpu::Buffer {
        &self.world
    }
    pub fn frame_graph(&self) -> crate::gpu_time::PassGraph<2> {
        self.timer.graph()
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.timer.timestamp_buffer()
    }
    pub fn statistics_buffer(&self) -> &wgpu::Buffer {
        &self.statistics
    }
    fn page_index(&self, page: [i32; 3]) -> Result<usize, GiError> {
        let coordinate = std::array::from_fn::<_, 3, _>(|axis| {
            i64::from(page[axis]) - i64::from(self.config.world_origin_page[axis])
        });
        if coordinate
            .iter()
            .enumerate()
            .any(|(axis, value)| *value < 0 || *value >= i64::from(self.config.world_pages[axis]))
        {
            return Err(GiError::InvalidPage);
        }
        Ok(ring_index(
            coordinate.map(|v| v as u32),
            self.config.world_pages,
            self.parameters.world_counts[3],
        ))
    }
    /// Identity of the source currently occupying this world page's ring slot.
    /// A recycled/Unknown slot is None even if a CPU producer still caches it.
    pub fn source_page_version(&self, page: [i32; 3]) -> Result<Option<u64>, GiError> {
        Ok(self.page_versions[self.page_index(page)?])
    }
    fn mark_page(&mut self, page: [i32; 3], tick: u64) {
        let low = page.map(|v| v as f32 * 16.0);
        let high = low.map(|v| v + 16.0);
        let distance2 = self.config.max_distance * self.config.max_distance;
        for probe in &mut self.metadata {
            let squared = (0..3)
                .map(|axis| {
                    let delta =
                        probe.position[axis] - probe.position[axis].clamp(low[axis], high[axis]);
                    delta * delta
                })
                .sum::<f32>();
            if squared <= distance2 {
                if !probe.dirty {
                    probe.dirty = true;
                    probe.dirty_since = tick;
                    self.dirty += 1;
                }
                probe.edited = true;
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn upload_page(
        &mut self,
        queue: &wgpu::Queue,
        page: [i32; 3],
        version: u64,
        voxels: &[GiVoxel; 4096],
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<u64, GiError> {
        if self.directional.is_some() {
            return Err(GiError::DirectionalRequired);
        }
        self.upload_page_core(queue, page, version, voxels, tick, budget)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn upload_page_directional(
        &mut self,
        queue: &wgpu::Queue,
        page: [i32; 3],
        version: u64,
        voxels: &[GiVoxel; 4096],
        surfaces: &[GiSurface; 4096],
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<u64, GiError> {
        let index = self.page_index(page)?;
        if self.directional.is_none() {
            return Err(GiError::InvalidConfig);
        }
        if let Some(prior) = self.page_versions[index] {
            if version < prior {
                return Err(GiError::StaleVersion);
            }
            if version == prior {
                return Ok(0);
            }
        }
        let additional = 4096 * 48;
        if budget.remaining_bytes < 4096 * 4 + 96 + additional {
            return Err(GiError::UploadBudget);
        }
        let bytes = self.upload_page_core(queue, page, version, voxels, tick, budget)?;
        queue.write_buffer(
            &self
                .directional
                .as_ref()
                .expect("checked directional mode")
                .surfaces,
            index as u64 * 4096 * 48,
            bytemuck::cast_slice(surfaces),
        );
        budget.remaining_bytes -= additional;
        Ok(bytes + additional)
    }
    #[allow(clippy::too_many_arguments)]
    fn upload_page_core(
        &mut self,
        queue: &wgpu::Queue,
        page: [i32; 3],
        version: u64,
        voxels: &[GiVoxel; 4096],
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<u64, GiError> {
        let index = self.page_index(page)?;
        if let Some(prior) = self.page_versions[index] {
            if version < prior {
                return Err(GiError::StaleVersion);
            }
            if version == prior {
                return Ok(0);
            }
        }
        let revision = self.parameters.stamps[0]
            .checked_add(1)
            .ok_or(GiError::Capacity)?;
        if budget.remaining_bytes < 4096 * 4 + 96 {
            return Err(GiError::UploadBudget);
        }
        self.invalidate_queued_jobs(tick);
        queue.write_buffer(
            &self.world,
            index as u64 * 4096 * 4,
            bytemuck::cast_slice(voxels),
        );
        self.parameters.stamps[0] = revision;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.parameters));
        budget.remaining_bytes -= 4096 * 4 + 96;
        self.page_versions[index] = Some(version);
        self.mark_page(page, tick);
        Ok(4096 * 4 + 96)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn upload_cells(
        &mut self,
        queue: &wgpu::Queue,
        page: [i32; 3],
        version: u64,
        updates: &[CellUpdate],
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<u64, GiError> {
        if self.directional.is_some() {
            return Err(GiError::DirectionalRequired);
        }
        self.upload_cells_core(queue, page, version, updates, tick, budget)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn upload_cells_directional(
        &mut self,
        queue: &wgpu::Queue,
        page: [i32; 3],
        version: u64,
        updates: &[CellUpdate],
        surfaces: &[SurfaceUpdate],
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<u64, GiError> {
        let index = self.page_index(page)?;
        if self.directional.is_none() {
            return Err(GiError::InvalidConfig);
        }
        let prior = self.page_versions[index].ok_or(GiError::InvalidPage)?;
        if version < prior {
            return Err(GiError::StaleVersion);
        }
        if version == prior || updates.is_empty() {
            return Ok(0);
        }
        if surfaces.len() != updates.len()
            || surfaces
                .iter()
                .zip(updates)
                .any(|(a, b)| a.local != b.local)
        {
            return Err(GiError::InvalidUpdates);
        }
        let additional = surfaces.len() as u64 * 48;
        if budget.remaining_bytes < updates.len() as u64 * 4 + 96 + additional {
            return Err(GiError::UploadBudget);
        }
        let bytes = self.upload_cells_core(queue, page, version, updates, tick, budget)?;
        let directional = self.directional.as_mut().expect("checked directional mode");
        for (entry, update) in directional.staging.iter_mut().zip(surfaces) {
            *entry = update.value;
        }
        let mut first = 0;
        while first < surfaces.len() {
            let mut end = first + 1;
            while end < surfaces.len() && surfaces[end].local == surfaces[end - 1].local + 1 {
                end += 1;
            }
            queue.write_buffer(
                &directional.surfaces,
                index as u64 * 4096 * 48 + u64::from(surfaces[first].local) * 48,
                bytemuck::cast_slice(&directional.staging[first..end]),
            );
            first = end;
        }
        budget.remaining_bytes -= additional;
        Ok(bytes + additional)
    }
    #[allow(clippy::too_many_arguments)]
    fn upload_cells_core(
        &mut self,
        queue: &wgpu::Queue,
        page: [i32; 3],
        version: u64,
        updates: &[CellUpdate],
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<u64, GiError> {
        let index = self.page_index(page)?;
        let Some(prior) = self.page_versions[index] else {
            return Err(GiError::InvalidPage);
        };
        if version < prior {
            return Err(GiError::StaleVersion);
        }
        if version == prior || updates.is_empty() {
            return Ok(0);
        }
        let mut previous = None;
        for update in updates {
            if update.local >= 4096 || previous.is_some_and(|prior| prior >= update.local) {
                return Err(GiError::InvalidUpdates);
            }
            previous = Some(update.local);
        }
        let revision = self.parameters.stamps[0]
            .checked_add(1)
            .ok_or(GiError::Capacity)?;
        let bytes = updates.len() as u64 * 4 + 96;
        if budget.remaining_bytes < bytes {
            return Err(GiError::UploadBudget);
        }
        self.invalidate_queued_jobs(tick);
        for (word, update) in self.page_staging.iter_mut().zip(updates) {
            *word = update.value.0;
        }
        let mut first = 0;
        while first < updates.len() {
            let mut end = first + 1;
            while end < updates.len() && updates[end].local == updates[end - 1].local + 1 {
                end += 1;
            }
            queue.write_buffer(
                &self.world,
                index as u64 * 4096 * 4 + u64::from(updates[first].local) * 4,
                bytemuck::cast_slice(&self.page_staging[first..end]),
            );
            first = end;
        }
        self.parameters.stamps[0] = revision;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.parameters));
        budget.remaining_bytes -= bytes;
        self.page_versions[index] = Some(version);
        self.mark_page(page, tick);
        Ok(bytes)
    }
    pub fn trace_tick(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        tick: u64,
        budget: &mut UploadBudget,
        remaining_rays: &mut u32,
    ) -> GiWork {
        self.timer.begin();
        let mut work = GiWork {
            pending_probes: self.dirty,
            ..GiWork::default()
        };
        if self.dirty == 0 {
            return work;
        }
        let rays = self.config.rays_per_probe + 6;
        let capacity = self
            .config
            .max_jobs
            .min(*remaining_rays / rays)
            .min((budget.remaining_bytes / 16).min(u64::from(u32::MAX)) as u32);
        if capacity == 0 {
            return work;
        }
        self.candidates.clear();
        for (index, probe) in self.metadata.iter().enumerate() {
            if probe.dirty {
                self.candidates.push(index);
            }
        }
        work.candidates_scanned = self.metadata.len() as u32;
        let key = |index: usize| {
            let probe = &self.metadata[index];
            let age = tick.saturating_sub(probe.dirty_since);
            match self.config.priority {
                Priority::EditedVisible => (
                    age >= 12,
                    probe.edited && probe.visible,
                    probe.visible,
                    probe.edited,
                    age,
                ),
                Priority::FifoControl => (false, false, false, false, age),
            }
        };
        self.candidates
            .sort_unstable_by(|a, b| key(*b).cmp(&key(*a)).then(a.cmp(b)));
        self.jobs.clear();
        for index in self.candidates.iter().take(capacity as usize) {
            let probe = &mut self.metadata[*index];
            self.jobs.push(Job {
                slot: *index as u32,
                generation: probe.generation,
                revision: self.parameters.stamps[0],
                rays: self.config.rays_per_probe,
            });
            probe.dirty = false;
            probe.edited = false;
            self.dirty -= 1;
        }
        let bytes = self.jobs.len() as u64 * 16;
        queue.write_buffer(&self.job_buffer, 0, bytemuck::cast_slice(&self.jobs));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("reset GI DDA counters"),
                timestamp_writes: self.timer.compute(1),
            });
            pass.set_pipeline(
                self.directional
                    .as_ref()
                    .map_or(&self.reset, |mode| &mode.reset),
            );
            pass.set_bind_group(
                0,
                self.directional
                    .as_ref()
                    .map_or(&self.bind, |mode| &mode.bind),
                &[],
            );
            pass.dispatch_workgroups(1, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("edited-visible GI DDA probes"),
                timestamp_writes: self.timer.compute(2),
            });
            pass.set_pipeline(
                self.directional
                    .as_ref()
                    .map_or(&self.trace, |mode| &mode.trace),
            );
            pass.set_bind_group(
                0,
                self.directional
                    .as_ref()
                    .map_or(&self.bind, |mode| &mode.bind),
                &[],
            );
            pass.dispatch_workgroups(self.jobs.len() as u32, 1, 1);
        }
        self.timer.finish(encoder);
        self.has_traced = true;
        work.updated_probes = self.jobs.len() as u32;
        work.rays = work.updated_probes * rays;
        work.max_dda_steps = u64::from(work.rays) * u64::from(self.config.max_steps);
        work.max_shadow_samples = if self.directional.is_some() {
            u64::from(work.updated_probes) * u64::from(self.config.rays_per_probe) * 8
        } else {
            0
        };
        work.upload_bytes = bytes;
        work.pending_probes = self.dirty;
        work.passes = 2;
        budget.remaining_bytes -= bytes;
        *remaining_rays -= work.rays;
        work
    }
    /// Called if an encoded trace batch was abandoned before submission.
    pub fn invalidate(&mut self, tick: u64) {
        for probe in &mut self.metadata {
            if !probe.dirty {
                probe.dirty = true;
                probe.dirty_since = tick;
                self.dirty += 1;
            }
        }
    }
}

// Fixed numerical quadrature, generated from the golden-angle sphere. Using
// literal f32 values avoids backend differences in transcendental functions.
#[allow(clippy::excessive_precision)] // Fixed quadrature coefficients retain explicit decimal provenance.
pub const RAYS: [[f32; 4]; 64] = [
    [0.176084807, 0.984375000, 0.000000000, 0.0],
    [-0.223110733, 0.953125000, 0.204387708, 0.0],
    [0.033876367, 0.921875000, -0.386003726, 0.0],
    [0.276680579, 0.890625000, 0.360880820, 0.0],
    [-0.503529184, 0.859375000, -0.089067223, 0.0],
    [0.472961523, 0.828125000, -0.300859406, 0.0],
    [-0.156838439, 0.796875000, 0.583431177, 0.0],
    [-0.296495567, 0.765625000, -0.570884172, 0.0],
    [0.637558654, 0.734375000, 0.232835398, 0.0],
    [-0.657270943, 0.703125000, 0.271311891, 0.0],
    [0.313927706, 0.671875000, -0.670845273, 0.0],
    [0.229806282, 0.640625000, 0.732658640, 0.0],
    [-0.686010527, 0.609375000, -0.397557123, 0.0],
    [0.796916851, 0.578125000, -0.175199934, 0.0],
    [-0.481506565, 0.546875000, 0.684893541, 0.0],
    [-0.110109805, 0.515625000, -0.849709768, 0.0],
    [0.668961187, 0.484375000, 0.563801197, 0.0],
    [-0.890685742, 0.453125000, 0.036832652, 0.0],
    [0.642663027, 0.421875000, -0.639535549, 0.0],
    [-0.042521531, 0.390625000, 0.919567305, 0.0],
    [-0.597905438, 0.359375000, -0.716490542, 0.0],
    [0.936198173, 0.328125000, 0.125964140, 0.0],
    [-0.783851047, 0.296875000, 0.545383142, 0.0],
    [0.211596815, 0.265625000, -0.940569055, 0.0],
    [0.483332530, 0.234375000, 0.843479712, 0.0],
    [-0.932831814, 0.203125000, -0.297598791, 0.0],
    [0.894282082, 0.171875000, -0.413181004, 0.0],
    [-0.382224767, 0.140625000, 0.913306540, 0.0],
    [-0.336421755, 0.109375000, -0.935338181, 0.0],
    [0.882483919, 0.078125000, 0.463808815, 0.0],
    [-0.965907076, 0.046875000, 0.254610005, 0.0],
    [0.540771714, 0.015625000, -0.841024264, 0.0],
    [0.169355495, -0.015625000, 0.985431162, 0.0],
    [-0.789754093, -0.046875000, -0.611629960, 0.0],
    [0.993539681, -0.078125000, -0.082312732, 0.0],
    [-0.675005249, -0.109375000, 0.729660896, 0.0],
    [0.004829801, -0.140625000, -0.990051151, 0.0],
    [0.661886687, -0.171875000, 0.729633468, 0.0],
    [-0.974974476, -0.203125000, -0.090360415, 0.0],
    [0.774372286, -0.234375000, -0.587720956, 0.0],
    [-0.172553854, -0.265625000, 0.948508580, 0.0],
    [-0.508593789, -0.296875000, -0.808206404, 0.0],
    [0.911040435, -0.328125000, 0.249678413, 0.0],
    [-0.830249236, -0.359375000, 0.426070200, 0.0],
    [0.319998626, -0.390625000, -0.863141349, 0.0],
    [0.341847233, -0.421875000, 0.839739218, 0.0],
    [-0.805561293, -0.453125000, -0.381770530, 0.0],
    [0.836027835, -0.484375000, -0.257756317, 0.0],
    [-0.433224498, -0.515625000, 0.739220802, 0.0],
    [-0.175775128, -0.546875000, -0.818554115, 0.0],
    [0.665199377, -0.578125000, 0.472526479, 0.0],
    [-0.786795383, -0.609375000, 0.098056793, 0.0],
    [0.497699465, -0.640625000, -0.584717754, 0.0],
    [0.026992204, -0.671875000, 0.740172551, 0.0],
    [-0.499107141, -0.703125000, -0.506465494, 0.0],
    [0.677858457, -0.734375000, 0.034659359, 0.0],
    [-0.495910135, -0.765625000, 0.409745649, 0.0],
    [0.083480716, -0.796875000, -0.598348731, 0.0],
    [0.317895795, -0.828125000, 0.461683060, 0.0],
    [-0.498324146, -0.859375000, -0.114663222, 0.0],
    [0.395650686, -0.890625000, -0.224159862, 0.0],
    [-0.119570421, -0.921875000, 0.368577534, 0.0],
    [-0.125566078, -0.953125000, -0.275292380, 0.0],
    [0.162099964, -0.984375000, 0.068771078, 0.0],
];
