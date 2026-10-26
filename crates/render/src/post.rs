//! Histogram exposure, cached bloom pyramid and final HDR transfer for M10.
use crate::{OutputTransfer, terrain_gpu::UploadBudget};
use bytemuck::{Pod, Zeroable};

#[derive(Clone, Copy, Debug)]
pub enum Exposure {
    Manual(f32),
    Automatic {
        key: f32,
        min_ev: f32,
        max_ev: f32,
        brighten_seconds: f32,
        darken_seconds: f32,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct BloomConfig {
    pub threshold: f32,
    pub knee: f32,
    pub intensity: f32,
    pub levels: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct PostConfig {
    pub exposure: Exposure,
    pub bloom: Option<BloomConfig>,
    pub histogram_stride: u32,
}
impl Default for PostConfig {
    fn default() -> Self {
        Self {
            exposure: Exposure::Automatic {
                key: 0.18,
                min_ev: -8.0,
                max_ev: 8.0,
                brighten_seconds: 1.5,
                darken_seconds: 0.5,
            },
            bloom: Some(BloomConfig {
                threshold: 1.0,
                knee: 0.5,
                intensity: 0.06,
                levels: 5,
            }),
            histogram_stride: 4,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostError {
    InvalidConfig,
    UploadBudget,
    WorkBudget,
}
impl std::fmt::Display for PostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PostError {}
#[derive(Clone, Copy, Debug, Default)]
pub struct PostWork {
    pub histogram_samples: u64,
    pub bloom_texels: u64,
    pub tone_pixels: u64,
    pub passes: u32,
    pub draws: u32,
}
impl PostWork {
    pub fn pixels(self) -> u64 {
        self.histogram_samples + self.bloom_texels + self.tone_pixels
    }
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    exposure: [f32; 4],
    adaptation: [f32; 4],
    bloom: [f32; 4],
    image: [u32; 4],
}
struct AutoExposure {
    layout: wgpu::BindGroupLayout,
    bind: wgpu::BindGroup,
    histogram: wgpu::Buffer,
    reset: wgpu::ComputePipeline,
    measure: wgpu::ComputePipeline,
    adapt: wgpu::ComputePipeline,
}
struct BloomLevel {
    bind: wgpu::BindGroup,
    width: u32,
    height: u32,
}
struct Bloom {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    levels: Vec<BloomLevel>,
}
pub struct PostGpu {
    config: PostConfig,
    parameters: Parameters,
    output_extent: [u32; 2],
    uniform: wgpu::Buffer,
    state: wgpu::Buffer,
    auto: Option<AutoExposure>,
    bloom: Option<Bloom>,
    bloom_layout: Option<wgpu::BindGroupLayout>,
    bloom_pipeline: Option<wgpu::ComputePipeline>,
    tone_layout: wgpu::BindGroupLayout,
    tone_bind: wgpu::BindGroup,
    tone: wgpu::RenderPipeline,
    last_source: Option<u64>,
    output_requested: bool,
    adaptation_remaining: f32,
    timer: crate::gpu_time::GpuPassTimer<12>,
}

fn buffer_entry(binding: u32, uniform: bool, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE | wgpu::ShaderStages::FRAGMENT,
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
fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
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
fn texture_binding(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}
fn tone_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &wgpu::TextureView,
    state: &wgpu::Buffer,
    uniform: &wgpu::Buffer,
    bloom: Option<&Bloom>,
) -> wgpu::BindGroup {
    let mut entries = vec![
        texture_binding(0, source),
        binding(1, state),
        binding(2, uniform),
    ];
    if let Some(bloom) = bloom {
        entries.push(texture_binding(3, &bloom.view));
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("HDR exposure bloom and tone"),
        layout,
        entries: &entries,
    })
}
fn exposure_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &wgpu::TextureView,
    state: &wgpu::Buffer,
    uniform: &wgpu::Buffer,
    histogram: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("HDR histogram and persistent exposure"),
        layout,
        entries: &[
            texture_binding(0, source),
            binding(1, histogram),
            binding(2, state),
            binding(3, uniform),
        ],
    })
}
fn levels(width: u32, height: u32, config: BloomConfig) -> u32 {
    config
        .levels
        .min(width.div_ceil(2).max(height.div_ceil(2)).ilog2() + 1)
}
#[allow(clippy::too_many_arguments)]
fn create_bloom(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::TextureView,
    width: u32,
    height: u32,
    config: BloomConfig,
    layout: &wgpu::BindGroupLayout,
) -> Bloom {
    let count = levels(width, height, config);
    let width = width.div_ceil(2);
    let height = height.div_ceil(2);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bounded HDR bloom pyramid"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let views = (0..count)
        .map(|level| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(count as usize);
    for level in 0..count {
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bloom extraction parameters"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &params,
            0,
            bytemuck::cast_slice(&[
                config.threshold,
                config.knee,
                if level == 0 { 1.0 } else { 0.0 },
                0.0,
            ]),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom neighboring mip inputs"),
            layout,
            entries: &[
                texture_binding(
                    0,
                    if level == 0 {
                        source
                    } else {
                        &views[level as usize - 1]
                    },
                ),
                texture_binding(1, &views[level as usize]),
                binding(2, &params),
            ],
        });
        result.push(BloomLevel {
            bind,
            width: (width >> level).max(1),
            height: (height >> level).max(1),
        });
    }
    let view = texture.create_view(&Default::default());
    Bloom {
        _texture: texture,
        view,
        levels: result,
    }
}

impl PostGpu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::TextureView,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        transfer: OutputTransfer,
        config: PostConfig,
        budget: &mut UploadBudget,
    ) -> Result<Self, PostError> {
        if width == 0
            || height == 0
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
            || !(1..=16).contains(&config.histogram_stride)
            || transfer == OutputTransfer::ShaderSrgb && format.is_srgb()
        {
            return Err(PostError::InvalidConfig);
        }
        let exposure = match config.exposure {
            Exposure::Manual(value) => {
                if !value.is_finite() || !(0.0..=256.0).contains(&value) {
                    return Err(PostError::InvalidConfig);
                }
                [value, 0.18, -8.0, 8.0]
            }
            Exposure::Automatic {
                key,
                min_ev,
                max_ev,
                brighten_seconds,
                darken_seconds,
            } => {
                if [key, min_ev, max_ev, brighten_seconds, darken_seconds]
                    .iter()
                    .any(|v| !v.is_finite())
                    || key <= 0.0
                    || min_ev < -16.0
                    || max_ev > 16.0
                    || min_ev > max_ev
                    || brighten_seconds < 0.01
                    || darken_seconds < 0.01
                {
                    return Err(PostError::InvalidConfig);
                }
                [-1.0, key, min_ev, max_ev]
            }
        };
        if let Some(bloom) = config.bloom
            && ([bloom.threshold, bloom.knee, bloom.intensity]
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0)
                || !(1..=6).contains(&bloom.levels)
                || bloom.intensity > 1.0)
        {
            return Err(PostError::InvalidConfig);
        }
        let upload = 80
            + config
                .bloom
                .map_or(0, |bloom| u64::from(levels(width, height, bloom)) * 16);
        if budget.remaining_bytes < upload {
            return Err(PostError::UploadBudget);
        }
        let mut adaptation = match config.exposure {
            Exposure::Automatic {
                brighten_seconds,
                darken_seconds,
                ..
            } => [1.0 / 60.0, brighten_seconds, darken_seconds, 0.0],
            Exposure::Manual(_) => [0.0; 4],
        };
        adaptation[3] = 1.;
        let parameters = Parameters {
            exposure,
            adaptation,
            bloom: config
                .bloom
                .map_or([0.0; 4], |b| [b.threshold, b.knee, b.intensity, 0.0]),
            image: [width, height, config.histogram_stride, 0],
        };
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("HDR post settings"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let state = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("persistent HDR exposure"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let initial = match config.exposure {
            Exposure::Manual(value) => value,
            _ => 1.0,
        };
        queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&parameters));
        queue.write_buffer(
            &state,
            0,
            bytemuck::cast_slice(&[initial, initial, 0.0, 0.0]),
        );
        let auto = if matches!(config.exposure, Exposure::Automatic { .. }) {
            let histogram = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("256 log luminance bins"),
                size: 1024,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("histogram exposure layout"),
                entries: &[
                    texture_entry(0),
                    buffer_entry(1, false, false),
                    buffer_entry(2, false, false),
                    buffer_entry(3, true, true),
                ],
            });
            let bind = exposure_bind(device, &layout, source, &state, &uniform, &histogram);
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("histogram exposure pipeline"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("histogram slow exposure adaptation"),
                source: wgpu::ShaderSource::Wgsl(include_str!("post.wgsl").into()),
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
            Some(AutoExposure {
                layout,
                bind,
                histogram,
                reset: pipeline("reset"),
                measure: pipeline("histogram_luminance"),
                adapt: pipeline("adapt"),
            })
        } else {
            None
        };
        let bloom_layout = config.bloom.map(|_| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bounded bloom layout"),
                entries: &[
                    texture_entry(0),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba16Float,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    buffer_entry(2, true, true),
                ],
            })
        });
        let bloom_pipeline = bloom_layout.as_ref().map(|layout| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("bounded bloom filter"),
                source: wgpu::ShaderSource::Wgsl(include_str!("bloom.wgsl").into()),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("bloom compute pipeline"),
                bind_group_layouts: &[layout],
                push_constant_ranges: &[],
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("HDR bloom pyramid"),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        });
        let bloom = config
            .bloom
            .zip(bloom_layout.as_ref())
            .map(|(config, layout)| {
                create_bloom(device, queue, source, width, height, config, layout)
            });
        let mut entries = vec![
            texture_entry(0),
            buffer_entry(1, false, true),
            buffer_entry(2, true, true),
        ];
        if bloom.is_some() {
            entries.push(texture_entry(3));
        }
        let tone_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("HDR tone layout"),
            entries: &entries,
        });
        let tone_bind = tone_bind(
            device,
            &tone_layout,
            source,
            &state,
            &uniform,
            bloom.as_ref(),
        );
        let mut source = include_str!("post_tone.wgsl").to_owned();
        if bloom.is_some() {
            source = source
                .replace("// BLOOM_DECLARATIONS", BLOOM_SAMPLING)
                .replace(
                    "// BLOOM_COMPOSITE",
                    "color+=sample_bloom(input.uv)*params.bloom.z;",
                );
        }
        if transfer == OutputTransfer::ShaderSrgb {
            source = source.replace(
                "return vec4(mapped,1.0);",
                "return vec4(linear_to_srgb(mapped),1.0);",
            );
        }
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("HDR tone and selected output transfer"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("HDR tone pipeline"),
            bind_group_layouts: &[&tone_layout],
            push_constant_ranges: &[],
        });
        let tone = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("HDR final post"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        budget.remaining_bytes -= upload;
        Ok(Self {
            config,
            parameters,
            output_extent: [width, height],
            uniform,
            state,
            auto,
            bloom,
            bloom_layout,
            bloom_pipeline,
            tone_layout,
            tone_bind,
            tone,
            last_source: None,
            output_requested: false,
            adaptation_remaining: 0.0,
            timer: crate::gpu_time::GpuPassTimer::new(device, queue, "HDR post pass timings"),
        })
    }

    pub fn set_delta(
        &mut self,
        queue: &wgpu::Queue,
        seconds: f32,
        budget: &mut UploadBudget,
    ) -> Result<u64, PostError> {
        if !seconds.is_finite() || !(0.0..=0.1).contains(&seconds) {
            return Err(PostError::InvalidConfig);
        }
        if self.auto.is_none() || self.parameters.adaptation[0] == seconds {
            return Ok(0);
        }
        if budget.remaining_bytes < 64 {
            return Err(PostError::UploadBudget);
        }
        self.parameters.adaptation[0] = seconds;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.parameters));
        budget.remaining_bytes -= 64;
        Ok(64)
    }
    /// Display compensation leaves the measured luminance and adaptation state
    /// intact. A cached scene needs only a new tone pass after this64B update.
    pub fn compensation(&self) -> f32 {
        self.parameters.adaptation[3]
    }
    pub fn set_compensation(
        &mut self,
        queue: &wgpu::Queue,
        gain: f32,
        budget: &mut UploadBudget,
    ) -> Result<u64, PostError> {
        if !gain.is_finite() || !(0.25..=2.).contains(&gain) {
            return Err(PostError::InvalidConfig);
        }
        if self.compensation() == gain {
            return Ok(0);
        }
        if budget.remaining_bytes < 64 {
            return Err(PostError::UploadBudget);
        }
        self.parameters.adaptation[3] = gain;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.parameters));
        budget.remaining_bytes -= 64;
        self.output_requested = true;
        Ok(64)
    }
    pub fn needs_redraw(&self, source_revision: u64) -> bool {
        self.output_requested
            || self.last_source != Some(source_revision)
            || self.adaptation_remaining > 0.0
    }
    /// Present cached HDR to a newly acquired output without invalidating it.
    pub fn request_present(&mut self) {
        self.output_requested = true;
    }
    pub fn invalidate(&mut self) {
        self.last_source = None;
    }
    pub fn frame_graph(&self) -> crate::gpu_time::PassGraph<12> {
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
        self.timer.copy_into(encoder, destination, offset);
    }
    pub fn state_buffer(&self) -> &wgpu::Buffer {
        &self.state
    }
    pub fn pipeline_count(&self) -> u32 {
        1 + if self.auto.is_some() { 3 } else { 0 } + u32::from(self.bloom.is_some())
    }
    pub(crate) fn resize_upload_bytes(&self, width: u32, height: u32) -> u64 {
        64 + self
            .config
            .bloom
            .map_or(0, |b| u64::from(levels(width, height, b)) * 16)
    }

    /// Input rebinding on resize reuses every prepared pipeline.
    #[allow(clippy::too_many_arguments)]
    pub fn resize_input(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::TextureView,
        width: u32,
        height: u32,
        budget: &mut UploadBudget,
    ) -> Result<(), PostError> {
        if width == 0
            || height == 0
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            return Err(PostError::InvalidConfig);
        }
        let upload = self.resize_upload_bytes(width, height);
        if budget.remaining_bytes < upload {
            return Err(PostError::UploadBudget);
        }
        self.bloom = self
            .config
            .bloom
            .zip(self.bloom_layout.as_ref())
            .map(|(config, layout)| {
                create_bloom(device, queue, source, width, height, config, layout)
            });
        self.parameters.image[0] = width;
        self.parameters.image[1] = height;
        self.output_extent = [width, height];
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.parameters));
        self.tone_bind = tone_bind(
            device,
            &self.tone_layout,
            source,
            &self.state,
            &self.uniform,
            self.bloom.as_ref(),
        );
        if let Some(auto) = &mut self.auto {
            auto.bind = exposure_bind(
                device,
                &auto.layout,
                source,
                &self.state,
                &self.uniform,
                &auto.histogram,
            );
        }
        budget.remaining_bytes -= upload;
        self.invalidate();
        Ok(())
    }
    /// The final surface can retain native resolution while HDR inputs shrink.
    /// This changes only accounting/presentation dirtiness, never GPU resources.
    pub fn set_output_extent(&mut self, width: u32, height: u32) -> Result<(), PostError> {
        if width == 0 || height == 0 {
            return Err(PostError::InvalidConfig);
        }
        if self.output_extent != [width, height] {
            self.output_extent = [width, height];
            self.output_requested = true;
        }
        Ok(())
    }
    pub fn output_extent(&self) -> [u32; 2] {
        self.output_extent
    }
    pub fn pending_work(&self, source_revision: u64) -> PostWork {
        let changed = self.last_source != Some(source_revision);
        let automatic = self.auto.is_some();
        let histogram = if automatic && changed {
            u64::from(self.parameters.image[0].div_ceil(self.config.histogram_stride))
                * u64::from(self.parameters.image[1].div_ceil(self.config.histogram_stride))
        } else {
            0
        };
        let bloom = if changed {
            self.bloom.as_ref().map_or(0, |b| {
                b.levels
                    .iter()
                    .map(|l| u64::from(l.width) * u64::from(l.height))
                    .sum()
            })
        } else {
            0
        };
        let adapting = automatic && (changed || self.adaptation_remaining > 0.0);
        PostWork {
            histogram_samples: histogram,
            bloom_texels: bloom,
            tone_pixels: u64::from(self.output_extent[0]) * u64::from(self.output_extent[1]),
            passes: 1
                + if histogram > 0 { 2 } else { 0 }
                + u32::from(adapting)
                + if changed {
                    self.bloom.as_ref().map_or(0, |b| b.levels.len() as u32)
                } else {
                    0
                },
            draws: 1,
        }
    }
    pub fn has_cached_source(&self) -> bool {
        self.last_source.is_some()
    }
    /// Present the retained HDR only. Geometry admission can pause without
    /// advancing exposure, histogram, bloom or the retained source revision.
    pub fn present_cached(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        remaining_pixels: &mut u64,
    ) -> Result<PostWork, PostError> {
        if !self.has_cached_source() {
            return Err(PostError::InvalidConfig);
        }
        let pixels = u64::from(self.output_extent[0]) * u64::from(self.output_extent[1]);
        if pixels > *remaining_pixels {
            return Err(PostError::WorkBudget);
        }
        self.timer.begin();
        self.encode_output(encoder, output);
        self.timer.finish(encoder);
        self.output_requested = false;
        *remaining_pixels -= pixels;
        Ok(PostWork {
            tone_pixels: pixels,
            passes: 1,
            draws: 1,
            ..PostWork::default()
        })
    }
    fn encode_output(&self, encoder: &mut wgpu::CommandEncoder, output: &wgpu::TextureView) {
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("HDR exposure bloom tone output"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: self.timer.render(5),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.tone);
            pass.set_bind_group(0, &self.tone_bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        source_revision: u64,
        remaining_pixels: &mut u64,
    ) -> Result<PostWork, PostError> {
        let work = self.pending_work(source_revision);
        if work.pixels() > *remaining_pixels {
            return Err(PostError::WorkBudget);
        }
        self.timer.begin();
        let changed = self.last_source != Some(source_revision);
        if let Some(auto) = &self.auto {
            if changed {
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("clear exposure histogram"),
                        timestamp_writes: self.timer.compute(1),
                    });
                    pass.set_pipeline(&auto.reset);
                    pass.set_bind_group(0, &auto.bind, &[]);
                    pass.dispatch_workgroups(1, 1, 1);
                }
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("measure log luminance"),
                        timestamp_writes: self.timer.compute(2),
                    });
                    pass.set_pipeline(&auto.measure);
                    pass.set_bind_group(0, &auto.bind, &[]);
                    pass.dispatch_workgroups(
                        self.parameters.image[0]
                            .div_ceil(self.config.histogram_stride)
                            .div_ceil(8),
                        self.parameters.image[1]
                            .div_ceil(self.config.histogram_stride)
                            .div_ceil(8),
                        1,
                    );
                }
                self.adaptation_remaining =
                    8.0 * self.parameters.adaptation[1].max(self.parameters.adaptation[2]);
            }
            if self.adaptation_remaining > 0.0 {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("slow exposure adaptation"),
                    timestamp_writes: self.timer.compute(3),
                });
                pass.set_pipeline(&auto.adapt);
                pass.set_bind_group(0, &auto.bind, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
        }
        if changed && let Some(bloom) = &self.bloom {
            for level in &bloom.levels {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("bounded bloom mip"),
                    timestamp_writes: self.timer.compute(4),
                });
                pass.set_pipeline(self.bloom_pipeline.as_ref().expect("prepared bloom"));
                pass.set_bind_group(0, &level.bind, &[]);
                pass.dispatch_workgroups(level.width.div_ceil(8), level.height.div_ceil(8), 1);
            }
        }
        self.encode_output(encoder, output);
        self.timer.finish(encoder);
        debug_assert_eq!(self.timer.graph().count, work.passes);
        self.adaptation_remaining =
            (self.adaptation_remaining - self.parameters.adaptation[0]).max(0.0);
        self.last_source = Some(source_revision);
        self.output_requested = false;
        *remaining_pixels -= work.pixels();
        Ok(work)
    }
}

const BLOOM_SAMPLING: &str = r#"
@group(0) @binding(3) var bloom_pyramid:texture_2d<f32>;
fn sample_bloom(uv:vec2<f32>)->vec3<f32> {
    var result=vec3(0.0);let levels=textureNumLevels(bloom_pyramid);
    for(var level=0u;level<levels;level++){
        let size=vec2<i32>(textureDimensions(bloom_pyramid,level));let point=uv*vec2<f32>(size)-vec2(0.5);let low=vec2<i32>(floor(point));let weight=fract(point);
        let a=textureLoad(bloom_pyramid,clamp(low,vec2(0),size-vec2(1)),i32(level)).rgb;
        let b=textureLoad(bloom_pyramid,clamp(low+vec2(1,0),vec2(0),size-vec2(1)),i32(level)).rgb;
        let c=textureLoad(bloom_pyramid,clamp(low+vec2(0,1),vec2(0),size-vec2(1)),i32(level)).rgb;
        let d=textureLoad(bloom_pyramid,clamp(low+vec2(1,1),vec2(0),size-vec2(1)),i32(level)).rgb;
        result+=mix(mix(a,b,weight.x),mix(c,d,weight.x),weight.y);
    }
    return result/f32(levels);
}
"#;
