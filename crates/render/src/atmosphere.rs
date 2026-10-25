//! Physical transmittance and bounded single-scattering LUT foundation for M10.
//! No asset source or donor renderer code is used. Multiple scattering, the sky
//! draw, clouds and water integration are separate work; this is not full M10.

use crate::terrain_gpu::UploadBudget;
use bytemuck::{Pod, Zeroable};

#[derive(Clone, Copy, Debug)]
pub struct AtmosphereConfig {
    pub transmittance_size: [u32; 2],
    pub sky_size: [u32; 2],
    pub transmittance_steps: u32,
    pub sky_steps: u32,
}
impl Default for AtmosphereConfig {
    fn default() -> Self {
        Self {
            transmittance_size: [256, 64],
            sky_size: [192, 108],
            transmittance_steps: 64,
            sky_steps: 48,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmosphereState {
    pub sun_direction: [f32; 3],
    pub altitude_km: f32,
}
impl Default for AtmosphereState {
    fn default() -> Self {
        Self {
            sun_direction: [0.35, 0.8, 0.2],
            altitude_km: 0.072,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtmosphereError {
    InvalidConfig,
    InvalidState,
    UploadBudget,
    WorkBudget,
}
impl std::fmt::Display for AtmosphereError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for AtmosphereError {}

#[derive(Clone, Copy, Debug, Default)]
pub struct AtmosphereWork {
    pub lut_pixels: u64,
    pub integration_steps: u64,
    pub passes: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    radii: [f32; 4],
    rayleigh: [f32; 4],
    mie: [f32; 4],
    ozone: [f32; 4],
    sun_eye: [f32; 4],
    integration: [u32; 4],
}
impl Parameters {
    fn new(config: AtmosphereConfig, state: AtmosphereState) -> Result<Self, AtmosphereError> {
        if state.sun_direction.iter().any(|v| !v.is_finite())
            || !state.altitude_km.is_finite()
            || !(0.0..100.0).contains(&state.altitude_km)
        {
            return Err(AtmosphereError::InvalidState);
        }
        let length = state
            .sun_direction
            .iter()
            .map(|v| v * v)
            .sum::<f32>()
            .sqrt();
        if !length.is_finite() || length < 1e-6 {
            return Err(AtmosphereError::InvalidState);
        }
        Ok(Self {
            radii: [6360.0, 6460.0, 8.0, 1.2],
            rayleigh: [0.005802, 0.013558, 0.0331, 0.0],
            mie: [0.003996, 0.000444, 0.8, 20.0],
            ozone: [0.000650, 0.001881, 0.000085, 25.0],
            sun_eye: [
                state.sun_direction[0] / length,
                state.sun_direction[1] / length,
                state.sun_direction[2] / length,
                state.altitude_km,
            ],
            integration: [config.transmittance_steps, config.sky_steps, 0, 0],
        })
    }
}

pub struct AtmosphereGpu {
    config: AtmosphereConfig,
    state: AtmosphereState,
    parameters: wgpu::Buffer,
    transmittance: wgpu::Texture,
    sky: wgpu::Texture,
    transmittance_view: wgpu::TextureView,
    sky_view: wgpu::TextureView,
    transmittance_pipeline: wgpu::ComputePipeline,
    sky_pipeline: wgpu::ComputePipeline,
    transmittance_bind: wgpu::BindGroup,
    sky_bind: wgpu::BindGroup,
    transmittance_dirty: bool,
    sky_dirty: bool,
}

impl AtmosphereGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: AtmosphereConfig,
        state: AtmosphereState,
        budget: &mut UploadBudget,
    ) -> Result<Self, AtmosphereError> {
        if config
            .transmittance_size
            .into_iter()
            .chain(config.sky_size)
            .any(|v| v == 0 || v > device.limits().max_texture_dimension_2d)
            || !(1..=128).contains(&config.transmittance_steps)
            || !(1..=128).contains(&config.sky_steps)
        {
            return Err(AtmosphereError::InvalidConfig);
        }
        let data = Parameters::new(config, state)?;
        if budget.remaining_bytes < std::mem::size_of::<Parameters>() as u64 {
            return Err(AtmosphereError::UploadBudget);
        }
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("physical atmosphere parameters"),
            size: std::mem::size_of::<Parameters>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&parameters, 0, bytemuck::bytes_of(&data));
        budget.remaining_bytes -= std::mem::size_of::<Parameters>() as u64;
        let texture = |label, size: [u32; 2]| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
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
        let transmittance = texture("atmosphere transmittance LUT", config.transmittance_size);
        let sky = texture("atmosphere sky-view LUT", config.sky_size);
        let transmittance_view = transmittance.create_view(&Default::default());
        let sky_view = sky.create_view(&Default::default());
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
        let uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let input = wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let transmittance_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("atmosphere transmittance graph"),
                entries: &[uniform, storage(1)],
            });
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atmosphere sky graph"),
            entries: &[uniform, input, storage(3)],
        });
        let transmittance_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere transmittance output"),
            layout: &transmittance_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&transmittance_view),
                },
            ],
        });
        let sky_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere transmittance to sky"),
            layout: &sky_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&transmittance_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&sky_view),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded atmospheric scattering"),
            source: wgpu::ShaderSource::Wgsl(include_str!("atmosphere.wgsl").into()),
        });
        let pipeline = |entry, layout: &wgpu::BindGroupLayout| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[layout],
                push_constant_ranges: &[],
            });
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
            state,
            parameters,
            transmittance,
            sky,
            transmittance_view,
            sky_view,
            transmittance_pipeline: pipeline("build_transmittance", &transmittance_layout),
            sky_pipeline: pipeline("build_sky", &sky_layout),
            transmittance_bind,
            sky_bind,
            transmittance_dirty: true,
            sky_dirty: true,
        })
    }

    pub fn set_state(
        &mut self,
        queue: &wgpu::Queue,
        state: AtmosphereState,
        budget: &mut UploadBudget,
    ) -> Result<(), AtmosphereError> {
        let data = Parameters::new(self.config, state)?;
        if state == self.state {
            return Ok(());
        }
        let bytes = std::mem::size_of::<Parameters>() as u64;
        if budget.remaining_bytes < bytes {
            return Err(AtmosphereError::UploadBudget);
        }
        queue.write_buffer(&self.parameters, 0, bytemuck::bytes_of(&data));
        budget.remaining_bytes -= bytes;
        self.state = state;
        self.sky_dirty = true;
        Ok(())
    }

    pub fn pending_work(&self) -> AtmosphereWork {
        let pixels = |size: [u32; 2]| u64::from(size[0]) * u64::from(size[1]);
        let trans = if self.transmittance_dirty {
            pixels(self.config.transmittance_size)
        } else {
            0
        };
        let sky = if self.sky_dirty {
            pixels(self.config.sky_size)
        } else {
            0
        };
        AtmosphereWork {
            lut_pixels: trans + sky,
            integration_steps: trans * u64::from(self.config.transmittance_steps)
                + sky * u64::from(self.config.sky_steps),
            passes: u32::from(self.transmittance_dirty) + u32::from(self.sky_dirty),
        }
    }

    /// Rebuild after an encoded update was abandoned before queue submission.
    pub fn invalidate(&mut self) {
        self.transmittance_dirty = true;
        self.sky_dirty = true;
    }

    /// Refuse the whole update before encoding when its logical integration budget is exhausted.
    pub fn encode_update(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        remaining_steps: &mut u64,
    ) -> Result<AtmosphereWork, AtmosphereError> {
        let work = self.pending_work();
        if work.integration_steps > *remaining_steps {
            return Err(AtmosphereError::WorkBudget);
        }
        for (dirty, pipeline, bind, size) in [
            (
                self.transmittance_dirty,
                &self.transmittance_pipeline,
                &self.transmittance_bind,
                self.config.transmittance_size,
            ),
            (
                self.sky_dirty,
                &self.sky_pipeline,
                &self.sky_bind,
                self.config.sky_size,
            ),
        ] {
            if dirty {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("bounded atmosphere LUT update"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind, &[]);
                pass.dispatch_workgroups(size[0].div_ceil(8), size[1].div_ceil(8), 1);
            }
        }
        *remaining_steps -= work.integration_steps;
        self.transmittance_dirty = false;
        self.sky_dirty = false;
        Ok(work)
    }

    pub fn transmittance_texture(&self) -> &wgpu::Texture {
        &self.transmittance
    }
    pub fn sky_texture(&self) -> &wgpu::Texture {
        &self.sky
    }
    pub fn transmittance_view(&self) -> &wgpu::TextureView {
        &self.transmittance_view
    }
    pub fn sky_view(&self) -> &wgpu::TextureView {
        &self.sky_view
    }
}

/// Draws the same physical sky-view LUT into the MSAA4 HDR terrain graph.
/// Construct during loading; the LUT owner encodes optical updates before this draw.
pub struct AtmosphereSkyGpu {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    camera: wgpu::Buffer,
    last_camera: Option<crate::terrain_gpu::Camera>,
}
impl AtmosphereSkyGpu {
    pub fn new(device: &wgpu::Device, sky_view: &wgpu::TextureView) -> Self {
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atmosphere camera inverse"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("continuous physical sky"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atmosphere sky draw"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("physical sky radiance and camera"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(sky_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: camera.as_entire_binding(),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("physical sky pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shared LUT sky radiance"),
            source: wgpu::ShaderSource::Wgsl(include_str!("atmosphere_sky.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("physical atmosphere MSAA4 HDR"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: crate::MSAA_SAMPLES,
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
        Self {
            pipeline,
            bind,
            camera,
            last_camera: None,
        }
    }
    pub fn update_camera(
        &mut self,
        queue: &wgpu::Queue,
        camera: &crate::terrain_gpu::Camera,
        budget: &mut UploadBudget,
    ) -> Result<u64, AtmosphereError> {
        if self.last_camera == Some(*camera) {
            return Ok(0);
        }
        let inverse = inverse(camera.view_projection).ok_or(AtmosphereError::InvalidState)?;
        if budget.remaining_bytes < 64 {
            return Err(AtmosphereError::UploadBudget);
        }
        queue.write_buffer(&self.camera, 0, bytemuck::cast_slice(&inverse));
        budget.remaining_bytes -= 64;
        self.last_camera = Some(*camera);
        Ok(64)
    }
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
fn inverse(matrix: [[f32; 4]; 4]) -> Option<[[f32; 4]; 4]> {
    let mut rows = [[0.0f64; 8]; 4];
    for (row_index, row) in rows.iter_mut().enumerate() {
        for (column_index, column) in matrix.iter().enumerate() {
            row[column_index] = f64::from(column[row_index]);
        }
        row[row_index + 4] = 1.0;
    }
    for column in 0..4 {
        let pivot =
            (column..4).max_by(|a, b| rows[*a][column].abs().total_cmp(&rows[*b][column].abs()))?;
        if !rows[pivot][column].is_finite() || rows[pivot][column].abs() < 1e-20 {
            return None;
        }
        rows.swap(column, pivot);
        let divisor = rows[column][column];
        for value in &mut rows[column] {
            *value /= divisor;
        }
        for row in 0..4 {
            if row != column {
                let scale = rows[row][column];
                for item in 0..8 {
                    rows[row][item] -= scale * rows[column][item];
                }
            }
        }
    }
    let inverse =
        std::array::from_fn(|column| std::array::from_fn(|row| rows[row][column + 4] as f32));
    if inverse.iter().flatten().any(|v| !v.is_finite()) {
        None
    } else {
        Some(inverse)
    }
}

/// Independent f64 midpoint integration for the GPU transmittance oracle.
/// Returns zero when the solid planet blocks the ray. Distances are kilometres.
pub fn reference_transmittance(altitude_km: f64, cosine_zenith: f64, steps: u32) -> [f64; 3] {
    assert!(
        (0.0..=100.0).contains(&altitude_km) && (-1.0..=1.0).contains(&cosine_zenith) && steps > 0
    );
    let radius = 6360.0 + altitude_km;
    let b = radius * cosine_zenith;
    let ground = b * b - altitude_km * (12720.0 + altitude_km);
    if b < 0.0 && ground >= 0.0 {
        return [0.0; 3];
    }
    let distance = -b
        + (b * b - (radius - 6460.0) * (radius + 6460.0))
            .max(0.0)
            .sqrt();
    let step = distance / f64::from(steps);
    let mut depth = [0.0; 3];
    for index in 0..steps {
        let t = (f64::from(index) + 0.5) * step;
        let height = (radius * radius + t * t + 2.0 * b * t).sqrt() - 6360.0;
        let rayleigh = (-height.max(0.0) / 8.0).exp();
        let mie = (-height.max(0.0) / 1.2).exp();
        let ozone = (1.0 - (height.max(0.0) - 25.0).abs() / 15.0).max(0.0);
        for channel in 0..3 {
            depth[channel] += ([0.005802, 0.013558, 0.0331][channel] * rayleigh
                + 0.00444 * mie
                + [0.000650, 0.001881, 0.000085][channel] * ozone)
                * step;
        }
    }
    depth.map(|v| (-v).exp())
}
