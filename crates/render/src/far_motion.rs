//! Optional public Height/Proxy motion. All snapshots are GPU-only candidate
//! banks; checked CPU slot generations distinguish reuse without a hash.
use crate::{
    far_motion_shader,
    height_gpu::HeightGpu,
    lod_gpu::LodProxyGpu,
    taa::FrameToken,
    terrain_gpu::{Camera, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
const SNAPSHOT_BYTES: u64 = 80;
const PARAMETERS_BYTES: u64 = 128;
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub memory_budget_bytes: u64,
    pub opaque_height_layers: [u32; 8],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSource,
    NotPrepared,
    InvalidFrame,
    MemoryBudget,
    UploadBudget,
    Busy,
    WrongToken,
    NotEncoded,
    Shader,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    previous_vp: [[f32; 4]; 4],
    inverse_extent_jitter: [f32; 4],
    flags: [u32; 4],
    opaque_layers: [[u32; 4]; 2],
}
const _: () = assert!(std::mem::size_of::<Parameters>() == PARAMETERS_BYTES as usize);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub upload_bytes: u64,
    pub draws: u32,
    pub snapshot_passes: u32,
    pub snapshot_records: u32,
    pub snapshot_read_upper_bound_bytes: u64,
    pub snapshot_write_bytes: u64,
}
#[derive(Clone, Copy)]
struct Candidate {
    token: FrameToken,
    vp: [[f32; 4]; 4],
    encoded: bool,
}
struct Bank {
    source: wgpu::Buffer,
    count: u32,
    generations: wgpu::Buffer,
    previous: [wgpu::Buffer; 2],
    binds: [wgpu::BindGroup; 2],
    snapshot_binds: [wgpu::BindGroup; 2],
    snapshot: wgpu::ComputePipeline,
    generation_cache: Vec<u32>,
    generation_staging: Vec<u32>,
}
impl Bank {
    fn new(
        device: &wgpu::Device,
        source: &wgpu::Buffer,
        count: u32,
        height: bool,
        parameters: &wgpu::Buffer,
        extra: &wgpu::BindGroupLayout,
    ) -> Self {
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let generations = buffer(
            "checked current public slot generations",
            u64::from(count) * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let previous = std::array::from_fn(|_| {
            buffer(
                "presented or candidate public transforms",
                u64::from(count) * SNAPSHOT_BYTES,
                wgpu::BufferUsages::STORAGE,
            )
        });
        let binds = std::array::from_fn(|index| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("previous presented public transforms"),
                layout: extra,
                entries: &[
                    binding(0, parameters),
                    binding(1, &previous[index]),
                    binding(2, &generations),
                ],
            })
        });
        let snapshot_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("public transform candidate capture"),
            entries: &[
                buffer_entry(0, wgpu::ShaderStages::COMPUTE, false, true),
                buffer_entry(1, wgpu::ShaderStages::COMPUTE, false, true),
                buffer_entry(2, wgpu::ShaderStages::COMPUTE, false, false),
            ],
        });
        let snapshot_binds = std::array::from_fn(|index| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("public candidate transform destination"),
                layout: &snapshot_layout,
                entries: &[
                    binding(0, source),
                    binding(1, &generations),
                    binding(2, &previous[index]),
                ],
            })
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&snapshot_layout],
            push_constant_ranges: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded public transform capture without readback"),
            source: wgpu::ShaderSource::Wgsl(far_motion_shader::snapshot(height).into()),
        });
        let snapshot = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("capture candidate public transforms"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("snapshot"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            source: source.clone(),
            count,
            generations,
            previous,
            binds,
            snapshot_binds,
            snapshot,
            generation_cache: vec![0; count as usize],
            generation_staging: vec![0; count as usize],
        }
    }
    fn collect(&mut self, source: impl ExactSizeIterator<Item = u32>) -> Result<u64, Error> {
        if source.len() != self.count as usize {
            return Err(Error::InvalidSource);
        }
        for (destination, generation) in self.generation_staging.iter_mut().zip(source) {
            *destination = generation;
        }
        Ok(u64::from(self.generation_cache != self.generation_staging) * u64::from(self.count) * 4)
    }
    fn upload(&mut self, queue: &wgpu::Queue, bytes: u64) {
        if bytes != 0 {
            queue.write_buffer(
                &self.generations,
                0,
                bytemuck::cast_slice(&self.generation_staging),
            );
            self.generation_cache
                .copy_from_slice(&self.generation_staging);
        }
    }
    fn bytes(&self) -> u64 {
        self.generations.size() + self.previous.iter().map(wgpu::Buffer::size).sum::<u64>()
    }
}
pub struct FarMotionGpu {
    height: Bank,
    proxy: Bank,
    parameters: wgpu::Buffer,
    config: Config,
    height_base: wgpu::BindGroup,
    proxy_base: wgpu::BindGroup,
    cutout_base: wgpu::BindGroup,
    indirect: [wgpu::Buffer; 2],
    height_pipeline: wgpu::RenderPipeline,
    near_pipeline: wgpu::RenderPipeline,
    proxy_pipeline: wgpu::RenderPipeline,
    cutout_pipeline: wgpu::RenderPipeline,
    timer: crate::gpu_time::GpuPassTimer<1>,
    front: Option<(usize, [[f32; 4]; 4])>,
    candidate: Option<Candidate>,
    presented: Option<FrameToken>,
    last_serial: u64,
}
impl FarMotionGpu {
    pub fn required_buffer_bytes(max_tiles: u32, max_objects: u32, timestamps: bool) -> u64 {
        (u64::from(max_tiles) + u64::from(max_objects)) * (SNAPSHOT_BYTES * 2 + 4)
            + PARAMETERS_BYTES
            + u64::from(timestamps) * 256
    }
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        height: &HeightGpu,
        proxy: &LodProxyGpu,
        config: Option<Config>,
    ) -> Result<Option<Self>, Error> {
        let Some(config) = config else {
            return Ok(None);
        };
        if !height.temporal_raster_prepared() || !proxy.temporal_raster_prepared() {
            return Err(Error::NotPrepared);
        }
        if height.sample_count() != 1 || proxy.sample_count() != 1 {
            return Err(Error::InvalidSource);
        }
        let hs = height.shadow_source();
        let ps = proxy.shadow_source();
        let timestamps = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        if hs.max_tiles == 0
            || ps.max_objects == 0
            || hs.headers.size() != u64::from(hs.max_tiles) * 64
            || ps.objects.size() != u64::from(ps.max_objects) * 256
        {
            return Err(Error::InvalidSource);
        }
        if u64::from(hs.max_tiles.max(ps.max_objects)) * SNAPSHOT_BYTES
            > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(Error::InvalidSource);
        }
        if Self::required_buffer_bytes(hs.max_tiles, ps.max_objects, timestamps)
            > config.memory_budget_bytes
        {
            return Err(Error::MemoryBudget);
        }
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("public previous camera and current jitter"),
            size: PARAMETERS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let extra = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bounded public temporal sources"),
            entries: &[
                buffer_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT, true, true),
                buffer_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT, false, true),
                buffer_entry(2, wgpu::ShaderStages::VERTEX_FRAGMENT, false, true),
            ],
        });
        let h = Bank::new(device, hs.headers, hs.max_tiles, true, &parameters, &extra);
        let p = Bank::new(
            device,
            ps.objects,
            ps.max_objects,
            false,
            &parameters,
            &extra,
        );
        let height_source = far_motion_shader::height().map_err(|_| Error::Shader)?;
        let height_pipeline = motion_pipeline(
            device,
            height.primary_render_layout(),
            &extra,
            &height_source,
            "temporal_vertex",
            false,
        )?;
        let near_pipeline = motion_pipeline(
            device,
            height.primary_render_layout(),
            &extra,
            &height_source,
            "temporal_near_vertex",
            false,
        )?;
        let proxy_pipeline = motion_pipeline(
            device,
            proxy.primary_render_layout(),
            &extra,
            &far_motion_shader::proxy_opaque().map_err(|_| Error::Shader)?,
            "vertex",
            false,
        )?;
        let cutout_pipeline = motion_pipeline(
            device,
            proxy.primary_render_layout(),
            &extra,
            &far_motion_shader::proxy_cutout().map_err(|_| Error::Shader)?,
            "temporal_vertex",
            true,
        )?;
        Ok(Some(Self {
            height: h,
            proxy: p,
            parameters,
            config,
            height_base: height.primary_render_bind().clone(),
            proxy_base: proxy.primary_render_bind().clone(),
            cutout_base: proxy.primary_cutout_bind().clone(),
            indirect: [
                height.indirect_buffer().clone(),
                proxy.indirect_buffer().clone(),
            ],
            height_pipeline,
            near_pipeline,
            proxy_pipeline,
            cutout_pipeline,
            timer: crate::gpu_time::GpuPassTimer::new(
                device,
                queue,
                "public candidate transform capture",
            ),
            front: None,
            candidate: None,
            presented: None,
            last_serial: 0,
        }))
    }
    pub fn buffer_bytes(&self) -> u64 {
        self.height.bytes()
            + self.proxy.bytes()
            + self.parameters.size()
            + self
                .timer
                .timestamp_buffer()
                .map_or(0, |(buffer, _)| buffer.size())
    }
    pub fn pipeline_compilations(&self) -> u32 {
        6
    }
    pub fn pending(&self) -> Option<FrameToken> {
        self.candidate.map(|candidate| candidate.token)
    }
    pub fn presented(&self) -> Option<FrameToken> {
        self.presented
    }
    fn upload_prices(
        &mut self,
        height: &HeightGpu,
        proxy: &LodProxyGpu,
    ) -> Result<(u64, u64), Error> {
        if self.candidate.is_some() {
            return Err(Error::Busy);
        }
        if self.height.source != *height.shadow_source().headers
            || self.proxy.source != *proxy.shadow_source().objects
        {
            return Err(Error::InvalidSource);
        }
        Ok((
            self.height.collect(height.temporal_slot_generations())?,
            self.proxy.collect(proxy.temporal_slot_generations())?,
        ))
    }
    /// Metadata-only global graph preflight. Only indexed GPU slot generations
    /// are examined; no World cells, allocations, writes, or front mutations.
    pub fn pending_upload_bytes(
        &mut self,
        height: &HeightGpu,
        proxy: &LodProxyGpu,
    ) -> Result<u64, Error> {
        let (h, p) = self.upload_prices(height, proxy)?;
        Ok(PARAMETERS_BYTES + h + p)
    }
    #[allow(clippy::too_many_arguments)] // A single candidate binds the same current public source and camera.
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        token: FrameToken,
        camera: &Camera,
        extent: [u32; 2],
        jitter: [f32; 2],
        height: &HeightGpu,
        proxy: &LodProxyGpu,
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if self.candidate.is_some() {
            return Err(Error::Busy);
        }
        if token.serial() <= self.last_serial {
            return Err(Error::WrongToken);
        }
        if extent.contains(&0)
            || camera
                .view_projection
                .iter()
                .flatten()
                .any(|value| !value.is_finite())
            || jitter
                .iter()
                .any(|value| !value.is_finite() || value.abs() > 0.5)
        {
            return Err(Error::InvalidFrame);
        }
        let (h, p) = self.upload_prices(height, proxy)?;
        let bytes = PARAMETERS_BYTES + h + p;
        if budget.remaining_bytes < bytes {
            return Err(Error::UploadBudget);
        }
        let parameters = Parameters {
            previous_vp: self.front.map_or(camera.view_projection, |(_, vp)| vp),
            inverse_extent_jitter: [
                1. / extent[0] as f32,
                1. / extent[1] as f32,
                jitter[0],
                jitter[1],
            ],
            flags: [
                u32::from(self.front.is_some()),
                0,
                u32::from(
                    self.front
                        .is_some_and(|(_, vp)| vp == camera.view_projection),
                ),
                0,
            ],
            opaque_layers: std::array::from_fn(|i| {
                std::array::from_fn(|j| self.config.opaque_height_layers[i * 4 + j])
            }),
        };
        queue.write_buffer(&self.parameters, 0, bytemuck::bytes_of(&parameters));
        self.height.upload(queue, h);
        self.proxy.upload(queue, p);
        budget.remaining_bytes -= bytes;
        self.last_serial = token.serial();
        self.candidate = Some(Candidate {
            token,
            vp: camera.view_projection,
            encoded: false,
        });
        Ok(Work {
            upload_bytes: bytes,
            ..Default::default()
        })
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> Work {
        if self.candidate.is_none() {
            return Work::default();
        }
        let front = self.front.map_or(1, |(index, _)| index);
        pass.set_bind_group(0, &self.height_base, &[]);
        pass.set_bind_group(1, &self.height.binds[front], &[]);
        pass.set_pipeline(&self.height_pipeline);
        pass.draw_indirect(&self.indirect[0], 0);
        pass.set_pipeline(&self.near_pipeline);
        pass.draw_indirect(&self.indirect[0], 16);
        pass.set_bind_group(0, &self.proxy_base, &[]);
        pass.set_bind_group(1, &self.proxy.binds[front], &[]);
        pass.set_pipeline(&self.proxy_pipeline);
        pass.draw_indirect(&self.indirect[1], 0);
        pass.set_bind_group(0, &self.cutout_base, &[]);
        pass.set_pipeline(&self.cutout_pipeline);
        pass.draw_indirect(&self.indirect[1], 16);
        Work {
            draws: 4,
            ..Default::default()
        }
    }
    pub fn encode_snapshot(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        token: FrameToken,
    ) -> Result<Work, Error> {
        let candidate = self
            .candidate
            .as_mut()
            .filter(|candidate| candidate.token == token)
            .ok_or(Error::WrongToken)?;
        if candidate.encoded {
            return Err(Error::Busy);
        }
        let back = self.front.map_or(0, |(index, _)| 1 - index);
        self.timer.begin();
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("capture current public transforms for candidate presentation"),
                timestamp_writes: self.timer.compute(1),
            });
            for bank in [&self.height, &self.proxy] {
                pass.set_pipeline(&bank.snapshot);
                pass.set_bind_group(0, &bank.snapshot_binds[back], &[]);
                pass.dispatch_workgroups(bank.count.div_ceil(64), 1, 1);
            }
        }
        self.timer.finish(encoder);
        candidate.encoded = true;
        Ok(Work {
            snapshot_passes: 1,
            snapshot_records: self.height.count + self.proxy.count,
            snapshot_read_upper_bound_bytes: self.height.source.size()
                + self.proxy.source.size()
                + self.height.generations.size()
                + self.proxy.generations.size(),
            snapshot_write_bytes: u64::from(self.height.count + self.proxy.count) * SNAPSHOT_BYTES,
            ..Default::default()
        })
    }
    pub fn validate_commit(&self, token: FrameToken) -> Result<(), Error> {
        let candidate = self
            .candidate
            .filter(|candidate| candidate.token == token)
            .ok_or(Error::WrongToken)?;
        if !candidate.encoded {
            return Err(Error::NotEncoded);
        }
        Ok(())
    }
    pub fn commit_presented(&mut self, token: FrameToken) -> Result<(), Error> {
        self.validate_commit(token)?;
        let candidate = self.candidate.ok_or(Error::WrongToken)?;
        self.front = Some((self.front.map_or(0, |(index, _)| 1 - index), candidate.vp));
        self.candidate = None;
        self.presented = Some(token);
        Ok(())
    }
    pub fn discard(&mut self, token: FrameToken) -> Result<(), Error> {
        if self
            .candidate
            .is_none_or(|candidate| candidate.token != token)
        {
            return Err(Error::WrongToken);
        }
        self.candidate = None;
        Ok(())
    }
    pub fn clear_timing(&self) {
        self.timer.begin();
    }
    pub fn timing_source(&self, kind: u8) -> Option<crate::gpu_time::TimingSource<'_>> {
        self.timer
            .timestamp_buffer()
            .map(|(buffer, period)| crate::gpu_time::TimingSource {
                buffer,
                period,
                kinds: [kind; 32],
                count: self.timer.graph().count,
            })
    }
}
fn buffer_entry(
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
fn binding(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}
fn motion_pipeline(
    device: &wgpu::Device,
    base: &wgpu::BindGroupLayout,
    extra: &wgpu::BindGroupLayout,
    source: &str,
    vertex: &str,
    strip: bool,
) -> Result<wgpu::RenderPipeline, Error> {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("primary public bindings plus presented transforms"),
        bind_group_layouts: &[base, extra],
        push_constant_ranges: &[],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("exact public geometry motion"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    Ok(
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("public exact-depth motion and cell identity"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(vertex),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("temporal_fragment"),
                compilation_options: Default::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: crate::taa::MOTION_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: crate::taa::SURFACE_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            primitive: wgpu::PrimitiveState {
                topology: if strip {
                    wgpu::PrimitiveTopology::TriangleStrip
                } else {
                    wgpu::PrimitiveTopology::TriangleList
                },
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Equal,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        }),
    )
}
