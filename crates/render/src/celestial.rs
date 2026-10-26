//! Prepared procedural celestial bodies, without external image dependencies.
use crate::{
    MSAA_SAMPLES,
    atmosphere::AtmosphereGpu,
    terrain_gpu::{Camera, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
#[derive(Clone, Copy, Debug)]
pub struct CelestialConfig {
    pub stars: u32,
    pub sun_style: u32,
    pub moon_style: u32,
    pub star_style: u32,
    pub sun_radius: f32,
    pub moon_radius: f32,
}
impl Default for CelestialConfig {
    fn default() -> Self {
        Self {
            stars: 2048,
            sun_style: 0,
            moon_style: 0,
            star_style: 0,
            sun_radius: 0.025,
            moon_radius: 0.03,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CelestialState {
    pub camera: Camera,
    pub sun: [f32; 3],
    pub moon: [f32; 3],
    pub rotation_radians: f32,
    pub time_seconds: f32,
    pub moon_phase: u8,
    pub sun_enabled: bool,
    pub moon_radiance: f32,
    pub star_radiance: f32,
    pub altitude_km: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CelestialError {
    InvalidConfig,
    InvalidState,
    UploadBudget,
}
impl std::fmt::Display for CelestialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CelestialError {}
#[derive(Clone, Copy, Debug, Default)]
pub struct CelestialWork {
    pub upload_bytes: u64,
    pub catalog_bytes: u64,
    pub vertices: u32,
    pub draws: u32,
    pub pipeline_compilations: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    camera: [[f32; 4]; 4],
    sun: [f32; 4],
    moon: [f32; 4],
    clock: [f32; 4],
    appearance: [f32; 4],
    styles: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Star {
    direction: [f32; 4],
    light: [f32; 4],
}
pub struct CelestialGpu {
    config: CelestialConfig,
    uniform: wgpu::Buffer,
    _catalog: wgpu::Buffer,
    bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    last: Option<CelestialState>,
    work: CelestialWork,
}
fn random(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    (*seed >> 8) as f32 / 16777216.
}
fn normalized(v: [f32; 3]) -> Option<[f32; 3]> {
    let length = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    (length.is_finite() && length > 1e-6).then(|| v.map(|v| v / length))
}
impl CelestialGpu {
    fn canonical_state(&self, mut state: CelestialState) -> CelestialState {
        if self.config.stars == 0 || state.sun[1] >= -0.08 || state.star_radiance == 0.0 {
            state.time_seconds = 0.0;
            state.rotation_radians = 0.0;
        }
        state
    }
    pub fn needs_redraw(&self, state: CelestialState) -> bool {
        self.last != Some(self.canonical_state(state))
    }
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atmosphere: &AtmosphereGpu,
        config: CelestialConfig,
        budget: &mut UploadBudget,
    ) -> Result<Self, CelestialError> {
        Self::new_with_samples(device, queue, atmosphere, config, budget, MSAA_SAMPLES)
    }
    pub fn new_with_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atmosphere: &AtmosphereGpu,
        config: CelestialConfig,
        budget: &mut UploadBudget,
        sample_count: u32,
    ) -> Result<Self, CelestialError> {
        if !matches!(sample_count, 1 | 4) {
            return Err(CelestialError::InvalidConfig);
        }
        if config.stars > 8192
            || config.sun_style > 2
            || config.moon_style > 1
            || config.star_style > 2
            || [config.sun_radius, config.moon_radius]
                .iter()
                .any(|v| !v.is_finite() || !(0.005..=0.08).contains(v))
        {
            return Err(CelestialError::InvalidConfig);
        }
        let bytes = u64::from(config.stars.max(1)) * 32;
        if budget.remaining_bytes < bytes {
            return Err(CelestialError::UploadBudget);
        }
        let mut seed = 0x574f_524c;
        let catalog = (0..config.stars.max(1))
            .map(|_| {
                let y = random(&mut seed) * 2. - 1.;
                let phi = random(&mut seed) * std::f32::consts::TAU;
                let r = (1. - y * y).sqrt();
                Star {
                    direction: [
                        r * phi.cos(),
                        y,
                        r * phi.sin(),
                        0.0025 + random(&mut seed) * 0.0025,
                    ],
                    light: [
                        0.15 + random(&mut seed).powi(4) * 2.,
                        random(&mut seed) * std::f32::consts::TAU,
                        random(&mut seed),
                        0.7 + random(&mut seed) * 2.,
                    ],
                }
            })
            .collect::<Vec<_>>();
        let catalog_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("immutable procedural star directions"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&catalog_buffer, 0, bytemuck::cast_slice(&catalog));
        budget.remaining_bytes -= bytes;
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("celestial view and calendar"),
            size: 144,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let entries = [
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ];
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("same atmosphere for celestial optics"),
            entries: &entries,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("prepared celestial resources"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: catalog_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(atmosphere.transmittance_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: atmosphere.lighting_buffer().as_entire_binding(),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("original celestial disk and star shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("celestial.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("celestial HDR pipeline"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("MSAA celestial bodies behind real depth"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Equal,
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
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        Ok(Self {
            config,
            uniform,
            _catalog: catalog_buffer,
            bind,
            pipeline,
            last: None,
            work: CelestialWork {
                catalog_bytes: bytes,
                pipeline_compilations: 1,
                ..Default::default()
            },
        })
    }
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        state: CelestialState,
        budget: &mut UploadBudget,
    ) -> Result<CelestialWork, CelestialError> {
        let sun = normalized(state.sun).ok_or(CelestialError::InvalidState)?;
        let moon = normalized(state.moon).ok_or(CelestialError::InvalidState)?;
        if state.camera.inverse_view_projection().is_none()
            || state.moon_phase > 7
            || [
                state.rotation_radians,
                state.time_seconds,
                state.moon_radiance,
                state.star_radiance,
                state.altitude_km,
            ]
            .iter()
            .any(|v| !v.is_finite())
            || !(0.0..=10.).contains(&state.moon_radiance)
            || !(0.0..=10.).contains(&state.star_radiance)
            || !(0.0..100.).contains(&state.altitude_km)
        {
            return Err(CelestialError::InvalidState);
        }
        let state = self.canonical_state(state);
        if self.last == Some(state) {
            return Ok(CelestialWork::default());
        }
        if budget.remaining_bytes < 144 {
            return Err(CelestialError::UploadBudget);
        }
        let p = Parameters {
            camera: state.camera.view_projection,
            sun: [sun[0], sun[1], sun[2], f32::from(state.sun_enabled)],
            moon: [moon[0], moon[1], moon[2], state.moon_radiance],
            clock: [
                state.rotation_radians.cos(),
                state.rotation_radians.sin(),
                state.time_seconds,
                state.altitude_km,
            ],
            appearance: [
                self.config.sun_radius,
                self.config.moon_radius,
                f32::from(state.moon_phase) * std::f32::consts::TAU / 8.,
                state.star_radiance,
            ],
            styles: [
                self.config.sun_style,
                self.config.moon_style,
                self.config.star_style,
                self.config.stars,
            ],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&p));
        budget.remaining_bytes -= 144;
        self.last = Some(state);
        self.work.upload_bytes += 144;
        Ok(CelestialWork {
            upload_bytes: 144,
            ..Default::default()
        })
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        if self.last.is_none() {
            return 0;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..4, 0..self.config.stars + 2);
        1
    }
    pub fn take_work(&mut self) -> CelestialWork {
        std::mem::take(&mut self.work)
    }
    pub fn vertex_count(&self) -> u32 {
        (self.config.stars + 2) * 4
    }
}
