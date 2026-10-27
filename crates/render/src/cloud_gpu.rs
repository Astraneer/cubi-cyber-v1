//! Physical cloud slab integration and sample-preserving MSAA composition.
//! Inputs are immutable scene attachments; output is a separate HDR target.
use crate::{
    MSAA_SAMPLES,
    terrain_gpu::{Camera, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudQuality {
    LayerA,
    VolumeB,
}
#[derive(Clone, Copy, Debug)]
pub enum CloudDensity {
    Procedural,
    Uniform(f32),
}
#[derive(Clone, Copy, Debug)]
pub struct CloudConfig {
    pub quality: CloudQuality,
    pub downsample: u32,
    pub steps: u32,
    pub light_steps: u32,
    pub bottom: f32,
    pub thickness: f32,
    pub coverage: f32,
    pub softness: f32,
    pub wind: [f32; 2],
    pub noise_scale: f32,
    pub extinction: f32,
    pub anisotropy: f32,
    pub sky_strength: f32,
    pub max_distance: f32,
    pub density: CloudDensity,
}
impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            quality: CloudQuality::LayerA,
            downsample: 2,
            steps: 32,
            light_steps: 4,
            bottom: 192.,
            thickness: 48.,
            coverage: 0.55,
            softness: 0.15,
            wind: [0.02, 0.01],
            noise_scale: 0.015,
            extinction: 0.045,
            anisotropy: 0.6,
            sky_strength: 0.8,
            max_distance: 32768.,
            density: CloudDensity::Procedural,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudState {
    pub camera: Camera,
    pub eye: [f32; 3],
    pub time_seconds: f32,
    pub sun: [f32; 3],
    pub sun_intensity: f32,
    pub scene_revision: u64,
}
#[derive(Clone, Copy)]
pub struct CloudInputs<'a> {
    pub transmittance: &'a wgpu::TextureView,
    pub sky: &'a wgpu::TextureView,
    pub scene_color_msaa: &'a wgpu::TextureView,
    pub scene_depth_msaa: &'a wgpu::TextureView,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudError {
    InvalidConfig,
    InvalidState,
    UploadBudget,
    WorkBudget,
}
impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CloudError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloudWork {
    pub march_pixels: u64,
    pub max_view_steps: u64,
    pub max_density_samples: u64,
    pub composition_samples: u64,
    pub upload_bytes: u64,
    pub passes: u32,
    pub draws: u32,
    pub pipeline_compilations: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    inverse_view_projection: [[f32; 4]; 4],
    eye_time: [f32; 4],
    sun: [f32; 4],
    layer: [f32; 4],
    wind: [f32; 4],
    image: [u32; 4],
    quality: [u32; 4],
    optics: [f32; 4],
}
/// Actual current low-resolution transport/depth composite bindings for the
/// optional temporal mask; retains the same depth-aware upsampling weights.
pub struct CloudTemporalSource<'a> {
    pub bind: &'a wgpu::BindGroup,
    pub extent: [u32; 2],
    pub samples: u32,
    pub prepared: bool,
}
pub struct CloudGpu {
    sample_count: u32,
    config: CloudConfig,
    width: u32,
    height: u32,
    march_size: [u32; 2],
    uniform: wgpu::Buffer,
    statistics: wgpu::Buffer,
    _full: wgpu::Texture,
    full: wgpu::TextureView,
    _opaque: wgpu::Texture,
    opaque: wgpu::TextureView,
    _end: wgpu::Texture,
    _msaa: wgpu::Texture,
    msaa: wgpu::TextureView,
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    compute_bind: wgpu::BindGroup,
    composite_bind: wgpu::BindGroup,
    reset: wgpu::ComputePipeline,
    march: wgpu::ComputePipeline,
    composite: wgpu::RenderPipeline,
    last: Option<CloudState>,
    timer: crate::gpu_time::GpuPassTimer<2>,
}
impl CloudGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: CloudConfig,
        width: u32,
        height: u32,
        inputs: CloudInputs<'_>,
    ) -> Result<Self, CloudError> {
        Self::new_with_samples(device, queue, config, width, height, inputs, MSAA_SAMPLES)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: CloudConfig,
        width: u32,
        height: u32,
        inputs: CloudInputs<'_>,
        sample_count: u32,
    ) -> Result<Self, CloudError> {
        if !matches!(sample_count, 1 | 4) {
            return Err(CloudError::InvalidConfig);
        }
        let uniform_density = match config.density {
            CloudDensity::Procedural => -1.,
            CloudDensity::Uniform(d) if d.is_finite() && (0.0..=1.0).contains(&d) => d,
            _ => return Err(CloudError::InvalidConfig),
        };
        let _ = uniform_density;
        if width == 0
            || height == 0
            || width.max(height) > device.limits().max_texture_dimension_2d
            || !(1..=4).contains(&config.downsample)
            || !(1..=96).contains(&config.steps)
            || config.light_steps > 8
            || [
                config.bottom,
                config.thickness,
                config.coverage,
                config.softness,
                config.wind[0],
                config.wind[1],
                config.noise_scale,
                config.extinction,
                config.anisotropy,
                config.sky_strength,
                config.max_distance,
            ]
            .iter()
            .any(|v| !v.is_finite())
            || config.thickness <= 0.
            || !(0.0..=1.0).contains(&config.coverage)
            || config.softness <= 0.
            || config.noise_scale <= 0.
            || config.extinction < 0.
            || config.anisotropy.abs() >= 0.95
            || config.sky_strength < 0.
            || config.max_distance <= 0.
        {
            return Err(CloudError::InvalidConfig);
        }
        let march_size = [
            width.div_ceil(config.downsample),
            height.div_ceil(config.downsample),
        ];
        let steps = if config.quality == CloudQuality::LayerA {
            1
        } else {
            u64::from(config.steps) * (1 + u64::from(config.light_steps))
        };
        if u64::from(march_size[0]) * u64::from(march_size[1]) * steps > u64::from(u32::MAX) {
            return Err(CloudError::InvalidConfig);
        }
        let texture = |label, size: [u32; 2], format, samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let storage = wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        let full_texture = texture(
            "full cloud scattering and transmittance",
            march_size,
            wgpu::TextureFormat::Rgba16Float,
            1,
            storage,
        );
        let full = full_texture.create_view(&Default::default());
        let opaque_texture = texture(
            "cloud scattering before known opaque depth",
            march_size,
            wgpu::TextureFormat::Rgba16Float,
            1,
            storage,
        );
        let opaque = opaque_texture.create_view(&Default::default());
        let end = texture(
            "cloud depth-aware reconstruction distance",
            march_size,
            wgpu::TextureFormat::R32Float,
            1,
            storage,
        );
        let end_view = end.create_view(&Default::default());
        let resolved = texture(
            "cloud-composited resolved HDR",
            [width, height],
            wgpu::TextureFormat::Rgba16Float,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let msaa_texture = if sample_count == 1 {
            resolved.clone()
        } else {
            texture(
                "separate cloud-composited HDR MSAA4",
                [width, height],
                wgpu::TextureFormat::Rgba16Float,
                sample_count,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        };
        let msaa = msaa_texture.create_view(&Default::default());
        let resolved_view = resolved.create_view(&Default::default());
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("physical cloud state176B"),
            size: 176,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let statistics = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual cloud GPU work counters"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let c = wgpu::ShaderStages::COMPUTE;
        let f = wgpu::ShaderStages::FRAGMENT;
        let uniform_entry = |visibility| wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let sample = |binding, visibility, depth, multisampled| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type: if depth {
                    wgpu::TextureSampleType::Depth
                } else {
                    wgpu::TextureSampleType::Float { filterable: false }
                },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled,
            },
            count: None,
        };
        let store = |binding, format| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: c,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let compute_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bounded physical cloud march"),
            entries: &[
                uniform_entry(c),
                sample(1, c, false, false),
                sample(2, c, false, false),
                sample(3, c, true, sample_count > 1),
                store(4, wgpu::TextureFormat::Rgba16Float),
                store(5, wgpu::TextureFormat::Rgba16Float),
                store(6, wgpu::TextureFormat::R32Float),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: c,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("per-sample cloud composition"),
            entries: &[
                uniform_entry(f),
                sample(1, f, false, false),
                sample(2, f, false, false),
                sample(3, f, false, false),
                sample(4, f, true, sample_count > 1),
                sample(5, f, false, sample_count > 1),
            ],
        });
        let texture_entry = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let compute_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("physical cloud source and output tables"),
            layout: &compute_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                texture_entry(1, inputs.transmittance),
                texture_entry(2, inputs.sky),
                texture_entry(3, inputs.scene_depth_msaa),
                texture_entry(4, &full),
                texture_entry(5, &opaque),
                texture_entry(6, &end_view),
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: statistics.as_entire_binding(),
                },
            ],
        });
        let composite_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("separate cloud output preserving scene samples"),
            layout: &composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                texture_entry(1, &full),
                texture_entry(2, &opaque),
                texture_entry(3, &end_view),
                texture_entry(4, inputs.scene_depth_msaa),
                texture_entry(5, inputs.scene_color_msaa),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("original physical cloud density integration"),
            source: wgpu::ShaderSource::Wgsl(crate::sampling::shader(
                include_str!("clouds.wgsl"),
                sample_count,
            )),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud compute"),
            bind_group_layouts: &[&compute_layout],
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
        let march = compute("march");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cloud MSAA4 depth-aware HDR reconstruction"),
            source: wgpu::ShaderSource::Wgsl(crate::sampling::shader(
                include_str!("cloud_composite.wgsl"),
                sample_count,
            )),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud composition"),
            bind_group_layouts: &[&composite_layout],
            push_constant_ranges: &[],
        });
        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("physical clouds per MSAA sample"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
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
        Ok(Self {
            sample_count,
            config,
            width,
            height,
            march_size,
            uniform,
            statistics,
            _full: full_texture,
            full,
            _opaque: opaque_texture,
            opaque,
            _end: end,
            _msaa: msaa_texture,
            msaa,
            resolved,
            resolved_view,
            compute_bind,
            composite_bind,
            reset,
            march,
            composite,
            last: None,
            timer: crate::gpu_time::GpuPassTimer::new(
                device,
                queue,
                "physical cloud pass timestamps",
            ),
        })
    }
    /// Explicit resize only. Both compute and composition pipelines are reused.
    pub fn resize_inputs(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        inputs: CloudInputs<'_>,
    ) -> Result<(), CloudError> {
        let sample_count = self.sample_count;
        if width == 0 || height == 0 || width.max(height) > device.limits().max_texture_dimension_2d
        {
            return Err(CloudError::InvalidConfig);
        }
        let march_size = [
            width.div_ceil(self.config.downsample),
            height.div_ceil(self.config.downsample),
        ];
        let steps = if self.config.quality == CloudQuality::LayerA {
            1
        } else {
            u64::from(self.config.steps) * (1 + u64::from(self.config.light_steps))
        };
        if u64::from(march_size[0]) * u64::from(march_size[1]) * steps > u64::from(u32::MAX) {
            return Err(CloudError::InvalidConfig);
        }
        let texture = |label, size: [u32; 2], format, samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let storage = wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        let full_texture = texture(
            "resized full cloud integral",
            march_size,
            wgpu::TextureFormat::Rgba16Float,
            1,
            storage,
        );
        let full = full_texture.create_view(&Default::default());
        let opaque_texture = texture(
            "resized opaque cloud integral",
            march_size,
            wgpu::TextureFormat::Rgba16Float,
            1,
            storage,
        );
        let opaque = opaque_texture.create_view(&Default::default());
        let end = texture(
            "resized cloud depth distance",
            march_size,
            wgpu::TextureFormat::R32Float,
            1,
            storage,
        );
        let end_view = end.create_view(&Default::default());
        let resolved = texture(
            "resized cloud resolved HDR",
            [width, height],
            wgpu::TextureFormat::Rgba16Float,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let msaa_texture = if sample_count == 1 {
            resolved.clone()
        } else {
            texture(
                "resized cloud HDR MSAA4",
                [width, height],
                wgpu::TextureFormat::Rgba16Float,
                sample_count,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        };
        let msaa = msaa_texture.create_view(&Default::default());
        let resolved_view = resolved.create_view(&Default::default());
        let tex = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let compute_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resized cloud integration inputs"),
            layout: &self.march.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                tex(1, inputs.transmittance),
                tex(2, inputs.sky),
                tex(3, inputs.scene_depth_msaa),
                tex(4, &full),
                tex(5, &opaque),
                tex(6, &end_view),
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: self.statistics.as_entire_binding(),
                },
            ],
        });
        let composite_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resized cloud per-sample inputs"),
            layout: &self.composite.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                tex(1, &full),
                tex(2, &opaque),
                tex(3, &end_view),
                tex(4, inputs.scene_depth_msaa),
                tex(5, inputs.scene_color_msaa),
            ],
        });
        self.width = width;
        self.height = height;
        self.march_size = march_size;
        self._full = full_texture;
        self.full = full;
        self._opaque = opaque_texture;
        self.opaque = opaque;
        self._end = end;
        self._msaa = msaa_texture;
        self.msaa = msaa;
        self.resolved = resolved;
        self.resolved_view = resolved_view;
        self.compute_bind = compute_bind;
        self.composite_bind = composite_bind;
        self.last = None;
        self.timer.begin();
        Ok(())
    }
    pub fn needs_redraw(&self, state: CloudState) -> bool {
        self.last != Some(state)
    }
    pub fn startup_work(&self) -> CloudWork {
        CloudWork {
            pipeline_compilations: 3,
            ..CloudWork::default()
        }
    }
    pub fn worst_work(&self) -> CloudWork {
        let pixels = u64::from(self.march_size[0]) * u64::from(self.march_size[1]);
        let steps = if self.config.quality == CloudQuality::LayerA {
            1
        } else {
            u64::from(self.config.steps)
        };
        let lights = if self.config.quality == CloudQuality::LayerA {
            0
        } else {
            u64::from(self.config.light_steps)
        };
        CloudWork {
            march_pixels: pixels,
            max_view_steps: pixels * steps,
            max_density_samples: pixels * steps * (1 + lights),
            composition_samples: u64::from(self.width)
                * u64::from(self.height)
                * u64::from(self.sample_count),
            passes: 2,
            draws: 1,
            ..CloudWork::default()
        }
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        state: CloudState,
        budget: &mut UploadBudget,
        max_density_samples: u64,
    ) -> Result<CloudWork, CloudError> {
        self.timer.begin();
        if state
            .eye
            .iter()
            .chain(state.sun.iter())
            .chain([state.time_seconds, state.sun_intensity].iter())
            .any(|v| !v.is_finite())
            || state.sun_intensity < 0.
        {
            return Err(CloudError::InvalidState);
        }
        let length = state.sun.iter().map(|v| v * v).sum::<f32>().sqrt();
        if length < 1e-6 || !length.is_finite() {
            return Err(CloudError::InvalidState);
        }
        let inverse = state
            .camera
            .inverse_view_projection()
            .ok_or(CloudError::InvalidState)?;
        if self.last == Some(state) {
            return Ok(CloudWork::default());
        }
        let mut work = self.worst_work();
        if work.max_density_samples > max_density_samples {
            return Err(CloudError::WorkBudget);
        }
        if budget.remaining_bytes < 176 {
            return Err(CloudError::UploadBudget);
        }
        let uniform_density = match self.config.density {
            CloudDensity::Procedural => -1.,
            CloudDensity::Uniform(d) => d,
        };
        let p = Params {
            inverse_view_projection: inverse,
            eye_time: [state.eye[0], state.eye[1], state.eye[2], state.time_seconds],
            sun: [
                state.sun[0] / length,
                state.sun[1] / length,
                state.sun[2] / length,
                state.sun_intensity,
            ],
            layer: [
                self.config.bottom,
                self.config.thickness,
                self.config.coverage,
                self.config.softness,
            ],
            wind: [
                self.config.wind[0],
                self.config.wind[1],
                self.config.noise_scale,
                uniform_density,
            ],
            image: [
                self.width,
                self.height,
                self.march_size[0],
                self.march_size[1],
            ],
            quality: [
                self.config.steps,
                self.config.light_steps,
                u32::from(self.config.quality == CloudQuality::VolumeB),
                0,
            ],
            optics: [
                self.config.extinction,
                self.config.anisotropy,
                self.config.sky_strength,
                self.config.max_distance,
            ],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&p));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("bounded cloud view and sun integration"),
                timestamp_writes: self.timer.compute(1),
            });
            pass.set_bind_group(0, &self.compute_bind, &[]);
            pass.set_pipeline(&self.reset);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.march);
            pass.dispatch_workgroups(
                self.march_size[0].div_ceil(8),
                self.march_size[1].div_ceil(8),
                1,
            );
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cloud physical scattering into separate MSAA4 HDR"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa,
                    depth_slice: None,
                    resolve_target: (self.sample_count > 1).then_some(&self.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: self.timer.render(2),
            });
            pass.set_pipeline(&self.composite);
            pass.set_bind_group(0, &self.composite_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.timer.finish(encoder);
        budget.remaining_bytes -= 176;
        work.upload_bytes = 176;
        self.last = Some(state);
        Ok(work)
    }
    pub fn graph(&self) -> crate::gpu_time::PassGraph<2> {
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
        self.timer.copy_into(encoder, destination, offset)
    }
    pub fn invalidate(&mut self) {
        self.last = None
    }
    pub fn resolved_view(&self) -> &wgpu::TextureView {
        &self.resolved_view
    }
    pub fn resolved_texture(&self) -> &wgpu::Texture {
        &self.resolved
    }
    pub fn full_cloud_texture(&self) -> &wgpu::Texture {
        &self._full
    }
    pub fn full_cloud_view(&self) -> &wgpu::TextureView {
        &self.full
    }
    pub fn opaque_cloud_view(&self) -> &wgpu::TextureView {
        &self.opaque
    }
    pub fn statistics_buffer(&self) -> &wgpu::Buffer {
        &self.statistics
    }
    pub fn temporal_source(&self) -> CloudTemporalSource<'_> {
        CloudTemporalSource {
            bind: &self.composite_bind,
            extent: [self.width, self.height],
            samples: self.sample_count,
            prepared: self.last.is_some(),
        }
    }
    /// Loading only; no bind/layout work is performed by temporal_source.
    pub fn temporal_layout(&self) -> wgpu::BindGroupLayout {
        self.composite.get_bind_group_layout(0)
    }
}
/// Loading-only source for an optional consumer of the exact color composite.
pub fn temporal_composite_source(samples: u32) -> String {
    crate::sampling::shader(include_str!("cloud_composite.wgsl"), samples).into_owned()
}
