//! Optional single-sample motion raster. Reuses immutable model catalogs and
//! the primary color visibility/indirect buffers; never replaces their lists.
use crate::{
    block_models::{BlockInstance, BlockModelsGpu},
    entity_gpu::{EntityGpu, EntityInstance},
    model_motion_shader,
    presented_motion::{GpuMatch, Token},
    terrain_gpu::UploadBudget,
};
use bytemuck::{Pod, Zeroable};
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Parameters {
    pub previous_unjittered_view_projection: [[f32; 4]; 4],
    /// inverse render extent XY, positive-right/down current jitter in pixels ZW.
    pub inverse_extent_jitter: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<Parameters>() == 80);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotPrepared,
    InvalidSource,
    InvalidFrame,
    UploadBudget,
    Shader,
    MemoryBudget,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug)]
pub struct TargetConfig {
    pub extent: [u32; 2],
    pub samples: u32,
    pub max_pixels: u64,
    /// This owner's24B/pixel images plus its actual optional query buffer.
    pub memory_budget_bytes: u64,
}
pub struct MotionTargets {
    _motion: wgpu::Texture,
    motion: wgpu::TextureView,
    _surface: wgpu::Texture,
    surface: wgpu::TextureView,
    extent: [u32; 2],
    timer: crate::gpu_time::GpuPassTimer<1>,
}
impl MotionTargets {
    /// Pure preparation preflight. Image bytes are the motion24B/pixel part of
    /// the complete TAA88B/pixel graph; query storage is counted separately.
    pub fn required_allocation_bytes(
        config: TargetConfig,
        max_texture_dimension_2d: u32,
        timestamp_queries: bool,
    ) -> Result<u64, Error> {
        if config.samples != 1
            || config.extent.contains(&0)
            || config.extent.iter().any(|x| *x > max_texture_dimension_2d)
            || config.max_pixels == 0
        {
            return Err(Error::InvalidFrame);
        }
        let pixels = u64::from(config.extent[0]) * u64::from(config.extent[1]);
        let bytes = pixels
            .checked_mul(24)
            .and_then(|x| x.checked_add(if timestamp_queries { 256 } else { 0 }))
            .ok_or(Error::MemoryBudget)?;
        if pixels > config.max_pixels || bytes > config.memory_budget_bytes {
            return Err(Error::MemoryBudget);
        }
        Ok(bytes)
    }
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: Option<TargetConfig>,
    ) -> Result<Option<Self>, Error> {
        let Some(config) = config else {
            return Ok(None);
        };
        Self::required_allocation_bytes(
            config,
            device.limits().max_texture_dimension_2d,
            device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
        )?;
        let texture = |label, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: config.extent[0],
                    height: config.extent[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let motion = texture(
            "actual unjittered object motion",
            wgpu::TextureFormat::Rgba16Float,
        );
        let surface = texture(
            "exact family owner UID and generation",
            wgpu::TextureFormat::Rgba32Uint,
        );
        Ok(Some(Self {
            motion: motion.create_view(&Default::default()),
            surface: surface.create_view(&Default::default()),
            _motion: motion,
            _surface: surface,
            extent: config.extent,
            timer: crate::gpu_time::GpuPassTimer::new(device, queue, "actual model motion raster"),
        }))
    }
    pub fn motion_view(&self) -> &wgpu::TextureView {
        &self.motion
    }
    pub fn surface_view(&self) -> &wgpu::TextureView {
        &self.surface
    }
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
    pub fn allocation_bytes(&self) -> u64 {
        u64::from(self.extent[0]) * u64::from(self.extent[1]) * 24
            + self.timer.timestamp_buffer().map_or(0, |(b, _)| b.size())
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
    /// Call only for a newly encoded scene. The depth attachment is read-only;
    /// unsupported/background samples start invalid instead of camera-only data.
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
        draw: impl FnOnce(&mut wgpu::RenderPass<'_>) -> Work,
    ) -> Work {
        self.timer.begin();
        let work = {
            let color = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("single-sample exact-depth object motion"),
                color_attachments: &[color(&self.motion), color(&self.surface)],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: None,
                    stencil_ops: None,
                }),
                timestamp_writes: self.timer.render(1),
                occlusion_query_set: None,
            });
            draw(&mut pass)
        };
        self.timer.finish(encoder);
        work
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub upload_bytes: u64,
    pub previous_records: u32,
    pub matches: u32,
    pub pipeline_compilations: u32,
    pub draws: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Entity,
    Block,
}
pub struct ModelMotionGpu {
    family: Family,
    capacity: u32,
    count: usize,
    active: u32,
    glass_active: bool,
    base: wgpu::BindGroup,
    indirect: wgpu::Buffer,
    vertices: Option<wgpu::Buffer>,
    previous: wgpu::Buffer,
    matches: wgpu::Buffer,
    camera: wgpu::Buffer,
    bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    glass: Option<wgpu::RenderPipeline>,
    last_previous: Vec<u8>,
    last_matches: Vec<GpuMatch>,
    last_parameters: Option<Parameters>,
    prepared_token: Option<Token>,
}
impl ModelMotionGpu {
    /// Both pose and metadata banks plus the exact80B camera uniform.
    pub fn required_buffer_bytes(capacity: u32) -> Result<u64, Error> {
        if capacity == 0 {
            return Err(Error::InvalidSource);
        }
        Ok(u64::from(capacity) * 96 + 80)
    }
    pub fn entities(device: &wgpu::Device, source: &EntityGpu) -> Result<Self, Error> {
        if !source.temporal_raster_prepared() {
            return Err(Error::NotPrepared);
        }
        let mut gpu = Self::new(
            device,
            Family::Entity,
            source.instance_capacity(),
            source.primary_render_bind(),
            source.primary_render_layout(),
            source.indirect_buffer(),
            Some(source.model_vertex_buffer()),
        )?;
        gpu.synchronize_entities(source)?;
        Ok(gpu)
    }
    pub fn blocks(device: &wgpu::Device, source: &BlockModelsGpu) -> Result<Self, Error> {
        if !source.temporal_raster_prepared() {
            return Err(Error::NotPrepared);
        }
        let mut gpu = Self::new(
            device,
            Family::Block,
            source.instance_capacity(),
            source.primary_render_bind(),
            source.primary_render_layout(),
            source.indirect_buffer(),
            None,
        )?;
        gpu.synchronize_blocks(source)?;
        Ok(gpu)
    }
    #[allow(clippy::too_many_arguments)] // The exact immutable source handles are validated together.
    fn new(
        device: &wgpu::Device,
        family: Family,
        capacity: u32,
        base: &wgpu::BindGroup,
        base_layout: wgpu::BindGroupLayout,
        indirect: &wgpu::Buffer,
        vertices: Option<&wgpu::Buffer>,
    ) -> Result<Self, Error> {
        Self::required_buffer_bytes(capacity)?;
        let pose_bytes = u64::from(capacity) * 64;
        let match_bytes = u64::from(capacity) * 32;
        if capacity == 0
            || pose_bytes > u64::from(device.limits().max_storage_buffer_binding_size)
            || match_bytes > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(Error::InvalidSource);
        }
        let source = match family {
            Family::Entity => model_motion_shader::entity_motion(
                &crate::entity_gpu::shader_source(false, false, false)
                    .map_err(|_| Error::Shader)?,
            ),
            Family::Block => model_motion_shader::block_motion(
                &crate::block_models::shader_source(false, false, false)
                    .map_err(|_| Error::Shader)?,
            ),
        }
        .map_err(|_| Error::Shader)?;
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let previous = buffer(
            "previous PRESENTED model pose bank",
            pose_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let matches = buffer(
            "exact model identity and previous index bank",
            match_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let camera = buffer(
            "previous unjittered model camera",
            80,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let entry = |binding, uniform, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: if uniform {
                    wgpu::BufferBindingType::Uniform
                } else {
                    wgpu::BufferBindingType::Storage { read_only: true }
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let extra = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model motion has no new fragment storage"),
            entries: &[
                entry(0, true, wgpu::ShaderStages::VERTEX_FRAGMENT),
                entry(1, false, wgpu::ShaderStages::VERTEX),
                entry(2, false, wgpu::ShaderStages::VERTEX),
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("presented model motion inputs"),
            layout: &extra,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: previous.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: matches.as_entire_binding(),
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("primary color bindings plus presented model bank"),
            bind_group_layouts: &[&base_layout, &extra],
            push_constant_ranges: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("same GPU catalog bone transforms for model motion"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let attributes = model_motion_shader::entity_vertex_attributes();
        let entity_buffers = [wgpu::VertexBufferLayout {
            array_stride: 64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        }];
        let targets = [
            Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
            Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba32Uint,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
        ];
        let pipeline = |glass| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(if glass {
                    "unsupported colored-glass motion invalidation"
                } else {
                    "exact depth-equal object motion"
                }),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("temporal_vertex"),
                    compilation_options: Default::default(),
                    buffers: if family == Family::Entity {
                        &entity_buffers
                    } else {
                        &[]
                    },
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: if family == Family::Entity {
                        Some(wgpu::Face::Back)
                    } else {
                        None
                    },
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: false,
                    depth_compare: if glass {
                        wgpu::CompareFunction::GreaterEqual
                    } else {
                        wgpu::CompareFunction::Equal
                    },
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(if glass {
                        "temporal_glass_fragment"
                    } else {
                        "temporal_fragment"
                    }),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                multiview: None,
                cache: None,
            })
        };
        Ok(Self {
            family,
            capacity,
            count: 0,
            active: 0,
            glass_active: false,
            base: base.clone(),
            indirect: indirect.clone(),
            vertices: vertices.cloned(),
            previous,
            matches,
            camera,
            bind,
            pipeline: pipeline(false),
            glass: (family == Family::Block).then(|| pipeline(true)),
            last_previous: Vec::with_capacity(pose_bytes as usize),
            last_matches: Vec::with_capacity(capacity as usize),
            last_parameters: None,
            prepared_token: None,
        })
    }
    pub fn buffer_bytes(&self) -> u64 {
        u64::from(self.capacity) * 96 + 80
    }
    pub fn startup_pipeline_compilations(&self) -> u32 {
        1 + u32::from(self.glass.is_some())
    }
    pub fn synchronize_entities(&mut self, source: &EntityGpu) -> Result<(), Error> {
        if self.family != Family::Entity
            || self.capacity != source.instance_capacity()
            || self.base != *source.primary_render_bind()
            || self.indirect != *source.indirect_buffer()
        {
            return Err(Error::InvalidSource);
        }
        self.count = source.instance_records().len();
        self.active = if self.count == 0 {
            0
        } else {
            (1 << crate::entity_models::MODEL_KINDS) - 1
        };
        Ok(())
    }
    pub fn synchronize_blocks(&mut self, source: &BlockModelsGpu) -> Result<(), Error> {
        if self.family != Family::Block
            || self.capacity != source.instance_capacity()
            || self.base != *source.primary_render_bind()
            || self.indirect != *source.indirect_buffer()
        {
            return Err(Error::InvalidSource);
        }
        self.count = source.instance_records().len();
        self.active = source.primary_draw_mask();
        self.glass_active = source.colored_transporter_count() != 0;
        Ok(())
    }
    pub fn upload_entities(
        &mut self,
        queue: &wgpu::Queue,
        token: Token,
        previous: &[EntityInstance],
        matches: &[GpuMatch],
        parameters: Parameters,
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if self.family != Family::Entity {
            return Err(Error::InvalidSource);
        }
        self.upload(
            queue,
            token,
            bytemuck::cast_slice(previous),
            matches,
            parameters,
            budget,
        )
    }
    pub fn upload_blocks(
        &mut self,
        queue: &wgpu::Queue,
        token: Token,
        previous: &[BlockInstance],
        matches: &[GpuMatch],
        parameters: Parameters,
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if self.family != Family::Block {
            return Err(Error::InvalidSource);
        }
        self.upload(
            queue,
            token,
            bytemuck::cast_slice(previous),
            matches,
            parameters,
            budget,
        )
    }
    pub fn required_entity_upload_bytes(
        &self,
        previous: &[EntityInstance],
        matches: &[GpuMatch],
        parameters: Parameters,
    ) -> Result<u64, Error> {
        if self.family != Family::Entity {
            return Err(Error::InvalidSource);
        }
        self.required_upload_bytes(bytemuck::cast_slice(previous), matches, parameters)
    }
    pub fn required_block_upload_bytes(
        &self,
        previous: &[BlockInstance],
        matches: &[GpuMatch],
        parameters: Parameters,
    ) -> Result<u64, Error> {
        if self.family != Family::Block {
            return Err(Error::InvalidSource);
        }
        self.required_upload_bytes(bytemuck::cast_slice(previous), matches, parameters)
    }
    fn required_upload_bytes(
        &self,
        previous: &[u8],
        matches: &[GpuMatch],
        parameters: Parameters,
    ) -> Result<u64, Error> {
        if previous.len() > self.capacity as usize * 64
            || matches.len() > self.capacity as usize
            || self.active != 0 && matches.len() < self.count
            || parameters
                .previous_unjittered_view_projection
                .iter()
                .flatten()
                .any(|x| !x.is_finite())
            || parameters
                .inverse_extent_jitter
                .iter()
                .any(|x| !x.is_finite())
            || parameters.inverse_extent_jitter[..2]
                .iter()
                .any(|x| *x <= 0.)
            || parameters.inverse_extent_jitter[2..]
                .iter()
                .any(|x| x.abs() > 0.5)
            || matches.iter().any(|m| {
                m.valid > 1
                    || m.surface[0] & 0xffffff != 0
                    || m.reserved != [0; 2]
                    || m.valid != 0
                        && (m.surface[0] == 0
                            || m.surface[3] == 0
                            || m.previous_index as usize >= previous.len() / 64)
            })
        {
            return Err(Error::InvalidFrame);
        }
        let poses_changed = self.last_previous != previous;
        let matches_changed = self.last_matches != matches;
        let parameters_changed = self.last_parameters != Some(parameters);
        let bytes = if poses_changed {
            previous.len() as u64
        } else {
            0
        } + if matches_changed {
            std::mem::size_of_val(matches) as u64
        } else {
            0
        } + if parameters_changed { 80 } else { 0 };
        Ok(bytes)
    }
    fn upload(
        &mut self,
        queue: &wgpu::Queue,
        token: Token,
        previous: &[u8],
        matches: &[GpuMatch],
        parameters: Parameters,
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if token.serial == 0 {
            return Err(Error::InvalidFrame);
        }
        let bytes = self.required_upload_bytes(previous, matches, parameters)?;
        if budget.remaining_bytes < bytes {
            return Err(Error::UploadBudget);
        }
        let poses_changed = self.last_previous != previous;
        let matches_changed = self.last_matches != matches;
        let parameters_changed = self.last_parameters != Some(parameters);
        if poses_changed && !previous.is_empty() {
            queue.write_buffer(&self.previous, 0, previous);
        }
        if matches_changed && !matches.is_empty() {
            queue.write_buffer(&self.matches, 0, bytemuck::cast_slice(matches));
        }
        if parameters_changed {
            queue.write_buffer(&self.camera, 0, bytemuck::bytes_of(&parameters));
        }
        self.last_previous.clear();
        self.last_previous.extend_from_slice(previous);
        self.last_matches.clear();
        self.last_matches.extend_from_slice(matches);
        self.last_parameters = Some(parameters);
        self.prepared_token = Some(token);
        budget.remaining_bytes -= bytes;
        Ok(Work {
            upload_bytes: bytes,
            previous_records: if poses_changed {
                (previous.len() / 64) as u32
            } else {
                0
            },
            matches: if matches_changed {
                matches.len() as u32
            } else {
                0
            },
            ..Default::default()
        })
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, token: Token) -> Result<Work, Error> {
        if self.prepared_token != Some(token) {
            return Err(Error::InvalidFrame);
        }
        if self.active == 0 {
            return Ok(Work::default());
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.base, &[]);
        pass.set_bind_group(1, &self.bind, &[]);
        if let Some(vertices) = &self.vertices {
            pass.set_vertex_buffer(0, vertices.slice(..));
        }
        let kinds = if self.family == Family::Entity {
            crate::entity_models::MODEL_KINDS
        } else {
            crate::block_models::MODEL_KINDS
        };
        let mut draws = 0;
        for kind in 0..kinds {
            if self.active & (1 << kind) != 0 {
                pass.draw_indirect(&self.indirect, kind as u64 * 16);
                draws += 1;
            }
        }
        if self.glass_active
            && let Some(glass) = &self.glass
        {
            pass.set_pipeline(glass);
            pass.draw_indirect(
                &self.indirect,
                u64::from(crate::block_models::kind::TRANSPORTER) * 16,
            );
            draws += 1;
        }
        Ok(Work {
            draws,
            ..Default::default()
        })
    }
}
