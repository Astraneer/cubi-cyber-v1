//! Bounded actual-world height tiles, with implicit grid vertices and GPU culling.
//! This renderer accepts only validated public tiles. It never generates terrain.
use crate::{
    height_tile::{HeightTile, HeightTileKey},
    shadows::ShadowGpu,
    terrain_gpu::{Camera, TerrainGeometry, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
pub const PAYLOAD_WORDS: usize = 819;
pub const TILE_BYTES: u64 = PAYLOAD_WORDS as u64 * 4;
#[derive(Clone, Copy, Debug)]
pub struct HeightGpuConfig {
    pub max_tiles: u32,
    pub atlas_layers: u32,
}
impl Default for HeightGpuConfig {
    fn default() -> Self {
        Self {
            max_tiles: 512,
            atlas_layers: 186,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeightHandle {
    pub slot: u32,
    pub generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightGpuError {
    InvalidConfig,
    InvalidTile,
    InvalidTransform,
    InvalidCamera,
    Capacity,
    StaleHandle,
    StaleSource,
    ConflictingSource,
    UploadBudget,
    InvalidBatch,
}
impl std::fmt::Display for HeightGpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for HeightGpuError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeightGpuWork {
    pub upload_bytes: u64,
    pub tile_uploads: u32,
    pub header_uploads: u32,
    pub cpu_vertices: u32,
    pub cull_dispatches: u32,
    pub camera_upload_bytes: u64,
    pub pipeline_compilations: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct TileGpu {
    origin: [f32; 3],
    step: f32,
    params: [u32; 4],
    minimum: [f32; 4],
    maximum: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    view_projection: [[f32; 4]; 4],
    params: [u32; 4],
    reserved: [u32; 4],
}
#[derive(Clone, Copy, Default)]
struct Slot {
    generation: u32,
    key: Option<HeightTileKey>,
    source: u64,
    hash: u64,
    anchor: i64,
    minimum: i32,
    maximum: i32,
    drawable: bool,
    enabled: bool,
    gpu: TileGpu,
}
impl Default for TileGpu {
    fn default() -> Self {
        Self::zeroed()
    }
}
pub struct HeightGpu {
    config: HeightGpuConfig,
    headers: wgpu::Buffer,
    payload: wgpu::Buffer,
    camera: wgpu::Buffer,
    indirect: wgpu::Buffer,
    bind: wgpu::BindGroup,
    cull_bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    slots: Vec<Slot>,
    free: Vec<u32>,
    staging: [u32; PAYLOAD_WORDS],
    revision: u64,
    last: Option<(Camera, u64)>,
    work: HeightGpuWork,
}
impl HeightGpu {
    pub fn new(
        device: &wgpu::Device,
        geometry: TerrainGeometry<'_>,
        config: HeightGpuConfig,
        shadows: Option<&ShadowGpu>,
    ) -> Result<Self, HeightGpuError> {
        Self::new_prepared(device, geometry, config, shadows, None)
    }
    /// Prepare shared atmospheric lighting once during loading.
    pub fn new_with_atmosphere(
        device: &wgpu::Device,
        geometry: TerrainGeometry<'_>,
        config: HeightGpuConfig,
        shadows: Option<&ShadowGpu>,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
    ) -> Result<Self, HeightGpuError> {
        Self::new_prepared(device, geometry, config, shadows, Some(atmosphere))
    }
    fn new_prepared(
        device: &wgpu::Device,
        geometry: TerrainGeometry<'_>,
        config: HeightGpuConfig,
        shadows: Option<&ShadowGpu>,
        atmosphere: Option<&crate::atmosphere::AtmosphereGpu>,
    ) -> Result<Self, HeightGpuError> {
        let sample_count = geometry.sample_count;
        if config.max_tiles == 0
            || config.atlas_layers == 0
            || u64::from(config.max_tiles) * TILE_BYTES
                > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(HeightGpuError::InvalidConfig);
        }
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let headers = buffer(
            "64B stable height tile descriptors",
            u64::from(config.max_tiles) * 64,
            storage,
        );
        let payload = buffer(
            "public height tile samples and holes",
            u64::from(config.max_tiles) * TILE_BYTES,
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let camera = buffer(
            "height tile camera",
            96,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let visible = buffer(
            "GPU visible height tiles",
            u64::from(config.max_tiles) * 4,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = buffer(
            "one height grid indirect draw",
            16,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
        );
        let v = wgpu::ShaderStages::VERTEX;
        let f = wgpu::ShaderStages::FRAGMENT;
        let c = wgpu::ShaderStages::COMPUTE;
        let mut entries = vec![
            buffer_layout(0, v, true, true),
            buffer_layout(1, v, false, true),
            buffer_layout(2, v, false, true),
            buffer_layout(3, v, false, true),
            texture_layout(4, f, wgpu::TextureSampleType::Float { filterable: true }),
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: f,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        if shadows.is_some() {
            entries.extend([
                buffer_layout(9, f, true, true),
                texture_layout(10, f, wgpu::TextureSampleType::Depth),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: f,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ]);
        }
        if atmosphere.is_some() {
            entries.push(buffer_layout(20, f, false, true));
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("public height grid draw layout"),
            entries: &entries,
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("height tile GPU visibility"),
            entries: &[
                buffer_layout(0, c, true, true),
                buffer_layout(1, c, false, true),
                buffer_layout(2, c, false, false),
                buffer_layout(3, c, false, false),
            ],
        });
        let mut entries = vec![
            buffer_binding(0, &camera),
            buffer_binding(1, &headers),
            buffer_binding(2, &payload),
            buffer_binding(3, &visible),
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(geometry.colors),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(geometry.sampler),
            },
        ];
        if let Some(s) = shadows {
            entries.extend([
                buffer_binding(9, s.sampling_buffer()),
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(s.depth_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(s.sampler()),
                },
            ]);
        }
        if let Some(atmosphere) = atmosphere {
            entries.push(buffer_binding(20, atmosphere.lighting_buffer()));
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("public height grid sources"),
            layout: &layout,
            entries: &entries,
        });
        let cull_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("height grid indirect compaction"),
            layout: &cull_layout,
            entries: &[
                buffer_binding(0, &camera),
                buffer_binding(1, &headers),
                buffer_binding(2, &visible),
                buffer_binding(3, &indirect),
            ],
        });
        let source = crate::atmosphere::surface_shader(
            include_str!("height_grid.wgsl"),
            shadows.is_some(),
            atmosphere.is_some(),
            false,
            if shadows.is_some() {
                "return vec3(0.15+0.85*cascade_shadow(world,normal)*max(dot(normal,normalize(shadow_sampling.sun.xyz)),0.));"
            } else {
                "return vec3(0.65+0.35*max(dot(normal,normalize(vec3(0.35,0.8,0.2))),0.));"
            },
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("implicit stitched height grid"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("height grid HDR layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("actual public height tiles HDR MSAA4"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("height frustum compaction"),
            source: wgpu::ShaderSource::Wgsl(include_str!("height_cull.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("height cull layout"),
            bind_group_layouts: &[&cull_layout],
            push_constant_ranges: &[],
        });
        let compute = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let reset = compute("reset");
        let cull = compute("cull");
        Ok(Self {
            config,
            headers,
            payload,
            camera,
            indirect,
            bind,
            cull_bind,
            pipeline,
            reset,
            cull,
            slots: vec![Slot::default(); config.max_tiles as usize],
            free: (0..config.max_tiles).rev().collect(),
            staging: [0; PAYLOAD_WORDS],
            revision: 0,
            last: None,
            work: HeightGpuWork {
                pipeline_compilations: 3,
                ..HeightGpuWork::default()
            },
        })
    }
    /// New covers can be prepared disabled, then enabled atomically with the old
    /// cover disabled through set_enabled_batch after protected proxies are ready.
    #[allow(clippy::too_many_arguments)] // Validated source and its publication epoch are supplied together.
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        handle: Option<HeightHandle>,
        tile: &HeightTile,
        seams: u32,
        origin: [i64; 3],
        enabled: bool,
        budget: &mut UploadBudget,
    ) -> Result<HeightHandle, HeightGpuError> {
        self.upload_inner(queue, handle, tile, seams, origin, enabled, false, budget)
    }
    /// A disabled replacement may coexist with the active copy of the same
    /// tile. Its writes cannot alter the current front; publish all affected
    /// handles together only after source/seam/protected-geometry validation.
    pub fn upload_staged(
        &mut self,
        queue: &wgpu::Queue,
        tile: &HeightTile,
        seams: u32,
        origin: [i64; 3],
        budget: &mut UploadBudget,
    ) -> Result<HeightHandle, HeightGpuError> {
        let hash = tile.canonical_hash();
        let mut copies = 0;
        for old in &self.slots {
            if old.key != Some(tile.key) {
                continue;
            }
            copies += 1;
            if tile.source < old.source {
                return Err(HeightGpuError::StaleSource);
            }
            if tile.source == old.source && hash != old.hash {
                return Err(HeightGpuError::ConflictingSource);
            }
        }
        if copies >= 2 {
            return Err(HeightGpuError::Capacity);
        }
        self.upload_inner(queue, None, tile, seams, origin, false, true, budget)
    }
    #[allow(clippy::too_many_arguments)] // Internal preflight shared by active updates and disabled staging.
    fn upload_inner(
        &mut self,
        queue: &wgpu::Queue,
        handle: Option<HeightHandle>,
        tile: &HeightTile,
        seams: u32,
        origin: [i64; 3],
        enabled: bool,
        duplicate_staging: bool,
        budget: &mut UploadBudget,
    ) -> Result<HeightHandle, HeightGpuError> {
        tile.validate(self.config.atlas_layers)
            .map_err(|_| HeightGpuError::InvalidTile)?;
        if seams & !15 != 0
            || origin
                .iter()
                .any(|v| !((i64::from(i32::MIN))..=i64::from(i32::MAX)).contains(v))
        {
            return Err(HeightGpuError::InvalidTransform);
        }
        let hash = tile.canonical_hash();
        let (slot, old) = if let Some(h) = handle {
            let old = *self.checked(h)?;
            if old.key != Some(tile.key) {
                return Err(HeightGpuError::StaleHandle);
            }
            if tile.source < old.source {
                return Err(HeightGpuError::StaleSource);
            }
            if tile.source == old.source && hash != old.hash {
                return Err(HeightGpuError::ConflictingSource);
            }
            (h.slot, old)
        } else {
            if !duplicate_staging && self.slots.iter().any(|s| s.key == Some(tile.key)) {
                return Err(HeightGpuError::InvalidTile);
            }
            let slot = *self.free.last().ok_or(HeightGpuError::Capacity)?;
            (slot, self.slots[slot as usize])
        };
        let mut minimum = i32::MAX;
        let mut maximum = i32::MIN;
        for (i, h) in tile.heights.iter().enumerate() {
            if tile.sample_known(i % 17, i / 17) {
                minimum = minimum.min(*h);
                maximum = maximum.max(*h);
            }
        }
        let drawable = tile.drawable_cells(seams) > 0;
        if minimum == i32::MAX {
            minimum = 0;
            maximum = 0;
        }
        let anchor = i64::from(minimum).div_euclid(16) * 16;
        let gpu = descriptor(
            tile.key, anchor, minimum, maximum, drawable, enabled, seams, origin,
        )?;
        let payload_changed = old.key.is_none() || old.hash != hash;
        let header_changed = old.key.is_none() || old.gpu != gpu;
        let bytes = u64::from(payload_changed) * TILE_BYTES + u64::from(header_changed) * 64;
        if budget.remaining_bytes < bytes {
            return Err(HeightGpuError::UploadBudget);
        }
        // Generation exhaustion must fail before any queue write.
        let generation = if old.key.is_some() {
            old.generation
        } else {
            old.generation
                .checked_add(1)
                .ok_or(HeightGpuError::Capacity)?
        };
        if payload_changed {
            self.staging.fill(0);
            for (out, h) in self.staging[..289].iter_mut().zip(tile.heights) {
                *out = ((f64::from(h) - anchor as f64) as f32).to_bits();
            }
            for (i, b) in tile.known.iter().enumerate() {
                self.staging[289 + i / 4] |= u32::from(*b) << ((i % 4) * 8);
            }
            for i in 0..256 {
                self.staging[299 + i] = u32::from(tile.atlas[i]);
                self.staging[555 + i] = tile.tint[i];
            }
            for (i, b) in tile.protected.iter().enumerate() {
                self.staging[811 + i / 4] |= u32::from(*b) << ((i % 4) * 8);
            }
            queue.write_buffer(
                &self.payload,
                u64::from(slot) * TILE_BYTES,
                bytemuck::cast_slice(&self.staging),
            );
        }
        if header_changed {
            queue.write_buffer(
                &self.headers,
                u64::from(slot) * 64,
                bytemuck::bytes_of(&gpu),
            );
            self.revision = self.revision.wrapping_add(1)
        }
        if old.key.is_none() {
            self.free.pop();
        }
        self.slots[slot as usize] = Slot {
            generation,
            key: Some(tile.key),
            source: tile.source,
            hash,
            anchor,
            minimum,
            maximum,
            drawable,
            enabled,
            gpu,
        };
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        self.work.tile_uploads += u32::from(payload_changed);
        self.work.header_uploads += u32::from(header_changed);
        Ok(HeightHandle { slot, generation })
    }
    fn checked(&self, h: HeightHandle) -> Result<&Slot, HeightGpuError> {
        self.slots
            .get(h.slot as usize)
            .filter(|s| s.key.is_some() && s.generation == h.generation)
            .ok_or(HeightGpuError::StaleHandle)
    }
    pub fn set_enabled_batch(
        &mut self,
        queue: &wgpu::Queue,
        changes: &[(HeightHandle, bool)],
        budget: &mut UploadBudget,
    ) -> Result<u64, HeightGpuError> {
        if changes.windows(2).any(|p| p[0].0.slot >= p[1].0.slot) {
            return Err(HeightGpuError::InvalidBatch);
        }
        let mut bytes = 0;
        for (h, on) in changes {
            if self.checked(*h)?.enabled != *on {
                bytes += 4
            }
        }
        if budget.remaining_bytes < bytes {
            return Err(HeightGpuError::UploadBudget);
        }
        for (h, on) in changes {
            let s = &mut self.slots[h.slot as usize];
            if s.enabled != *on {
                s.enabled = *on;
                s.gpu.params[0] = u32::from(*on && s.drawable);
                queue.write_buffer(
                    &self.headers,
                    u64::from(h.slot) * 64 + 16,
                    bytemuck::bytes_of(&s.gpu.params[0]),
                );
            }
        }
        if bytes > 0 {
            self.revision = self.revision.wrapping_add(1)
        }
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        Ok(bytes)
    }
    pub fn release(
        &mut self,
        queue: &wgpu::Queue,
        handle: HeightHandle,
        budget: &mut UploadBudget,
    ) -> Result<(), HeightGpuError> {
        self.checked(handle)?;
        if budget.remaining_bytes < 4 {
            return Err(HeightGpuError::UploadBudget);
        }
        queue.write_buffer(&self.headers, u64::from(handle.slot) * 64 + 16, &[0; 4]);
        let s = &mut self.slots[handle.slot as usize];
        s.key = None;
        s.enabled = false;
        s.gpu.params[0] = 0;
        if s.generation < u32::MAX {
            self.free.push(handle.slot)
        }
        self.revision = self.revision.wrapping_add(1);
        budget.remaining_bytes -= 4;
        self.work.upload_bytes += 4;
        Ok(())
    }
    pub fn rebase(
        &mut self,
        queue: &wgpu::Queue,
        origin: [i64; 3],
        budget: &mut UploadBudget,
    ) -> Result<u64, HeightGpuError> {
        let mut changed = 0;
        for s in &self.slots {
            if let Some(key) = s.key {
                let d = descriptor(
                    key,
                    s.anchor,
                    s.minimum,
                    s.maximum,
                    s.drawable,
                    s.enabled,
                    s.gpu.params[1],
                    origin,
                )?;
                if s.gpu != d {
                    changed += 1;
                }
            }
        }
        let bytes = changed * 64;
        if budget.remaining_bytes < bytes {
            return Err(HeightGpuError::UploadBudget);
        }
        for (i, s) in self.slots.iter_mut().enumerate() {
            if let Some(key) = s.key {
                let d = descriptor(
                    key,
                    s.anchor,
                    s.minimum,
                    s.maximum,
                    s.drawable,
                    s.enabled,
                    s.gpu.params[1],
                    origin,
                )
                .expect("preflighted");
                if s.gpu != d {
                    s.gpu = d;
                    queue.write_buffer(&self.headers, i as u64 * 64, bytemuck::bytes_of(&d));
                }
            }
        }
        if changed > 0 {
            self.revision = self.revision.wrapping_add(1)
        }
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        self.work.header_uploads += changed as u32;
        Ok(bytes)
    }
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: Camera,
    ) -> Result<HeightGpuWork, HeightGpuError> {
        if camera
            .view_projection
            .iter()
            .flatten()
            .any(|v| !v.is_finite())
        {
            return Err(HeightGpuError::InvalidCamera);
        }
        if self.last == Some((camera, self.revision)) {
            return Ok(HeightGpuWork::default());
        }
        queue.write_buffer(
            &self.camera,
            0,
            bytemuck::bytes_of(&CameraGpu {
                view_projection: camera.view_projection,
                params: [self.config.max_tiles, 0, 0, 0],
                reserved: [0; 4],
            }),
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("height tile GPU bounded cull"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &self.cull_bind, &[]);
            pass.set_pipeline(&self.reset);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.cull);
            pass.dispatch_workgroups(self.config.max_tiles.div_ceil(64), 1, 1);
        }
        self.last = Some((camera, self.revision));
        Ok(HeightGpuWork {
            cull_dispatches: 2,
            camera_upload_bytes: 96,
            ..HeightGpuWork::default()
        })
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw_indirect(&self.indirect, 0);
        1
    }
    pub fn indirect_buffer(&self) -> &wgpu::Buffer {
        &self.indirect
    }
    pub fn payload_buffer(&self) -> &wgpu::Buffer {
        &self.payload
    }
    pub fn take_work(&mut self) -> HeightGpuWork {
        std::mem::take(&mut self.work)
    }
}
#[allow(clippy::too_many_arguments)] // Fixed 64B descriptor fields, shared by upload/rebase preflight.
fn descriptor(
    key: HeightTileKey,
    anchor: i64,
    minimum: i32,
    maximum: i32,
    drawable: bool,
    enabled: bool,
    seams: u32,
    origin: [i64; 3],
) -> Result<TileGpu, HeightGpuError> {
    if origin
        .iter()
        .any(|v| !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(v))
    {
        return Err(HeightGpuError::InvalidTransform);
    }
    let b = key.bounds().ok_or(HeightGpuError::InvalidTransform)?;
    let relative = [
        (b[0] as f64 - origin[0] as f64) as f32,
        (anchor as f64 - origin[1] as f64) as f32,
        (b[1] as f64 - origin[2] as f64) as f32,
    ];
    let minimum = [
        relative[0],
        (f64::from(minimum) - origin[1] as f64) as f32,
        relative[2],
        0.,
    ];
    let maximum = [
        (b[2] as f64 - origin[0] as f64) as f32,
        (f64::from(maximum) - origin[1] as f64) as f32,
        (b[3] as f64 - origin[2] as f64) as f32,
        0.,
    ];
    Ok(TileGpu {
        origin: relative,
        step: key.step().expect("validated") as f32,
        params: [u32::from(drawable && enabled), seams, 0, 0],
        minimum,
        maximum,
    })
}
fn buffer_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    uniform: bool,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
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
    }
}
fn texture_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}
fn buffer_binding(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}
