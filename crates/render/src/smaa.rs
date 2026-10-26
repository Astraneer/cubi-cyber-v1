//! Original bounded SMAA-family 1x reconstruction: contrast edges, pattern search,
//! analytic subpixel areas and neighborhood resolve. No SDK code or LUT assets.
use crate::{OutputTransfer, gpu_time::GpuPassTimer, terrain_gpu::UploadBudget};
use bytemuck::{Pod, Zeroable};
pub const SEARCH_LIMIT: u32 = 16;
pub const DIAGONAL_LIMIT: u32 = 8;
pub const AREA_SIDE: u32 = 51;
pub const AREA_ENTRIES: u32 = AREA_SIDE * AREA_SIDE;
const INPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const WEIGHTS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
#[derive(Clone, Copy, Debug)]
pub struct SmaaConfig {
    pub threshold: f32,
    pub local_contrast: f32,
    pub max_pixels: u64,
}
impl Default for SmaaConfig {
    fn default() -> Self {
        Self {
            threshold: 0.1,
            local_contrast: 2.,
            max_pixels: 16_777_216,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmaaError {
    InvalidConfig,
    UploadBudget,
}
impl std::fmt::Display for SmaaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SmaaError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmaaWork {
    pub pixels: u64,
    pub search_reads_bound: u64,
    pub area_lookups_bound: u64,
    pub passes: u32,
    pub draws: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    threshold: f32,
    contrast: f32,
    search: u32,
    diagonal: u32,
}
struct Targets {
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    edges: wgpu::Texture,
    edges_view: wgpu::TextureView,
    weights: wgpu::Texture,
    weights_view: wgpu::TextureView,
    edge_bind: wgpu::BindGroup,
    weight_bind: wgpu::BindGroup,
    blend_bind: wgpu::BindGroup,
}
pub struct SmaaGpu {
    config: SmaaConfig,
    extent: [u32; 2],
    max_dimension: u32,
    uniform: wgpu::Buffer,
    area: wgpu::Buffer,
    edge_pipeline: wgpu::RenderPipeline,
    weight_pipeline: wgpu::RenderPipeline,
    blend_pipeline: wgpu::RenderPipeline,
    targets: Targets,
}
/// Exact signed area of the declared affine contour. Search/corner rejection is
/// separate from this geometry; tests use an independent clipped-polygon oracle.
pub fn affine_areas(d0: u32, d1: u32, e0: i8, e1: i8) -> Option<[f64; 2]> {
    if d0 > SEARCH_LIMIT || d1 > SEARCH_LIMIT || !(-1..=1).contains(&e0) || !(-1..=1).contains(&e1)
    {
        return None;
    }
    let left = f64::from(e0) * 0.5;
    let difference = f64::from(e1 - e0) * 0.5;
    let span = f64::from(d0 + d1 + 1);
    let y0 = left + difference * f64::from(d0) / span;
    let y1 = left + difference * f64::from(d0 + 1) / span;
    fn positive(a: f64, b: f64) -> f64 {
        if a >= 0. && b >= 0. {
            (a + b) * 0.5
        } else if a <= 0. && b <= 0. {
            0.
        } else {
            let high = a.max(b);
            high * high / (2. * (b - a).abs())
        }
    }
    Some([positive(y0, y1), positive(-y0, -y1)])
}
/// Generated at loading time; this is numerical data, not an imported LUT asset.
pub fn area_table() -> Vec<[f32; 2]> {
    let mut table = Vec::with_capacity(AREA_ENTRIES as usize);
    for row in 0..AREA_SIDE {
        for column in 0..AREA_SIDE {
            let values = affine_areas(
                column % 17,
                row % 17,
                (column / 17) as i8 - 1,
                (row / 17) as i8 - 1,
            )
            .expect("bounded area table");
            table.push([values[0] as f32, values[1] as f32]);
        }
    }
    table
}
fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
fn buffer_entry(binding: u32, uniform: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
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
    }
}
fn pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
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
    })
}
fn target(
    device: &wgpu::Device,
    label: &str,
    extent: [u32; 2],
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: extent[0],
            height: extent[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}
impl SmaaGpu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        transfer: OutputTransfer,
        config: SmaaConfig,
        budget: &mut UploadBudget,
    ) -> Result<Self, SmaaError> {
        if !config.threshold.is_finite()
            || !(0.01..=0.5).contains(&config.threshold)
            || !config.local_contrast.is_finite()
            || !(1. ..=4.).contains(&config.local_contrast)
            || config.max_pixels == 0
            || transfer == OutputTransfer::ShaderSrgb && format.is_srgb()
        {
            return Err(SmaaError::InvalidConfig);
        }
        validate_extent(
            [width, height],
            device.limits().max_texture_dimension_2d,
            config.max_pixels,
        )?;
        let bytes = u64::from(AREA_ENTRIES) * 8 + 16;
        if budget.remaining_bytes < bytes {
            return Err(SmaaError::UploadBudget);
        }
        let areas = area_table();
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("SMAA contrast/search configuration"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let area = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("original analytic SMAA area table"),
            size: u64::from(AREA_ENTRIES) * 8,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &uniform,
            0,
            bytemuck::bytes_of(&Parameters {
                threshold: config.threshold,
                contrast: config.local_contrast,
                search: SEARCH_LIMIT,
                diagonal: DIAGONAL_LIMIT,
            }),
        );
        queue.write_buffer(&area, 0, bytemuck::cast_slice(&areas));
        let edge_pipeline = pipeline(
            device,
            "SMAA perceptual contrast edges",
            include_str!("smaa_edges.wgsl"),
            &[texture_entry(0), buffer_entry(1, true)],
            wgpu::TextureFormat::Rg8Unorm,
        );
        let weight_pipeline = pipeline(
            device,
            "SMAA bounded patterns and exact area",
            include_str!("smaa_weights.wgsl"),
            &[
                texture_entry(0),
                buffer_entry(1, false),
                buffer_entry(2, true),
            ],
            WEIGHTS_FORMAT,
        );
        let mut blend_source = include_str!("smaa_blend.wgsl").to_owned();
        if transfer == OutputTransfer::ShaderSrgb {
            blend_source = blend_source.replace(
                "return vec4(result,1.0);",
                "return vec4(linear_to_srgb(result),1.0);",
            );
        }
        let blend_pipeline = pipeline(
            device,
            "SMAA linear neighborhood and final transfer",
            &blend_source,
            &[texture_entry(0), texture_entry(1)],
            format,
        );
        let targets = Targets::new(
            device,
            [width, height],
            &uniform,
            &area,
            &edge_pipeline,
            &weight_pipeline,
            &blend_pipeline,
        );
        budget.remaining_bytes -= bytes;
        Ok(Self {
            config,
            extent: [width, height],
            max_dimension: device.limits().max_texture_dimension_2d,
            uniform,
            area,
            edge_pipeline,
            weight_pipeline,
            blend_pipeline,
            targets,
        })
    }
    pub fn input_view(&self) -> &wgpu::TextureView {
        &self.targets.color_view
    }
    pub fn input_texture(&self) -> &wgpu::Texture {
        &self.targets.color
    }
    pub fn edge_texture(&self) -> &wgpu::Texture {
        &self.targets.edges
    }
    pub fn weight_texture(&self) -> &wgpu::Texture {
        &self.targets.weights
    }
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
    pub fn reserved_bytes(&self) -> u64 {
        u64::from(self.extent[0]) * u64::from(self.extent[1]) * 18
            + u64::from(AREA_ENTRIES) * 8
            + 16
    }
    pub fn config(&self) -> SmaaConfig {
        self.config
    }
    /// Only targets/bindings change. The analytic table and all three prepared
    /// pipelines survive both native resize and independent HDR render scaling.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<bool, SmaaError> {
        validate_extent([width, height], self.max_dimension, self.config.max_pixels)?;
        if self.extent == [width, height] {
            return Ok(false);
        }
        self.targets = Targets::new(
            device,
            [width, height],
            &self.uniform,
            &self.area,
            &self.edge_pipeline,
            &self.weight_pipeline,
            &self.blend_pipeline,
        );
        self.extent = [width, height];
        Ok(true)
    }
    pub fn pending_work(&self) -> SmaaWork {
        let pixels = u64::from(self.extent[0]) * u64::from(self.extent[1]);
        SmaaWork {
            pixels: pixels * 3,
            search_reads_bound: pixels * u64::from(4 * SEARCH_LIMIT + 28 * DIAGONAL_LIMIT + 16),
            area_lookups_bound: pixels * 2,
            passes: 3,
            draws: 3,
        }
    }
    /// The Post owner preflights the entire histogram/bloom/tone/SMAA pixel work
    /// before encoding anything. This function performs no allocations/uploads.
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        timer: Option<&GpuPassTimer<16>>,
    ) -> SmaaWork {
        for (kind, label, target, pipeline, bind) in [
            (
                6,
                "SMAA edge classification",
                &self.targets.edges_view,
                &self.edge_pipeline,
                &self.targets.edge_bind,
            ),
            (
                7,
                "SMAA pattern/area weights",
                &self.targets.weights_view,
                &self.weight_pipeline,
                &self.targets.weight_bind,
            ),
            (
                8,
                "SMAA neighborhood resolve",
                output,
                &self.blend_pipeline,
                &self.targets.blend_bind,
            ),
        ] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: timer.and_then(|timer| timer.render(kind)),
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.pending_work()
    }
}
fn validate_extent(extent: [u32; 2], limit: u32, max_pixels: u64) -> Result<(), SmaaError> {
    if extent.iter().any(|v| *v == 0 || *v > limit)
        || u64::from(extent[0]) * u64::from(extent[1]) > max_pixels
    {
        Err(SmaaError::InvalidConfig)
    } else {
        Ok(())
    }
}
impl Targets {
    fn new(
        device: &wgpu::Device,
        extent: [u32; 2],
        uniform: &wgpu::Buffer,
        area: &wgpu::Buffer,
        edge: &wgpu::RenderPipeline,
        weight: &wgpu::RenderPipeline,
        blend: &wgpu::RenderPipeline,
    ) -> Self {
        let (color, color_view) = target(device, "SMAA physical linear LDR", extent, INPUT_FORMAT);
        let (edges, edges_view) = target(
            device,
            "SMAA classified edges",
            extent,
            wgpu::TextureFormat::Rg8Unorm,
        );
        let (weights, weights_view) = target(device, "SMAA area weights", extent, WEIGHTS_FORMAT);
        let edge_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("SMAA edge inputs"),
            layout: &edge.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let weight_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("SMAA search inputs"),
            layout: &weight.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&edges_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: area.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let blend_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("SMAA neighborhood inputs"),
            layout: &blend.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&weights_view),
                },
            ],
        });
        Self {
            color,
            color_view,
            edges,
            edges_view,
            weights,
            weights_view,
            edge_bind,
            weight_bind,
            blend_bind,
        }
    }
}
