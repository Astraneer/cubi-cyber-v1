//! Bounded aerial perspective, preserving every scene MSAA sample separately.
use crate::{
    MSAA_SAMPLES,
    gpu_time::{GpuPassTimer, PassGraph},
    terrain_gpu::{Camera, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
#[derive(Clone, Copy, Debug)]
pub struct AerialConfig {
    pub grid: [u32; 3],
    pub steps: u32,
    pub max_distance: f32,
    pub max_target_bytes: u64,
}
impl Default for AerialConfig {
    fn default() -> Self {
        Self {
            grid: [32, 18, 32],
            steps: 24,
            max_distance: 16384.,
            max_target_bytes: 512 * 1024 * 1024,
        }
    }
}
pub struct AerialInputs<'a> {
    pub color_msaa: &'a wgpu::TextureView,
    pub depth_msaa: &'a wgpu::TextureView,
    pub optics: &'a wgpu::Buffer,
    pub transmittance: &'a wgpu::TextureView,
    pub shadows: Option<AerialShadows<'a>>,
    pub multiple_scattering: Option<&'a wgpu::TextureView>,
}
#[derive(Clone, Copy)]
pub struct AerialShadows<'a> {
    pub parameters: &'a wgpu::Buffer,
    pub depth: &'a wgpu::TextureView,
    pub sampler: &'a wgpu::Sampler,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AerialState {
    pub camera: Camera,
    pub eye: [f32; 3],
    pub source_revision: u64,
    pub atmosphere_revision: u64,
    pub shadow_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerialError {
    InvalidConfig,
    InvalidState,
    UploadBudget,
    WorkBudget,
}
impl std::fmt::Display for AerialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for AerialError {}
#[derive(Clone, Copy, Debug, Default)]
pub struct AerialWork {
    pub integration_steps: u64,
    pub composition_samples: u64,
    pub upload_bytes: u64,
    pub volume_bytes: u64,
    pub target_bytes: u64,
    pub shadow_samples: u64,
    pub passes: u32,
    pub draws: u32,
    pub pipeline_compilations: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    inverse: [[f32; 4]; 4],
    eye_distance: [f32; 4],
    grid: [u32; 4],
    work: [u32; 4],
}
struct Targets {
    _color: wgpu::Texture,
    color: wgpu::TextureView,
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
}
pub struct AerialGpu {
    sample_count: u32,
    config: AerialConfig,
    width: u32,
    height: u32,
    uniform: wgpu::Buffer,
    _scattering: wgpu::Texture,
    _transmission: wgpu::Texture,
    scattering: wgpu::TextureView,
    transmission: wgpu::TextureView,
    sampler: wgpu::Sampler,
    compute_bind: wgpu::BindGroup,
    composite_bind: wgpu::BindGroup,
    compute: wgpu::ComputePipeline,
    composite: wgpu::RenderPipeline,
    targets: Targets,
    last: Option<AerialState>,
    timer: GpuPassTimer<2>,
    startup: AerialWork,
    shadowed: bool,
    multiple_enabled: bool,
}
fn buffer_layout(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn texture_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    dimension: wgpu::TextureViewDimension,
    multisampled: bool,
    depth: bool,
    filterable: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: if depth {
                wgpu::TextureSampleType::Depth
            } else {
                wgpu::TextureSampleType::Float { filterable }
            },
            view_dimension: dimension,
            multisampled,
        },
        count: None,
    }
}
fn texture(
    device: &wgpu::Device,
    label: &str,
    size: wgpu::Extent3d,
    dimension: wgpu::TextureDimension,
    samples: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: samples,
        dimension,
        format: wgpu::TextureFormat::Rgba16Float,
        usage,
        view_formats: &[],
    })
}
fn targets(device: &wgpu::Device, width: u32, height: u32, sample_count: u32) -> Targets {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let resolved = texture(
        device,
        "resolved aerial HDR",
        size,
        wgpu::TextureDimension::D2,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    );
    let color = if sample_count == 1 {
        resolved.clone()
    } else {
        texture(
            device,
            "separate MSAA aerial output",
            size,
            wgpu::TextureDimension::D2,
            sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        )
    };
    let view = color.create_view(&Default::default());
    let resolved_view = resolved.create_view(&Default::default());
    Targets {
        _color: color,
        color: view,
        resolved,
        resolved_view,
    }
}
impl AerialGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: AerialConfig,
        width: u32,
        height: u32,
        inputs: AerialInputs<'_>,
    ) -> Result<Self, AerialError> {
        Self::new_with_samples(device, queue, config, width, height, inputs, MSAA_SAMPLES)
    }
    pub fn new_with_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: AerialConfig,
        width: u32,
        height: u32,
        inputs: AerialInputs<'_>,
        sample_count: u32,
    ) -> Result<Self, AerialError> {
        if !matches!(sample_count, 1 | 4) {
            return Err(AerialError::InvalidConfig);
        }
        if config.grid.iter().any(|v| *v == 0 || *v > 128)
            || config.grid[2] < 2
            || config.steps == 0
            || config.steps > 64
            || !config.max_distance.is_finite()
            || !(1.0..=32768.).contains(&config.max_distance)
            || width == 0
            || height == 0
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
            || u64::from(width)
                * u64::from(height)
                * crate::sampling::color_bytes_per_pixel(sample_count)
                > config.max_target_bytes
        {
            return Err(AerialError::InvalidConfig);
        }
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerial camera and bounded march"),
            size: 112,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let size = wgpu::Extent3d {
            width: config.grid[0],
            height: config.grid[1],
            depth_or_array_layers: config.grid[2],
        };
        let volume = |label| {
            texture(
                device,
                label,
                size,
                wgpu::TextureDimension::D3,
                1,
                wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
            )
        };
        let scattering = volume("aerial scattering froxels");
        let transmission = volume("aerial RGB transmission froxels");
        let scattering_view = scattering.create_view(&Default::default());
        let transmission_view = transmission.create_view(&Default::default());
        let c = wgpu::ShaderStages::COMPUTE;
        let f = wgpu::ShaderStages::FRAGMENT;
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: c,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba16Float,
                view_dimension: wgpu::TextureViewDimension::D3,
            },
            count: None,
        };
        let mut compute_entries = vec![
            buffer_layout(0, c),
            buffer_layout(1, c),
            texture_layout(2, c, wgpu::TextureViewDimension::D2, false, false, false),
            storage(3),
            storage(4),
        ];
        if inputs.multiple_scattering.is_some() {
            compute_entries.push(texture_layout(
                5,
                c,
                wgpu::TextureViewDimension::D2,
                false,
                false,
                false,
            ));
        }
        if inputs.shadows.is_some() {
            compute_entries.extend([
                buffer_layout(9, c),
                texture_layout(
                    10,
                    c,
                    wgpu::TextureViewDimension::D2Array,
                    false,
                    true,
                    false,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: c,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ]);
        }
        let compute_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("same atmosphere aerial optics"),
            entries: &compute_entries,
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sample-preserving aerial composition"),
            entries: &[
                buffer_layout(0, f),
                texture_layout(
                    1,
                    f,
                    wgpu::TextureViewDimension::D2,
                    sample_count > 1,
                    false,
                    false,
                ),
                texture_layout(
                    2,
                    f,
                    wgpu::TextureViewDimension::D2,
                    sample_count > 1,
                    true,
                    false,
                ),
                texture_layout(3, f, wgpu::TextureViewDimension::D3, false, false, true),
                texture_layout(4, f, wgpu::TextureViewDimension::D3, false, false, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: f,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bounded aerial trilinear reconstruction"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let compute_bind = Self::compute_bind(
            device,
            &compute_layout,
            &uniform,
            &scattering_view,
            &transmission_view,
            &inputs,
        );
        let composite_bind = Self::composite_bind(
            device,
            &composite_layout,
            &uniform,
            &scattering_view,
            &transmission_view,
            &sampler,
            &inputs,
        );
        let source = include_str!("aerial.wgsl").replace(
            "// SHADOWS",
            if inputs.shadows.is_some() {
                include_str!("shadow_sampling.wgsl")
            } else {
                "fn cascade_shadow(world:vec3<f32>,normal:vec3<f32>)->f32{return 1.;}"
            },
        );
        let source = crate::multiple_scattering::shader_source(
            &source,
            inputs.multiple_scattering.is_some(),
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("original aerial froxel integration"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aerial integration pipeline"),
            bind_group_layouts: &[&compute_layout],
            push_constant_ranges: &[],
        });
        let compute = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("bounded aerial integration"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("build_aerial"),
            compilation_options: Default::default(),
            cache: None,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("original aerial MSAA composition"),
            source: wgpu::ShaderSource::Wgsl(crate::sampling::shader(
                include_str!("aerial_composite.wgsl"),
                sample_count,
            )),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aerial composition pipeline"),
            bind_group_layouts: &[&composite_layout],
            push_constant_ranges: &[],
        });
        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aerial MSAA4 HDR"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
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
        Ok(Self {
            sample_count,
            config,
            width,
            height,
            uniform,
            _scattering: scattering,
            _transmission: transmission,
            scattering: scattering_view,
            transmission: transmission_view,
            sampler,
            compute_bind,
            composite_bind,
            compute,
            composite,
            targets: targets(device, width, height, sample_count),
            last: None,
            timer: GpuPassTimer::new(device, queue, "aerial perspective pass times"),
            startup: AerialWork {
                pipeline_compilations: 2,
                volume_bytes: u64::from(config.grid[0])
                    * u64::from(config.grid[1])
                    * u64::from(config.grid[2])
                    * 16,
                target_bytes: u64::from(width)
                    * u64::from(height)
                    * crate::sampling::color_bytes_per_pixel(sample_count),
                ..Default::default()
            },
            shadowed: inputs.shadows.is_some(),
            multiple_enabled: inputs.multiple_scattering.is_some(),
        })
    }
    fn compute_bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        scattering: &wgpu::TextureView,
        transmission: &wgpu::TextureView,
        inputs: &AerialInputs<'_>,
    ) -> wgpu::BindGroup {
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: inputs.optics.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(inputs.transmittance),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(scattering),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(transmission),
            },
        ];
        if let Some(multiple) = inputs.multiple_scattering {
            entries.push(wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(multiple),
            });
        }
        if let Some(shadows) = inputs.shadows {
            entries.extend([
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: shadows.parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(shadows.depth),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(shadows.sampler),
                },
            ]);
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shared physical aerial coefficients"),
            layout,
            entries: &entries,
        })
    }
    #[allow(clippy::too_many_arguments)]
    fn composite_bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        scattering: &wgpu::TextureView,
        transmission: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        inputs: &AerialInputs<'_>,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("read-only scene and aerial volumes"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(inputs.color_msaa),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(inputs.depth_msaa),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(scattering),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(transmission),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }
    pub fn resolved_view(&self) -> &wgpu::TextureView {
        &self.targets.resolved_view
    }
    pub fn resolved_texture(&self) -> &wgpu::Texture {
        &self.targets.resolved
    }
    pub fn color_msaa(&self) -> &wgpu::TextureView {
        &self.targets.color
    }
    pub fn take_startup_work(&mut self) -> AerialWork {
        std::mem::take(&mut self.startup)
    }
    pub fn needs_redraw(&self, state: AerialState) -> bool {
        self.last != Some(state)
    }
    pub fn invalidate(&mut self) {
        self.last = None;
    }
    pub fn pending_work(&self, state: AerialState) -> AerialWork {
        if !self.needs_redraw(state) {
            return AerialWork::default();
        }
        let volume = self.last.is_none_or(|last| {
            last.camera != state.camera
                || last.eye != state.eye
                || last.atmosphere_revision != state.atmosphere_revision
                || (self.shadowed && last.shadow_revision != state.shadow_revision)
        });
        AerialWork {
            integration_steps: u64::from(volume)
                * u64::from(self.config.grid[0])
                * u64::from(self.config.grid[1])
                * u64::from(self.config.grid[2])
                * u64::from(self.config.steps),
            composition_samples: u64::from(self.width)
                * u64::from(self.height)
                * u64::from(self.sample_count),
            upload_bytes: u64::from(volume) * 112,
            shadow_samples: u64::from(volume && self.shadowed)
                * u64::from(self.config.grid[0])
                * u64::from(self.config.grid[1])
                * u64::from(self.config.grid[2])
                * u64::from(self.config.steps)
                * 8,
            passes: 1 + u32::from(volume),
            draws: 1,
            ..Default::default()
        }
    }
    pub fn frame_graph(&self) -> PassGraph<2> {
        self.timer.graph()
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.timer.timestamp_buffer()
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        state: AerialState,
        budget: &mut UploadBudget,
        steps: &mut u64,
        samples: &mut u64,
    ) -> Result<AerialWork, AerialError> {
        let inverse = state
            .camera
            .inverse_view_projection()
            .ok_or(AerialError::InvalidState)?;
        if state
            .eye
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 999000.)
        {
            return Err(AerialError::InvalidState);
        }
        self.timer.begin();
        let work = self.pending_work(state);
        if work.passes == 0 {
            return Ok(work);
        }
        if work.integration_steps > *steps || work.composition_samples > *samples {
            return Err(AerialError::WorkBudget);
        }
        if work.upload_bytes > budget.remaining_bytes {
            return Err(AerialError::UploadBudget);
        }
        let p = Parameters {
            inverse,
            eye_distance: [
                state.eye[0],
                state.eye[1],
                state.eye[2],
                self.config.max_distance,
            ],
            grid: [
                self.config.grid[0],
                self.config.grid[1],
                self.config.grid[2],
                0,
            ],
            work: [self.config.steps, 0, 0, 0],
        };
        if work.upload_bytes > 0 {
            queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&p));
        }
        if work.integration_steps > 0 {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("aerial integration bounded by froxels"),
                timestamp_writes: self.timer.compute(1),
            });
            pass.set_pipeline(&self.compute);
            pass.set_bind_group(0, &self.compute_bind, &[]);
            pass.dispatch_workgroups(
                self.config.grid[0].div_ceil(4),
                self.config.grid[1].div_ceil(4),
                self.config.grid[2].div_ceil(4),
            );
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("per-sample aerial composition"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.color,
                    depth_slice: None,
                    resolve_target: (self.sample_count > 1).then_some(&self.targets.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: self.timer.render(2),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.composite);
            pass.set_bind_group(0, &self.composite_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.timer.finish(encoder);
        self.last = Some(state);
        budget.remaining_bytes -= work.upload_bytes;
        *steps -= work.integration_steps;
        *samples -= work.composition_samples;
        Ok(work)
    }
    pub fn resize_inputs(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        inputs: AerialInputs<'_>,
    ) -> Result<(), AerialError> {
        if !self.can_resize(device, width, height)
            || self.shadowed != inputs.shadows.is_some()
            || self.multiple_enabled != inputs.multiple_scattering.is_some()
        {
            return Err(AerialError::InvalidConfig);
        }
        self.targets = targets(device, width, height, self.sample_count);
        self.compute_bind = Self::compute_bind(
            device,
            &self.compute.get_bind_group_layout(0),
            &self.uniform,
            &self.scattering,
            &self.transmission,
            &inputs,
        );
        self.composite_bind = Self::composite_bind(
            device,
            &self.composite.get_bind_group_layout(0),
            &self.uniform,
            &self.scattering,
            &self.transmission,
            &self.sampler,
            &inputs,
        );
        self.width = width;
        self.height = height;
        self.last = None;
        Ok(())
    }
    pub fn can_resize(&self, device: &wgpu::Device, width: u32, height: u32) -> bool {
        !(width == 0
            || height == 0
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
            || u64::from(width)
                * u64::from(height)
                * crate::sampling::color_bytes_per_pixel(self.sample_count)
                > self.config.max_target_bytes)
    }
}
