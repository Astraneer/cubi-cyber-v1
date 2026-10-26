//! Deferred bounded water reflection. The surface pass supplies its final
//! world normal and foam; an opaque/Unknown miss preserves source samples.
use crate::terrain_gpu::UploadBudget;
use bytemuck::{Pod, Zeroable};
#[derive(Clone, Copy, Debug)]
pub struct ReflectionConfig {
    pub steps: u32,
    pub max_distance: f32,
    pub tolerance: f32,
}
#[derive(Clone, Copy)]
pub struct ReflectionInputs<'a> {
    pub params: &'a wgpu::Buffer,
    pub water_color: &'a wgpu::TextureView,
    pub water_depth: &'a wgpu::TextureView,
    pub normal_foam: &'a wgpu::TextureView,
    pub opaque_color: &'a wgpu::TextureView,
    pub opaque_depth: &'a wgpu::TextureView,
    pub sky: &'a wgpu::TextureView,
    pub statistics: Option<&'a wgpu::Buffer>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionError {
    InvalidConfig,
    UploadBudget,
    WorkBudget,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReflectionWork {
    pub passes: u32,
    pub draws: u32,
    pub max_steps: u64,
    pub composition_samples: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ConfigGpu {
    steps: u32,
    max_distance: f32,
    tolerance: f32,
    padding: u32,
}
pub struct WaterReflectionGpu {
    samples: u32,
    width: u32,
    height: u32,
    config: ReflectionConfig,
    uniform: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    msaa_texture: wgpu::Texture,
    resolved: wgpu::Texture,
    msaa: wgpu::TextureView,
    resolved_view: wgpu::TextureView,
    owns_resolved: bool,
}
impl WaterReflectionGpu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        samples: u32,
        width: u32,
        height: u32,
        config: ReflectionConfig,
        inputs: ReflectionInputs<'_>,
        budget: &mut UploadBudget,
    ) -> Result<Self, ReflectionError> {
        if !matches!(samples, 1 | 4)
            || width == 0
            || height == 0
            || config.steps == 0
            || config.steps > 64
            || !config.max_distance.is_finite()
            || config.max_distance <= 0.
            || !config.tolerance.is_finite()
            || config.tolerance <= 0.
        {
            return Err(ReflectionError::InvalidConfig);
        }
        if budget.remaining_bytes < 16 {
            return Err(ReflectionError::UploadBudget);
        }
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded SSR configuration"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &uniform,
            0,
            bytemuck::bytes_of(&ConfigGpu {
                steps: config.steps,
                max_distance: config.max_distance,
                tolerance: config.tolerance,
                padding: 0,
            }),
        );
        budget.remaining_bytes -= 16;
        let mut entries = vec![
            buffer_layout(0, true),
            buffer_layout(1, true),
            texture_layout(2, false, samples > 1),
            texture_layout(3, true, samples > 1),
            texture_layout(4, false, samples > 1),
            texture_layout(5, false, false),
            texture_layout(6, true, samples > 1),
            texture_layout(7, false, false),
        ];
        if inputs.statistics.is_some() {
            entries.push(buffer_layout(8, false));
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("deferred SSR immutable inputs"),
            entries: &entries,
        });
        let source=include_str!("water_reflection.wgsl").replace("// STATS",if inputs.statistics.is_some(){
            "struct Stats{fragments:atomic<u32>,steps:atomic<u32>,hits:atomic<u32>,fallbacks:atomic<u32>};@group(0)@binding(8)var<storage,read_write>stats:Stats;fn record(steps:u32,hit:bool){atomicAdd(&stats.fragments,1u);atomicAdd(&stats.steps,steps);if hit{atomicAdd(&stats.hits,1u);}else{atomicAdd(&stats.fallbacks,1u);}}"
        }else{"fn record(steps:u32,hit:bool){}"});
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded fullscreen water SSR"),
            source: wgpu::ShaderSource::Wgsl(crate::sampling::shader(&source, samples)),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("deferred SSR layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bounded water SSR resolve"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: samples,
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
        let bind = make_bind(device, &layout, &uniform, inputs);
        let (msaa_texture, resolved, msaa, resolved_view) = targets(device, width, height, samples);
        Ok(Self {
            samples,
            width,
            height,
            config,
            uniform,
            layout,
            bind,
            pipeline,
            msaa_texture,
            resolved,
            msaa,
            resolved_view,
            owns_resolved: true,
        })
    }
    /// Reuses the loading-time pipeline and uniform; only targets and views change.
    pub fn resize_inputs(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        inputs: ReflectionInputs<'_>,
    ) -> Result<(), ReflectionError> {
        if width == 0 || height == 0 {
            return Err(ReflectionError::InvalidConfig);
        }
        let (msaa_texture, resolved, msaa, resolved_view) =
            targets(device, width, height, self.samples);
        self.bind = make_bind(device, &self.layout, &self.uniform, inputs);
        self.width = width;
        self.height = height;
        self.msaa_texture = msaa_texture;
        self.resolved = resolved;
        self.msaa = msaa;
        self.resolved_view = resolved_view;
        self.owns_resolved = true;
        Ok(())
    }
    /// The four-sample surface target is separate from its one-sample resolve,
    /// so its unused resolve may be reused without read/write aliasing. The
    /// one-sample path must retain its separate output texture.
    pub fn alias_resolve(&mut self, resolved: &wgpu::Texture) -> Result<(), ReflectionError> {
        if self.samples != 4
            || resolved.sample_count() != 1
            || resolved.format() != wgpu::TextureFormat::Rgba16Float
            || resolved.width() != self.width
            || resolved.height() != self.height
        {
            return Err(ReflectionError::InvalidConfig);
        }
        self.resolved = resolved.clone();
        self.resolved_view = resolved.create_view(&Default::default());
        self.owns_resolved = false;
        Ok(())
    }
    pub fn worst_work(&self) -> ReflectionWork {
        let samples = u64::from(self.width) * u64::from(self.height) * u64::from(self.samples);
        ReflectionWork {
            passes: 1,
            draws: 1,
            max_steps: samples * u64::from(self.config.steps),
            composition_samples: samples,
        }
    }
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        max_steps: u64,
        timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) -> Result<ReflectionWork, ReflectionError> {
        let work = self.worst_work();
        if work.max_steps > max_steps {
            return Err(ReflectionError::WorkBudget);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bounded per-sample deferred water SSR"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa,
                depth_slice: None,
                resolve_target: (self.samples > 1).then_some(&self.resolved_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(work)
    }
    pub fn color_msaa(&self) -> &wgpu::TextureView {
        &self.msaa
    }
    pub fn resolved_view(&self) -> &wgpu::TextureView {
        &self.resolved_view
    }
    pub fn resolved_texture(&self) -> &wgpu::Texture {
        &self.resolved
    }
    pub fn target_bytes(&self) -> u64 {
        u64::from(self.width)
            * u64::from(self.height)
            * if self.owns_resolved {
                crate::sampling::color_bytes_per_pixel(self.samples)
            } else {
                u64::from(self.samples) * 8
            }
    }
}
fn buffer_layout(binding: u32, uniform: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: if uniform {
                wgpu::BufferBindingType::Uniform
            } else {
                wgpu::BufferBindingType::Storage { read_only: false }
            },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn texture_layout(binding: u32, depth: bool, multisampled: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
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
    }
}
fn make_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    inputs: ReflectionInputs<'_>,
) -> wgpu::BindGroup {
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: 0,
            resource: inputs.params.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: uniform.as_entire_binding(),
        },
    ];
    for (binding, view) in [
        (2, inputs.water_color),
        (3, inputs.water_depth),
        (4, inputs.normal_foam),
        (5, inputs.opaque_color),
        (6, inputs.opaque_depth),
        (7, inputs.sky),
    ] {
        entries.push(wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        });
    }
    if let Some(stats) = inputs.statistics {
        entries.push(wgpu::BindGroupEntry {
            binding: 8,
            resource: stats.as_entire_binding(),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("deferred water SSR inputs"),
        layout,
        entries: &entries,
    })
}
fn targets(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    samples: u32,
) -> (
    wgpu::Texture,
    wgpu::Texture,
    wgpu::TextureView,
    wgpu::TextureView,
) {
    let make = |label, samples| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | if samples == 1 {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
    };
    let resolved = make("deferred SSR resolved HDR", 1);
    let msaa = if samples == 1 {
        resolved.clone()
    } else {
        make("deferred SSR MSAA HDR", samples)
    };
    let color = msaa.create_view(&Default::default());
    let view = resolved.create_view(&Default::default());
    (msaa, resolved, color, view)
}
