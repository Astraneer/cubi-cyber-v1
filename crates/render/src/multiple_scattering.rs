//! Hillaire2020 multiple-scattering approximation, independently implemented
//! from the public paper's equations5-10. No renderer implementation was read.
use crate::{
    gi::RAYS,
    gpu_time::{GpuPassTimer, PassGraph},
    terrain_gpu::UploadBudget,
};
use bytemuck::{Pod, Zeroable};
#[derive(Clone, Copy, Debug)]
pub struct MultipleScatteringConfig {
    pub size: [u32; 2],
    pub directions: u32,
    pub steps: u32,
    pub ground_albedo: [f32; 3],
}
impl Default for MultipleScatteringConfig {
    fn default() -> Self {
        Self {
            size: [32; 2],
            directions: 64,
            steps: 24,
            ground_albedo: [0.3; 3],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultipleScatteringError {
    InvalidConfig,
    UploadBudget,
    WorkBudget,
}
impl std::fmt::Display for MultipleScatteringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MultipleScatteringError {}
#[derive(Clone, Copy, Debug, Default)]
pub struct MultipleScatteringWork {
    pub rays: u64,
    pub integration_steps: u64,
    pub sunlight_lookups: u64,
    pub texture_bytes: u64,
    pub upload_bytes: u64,
    pub passes: u32,
    pub pipeline_compilations: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Settings {
    image_work: [u32; 4],
    ground: [f32; 4],
}
pub struct MultipleScatteringGpu {
    config: MultipleScatteringConfig,
    psi: wgpu::Texture,
    feedback: wgpu::Texture,
    view: wgpu::TextureView,
    feedback_view: wgpu::TextureView,
    _settings: wgpu::Buffer,
    _directions: wgpu::Buffer,
    bind: wgpu::BindGroup,
    pipeline: wgpu::ComputePipeline,
    last: Option<u64>,
    timer: GpuPassTimer<1>,
    startup: MultipleScatteringWork,
}
pub(crate) fn shader_source(source: &str, enabled: bool) -> String {
    if enabled {
        source.replace("fn sample_multiple_scattering(point:vec3<f32>,sun:vec3<f32>,ground_radius:f32,top_radius:f32)->vec3<f32>{return vec3(0.0);}",include_str!("multiple_scattering_sampling.wgsl"))
    } else {
        source.into()
    }
}
impl MultipleScatteringGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        optics: &wgpu::Buffer,
        transmittance: &wgpu::TextureView,
        config: MultipleScatteringConfig,
        budget: &mut UploadBudget,
    ) -> Result<Self, MultipleScatteringError> {
        if config.size.iter().any(|v| !(2..=64).contains(v))
            || ![8, 16, 32, 64].contains(&config.directions)
            || !(1..=128).contains(&config.steps)
            || config
                .ground_albedo
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.).contains(v))
        {
            return Err(MultipleScatteringError::InvalidConfig);
        }
        if budget.remaining_bytes < 1056 {
            return Err(MultipleScatteringError::UploadBudget);
        }
        let texture = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: config.size[0],
                    height: config.size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let psi = texture("multiple scattering Psi per unit illuminance");
        let feedback = texture("multiple scattering bounded feedback");
        let view = psi.create_view(&Default::default());
        let feedback_view = feedback.create_view(&Default::default());
        let settings = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("multiple scattering bounded work"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let directions = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("immutable sphere quadrature"),
            size: 1024,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params = Settings {
            image_work: [
                config.size[0],
                config.size[1],
                config.directions,
                config.steps,
            ],
            ground: [
                config.ground_albedo[0],
                config.ground_albedo[1],
                config.ground_albedo[2],
                0.,
            ],
        };
        queue.write_buffer(&settings, 0, bytemuck::bytes_of(&params));
        queue.write_buffer(&directions, 0, bytemuck::cast_slice(&RAYS));
        budget.remaining_bytes -= 1056;
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba16Float,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("same physical atmosphere for multiple scattering"),
            entries: &[
                uniform(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                storage(2),
                storage(3),
                uniform(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("multiple scattering input/output graph"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: optics.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(transmittance),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&feedback_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: settings.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: directions.as_entire_binding(),
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("original Hillaire equations5-10"),
            source: wgpu::ShaderSource::Wgsl(include_str!("multiple_scattering.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("single bounded multiple-scattering pass"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("integrate infinite-order transfer"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("build_multiple_scattering"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Self {
            config,
            psi,
            feedback,
            view,
            feedback_view,
            _settings: settings,
            _directions: directions,
            bind,
            pipeline,
            last: None,
            timer: GpuPassTimer::new(device, queue, "multiple scattering pass time"),
            startup: MultipleScatteringWork {
                texture_bytes: u64::from(config.size[0]) * u64::from(config.size[1]) * 16,
                upload_bytes: 1056,
                pipeline_compilations: 1,
                ..Default::default()
            },
        })
    }
    pub fn size(&self) -> [u32; 2] {
        self.config.size
    }
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
    pub fn texture(&self) -> &wgpu::Texture {
        &self.psi
    }
    pub fn feedback_view(&self) -> &wgpu::TextureView {
        &self.feedback_view
    }
    pub fn feedback_texture(&self) -> &wgpu::Texture {
        &self.feedback
    }
    pub fn take_startup_work(&mut self) -> MultipleScatteringWork {
        std::mem::take(&mut self.startup)
    }
    pub fn needs_redraw(&self, optical_revision: u64) -> bool {
        self.last != Some(optical_revision)
    }
    pub fn invalidate(&mut self) {
        self.last = None;
    }
    pub fn frame_graph(&self) -> PassGraph<1> {
        self.timer.graph()
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.timer.timestamp_buffer()
    }
    pub fn pending_work(&self, optical_revision: u64) -> MultipleScatteringWork {
        if !self.needs_redraw(optical_revision) {
            return MultipleScatteringWork::default();
        }
        let rays = u64::from(self.config.size[0])
            * u64::from(self.config.size[1])
            * u64::from(self.config.directions);
        MultipleScatteringWork {
            rays,
            integration_steps: rays * u64::from(self.config.steps),
            sunlight_lookups: rays * u64::from(self.config.steps + 1),
            passes: 1,
            ..Default::default()
        }
    }
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        optical_revision: u64,
        steps: &mut u64,
    ) -> Result<MultipleScatteringWork, MultipleScatteringError> {
        self.timer.begin();
        let work = self.pending_work(optical_revision);
        if work.passes == 0 {
            return Ok(work);
        }
        if work.integration_steps > *steps {
            return Err(MultipleScatteringError::WorkBudget);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("bounded isotropic multiple scattering"),
                timestamp_writes: self.timer.compute(1),
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(
                self.config.size[0].div_ceil(4),
                self.config.size[1].div_ceil(4),
                1,
            );
        }
        self.timer.finish(encoder);
        self.last = Some(optical_revision);
        *steps -= work.integration_steps;
        Ok(work)
    }
}
