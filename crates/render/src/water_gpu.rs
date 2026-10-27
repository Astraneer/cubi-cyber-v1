//! Independent water surface pass. Opaque MSAA input is never sampled while it
//! is attached for writing; each original sample is copied to a separate target.
use crate::{
    MSAA_SAMPLES,
    shadows::ShadowGpu,
    terrain_gpu::{Camera, UploadBudget},
    terrain_mesh::{Face, QuadDescriptor},
};
use bytemuck::{Pod, Zeroable};
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct WaterQuad {
    pub words: [u32; 4],
}
impl WaterQuad {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        origin: [u8; 3],
        face: Face,
        size: [u8; 2],
        heights: [f32; 4],
        flow: [f32; 2],
        tint: u32,
        foam: f32,
    ) -> Result<Self, WaterError> {
        if heights
            .iter()
            .chain(flow.iter())
            .chain([foam].iter())
            .any(|v| !v.is_finite())
            || heights.iter().any(|v| !(0.0..=1.0).contains(v))
            || flow.iter().any(|v| !(-1.0..=1.0).contains(v))
            || !(0.0..=1.0).contains(&foam)
        {
            return Err(WaterError::InvalidQuad);
        }
        let base = QuadDescriptor::new(origin, face, size, 0, [3; 4], tint)
            .map_err(|_| WaterError::InvalidQuad)?;
        let mut levels = 0;
        for (i, h) in heights.into_iter().enumerate() {
            levels |= ((h * 255.).round() as u32) << (8 * i);
        }
        let fx = ((flow[0] * 127.).round() as i8) as u8;
        let fz = ((flow[1] * 127.).round() as i8) as u8;
        Ok(Self {
            words: [
                base.words[0],
                levels,
                (tint & 0xffffff) | (((foam * 255.).round() as u32) << 24),
                (u32::from(fx) << 16) | (u32::from(fz) << 24),
            ],
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaterHandle {
    pub slot: u32,
    pub generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaterDraw {
    pub quad: WaterQuad,
    pub object: WaterHandle,
}
#[derive(Clone, Copy, Debug)]
pub struct WaterConfig {
    pub max_objects: u32,
    pub max_quads: u32,
    pub absorption: [f32; 3],
    pub turbidity: f32,
    pub scattering: [f32; 3],
    pub ior: f32,
    pub roughness: f32,
    pub max_thickness: f32,
    pub refraction_scale: f32,
    pub reflection_steps: u32,
    pub reflection_distance: f32,
    pub reflection_tolerance: f32,
    pub wave_amplitude: f32,
    pub wave_frequency: [f32; 2],
    pub wave_speed: f32,
    pub instrument: bool,
}
impl Default for WaterConfig {
    fn default() -> Self {
        Self {
            max_objects: 4096,
            max_quads: 65536,
            absorption: [0.18, 0.055, 0.025],
            turbidity: 0.015,
            scattering: [0.015, 0.08, 0.11],
            ior: 1.333,
            roughness: 0.08,
            max_thickness: 128.,
            refraction_scale: 0.02,
            reflection_steps: 24,
            reflection_distance: 64.,
            reflection_tolerance: 0.75,
            wave_amplitude: 0.025,
            wave_frequency: [0.7, 0.53],
            wave_speed: 1.,
            instrument: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterState {
    pub camera: Camera,
    pub eye: [f32; 3],
    pub time_seconds: f32,
    pub sun: [f32; 3],
    pub sun_intensity: f32,
    pub underwater: bool,
    pub scene_revision: u64,
}
#[derive(Clone, Copy)]
pub struct WaterInputs<'a> {
    pub color_msaa: &'a wgpu::TextureView,
    pub depth_msaa: &'a wgpu::TextureView,
    pub sky: &'a wgpu::TextureView,
    pub transmittance: &'a wgpu::TextureView,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterError {
    InvalidConfig,
    InvalidQuad,
    InvalidModel,
    InvalidState,
    Capacity,
    StaleHandle,
    ObjectInUse,
    UploadBudget,
    WorkBudget,
}
impl std::fmt::Display for WaterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WaterError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaterWork {
    pub upload_bytes: u64,
    pub quad_uploads: u32,
    pub object_uploads: u32,
    pub cpu_vertices: u32,
    pub passes: u32,
    pub draws: u32,
    pub cull_dispatches: u32,
    pub snapshot_pixels: u64,
    pub max_reflection_steps: u64,
    pub pipeline_compilations: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view_projection: [[f32; 4]; 4],
    inverse_view_projection: [[f32; 4]; 4],
    eye_time: [f32; 4],
    absorption: [f32; 4],
    scattering: [f32; 4],
    optics: [f32; 4],
    sun: [f32; 4],
    image: [u32; 4],
    quality: [u32; 4],
    waves: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct ObjectGpu {
    model: [[f32; 4]; 4],
    metadata: [u32; 4],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: u32,
    len: u32,
}
struct WaterArena {
    free: Vec<Span>,
    scratch: Vec<Span>,
    high: u32,
    count: u32,
}
#[derive(Clone, Copy)]
struct Slot {
    span: Option<Span>,
    generation: u32,
    allocated: bool,
    enabled: bool,
    references: u32,
    model: [[f32; 4]; 4],
}
/// Borrowed current geometry for optional temporal invalidation. No shading or
/// refraction is repeated; the bind/indirect list is the actual color list.
pub struct WaterTemporalSource<'a> {
    pub bind: &'a wgpu::BindGroup,
    pub indirect: &'a wgpu::Buffer,
    pub depth: &'a wgpu::TextureView,
    pub extent: [u32; 2],
    pub samples: u32,
    pub quads: u32,
    pub enabled: bool,
    pub prepared: bool,
}
pub struct WaterGpu {
    sample_count: u32,
    config: WaterConfig,
    width: u32,
    height: u32,
    uniform: wgpu::Buffer,
    objects: wgpu::Buffer,
    quads: wgpu::Buffer,
    indirect: wgpu::Buffer,
    statistics: wgpu::Buffer,
    visible: wgpu::Buffer,
    shadows: Option<(wgpu::Buffer, wgpu::TextureView, wgpu::Sampler)>,
    bind: wgpu::BindGroup,
    cull_bind: wgpu::BindGroup,
    snapshot_bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    background: wgpu::RenderPipeline,
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    snapshot: wgpu::ComputePipeline,
    _opaque: wgpu::Texture,
    _msaa: wgpu::Texture,
    msaa: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth: wgpu::TextureView,
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    slots: Vec<Slot>,
    free: Vec<u32>,
    previous: Vec<WaterDraw>,
    arena: Option<WaterArena>,
    staging: Vec<WaterQuad>,
    revision: u64,
    surface_enabled: bool,
    last: Option<(WaterState, u64)>,
    last_cull: Option<(Camera, u64)>,
    last_snapshot: Option<(Camera, u64)>,
    work: WaterWork,
    timer: crate::gpu_time::GpuPassTimer<4>,
    normal_texture: Option<wgpu::Texture>,
    normal_foam: Option<wgpu::TextureView>,
    reflection: Option<crate::water_reflection::WaterReflectionGpu>,
    flag_scratch: Vec<(u32, bool)>,
}
impl WaterGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: WaterConfig,
        width: u32,
        height: u32,
        inputs: WaterInputs<'_>,
        shadows: Option<&ShadowGpu>,
    ) -> Result<Self, WaterError> {
        Self::new_with_samples(
            device,
            queue,
            config,
            width,
            height,
            inputs,
            shadows,
            MSAA_SAMPLES,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: WaterConfig,
        width: u32,
        height: u32,
        inputs: WaterInputs<'_>,
        shadows: Option<&ShadowGpu>,
        sample_count: u32,
    ) -> Result<Self, WaterError> {
        if !matches!(sample_count, 1 | 4) {
            return Err(WaterError::InvalidConfig);
        }
        if width == 0
            || height == 0
            || width.max(height) > device.limits().max_texture_dimension_2d
            || config.max_objects == 0
            || config.max_objects > 65535
            || config.max_quads == 0
            || u64::from(config.max_quads) * 16
                > u64::from(device.limits().max_storage_buffer_binding_size)
            || config
                .absorption
                .iter()
                .chain(config.scattering.iter())
                .chain(
                    [
                        config.turbidity,
                        config.ior,
                        config.roughness,
                        config.max_thickness,
                        config.refraction_scale,
                        config.reflection_distance,
                        config.reflection_tolerance,
                        config.wave_amplitude,
                        config.wave_frequency[0],
                        config.wave_frequency[1],
                        config.wave_speed,
                    ]
                    .iter(),
                )
                .any(|v| !v.is_finite() || *v < 0.)
            || !(1.0..=2.5).contains(&config.ior)
            || config.roughness == 0.
            || config.max_thickness == 0.
            || config.reflection_distance == 0.
            || config.reflection_steps > 64
            || config.wave_amplitude > 0.25
        {
            return Err(WaterError::InvalidConfig);
        }
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let uniform = buffer(
            "water physical state256B",
            256,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let objects = buffer(
            "stable water object80B transforms",
            u64::from(config.max_objects) * 80,
            storage,
        );
        let quads = buffer(
            "16B water quads with corner levels and flow",
            u64::from(config.max_quads) * 16,
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let visible = buffer(
            "GPU water quad visibility",
            u64::from(config.max_quads) * 4,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = buffer(
            "one water quad indirect draw",
            16,
            storage | wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_SRC,
        );
        let statistics = buffer(
            "optional actual water ray counters",
            16,
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let texture = |label, format, samples, usage| {
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
                format,
                usage,
                view_formats: &[],
            })
        };
        let opaque_texture = texture(
            "opaque full-resolution water refraction snapshot",
            wgpu::TextureFormat::Rgba16Float,
            1,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let opaque = opaque_texture.create_view(&Default::default());
        let depth_texture = texture(
            "water depth with preserved opaque MSAA samples",
            wgpu::TextureFormat::Depth32Float,
            sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = depth_texture.create_view(&Default::default());
        let resolved = texture(
            "resolved physical water HDR",
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
                "physical water separate HDR MSAA4",
                wgpu::TextureFormat::Rgba16Float,
                sample_count,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        };
        let msaa = msaa_texture.create_view(&Default::default());
        let resolved_view = resolved.create_view(&Default::default());
        let normal_texture = (config.reflection_steps > 0).then(|| {
            texture(
                "water optional deferred reflection normal foam",
                wgpu::TextureFormat::Rgba16Float,
                sample_count,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        });
        let normal_foam = normal_texture
            .as_ref()
            .map(|t| t.create_view(&Default::default()));
        let v = wgpu::ShaderStages::VERTEX;
        let f = wgpu::ShaderStages::FRAGMENT;
        let c = wgpu::ShaderStages::COMPUTE;
        let mut entries = vec![
            buffer_layout(0, v | f, true, true),
            buffer_layout(1, v, false, true),
            buffer_layout(2, v, false, true),
            buffer_layout(3, v, false, true),
            texture_layout(4, f, false, false),
            texture_layout(5, f, false, false),
            texture_layout(6, f, true, sample_count > 1),
            texture_layout(7, f, false, false),
            texture_layout(12, f, false, sample_count > 1),
        ];
        if config.instrument {
            entries.push(buffer_layout(8, f, false, false))
        }
        if shadows.is_some() {
            entries.extend([
                buffer_layout(9, f, true, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: f,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: f,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ]);
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("physical water immutable opaque inputs"),
            entries: &entries,
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water GPU quad cull layout"),
            entries: &[
                buffer_layout(0, c, true, true),
                buffer_layout(1, c, false, true),
                buffer_layout(2, c, false, true),
                buffer_layout(3, c, false, false),
                buffer_layout(4, c, false, false),
            ],
        });
        let snapshot_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("opaque MSAA resolve before water"),
            entries: &[
                buffer_layout(0, c, true, true),
                texture_layout(1, c, false, sample_count > 1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: c,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let texture_entry = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let mut entries = vec![
            buffer_binding(0, &uniform),
            buffer_binding(1, &objects),
            buffer_binding(2, &quads),
            buffer_binding(3, &visible),
            texture_entry(4, inputs.sky),
            texture_entry(5, &opaque),
            texture_entry(6, inputs.depth_msaa),
            texture_entry(7, inputs.transmittance),
            texture_entry(12, inputs.color_msaa),
        ];
        if config.instrument {
            entries.push(buffer_binding(8, &statistics))
        }
        if let Some(s) = shadows {
            entries.extend([
                buffer_binding(9, s.sampling_buffer()),
                texture_entry(10, s.depth_view()),
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(s.sampler()),
                },
            ]);
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water optics and source radiance"),
            layout: &layout,
            entries: &entries,
        });
        let cull_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water indirect source tables"),
            layout: &cull_layout,
            entries: &[
                buffer_binding(0, &uniform),
                buffer_binding(1, &objects),
                buffer_binding(2, &quads),
                buffer_binding(3, &visible),
                buffer_binding(4, &indirect),
            ],
        });
        let snapshot_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("full-size opaque refraction snapshot"),
            layout: &snapshot_layout,
            entries: &[
                buffer_binding(0, &uniform),
                texture_entry(1, inputs.color_msaa),
                texture_entry(2, &opaque),
            ],
        });
        let source=include_str!("water.wgsl").replace("// SHADOWS",if shadows.is_some(){include_str!("shadow_sampling.wgsl")}else{"fn cascade_shadow(p:vec3<f32>,n:vec3<f32>)->f32{return 1.;}"}).replace("// STATS",if config.instrument{"struct Stats{fragments:atomic<u32>,steps:atomic<u32>,hits:atomic<u32>,fallbacks:atomic<u32>};@group(0)@binding(8)var<storage,read_write>stats:Stats;fn record(steps:u32,hit:bool){atomicAdd(&stats.fragments,1u);atomicAdd(&stats.steps,steps);if hit{atomicAdd(&stats.hits,1u);}else{atomicAdd(&stats.fallbacks,1u);}}"}else{"fn record(steps:u32,hit:bool){}"});
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("original Fresnel Beer refraction SSR wave optics"),
            source: wgpu::ShaderSource::Wgsl(crate::sampling::shader(&source, sample_count)),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water MSAA HDR pipeline"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let render = |background| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sample-preserving physical water"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if background {
                        "background_vertex"
                    } else {
                        "vertex"
                    }),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: if background {
                        wgpu::PrimitiveTopology::TriangleList
                    } else {
                        wgpu::PrimitiveTopology::TriangleStrip
                    },
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: if background {
                        wgpu::CompareFunction::Always
                    } else {
                        wgpu::CompareFunction::GreaterEqual
                    },
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(match (background, config.reflection_steps > 0) {
                        (true, true) => "background_fragment_with_normal",
                        (true, false) => "background_fragment",
                        (false, true) => "fragment_with_normal",
                        (false, false) => "fragment",
                    }),
                    compilation_options: Default::default(),
                    targets: if config.reflection_steps > 0 {
                        &[
                            Some(wgpu::ColorTargetState {
                                format: wgpu::TextureFormat::Rgba16Float,
                                blend: None,
                                write_mask: wgpu::ColorWrites::ALL,
                            }),
                            Some(wgpu::ColorTargetState {
                                format: wgpu::TextureFormat::Rgba16Float,
                                blend: None,
                                write_mask: wgpu::ColorWrites::ALL,
                            }),
                        ]
                    } else {
                        &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba16Float,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })]
                    },
                }),
                multiview: None,
                cache: None,
            })
        };
        let background = render(true);
        let pipeline = render(false);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water visibility and opaque resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("water_compute.wgsl").into()),
        });
        let compute = |entry, bl: &wgpu::BindGroupLayout| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[bl],
                push_constant_ranges: &[],
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let reset = compute("reset", &cull_layout);
        let cull = compute("cull", &cull_layout);
        let snapshot_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water full-resolution opaque snapshot"),
            source: wgpu::ShaderSource::Wgsl(crate::sampling::shader(
                include_str!("water_snapshot.wgsl"),
                sample_count,
            )),
        });
        let spl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water snapshot layout"),
            bind_group_layouts: &[&snapshot_layout],
            push_constant_ranges: &[],
        });
        let snapshot = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("opaque MSAA radiance resolve"),
            layout: Some(&spl),
            module: &snapshot_shader,
            entry_point: Some("snapshot"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mut reflection = if let Some(normal) = &normal_foam {
            Some(
                crate::water_reflection::WaterReflectionGpu::new(
                    device,
                    queue,
                    sample_count,
                    width,
                    height,
                    crate::water_reflection::ReflectionConfig {
                        steps: config.reflection_steps,
                        max_distance: config.reflection_distance,
                        tolerance: config.reflection_tolerance,
                    },
                    crate::water_reflection::ReflectionInputs {
                        params: &uniform,
                        water_color: &msaa,
                        water_depth: &depth,
                        normal_foam: normal,
                        opaque_color: &opaque,
                        opaque_depth: inputs.depth_msaa,
                        sky: inputs.sky,
                        statistics: config.instrument.then_some(&statistics),
                    },
                    &mut UploadBudget::new(16),
                )
                .map_err(|_| WaterError::InvalidConfig)?,
            )
        } else {
            None
        };
        if sample_count == 4
            && let Some(reflection) = &mut reflection
        {
            reflection
                .alias_resolve(&resolved)
                .map_err(|_| WaterError::InvalidConfig)?;
        }
        Ok(Self {
            sample_count,
            config,
            width,
            height,
            uniform,
            objects,
            quads,
            indirect,
            statistics,
            visible,
            shadows: shadows.map(|s| {
                (
                    s.sampling_buffer().clone(),
                    s.depth_view().clone(),
                    s.sampler().clone(),
                )
            }),
            bind,
            cull_bind,
            snapshot_bind,
            pipeline,
            background,
            reset,
            cull,
            snapshot,
            _opaque: opaque_texture,
            _msaa: msaa_texture,
            msaa,
            _depth: depth_texture,
            depth,
            resolved,
            resolved_view,
            normal_texture,
            normal_foam,
            reflection,
            slots: vec![
                Slot {
                    span: None,
                    generation: 0,
                    allocated: false,
                    enabled: false,
                    references: 0,
                    model: [[0.; 4]; 4]
                };
                config.max_objects as usize
            ],
            free: (0..config.max_objects).rev().collect(),
            previous: Vec::with_capacity(config.max_quads as usize),
            arena: None,
            staging: Vec::with_capacity(config.max_quads as usize),
            revision: 0,
            surface_enabled: true,
            last: None,
            last_cull: None,
            last_snapshot: None,
            timer: crate::gpu_time::GpuPassTimer::new(
                device,
                queue,
                "physical water pass timestamps",
            ),
            work: WaterWork {
                pipeline_compilations: 5 + u32::from(config.reflection_steps > 0),
                upload_bytes: u64::from(config.reflection_steps > 0) * 16,
                ..WaterWork::default()
            },
            flag_scratch: Vec::with_capacity(config.max_objects as usize),
        })
    }
    pub fn upload_object(
        &mut self,
        queue: &wgpu::Queue,
        handle: Option<WaterHandle>,
        model: [[f32; 4]; 4],
        budget: &mut UploadBudget,
    ) -> Result<WaterHandle, WaterError> {
        self.upload_object_state(queue, handle, model, None, budget)
    }
    /// Stage geometry while its replacement height/opaque cover is incomplete.
    /// A retained inactive handle stays hidden after a later range-budget retry.
    pub fn upload_object_inactive(
        &mut self,
        queue: &wgpu::Queue,
        handle: Option<WaterHandle>,
        model: [[f32; 4]; 4],
        budget: &mut UploadBudget,
    ) -> Result<WaterHandle, WaterError> {
        self.upload_object_state(queue, handle, model, Some(false), budget)
    }
    fn upload_object_state(
        &mut self,
        queue: &wgpu::Queue,
        handle: Option<WaterHandle>,
        model: [[f32; 4]; 4],
        enabled: Option<bool>,
        budget: &mut UploadBudget,
    ) -> Result<WaterHandle, WaterError> {
        if model.iter().flatten().any(|v| !v.is_finite())
            || model[0][3] != 0.
            || model[1][3] != 0.
            || model[2][3] != 0.
            || model[3][3] != 1.
        {
            return Err(WaterError::InvalidModel);
        }
        let slot = if let Some(h) = handle {
            self.checked(h)?;
            h.slot
        } else {
            *self.free.last().ok_or(WaterError::Capacity)?
        };
        let old = self.slots[slot as usize];
        let enabled = enabled.unwrap_or(!old.allocated || old.enabled);
        if old.allocated && old.model == model && old.enabled == enabled {
            return Ok(handle.expect("existing object"));
        }
        if budget.remaining_bytes < 80 {
            return Err(WaterError::UploadBudget);
        }
        let generation = if old.allocated {
            old.generation
        } else {
            self.free.pop();
            old.generation
                .checked_add(1)
                .expect("retired max generation")
        };
        queue.write_buffer(
            &self.objects,
            u64::from(slot) * 80,
            bytemuck::bytes_of(&ObjectGpu {
                model,
                metadata: [
                    generation,
                    u32::from(enabled),
                    old.span.map_or(0, |s| s.start),
                    old.references,
                ],
            }),
        );
        self.slots[slot as usize] = Slot {
            span: old.span,
            generation,
            allocated: true,
            enabled,
            references: old.references,
            model,
        };
        self.revision = self.revision.wrapping_add(1);
        budget.remaining_bytes -= 80;
        self.work.upload_bytes += 80;
        self.work.object_uploads += 1;
        Ok(WaterHandle { slot, generation })
    }
    /// Atomic publication preflight for a staged cover: every handle and the
    /// complete upload allowance are checked before any flag is written.
    /// Duplicate handles are rejected, including contradictory enable values.
    pub fn set_enabled_batch(
        &mut self,
        queue: &wgpu::Queue,
        changes: &[(WaterHandle, bool)],
        budget: &mut UploadBudget,
    ) -> Result<u64, WaterError> {
        let bytes = self.enabled_batch_upload_bytes(changes)?;
        if budget.remaining_bytes < bytes {
            return Err(WaterError::UploadBudget);
        }
        for &(slot, enabled) in &self.flag_scratch {
            let object = &mut self.slots[slot as usize];
            if object.enabled != enabled {
                queue.write_buffer(
                    &self.objects,
                    u64::from(slot) * 80 + 68,
                    bytemuck::bytes_of(&u32::from(enabled)),
                );
                object.enabled = enabled;
            }
        }
        if bytes > 0 {
            self.revision = self.revision.wrapping_add(1);
        }
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        Ok(bytes)
    }
    /// Combine this exact preflight with the height/opaque cover preflight
    /// before publishing any member of a cross-renderer cover transaction.
    pub fn enabled_batch_upload_bytes(
        &mut self,
        changes: &[(WaterHandle, bool)],
    ) -> Result<u64, WaterError> {
        if changes.len() > self.config.max_objects as usize {
            return Err(WaterError::InvalidState);
        }
        self.flag_scratch.clear();
        for &(handle, enabled) in changes {
            self.checked(handle)?;
            self.flag_scratch.push((handle.slot, enabled));
        }
        self.flag_scratch.sort_unstable_by_key(|&(slot, _)| slot);
        if self.flag_scratch.windows(2).any(|p| p[0].0 == p[1].0) {
            return Err(WaterError::InvalidState);
        }
        let count = self
            .flag_scratch
            .iter()
            .filter(|&&(slot, enabled)| self.slots[slot as usize].enabled != enabled)
            .count();
        let bytes = count as u64 * 4;
        Ok(bytes)
    }
    fn checked(&self, h: WaterHandle) -> Result<&Slot, WaterError> {
        self.slots
            .get(h.slot as usize)
            .filter(|s| s.allocated && s.generation == h.generation)
            .ok_or(WaterError::StaleHandle)
    }
    pub fn release_object(
        &mut self,
        queue: &wgpu::Queue,
        h: WaterHandle,
        budget: &mut UploadBudget,
    ) -> Result<(), WaterError> {
        if self.checked(h)?.references != 0 {
            return Err(WaterError::ObjectInUse);
        }
        if budget.remaining_bytes < 4 {
            return Err(WaterError::UploadBudget);
        }
        queue.write_buffer(&self.objects, u64::from(h.slot) * 80 + 68, &[0; 4]);
        let s = &mut self.slots[h.slot as usize];
        s.allocated = false;
        s.enabled = false;
        if s.generation < u32::MAX {
            self.free.push(h.slot)
        }
        budget.remaining_bytes -= 4;
        self.revision = self.revision.wrapping_add(1);
        self.work.upload_bytes += 4;
        Ok(())
    }
    pub fn upload_quads(
        &mut self,
        queue: &wgpu::Queue,
        draws: &[WaterDraw],
        budget: &mut UploadBudget,
    ) -> Result<WaterWork, WaterError> {
        if self.arena.is_some() {
            return Err(WaterError::InvalidState);
        }
        if draws.len() > self.config.max_quads as usize {
            return Err(WaterError::Capacity);
        }
        for d in draws {
            self.checked(d.object)?;
            let bits = d.quad.words[0];
            let face = Face::from_index(((bits >> 15) & 7) as u8).ok_or(WaterError::InvalidQuad)?;
            let origin = [
                (bits & 31) as u8,
                ((bits >> 5) & 31) as u8,
                ((bits >> 10) & 31) as u8,
            ];
            let size = [
                (((bits >> 18) & 15) + 1) as u8,
                (((bits >> 22) & 15) + 1) as u8,
            ];
            if bits >> 26 != 0
                || QuadDescriptor::new(origin, face, size, 0, [3; 4], 0).is_err()
                || ((d.quad.words[3] >> 16) & 255) == 128
                || (d.quad.words[3] >> 24) == 128
            {
                return Err(WaterError::InvalidQuad);
            }
        }
        if self.previous == draws {
            return Ok(WaterWork::default());
        }
        let bytes = draws.len() as u64 * 16;
        if budget.remaining_bytes < bytes {
            return Err(WaterError::UploadBudget);
        }
        self.staging.clear();
        for d in draws {
            let mut q = d.quad;
            q.words[3] = (q.words[3] & 0xffff0000) | d.object.slot;
            self.staging.push(q);
        }
        if bytes > 0 {
            queue.write_buffer(&self.quads, 0, bytemuck::cast_slice(&self.staging));
        }
        for old in &self.previous {
            self.slots[old.object.slot as usize].references -= 1;
        }
        for new in draws {
            self.slots[new.object.slot as usize].references += 1;
        }
        self.previous.clear();
        self.previous.extend_from_slice(draws);
        self.revision = self.revision.wrapping_add(1);
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        self.work.quad_uploads += draws.len() as u32;
        Ok(WaterWork {
            upload_bytes: bytes,
            quad_uploads: draws.len() as u32,
            ..WaterWork::default()
        })
    }
    pub fn active_quad_count(&self) -> u32 {
        self.arena
            .as_ref()
            .map_or(self.previous.len() as u32, |a| a.count)
    }
    /// Stable per-page ranges. A changed page uploads only its own16B records;
    /// free ranges stay hidden by the object range guard on GPU.
    pub fn upload_page_quads(
        &mut self,
        queue: &wgpu::Queue,
        object: WaterHandle,
        quads: &[WaterQuad],
        budget: &mut UploadBudget,
    ) -> Result<WaterWork, WaterError> {
        let old = *self.checked(object)?;
        if !self.previous.is_empty() {
            return Err(WaterError::InvalidState);
        }
        if quads.len() > self.config.max_quads as usize {
            return Err(WaterError::Capacity);
        }
        for q in quads {
            let b = q.words[0];
            let face = Face::from_index(((b >> 15) & 7) as u8).ok_or(WaterError::InvalidQuad)?;
            if b >> 26 != 0
                || QuadDescriptor::new(
                    [
                        (b & 31) as u8,
                        ((b >> 5) & 31) as u8,
                        ((b >> 10) & 31) as u8,
                    ],
                    face,
                    [(((b >> 18) & 15) + 1) as u8, (((b >> 22) & 15) + 1) as u8],
                    0,
                    [3; 4],
                    0,
                )
                .is_err()
            {
                return Err(WaterError::InvalidQuad);
            }
        }
        if self.arena.is_none()
            && budget.remaining_bytes
                < quads.len() as u64 * 16 + if quads.is_empty() { 0 } else { 8 }
        {
            return Err(WaterError::UploadBudget);
        }
        if self.arena.is_none() {
            let mut free = Vec::with_capacity(self.config.max_objects as usize + 2);
            free.push(Span {
                start: 0,
                len: self.config.max_quads,
            });
            self.arena = Some(WaterArena {
                free,
                scratch: Vec::with_capacity(self.config.max_objects as usize + 2),
                high: 0,
                count: 0,
            });
        }
        let count = quads.len() as u32;
        let arena = self.arena.as_mut().expect("initialized arena");
        let reuse = old.span.filter(|s| s.len >= count && count > 0);
        let mut chosen = reuse;
        if reuse.is_none() {
            arena.scratch.clear();
            arena.scratch.extend_from_slice(&arena.free);
            if let Some(span) = old.span {
                arena.scratch.push(span);
            }
            coalesce(&mut arena.scratch);
            if count > 0 {
                let fit = arena
                    .scratch
                    .iter()
                    .position(|s| s.len >= count)
                    .ok_or(WaterError::Capacity)?;
                chosen = Some(Span {
                    start: arena.scratch[fit].start,
                    len: count,
                });
                arena.scratch[fit].start += count;
                arena.scratch[fit].len -= count;
                if arena.scratch[fit].len == 0 {
                    arena.scratch.remove(fit);
                }
            }
        }
        let first = chosen.map_or(0, |s| s.start);
        let header_changed = old.span.map_or(0, |s| s.start) != first || old.references != count;
        let bytes = u64::from(count) * 16 + u64::from(header_changed) * 8;
        if budget.remaining_bytes < bytes {
            return Err(WaterError::UploadBudget);
        }
        self.staging.clear();
        for quad in quads {
            let mut q = *quad;
            q.words[3] = (q.words[3] & 0xffff0000) | object.slot;
            self.staging.push(q);
        }
        if count > 0 {
            queue.write_buffer(
                &self.quads,
                u64::from(first) * 16,
                bytemuck::cast_slice(&self.staging),
            );
        }
        if header_changed {
            queue.write_buffer(
                &self.objects,
                u64::from(object.slot) * 80 + 72,
                bytemuck::cast_slice(&[first, count]),
            );
        }
        if reuse.is_none() {
            arena.free.clear();
            arena.free.extend_from_slice(&arena.scratch);
        }
        arena.high = arena.high.max(first + count);
        arena.count = arena.count - old.references + count;
        let slot = &mut self.slots[object.slot as usize];
        slot.span = chosen;
        slot.references = count;
        self.revision = self.revision.wrapping_add(1);
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        self.work.quad_uploads += count;
        Ok(WaterWork {
            upload_bytes: bytes,
            quad_uploads: count,
            ..WaterWork::default()
        })
    }
    /// Explicit resize only: retain every pipeline, query set, object and quad.
    pub fn resize_inputs(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        inputs: WaterInputs<'_>,
    ) -> Result<(), WaterError> {
        let sample_count = self.sample_count;
        if width == 0 || height == 0 || width.max(height) > device.limits().max_texture_dimension_2d
        {
            return Err(WaterError::InvalidConfig);
        }
        let texture = |label, format, samples, usage| {
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
                format,
                usage,
                view_formats: &[],
            })
        };
        let opaque_texture = texture(
            "resized opaque water refraction snapshot",
            wgpu::TextureFormat::Rgba16Float,
            1,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let opaque = opaque_texture.create_view(&Default::default());
        let depth_texture = texture(
            "resized physical water MSAA4 depth",
            wgpu::TextureFormat::Depth32Float,
            sample_count,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = depth_texture.create_view(&Default::default());
        let resolved = texture(
            "resized physical water resolved HDR",
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
                "resized physical water MSAA4",
                wgpu::TextureFormat::Rgba16Float,
                sample_count,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        };
        let msaa = msaa_texture.create_view(&Default::default());
        let resolved_view = resolved.create_view(&Default::default());
        let normal_texture = self.normal_texture.as_ref().map(|_| {
            texture(
                "resized optional water reflection normal foam",
                wgpu::TextureFormat::Rgba16Float,
                sample_count,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        });
        let normal_foam = normal_texture
            .as_ref()
            .map(|t| t.create_view(&Default::default()));
        if let (Some(reflection), Some(normal)) = (&mut self.reflection, &normal_foam) {
            reflection
                .resize_inputs(
                    device,
                    width,
                    height,
                    crate::water_reflection::ReflectionInputs {
                        params: &self.uniform,
                        water_color: &msaa,
                        water_depth: &depth,
                        normal_foam: normal,
                        opaque_color: &opaque,
                        opaque_depth: inputs.depth_msaa,
                        sky: inputs.sky,
                        statistics: self.config.instrument.then_some(&self.statistics),
                    },
                )
                .map_err(|_| WaterError::InvalidConfig)?;
            if sample_count == 4 {
                reflection
                    .alias_resolve(&resolved)
                    .map_err(|_| WaterError::InvalidConfig)?;
            }
        }
        let tex = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let mut entries = vec![
            buffer_binding(0, &self.uniform),
            buffer_binding(1, &self.objects),
            buffer_binding(2, &self.quads),
            buffer_binding(3, &self.visible),
            tex(4, inputs.sky),
            tex(5, &opaque),
            tex(6, inputs.depth_msaa),
            tex(7, inputs.transmittance),
            tex(12, inputs.color_msaa),
        ];
        if self.config.instrument {
            entries.push(buffer_binding(8, &self.statistics));
        }
        if let Some((uniform, view, sampler)) = &self.shadows {
            entries.extend([
                buffer_binding(9, uniform),
                tex(10, view),
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ]);
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resized water optics inputs"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let snapshot_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resized full-resolution water snapshot"),
            layout: &self.snapshot.get_bind_group_layout(0),
            entries: &[
                buffer_binding(0, &self.uniform),
                tex(1, inputs.color_msaa),
                tex(2, &opaque),
            ],
        });
        self.width = width;
        self.height = height;
        self._opaque = opaque_texture;
        self._msaa = msaa_texture;
        self.msaa = msaa;
        self._depth = depth_texture;
        self.depth = depth;
        self.resolved = resolved;
        self.resolved_view = resolved_view;
        self.normal_texture = normal_texture;
        self.normal_foam = normal_foam;
        self.bind = bind;
        self.snapshot_bind = snapshot_bind;
        self.invalidate();
        self.timer.begin();
        Ok(())
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Keep the sample-preserving background copy while bootstrapping a cover.
    pub fn set_surface_enabled(&mut self, enabled: bool) {
        if self.surface_enabled != enabled {
            self.surface_enabled = enabled;
            self.revision = self.revision.wrapping_add(1);
        }
    }
    pub fn capacity(&self) -> (u32, u32) {
        (self.config.max_objects, self.config.max_quads)
    }
    pub fn needs_redraw(&self, state: WaterState) -> bool {
        self.last != Some((state, self.revision))
    }
    /// One fullscreen trace per output sample; independent of quad overdraw.
    pub fn worst_reflection_steps(&self) -> u64 {
        self.reflection
            .as_ref()
            .map_or(0, |r| r.worst_work().max_steps)
    }
    /// Actual extra steady targets. MSAA4 reuses the surface's old resolve.
    pub fn reflection_target_bytes(&self) -> u64 {
        self.reflection.as_ref().map_or(0, |reflection| {
            reflection.target_bytes()
                + u64::from(self.width) * u64::from(self.height) * u64::from(self.sample_count) * 8
        })
    }
    pub fn pending_work(&self, state: WaterState) -> Result<WaterWork, WaterError> {
        if state
            .eye
            .iter()
            .chain(state.sun.iter())
            .chain([state.time_seconds, state.sun_intensity].iter())
            .any(|v| !v.is_finite())
            || state.sun_intensity < 0.
            || state.camera.inverse_view_projection().is_none()
        {
            return Err(WaterError::InvalidState);
        }
        let length = state.sun.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !length.is_finite() || length < 1e-6 {
            return Err(WaterError::InvalidState);
        }
        if !self.needs_redraw(state) {
            return Ok(WaterWork::default());
        }
        let snapshot = self.last_snapshot != Some((state.camera, state.scene_revision));
        let cull = self.last_cull != Some((state.camera, self.revision));
        Ok(WaterWork {
            upload_bytes: 256,
            passes: 1
                + u32::from(snapshot)
                + u32::from(cull)
                + u32::from(self.reflection.is_some()),
            draws: 2 + u32::from(self.reflection.is_some()),
            cull_dispatches: 2 * u32::from(cull),
            snapshot_pixels: if snapshot {
                u64::from(self.width) * u64::from(self.height)
            } else {
                0
            },
            max_reflection_steps: self.worst_reflection_steps(),
            ..WaterWork::default()
        })
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        state: WaterState,
        budget: &mut UploadBudget,
        max_reflection_steps: u64,
    ) -> Result<WaterWork, WaterError> {
        self.timer.begin();
        if state
            .eye
            .iter()
            .chain(state.sun.iter())
            .chain([state.time_seconds, state.sun_intensity].iter())
            .any(|v| !v.is_finite())
            || state.sun_intensity < 0.
        {
            return Err(WaterError::InvalidState);
        }
        let inverse = state
            .camera
            .inverse_view_projection()
            .ok_or(WaterError::InvalidState)?;
        let length = state.sun.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !length.is_finite() || length < 1e-6 {
            return Err(WaterError::InvalidState);
        }
        if self.last == Some((state, self.revision)) {
            return Ok(WaterWork::default());
        }
        let max_steps = self.worst_reflection_steps();
        if max_steps > max_reflection_steps {
            return Err(WaterError::WorkBudget);
        }
        if budget.remaining_bytes < 256 {
            return Err(WaterError::UploadBudget);
        }
        let p = Params {
            view_projection: state.camera.view_projection,
            inverse_view_projection: inverse,
            eye_time: [state.eye[0], state.eye[1], state.eye[2], state.time_seconds],
            absorption: [
                self.config.absorption[0],
                self.config.absorption[1],
                self.config.absorption[2],
                self.config.turbidity,
            ],
            scattering: [
                self.config.scattering[0],
                self.config.scattering[1],
                self.config.scattering[2],
                self.config.reflection_tolerance,
            ],
            optics: [
                self.config.ior,
                self.config.roughness,
                self.config.max_thickness,
                self.config.refraction_scale,
            ],
            sun: [
                state.sun[0] / length,
                state.sun[1] / length,
                state.sun[2] / length,
                state.sun_intensity,
            ],
            image: [
                self.width,
                self.height,
                if self.surface_enabled {
                    self.arena
                        .as_ref()
                        .map_or(self.previous.len() as u32, |a| a.high)
                } else {
                    0
                },
                self.config.max_objects,
            ],
            quality: [
                0, // The optional fullscreen pass traces once per water sample.
                u32::from(state.underwater),
                self.config.reflection_distance.to_bits(),
                u32::from(self.arena.is_some()),
            ],
            waves: [
                self.config.wave_amplitude,
                self.config.wave_frequency[0],
                self.config.wave_frequency[1],
                self.config.wave_speed,
            ],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&p));
        let mut work = WaterWork {
            upload_bytes: 256,
            max_reflection_steps: max_steps,
            draws: 2,
            passes: 1,
            ..WaterWork::default()
        };
        if self.last_snapshot != Some((state.camera, state.scene_revision)) {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("opaque full-resolution snapshot for refraction"),
                timestamp_writes: self.timer.compute(1),
            });
            pass.set_pipeline(&self.snapshot);
            pass.set_bind_group(0, &self.snapshot_bind, &[]);
            pass.dispatch_workgroups(self.width.div_ceil(8), self.height.div_ceil(8), 1);
            work.snapshot_pixels = u64::from(self.width) * u64::from(self.height);
            work.passes += 1;
            self.last_snapshot = Some((state.camera, state.scene_revision));
        }
        if self.last_cull != Some((state.camera, self.revision)) {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("water GPU compact quad list"),
                timestamp_writes: self.timer.compute(2),
            });
            pass.set_bind_group(0, &self.cull_bind, &[]);
            pass.set_pipeline(&self.reset);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.cull);
            pass.dispatch_workgroups(
                self.arena
                    .as_ref()
                    .map_or(self.previous.len() as u32, |a| a.high)
                    .max(1)
                    .div_ceil(64),
                1,
                1,
            );
            work.cull_dispatches = 2;
            work.passes += 1;
            self.last_cull = Some((state.camera, self.revision));
        }
        if self.config.instrument {
            encoder.clear_buffer(&self.statistics, 0, None)
        }
        {
            let attachments = [
                Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa,
                    depth_slice: None,
                    resolve_target: (self.sample_count > 1 && self.reflection.is_none())
                        .then_some(&self.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                }),
                self.normal_foam
                    .as_ref()
                    .map(|view| wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
            ];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("physical water after opaque MSAA, before clouds"),
                color_attachments: if self.reflection.is_some() {
                    &attachments
                } else {
                    &attachments[..1]
                },
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.timer.render(3),
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_pipeline(&self.background);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.pipeline);
            pass.draw_indirect(&self.indirect, 0);
        }
        if let Some(reflection) = &self.reflection {
            let reflected = reflection
                .encode(encoder, max_reflection_steps, self.timer.render(4))
                .map_err(|_| WaterError::WorkBudget)?;
            work.passes += reflected.passes;
            work.draws += reflected.draws;
        }
        self.timer.finish(encoder);
        budget.remaining_bytes -= 256;
        self.last = Some((state, self.revision));
        Ok(work)
    }
    pub fn graph(&self) -> crate::gpu_time::PassGraph<4> {
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
    pub fn color_msaa(&self) -> &wgpu::TextureView {
        self.reflection
            .as_ref()
            .map_or(&self.msaa, |r| r.color_msaa())
    }
    pub fn depth_msaa(&self) -> &wgpu::TextureView {
        &self.depth
    }
    pub fn temporal_source(&self) -> WaterTemporalSource<'_> {
        WaterTemporalSource {
            bind: &self.bind,
            indirect: &self.indirect,
            depth: &self.depth,
            extent: [self.width, self.height],
            samples: self.sample_count,
            quads: self.active_quad_count(),
            enabled: self.surface_enabled,
            prepared: self.last.is_some(),
        }
    }
    /// Loading only; frame planning/encoding needs no layout handle allocation.
    pub fn temporal_layout(&self) -> wgpu::BindGroupLayout {
        self.pipeline.get_bind_group_layout(0)
    }
    pub fn resolved_view(&self) -> &wgpu::TextureView {
        self.reflection
            .as_ref()
            .map_or(&self.resolved_view, |r| r.resolved_view())
    }
    pub fn resolved_texture(&self) -> &wgpu::Texture {
        self.reflection
            .as_ref()
            .map_or(&self.resolved, |r| r.resolved_texture())
    }
    pub fn statistics_buffer(&self) -> &wgpu::Buffer {
        &self.statistics
    }
    pub fn indirect_buffer(&self) -> &wgpu::Buffer {
        &self.indirect
    }
    pub fn quad_buffer(&self) -> &wgpu::Buffer {
        &self.quads
    }
    pub fn take_work(&mut self) -> WaterWork {
        std::mem::take(&mut self.work)
    }
    pub fn invalidate(&mut self) {
        self.last = None;
        self.last_cull = None;
        self.last_snapshot = None
    }
}
/// The identical vertex declaration/body used by the physical surface. Global
/// optical declarations are retained, but this entry reaches bindings0..3 only.
pub fn temporal_vertex_source(samples: u32) -> String {
    let source = include_str!("water.wgsl");
    let (vertex, _) = source
        .split_once("@vertex fn background_vertex")
        .expect("water vertex source contract");
    crate::sampling::shader(vertex, samples).into_owned()
}
fn buffer_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    uniform: bool,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
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
    }
}
fn texture_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    depth: bool,
    multisampled: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
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
    }
}
fn buffer_binding(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn coalesce(spans: &mut Vec<Span>) {
    spans.sort_unstable_by_key(|s| s.start);
    let mut write = 0;
    for read in 0..spans.len() {
        let span = spans[read];
        if write > 0 && spans[write - 1].start + spans[write - 1].len == span.start {
            spans[write - 1].len += span.len;
        } else {
            spans[write] = span;
            write += 1;
        }
    }
    spans.truncate(write);
}
