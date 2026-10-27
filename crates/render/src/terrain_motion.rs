//! Optional exact-depth terrain motion. Candidate object matrices are copied on
//! the GPU and become previous matrices only after that same frame is presented.
use crate::{
    taa::{FrameToken, MOTION_FORMAT, SURFACE_FORMAT},
    terrain_gpu::{Camera, TerrainGpu, UploadBudget},
    terrain_motion_shader,
};
use bytemuck::{Pod, Zeroable};

const OBJECT_BYTES: u64 = 128;
const PARAMETER_BYTES: u64 = 96;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    previous_vp: [[f32; 4]; 4],
    inverse_extent_jitter: [f32; 4],
    flags: [u32; 4],
}
const _: () = assert!(std::mem::size_of::<Parameters>() == PARAMETER_BYTES as usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSource,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub upload_bytes: u64,
    pub draws: u32,
    pub snapshot_passes: u32,
    pub snapshot_records: u32,
    pub snapshot_read_bytes: u64,
    pub snapshot_write_bytes: u64,
}
#[derive(Clone, Copy)]
struct Candidate {
    token: FrameToken,
    vp: [[f32; 4]; 4],
    encoded: bool,
}

pub struct TerrainMotionGpu {
    objects: wgpu::Buffer,
    primary: wgpu::BindGroup,
    indirect: wgpu::Buffer,
    previous: [wgpu::Buffer; 2],
    parameters: wgpu::Buffer,
    binds: [wgpu::BindGroup; 2],
    snapshot_binds: [wgpu::BindGroup; 2],
    pipeline: wgpu::RenderPipeline,
    snapshot: wgpu::ComputePipeline,
    timer: crate::gpu_time::GpuPassTimer<1>,
    front: Option<(usize, [[f32; 4]; 4])>,
    candidate: Option<Candidate>,
    presented_token: Option<FrameToken>,
    last_serial: u64,
    max_pages: u32,
}
impl TerrainMotionGpu {
    pub fn required_buffer_bytes(max_pages: u32, timestamps: bool) -> u64 {
        u64::from(max_pages) * OBJECT_BYTES * 2 + PARAMETER_BYTES + u64::from(timestamps) * 256
    }
    /// None creates no GPU objects. Construct after all primary lighting/material
    /// preparation; the exact primary bind group remains immutable thereafter.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        memory_budget_bytes: Option<u64>,
    ) -> Result<Option<Self>, Error> {
        let Some(memory_budget_bytes) = memory_budget_bytes else {
            return Ok(None);
        };
        let source = terrain.geometry();
        let object_bytes = u64::from(source.max_pages) * OBJECT_BYTES;
        let timestamps = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        if source.sample_count != 1
            || source.max_pages == 0
            || source.objects.size() != object_bytes
            || object_bytes > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(Error::InvalidSource);
        }
        if Self::required_buffer_bytes(source.max_pages, timestamps) > memory_budget_bytes {
            return Err(Error::MemoryBudget);
        }
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let previous = std::array::from_fn(|_| {
            buffer(
                "presented or candidate terrain object matrix bank",
                object_bytes,
                wgpu::BufferUsages::STORAGE,
            )
        });
        let parameters = buffer(
            "actual previous terrain camera and current jitter",
            PARAMETER_BYTES,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let entry = |binding, uniform, visibility, read_only| wgpu::BindGroupLayoutEntry {
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
        let extra = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain temporal adds only one vertex storage"),
            entries: &[
                entry(0, true, wgpu::ShaderStages::VERTEX_FRAGMENT, true),
                entry(1, false, wgpu::ShaderStages::VERTEX, true),
            ],
        });
        let binds = std::array::from_fn(|index| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("previous PRESENTED terrain transforms"),
                layout: &extra,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: parameters.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: previous[index].as_entire_binding(),
                    },
                ],
            })
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("primary terrain layout plus presented transforms"),
            bind_group_layouts: &[terrain.primary_render_layout(), &extra],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("exact primary terrain geometry and H1 mask for motion"),
            source: wgpu::ShaderSource::Wgsl(
                terrain_motion_shader::shader_source()
                    .map_err(|_| Error::Shader)?
                    .into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain depth-equal motion and cell-face identity"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("temporal_vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("temporal_fragment"),
                compilation_options: Default::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: MOTION_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: SURFACE_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
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
        });
        let snapshot_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bounded GPU terrain matrix snapshot"),
            entries: &[
                entry(0, false, wgpu::ShaderStages::COMPUTE, true),
                entry(1, false, wgpu::ShaderStages::COMPUTE, false),
            ],
        });
        let snapshot_binds = std::array::from_fn(|index| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("candidate terrain snapshot destination"),
                layout: &snapshot_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: source.objects.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: previous[index].as_entire_binding(),
                    },
                ],
            })
        });
        let snapshot_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded candidate terrain snapshot"),
            source: wgpu::ShaderSource::Wgsl(terrain_motion_shader::SNAPSHOT_SHADER.into()),
        });
        let snapshot_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&snapshot_layout],
                push_constant_ranges: &[],
            });
        let snapshot = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("capture candidate object transforms without CPU readback"),
            layout: Some(&snapshot_pipeline_layout),
            module: &snapshot_module,
            entry_point: Some("snapshot"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Some(Self {
            objects: source.objects.clone(),
            primary: terrain.primary_render_bind().clone(),
            indirect: terrain.indirect_buffer().clone(),
            previous,
            parameters,
            binds,
            snapshot_binds,
            pipeline,
            snapshot,
            timer: crate::gpu_time::GpuPassTimer::new(
                device,
                queue,
                "terrain candidate matrix snapshot",
            ),
            front: None,
            candidate: None,
            presented_token: None,
            last_serial: 0,
            max_pages: source.max_pages,
        }))
    }
    pub fn buffer_bytes(&self) -> u64 {
        self.previous.iter().map(wgpu::Buffer::size).sum::<u64>()
            + self.parameters.size()
            + self
                .timer
                .timestamp_buffer()
                .map_or(0, |(buffer, _)| buffer.size())
    }
    pub fn pipeline_compilations(&self) -> u32 {
        2
    }
    pub fn required_upload_bytes(&self) -> u64 {
        PARAMETER_BYTES
    }
    pub fn pending(&self) -> Option<FrameToken> {
        self.candidate.map(|candidate| candidate.token)
    }
    pub fn presented(&self) -> Option<FrameToken> {
        self.presented_token
    }
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        token: FrameToken,
        unjittered_camera: &Camera,
        extent: [u32; 2],
        jitter: [f32; 2],
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if self.candidate.is_some() {
            return Err(Error::Busy);
        }
        if token.serial() <= self.last_serial {
            return Err(Error::WrongToken);
        }
        if extent.contains(&0)
            || unjittered_camera
                .view_projection
                .iter()
                .flatten()
                .any(|x| !x.is_finite())
            || jitter.iter().any(|x| !x.is_finite() || x.abs() > 0.5)
        {
            return Err(Error::InvalidFrame);
        }
        if budget.remaining_bytes < PARAMETER_BYTES {
            return Err(Error::UploadBudget);
        }
        let parameters = Parameters {
            previous_vp: self
                .front
                .map_or(unjittered_camera.view_projection, |(_, vp)| vp),
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
                        .is_some_and(|(_, vp)| vp == unjittered_camera.view_projection),
                ),
                0,
            ],
        };
        queue.write_buffer(&self.parameters, 0, bytemuck::bytes_of(&parameters));
        budget.remaining_bytes -= PARAMETER_BYTES;
        self.last_serial = token.serial();
        self.candidate = Some(Candidate {
            token,
            vp: unjittered_camera.view_projection,
            encoded: false,
        });
        Ok(Work {
            upload_bytes: PARAMETER_BYTES,
            ..Default::default()
        })
    }
    /// Draw after primary terrain visibility is finalized. The shared24B motion
    /// attachment owner clears once, then calls terrain and model draw methods.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> Work {
        if self.candidate.is_none() {
            return Work::default();
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.primary, &[]);
        pass.set_bind_group(
            1,
            &self.binds[self.front.map_or(1, |(index, _)| index)],
            &[],
        );
        for bucket in 0..6 {
            pass.draw_indirect(&self.indirect, bucket * 16);
        }
        Work {
            draws: 6,
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
                label: Some("GPU capture candidate terrain matrices"),
                timestamp_writes: self.timer.compute(1),
            });
            pass.set_pipeline(&self.snapshot);
            pass.set_bind_group(0, &self.snapshot_binds[back], &[]);
            pass.dispatch_workgroups(self.max_pages.div_ceil(64), 1, 1);
        }
        self.timer.finish(encoder);
        candidate.encoded = true;
        Ok(Work {
            snapshot_passes: 1,
            snapshot_records: self.max_pages,
            snapshot_read_bytes: self.objects.size(),
            snapshot_write_bytes: self.objects.size(),
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
        let back = self.front.map_or(0, |(index, _)| 1 - index);
        self.front = Some((back, candidate.vp));
        self.candidate = None;
        self.presented_token = Some(token);
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
