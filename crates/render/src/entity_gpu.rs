//! Static model catalog + 64-byte entity records. Pose generation and visibility
//! run on GPU. Frame uploads contain no vertices or per-part CPU matrices.
use crate::{
    entity_models::{MODEL_KINDS, ModelCatalog, ModelVertex, model_atlas},
    light_volume::PackedLight,
    shadows::{
        DynamicShadowCaster, ShadowDamageTracker, ShadowGpu, ShadowPrepareWork, ShadowRect,
        WorldBounds, crop_camera,
    },
    terrain_gpu::{Camera, TerrainGeometry, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct EntityInstance {
    pub position: [f32; 3],
    pub body_yaw: f32,
    pub head_yaw: f32,
    pub pitch: f32,
    pub limb_phase: f32,
    pub limb_amount: f32,
    pub kind: u32,
    pub flags: u32,
    pub color: u32,
    pub id_low: u32,
    pub id_high: u32,
    pub death_roll: f32,
    pub scale: f32,
    pub reserved: f32,
}
const _: () = assert!(std::mem::size_of::<EntityInstance>() == 64);
/// Local human crouch and camera-only hiding do not hide its shadow.
pub const CROUCH: u32 = 256;
pub const PRIMARY_HIDDEN: u32 = 1024;
/// Block drops use six terrain layers in canonical Down/Up/North/South/West/East
/// order. Three finite normal floats retain the 64-byte public instance ABI.
pub const BLOCK_FACE_LAYERS: u32 = 2048;
/// Public pipe packets use a fixed, centered pose without dropped-item bob/spin.
pub const PIPE: u32 = 4096;
impl EntityInstance {
    pub fn set_block_layers(&mut self, layers: [u16; 6]) -> Result<(), EntityError> {
        if self.kind != 6 || layers.iter().any(|layer| *layer > 255) {
            return Err(EntityError::InvalidInstance);
        }
        let pack = |a: u16, b: u16| f32::from_bits(0x3f800000 | u32::from(a) | (u32::from(b) << 8));
        self.head_yaw = pack(layers[0], layers[1]);
        self.pitch = pack(layers[2], layers[3]);
        self.limb_amount = pack(layers[4], layers[5]);
        self.flags |= BLOCK_FACE_LAYERS;
        Ok(())
    }
    pub fn block_layers(&self) -> Option<[u16; 6]> {
        if self.kind != 6 || self.flags & BLOCK_FACE_LAYERS == 0 {
            return None;
        }
        let words = [
            self.head_yaw.to_bits(),
            self.pitch.to_bits(),
            self.limb_amount.to_bits(),
        ];
        Some(std::array::from_fn(|face| {
            ((words[face / 2] >> (face % 2 * 8)) & 255) as u16
        }))
    }
}
#[derive(Clone, Copy, Debug)]
pub struct EntityConfig {
    pub max_entities: u32,
    pub atlas_layers: u32,
}
impl Default for EntityConfig {
    fn default() -> Self {
        Self {
            max_entities: 8192,
            atlas_layers: 186,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntityError {
    InvalidConfig,
    InvalidInstance,
    InvalidCamera,
    UploadBudget,
    Assets(String),
}
impl std::fmt::Display for EntityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EntityError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntityWork {
    pub upload_bytes: u64,
    pub instance_records: u32,
    pub cpu_vertices: u32,
    pub startup_vertices: u32,
    pub pipeline_compilations: u32,
    pub cull_dispatches: u32,
    pub objects_considered: u32,
    pub camera_upload_bytes: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    view_projection: [[f32; 4]; 4],
    params: [u32; 4],
    reserved: [u32; 4],
}
struct EntityShadow {
    cull_cameras: [wgpu::Buffer; 3],
    cameras: [wgpu::Buffer; 3],
    render_binds: [wgpu::BindGroup; 3],
    cull_binds: [wgpu::BindGroup; 3],
    _visible: wgpu::Buffer,
    indirect: wgpu::Buffer,
    pipeline: wgpu::RenderPipeline,
}
pub struct EntityGpu {
    config: EntityConfig,
    item_layers: u32,
    instances: wgpu::Buffer,
    lights: wgpu::Buffer,
    vertices: wgpu::Buffer,
    visible: wgpu::Buffer,
    indirect: wgpu::Buffer,
    camera: wgpu::Buffer,
    render_bind: wgpu::BindGroup,
    cull_bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    previous: Vec<EntityInstance>,
    previous_lights: Vec<u32>,
    staging_lights: Vec<u32>,
    count: u32,
    revision: u64,
    shadow_revision: u64,
    shadow_damage: ShadowDamageTracker,
    shadow: Option<EntityShadow>,
    last_frame: Option<(Camera, u64)>,
    work: EntityWork,
}
impl EntityGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: EntityConfig,
        shadows: Option<&ShadowGpu>,
    ) -> Result<Self, EntityError> {
        Self::build(device, queue, geometry, config, shadows, None, None, None)
    }
    /// Prepare an independent nonblock item texture array once during loading.
    pub fn new_with_items(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: EntityConfig,
        shadows: Option<&ShadowGpu>,
        items: &crate::terrain_gpu::AtlasImage,
    ) -> Result<Self, EntityError> {
        Self::build(
            device,
            queue,
            geometry,
            config,
            shadows,
            Some(items),
            None,
            None,
        )
    }
    /// Optional shared atmosphere prepared during loading, including item sprites.
    pub fn new_with_environment(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: EntityConfig,
        shadows: Option<&ShadowGpu>,
        items: Option<&crate::terrain_gpu::AtlasImage>,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
    ) -> Result<Self, EntityError> {
        Self::build(
            device,
            queue,
            geometry,
            config,
            shadows,
            items,
            Some(atmosphere),
            None,
        )
    }
    /// Prepare optional reflected lighting together with the shared physical
    /// atmosphere. The disabled variant preserves the existing shader exactly.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_lighting(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: EntityConfig,
        shadows: Option<&ShadowGpu>,
        items: Option<&crate::terrain_gpu::AtlasImage>,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
        gi: Option<&crate::gi::GiGpu>,
    ) -> Result<Self, EntityError> {
        Self::build(
            device,
            queue,
            geometry,
            config,
            shadows,
            items,
            Some(atmosphere),
            gi,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn build(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: EntityConfig,
        shadows: Option<&ShadowGpu>,
        items: Option<&crate::terrain_gpu::AtlasImage>,
        atmosphere: Option<&crate::atmosphere::AtmosphereGpu>,
        gi: Option<&crate::gi::GiGpu>,
    ) -> Result<Self, EntityError> {
        let sample_count = geometry.sample_count;
        if config.max_entities == 0
            || (gi.is_some() && atmosphere.is_none())
            || config.atlas_layers == 0
            || u64::from(config.max_entities) * 64
                > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(EntityError::InvalidConfig);
        }
        let catalog = ModelCatalog::new();
        let atlas = model_atlas().map_err(EntityError::Assets)?;
        if atlas.layers > device.limits().max_texture_array_layers {
            return Err(EntityError::InvalidConfig);
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("isolated mob face arrays with linear mips"),
            size: wgpu::Extent3d {
                width: atlas.tile_size,
                height: atlas.tile_size,
                depth_or_array_layers: atlas.layers,
            },
            mip_level_count: atlas.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut asset_bytes = 0;
        for (level, mip) in atlas.mips.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mip.size * 4),
                    rows_per_image: Some(mip.size),
                },
                wgpu::Extent3d {
                    width: mip.size,
                    height: mip.size,
                    depth_or_array_layers: atlas.layers,
                },
            );
            asset_bytes += mip.rgba.len() as u64;
        }
        let item_layers = items.map_or(0, |a| a.layers);
        let item_view = if let Some(items) = items {
            if items.tile_size != 16
                || items.layers == 0
                || items.layers > device.limits().max_texture_array_layers
                || items.mips.len() != 5
                || items.mips[0].size != 16
                || items.mips[0].rgba.len() != items.layers as usize * 16 * 16 * 4
            {
                return Err(EntityError::InvalidConfig);
            }
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("independent original nonblock item sprites"),
                size: wgpu::Extent3d {
                    width: 16,
                    height: 16,
                    depth_or_array_layers: items.layers,
                },
                mip_level_count: items.mips.len() as u32,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            for (level, mip) in items.mips.iter().enumerate() {
                if mip.size != 16u32.checked_shr(level as u32).unwrap_or(0)
                    || mip.rgba.len()
                        != items.layers as usize * mip.size as usize * mip.size as usize * 4
                {
                    return Err(EntityError::InvalidConfig);
                }
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: level as u32,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &mip.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(mip.size * 4),
                        rows_per_image: Some(mip.size),
                    },
                    wgpu::Extent3d {
                        width: mip.size,
                        height: mip.size,
                        depth_or_array_layers: items.layers,
                    },
                );
                asset_bytes += mip.rgba.len() as u64;
            }
            Some(tex.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            }))
        } else {
            None
        };
        let texture = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sharp isolated model texels"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let instances = buffer(
            "entity64B instance records",
            u64::from(config.max_entities) * 64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let lights = buffer(
            "entity packed RGB skylight anchors",
            u64::from(config.max_entities) * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let vertices = buffer(
            "one immutable startup model catalog",
            catalog.vertices.len() as u64 * 64,
            wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        );
        queue.write_buffer(&vertices, 0, bytemuck::cast_slice(&catalog.vertices));
        let visible = buffer(
            "GPU compacted entities per model",
            u64::from(config.max_entities) * MODEL_KINDS as u64 * 4,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = buffer(
            "eight entity indirect model buckets",
            MODEL_KINDS as u64 * 16,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
        );
        let camera = buffer(
            "entity camera",
            96,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let ranges = buffer(
            "model range metadata",
            MODEL_KINDS as u64 * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let range_data = catalog
            .ranges
            .map(|range| [range.vertex_count, range.first_vertex, 0, 0]);
        queue.write_buffer(&ranges, 0, bytemuck::cast_slice(&range_data));
        let v = wgpu::ShaderStages::VERTEX;
        let f = wgpu::ShaderStages::FRAGMENT;
        let c = wgpu::ShaderStages::COMPUTE;
        let mut entries = vec![
            buffer_layout(0, v, true, true),
            buffer_layout(1, v, false, true),
            buffer_layout(2, v, false, true),
            buffer_layout(3, v, false, true),
            texture_layout(4, f, wgpu::TextureSampleType::Float { filterable: true }),
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: f,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture_layout(6, f, wgpu::TextureSampleType::Float { filterable: true }),
            texture_layout(7, f, wgpu::TextureSampleType::Float { filterable: true }),
        ];
        if shadows.is_some() {
            entries.extend([
                buffer_layout(9, f, true, true),
                texture_layout(10, f, wgpu::TextureSampleType::Depth),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: f,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ]);
        }
        if atmosphere.is_some() {
            entries.push(buffer_layout(20, f, false, true));
        }
        if gi.is_some() {
            crate::gi_model::layout(&mut entries);
        }
        let render_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("instanced entity catalog layout"),
            entries: &entries,
        });
        let dummy_footprint = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("inactive color cull footprint binding"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("entity independent GPU cull"),
            entries: &[
                buffer_layout(0, c, true, true),
                buffer_layout(1, c, false, true),
                buffer_layout(2, c, false, false),
                buffer_layout(3, c, false, false),
                buffer_layout(4, c, false, true),
                buffer_layout(5, c, false, false),
            ],
        });
        let mut entries = vec![
            buffer_binding(0, &camera),
            buffer_binding(1, &instances),
            buffer_binding(2, &visible),
            buffer_binding(3, &lights),
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&texture),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(geometry.colors),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::TextureView(
                    item_view.as_ref().unwrap_or(geometry.colors),
                ),
            },
        ];
        if let Some(shadows) = shadows {
            entries.extend([
                buffer_binding(9, shadows.sampling_buffer()),
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(shadows.depth_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(shadows.sampler()),
                },
            ]);
        }
        if let Some(atmosphere) = atmosphere {
            entries.push(buffer_binding(20, atmosphere.lighting_buffer()));
        }
        if let Some(gi) = gi {
            crate::gi_model::bindings(&mut entries, gi);
        }
        let render_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("entity atlas lights and instance tables"),
            layout: &render_layout,
            entries: &entries,
        });
        let cull_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("entity GPU visibility"),
            layout: &cull_layout,
            entries: &[
                buffer_binding(0, &camera),
                buffer_binding(1, &instances),
                buffer_binding(2, &visible),
                buffer_binding(3, &indirect),
                buffer_binding(4, &ranges),
                buffer_binding(5, &dummy_footprint),
            ],
        });
        let source = shader_source_with_ambient(
            shadows.is_some(),
            atmosphere.is_some(),
            gi.is_some(),
            geometry.ambient_mrt,
        )?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU model pose and texture shading"),
            source: wgpu::ShaderSource::Wgsl(source.clone().into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("entity model pipeline"),
            bind_group_layouts: &[&render_layout],
            push_constant_ranges: &[],
        });
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 24,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 32,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 44,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 48,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 52,
                shader_location: 6,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: 56,
                shader_location: 7,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 60,
                shader_location: 8,
            },
        ];
        let color_targets = crate::ambient_mrt::targets(None, geometry.ambient_mrt);
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("GPU animated entity HDR MSAA4"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ModelVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                }],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                alpha_to_coverage_enabled: sample_count > 1,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &color_targets,
            }),
            multiview: None,
            cache: None,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU entity frustum model buckets"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}\n{}",
                    crate::shadows::footprint::COMMON,
                    crate::shadows::footprint::MODEL,
                    include_str!("entity_cull.wgsl")
                )
                .into(),
            ),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("entity cull pipeline"),
            bind_group_layouts: &[&cull_layout],
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
        let cull = compute("cull");
        let shadow = if shadows.is_some_and(|shadows| shadows.cache().config().dynamic_casters) {
            let cameras = std::array::from_fn(|_| {
                buffer(
                    "dynamic model cascade camera",
                    96,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                )
            });
            let visible = buffer(
                "separate model shadow visibility",
                u64::from(config.max_entities) * MODEL_KINDS as u64 * 4,
                wgpu::BufferUsages::STORAGE,
            );
            let indirect = buffer(
                "separate model shadow indirect",
                128,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
            );
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("dynamic caster model resources"),
                entries: &[
                    buffer_layout(0, v, true, true),
                    buffer_layout(1, v, false, true),
                    buffer_layout(2, v, false, true),
                    buffer_layout(3, v, false, true),
                    texture_layout(4, f, wgpu::TextureSampleType::Float { filterable: true }),
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: f,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    texture_layout(6, f, wgpu::TextureSampleType::Float { filterable: true }),
                    texture_layout(7, f, wgpu::TextureSampleType::Float { filterable: true }),
                ],
            });
            let cull_cameras = std::array::from_fn(|_| {
                buffer(
                    "dynamic caster cropped cull camera",
                    96,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                )
            });
            let render_binds = std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("dynamic caster tables"),
                    layout: &layout,
                    entries: &[
                        buffer_binding(0, &cameras[i]),
                        buffer_binding(1, &instances),
                        buffer_binding(2, &visible),
                        buffer_binding(3, &lights),
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&texture),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::Sampler(&sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(geometry.colors),
                        },
                        wgpu::BindGroupEntry {
                            binding: 7,
                            resource: wgpu::BindingResource::TextureView(
                                item_view.as_ref().unwrap_or(geometry.colors),
                            ),
                        },
                    ],
                })
            });
            let cull_binds = std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("dynamic caster frustum tables"),
                    layout: &cull_layout,
                    entries: &[
                        buffer_binding(0, &cull_cameras[i]),
                        buffer_binding(1, &instances),
                        buffer_binding(2, &visible),
                        buffer_binding(3, &indirect),
                        buffer_binding(4, &ranges),
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: shadows
                                .and_then(|s| s.footprint_binding(i))
                                .unwrap_or_else(|| dummy_footprint.as_entire_binding()),
                        },
                    ],
                })
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("dynamic model shadow layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("same model pose for shadow depth"),
                source: wgpu::ShaderSource::Wgsl(source.clone().into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("dynamic posed model shadow depth"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: 64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    }],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Greater,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("shadow_fragment"),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                multiview: None,
                cache: None,
            });
            Some(EntityShadow {
                cull_cameras,
                cameras,
                render_binds,
                cull_binds,
                _visible: visible,
                indirect,
                pipeline,
            })
        } else {
            None
        };
        Ok(Self {
            config,
            item_layers,
            instances,
            lights,
            vertices,
            visible,
            indirect,
            camera,
            render_bind,
            cull_bind,
            pipeline,
            reset,
            cull,
            previous: Vec::with_capacity(config.max_entities as usize),
            previous_lights: Vec::with_capacity(config.max_entities as usize),
            staging_lights: Vec::with_capacity(config.max_entities as usize),
            count: 0,
            revision: 0,
            shadow_revision: 0,
            shadow_damage: ShadowDamageTracker::new(config.max_entities as usize),
            shadow,
            last_frame: None,
            work: EntityWork {
                startup_vertices: catalog.vertices.len() as u32,
                pipeline_compilations: 3 + u32::from(
                    shadows.is_some_and(|shadows| shadows.cache().config().dynamic_casters),
                ),
                upload_bytes: asset_bytes + catalog.vertices.len() as u64 * 64 + 128,
                ..EntityWork::default()
            },
        })
    }
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        instances: &[EntityInstance],
        lights: &[PackedLight],
        budget: &mut UploadBudget,
    ) -> Result<EntityWork, EntityError> {
        if instances.len() > self.config.max_entities as usize
            || (!lights.is_empty() && lights.len() != instances.len())
            || instances.iter().any(|i| {
                i.position
                    .iter()
                    .chain(
                        [
                            i.body_yaw,
                            i.head_yaw,
                            i.pitch,
                            i.limb_phase,
                            i.limb_amount,
                            i.death_roll,
                            i.scale,
                            i.reserved,
                        ]
                        .iter(),
                    )
                    .any(|v| !v.is_finite())
                    || i.kind >= MODEL_KINDS as u32
                    || i.scale <= 0.
                    || i.scale > 16.
                    || i.flags & !8191 != 0
                    || (i.flags & PIPE != 0 && (i.kind < 6 || i.flags & 32 == 0))
                    || (i.flags & BLOCK_FACE_LAYERS != 0
                        && (i.kind != 6
                            || [i.head_yaw, i.pitch, i.limb_amount]
                                .iter()
                                .any(|v| v.to_bits() & 0xffff0000 != 0x3f800000)
                            || i.block_layers().is_some_and(|layers| {
                                layers
                                    .iter()
                                    .any(|v| u32::from(*v) >= self.config.atlas_layers)
                            })))
                    || (i.kind == 2 && i.color >= 6)
                    || ((i.kind == 3 || i.kind == 5) && !f32::from_bits(i.color).is_finite())
                    || (i.kind == 6 && i.color >= self.config.atlas_layers)
                    || (i.kind == 7 && i.color >= self.item_layers)
            })
        {
            return Err(EntityError::InvalidInstance);
        }
        self.staging_lights.clear();
        self.staging_lights.extend(
            (0..instances.len()).map(|index| lights.get(index).map_or(15, |light| light.raw())),
        );
        let instance_changed = self.previous != instances;
        let visibility_changed =
            self.previous.len() != instances.len()
                || self.previous.iter().zip(instances).any(|(a, b)| {
                    a.position != b.position || a.scale != b.scale || a.kind != b.kind
                });
        let shadow_changed = self.previous.len() != instances.len()
            || self
                .previous
                .iter()
                .zip(instances)
                .any(|(a, b)| !same_shadow_geometry(a, b));
        let light_changed = self.previous_lights != self.staging_lights;
        if !instance_changed && !light_changed {
            return Ok(EntityWork::default());
        }
        let bytes = if instance_changed {
            instances.len() as u64 * 64
        } else {
            0
        } + if light_changed {
            instances.len() as u64 * 4
        } else {
            0
        };
        if budget.remaining_bytes < bytes {
            return Err(EntityError::UploadBudget);
        }
        if shadow_changed {
            for index in 0..self.previous.len().max(instances.len()) {
                let old = self.previous.get(index);
                let new = instances.get(index);
                if old
                    .zip(new)
                    .is_none_or(|(old, new)| !same_shadow_geometry(old, new))
                {
                    self.shadow_damage
                        .record(index, old.map(entity_shadow_bounds));
                }
            }
        }
        if instance_changed && !instances.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));
        }
        if light_changed && !self.staging_lights.is_empty() {
            queue.write_buffer(&self.lights, 0, bytemuck::cast_slice(&self.staging_lights));
        }
        self.previous.clear();
        self.previous.extend_from_slice(instances);
        self.previous_lights.clear();
        self.previous_lights.extend_from_slice(&self.staging_lights);
        self.count = instances.len() as u32;
        if visibility_changed {
            self.revision = self.revision.wrapping_add(1);
        }
        if shadow_changed {
            self.shadow_revision = self.shadow_revision.wrapping_add(1);
        }
        budget.remaining_bytes -= bytes;
        let work = EntityWork {
            upload_bytes: bytes,
            instance_records: if instance_changed { self.count } else { 0 },
            ..EntityWork::default()
        };
        self.work.upload_bytes += bytes;
        self.work.instance_records += work.instance_records;
        Ok(work)
    }
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: Camera,
    ) -> Result<EntityWork, EntityError> {
        if camera
            .view_projection
            .iter()
            .flatten()
            .any(|v| !v.is_finite())
        {
            return Err(EntityError::InvalidCamera);
        }
        if self.last_frame == Some((camera, self.revision)) {
            return Ok(EntityWork::default());
        }
        queue.write_buffer(
            &self.camera,
            0,
            bytemuck::bytes_of(&CameraGpu {
                view_projection: camera.view_projection,
                params: [self.count, self.config.max_entities, 0, 0],
                reserved: [0; 4],
            }),
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("entity frustum compact model instances"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &self.cull_bind, &[]);
            pass.set_pipeline(&self.reset);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.cull);
            pass.dispatch_workgroups(self.count.max(1).div_ceil(64), 1, 1);
        }
        self.last_frame = Some((camera, self.revision));
        Ok(EntityWork {
            cull_dispatches: 2,
            objects_considered: self.count,
            camera_upload_bytes: 96,
            ..EntityWork::default()
        })
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.render_bind, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        for model in 0..MODEL_KINDS as u64 {
            pass.draw_indirect(&self.indirect, model * 16);
        }
        MODEL_KINDS as u32
    }
    pub fn indirect_buffer(&self) -> &wgpu::Buffer {
        &self.indirect
    }
    pub fn model_vertex_buffer(&self) -> &wgpu::Buffer {
        &self.vertices
    }
    pub fn visible_buffer(&self) -> &wgpu::Buffer {
        &self.visible
    }
    pub fn take_work(&mut self) -> EntityWork {
        std::mem::take(&mut self.work)
    }
}
fn entity_shadow_bounds(value: &EntityInstance) -> WorldBounds {
    let scale = value.scale;
    let (radius, low, high) = if value.kind >= 6 {
        if value.flags & PIPE != 0 {
            (0.72 * scale, -0.5 * scale, 0.5 * scale)
        } else {
            (0.72 * scale, 0., scale + 0.2)
        }
    } else {
        (2.2 * scale, -0.3 * scale, 2.3 * scale)
    };
    WorldBounds {
        min: [
            value.position[0] - radius,
            value.position[1] + low,
            value.position[2] - radius,
        ],
        max: [
            value.position[0] + radius,
            value.position[1] + high,
            value.position[2] + radius,
        ],
    }
}
fn same_shadow_geometry(a: &EntityInstance, b: &EntityInstance) -> bool {
    a.position == b.position
        && a.body_yaw == b.body_yaw
        && a.head_yaw == b.head_yaw
        && a.pitch == b.pitch
        && a.limb_phase == b.limb_phase
        && a.limb_amount == b.limb_amount
        && a.kind == b.kind
        && (a.flags & (771 | BLOCK_FACE_LAYERS | PIPE))
            == (b.flags & (771 | BLOCK_FACE_LAYERS | PIPE))
        && a.death_roll == b.death_roll
        && a.scale == b.scale
        && a.reserved == b.reserved
        && (!(a.kind == 3 || a.kind >= 5) || a.color == b.color)
}

impl DynamicShadowCaster for EntityGpu {
    fn available(&self) -> bool {
        self.shadow.is_some()
    }
    fn shadow_revision(&self) -> u64 {
        self.shadow_revision
    }
    fn visit_shadow_damage(&self, previous: [u64; 4], visit: &mut dyn FnMut(WorldBounds)) -> bool {
        if previous[1..].iter().any(|v| *v != 0) {
            return false;
        }
        self.shadow_damage.visit(
            previous[0],
            self.shadow_revision,
            |index| self.previous.get(index).map(entity_shadow_bounds),
            visit,
        )
    }
    fn commit_shadow_damage(&mut self) {
        self.shadow_damage.commit(self.shadow_revision);
    }
    fn prepare_shadow(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
    ) -> u32 {
        self.prepare_shadow_region(queue, encoder, cascade, camera, ShadowRect::full(1), 1)
            .dispatches
    }
    fn prepare_shadow_region(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        rect: ShadowRect,
        resolution: u32,
    ) -> ShadowPrepareWork {
        if self.count == 0 {
            return ShadowPrepareWork::default();
        }
        let Some(shadow) = &self.shadow else {
            return ShadowPrepareWork::default();
        };
        for (buffer, camera) in [
            (&shadow.cameras[cascade], camera),
            (
                &shadow.cull_cameras[cascade],
                crop_camera(camera, rect, resolution),
            ),
        ] {
            queue.write_buffer(
                buffer,
                0,
                bytemuck::bytes_of(&CameraGpu {
                    view_projection: camera.view_projection,
                    params: [self.count, self.config.max_entities, 1, 0],
                    reserved: [0; 4],
                }),
            );
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("localized model independent shadow cull"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &shadow.cull_binds[cascade], &[]);
        pass.set_pipeline(&self.reset);
        pass.dispatch_workgroups(1, 1, 1);
        pass.set_pipeline(&self.cull);
        pass.dispatch_workgroups(self.count.div_ceil(64), 1, 1);
        ShadowPrepareWork {
            dispatches: 2,
            upload_bytes: 192,
            bounds_complete: false,
        }
    }
    fn prepare_shadow_sparse(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        resolution: u32,
    ) -> ShadowPrepareWork {
        let mut work = self.prepare_shadow_region(
            queue,
            encoder,
            cascade,
            camera,
            ShadowRect::full(resolution),
            resolution,
        );
        if work.dispatches != 0 {
            let shadow = self
                .shadow
                .as_ref()
                .expect("prepared model shadow resources");
            queue.write_buffer(
                &shadow.cull_cameras[cascade],
                80,
                bytemuck::bytes_of(&[resolution, 1u32, 0, 0]),
            );
            work.upload_bytes += 16;
        }
        work.bounds_complete = true;
        work
    }
    fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, cascade: usize) -> u32 {
        if self.count == 0 {
            return 0;
        }
        let Some(shadow) = &self.shadow else {
            return 0;
        };
        pass.set_pipeline(&shadow.pipeline);
        pass.set_bind_group(0, &shadow.render_binds[cascade], &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        for model in 0..8u64 {
            pass.draw_indirect(&shadow.indirect, model * 16);
        }
        8
    }
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
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
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

/// Loading/tooling source generated by the exact constructor path.
pub fn shader_source(shadows: bool, atmosphere: bool, gi: bool) -> Result<String, EntityError> {
    if gi && !atmosphere {
        return Err(EntityError::InvalidConfig);
    }
    let source = crate::atmosphere::surface_shader(
        include_str!("entity.wgsl"),
        shadows,
        atmosphere,
        true,
        if shadows {
            "return light.rgb+vec3(light.a*(0.15+0.85*cascade_shadow(world,normal)*max(dot(normal,normalize(shadow_sampling.sun.xyz)),0.0)));"
        } else {
            "return vec3(0.65+0.35*max(dot(normal,normalize(vec3(0.35,0.8,0.2))),0.));"
        },
    );
    let source = if gi {
        crate::gi_model::shader(source)
    } else {
        source
    };
    Ok(source)
}

/// Exact optional MRT constructor source for independent backend oracles.
pub fn shader_source_with_ambient(
    shadows: bool,
    atmosphere: bool,
    gi: bool,
    ambient: bool,
) -> Result<String, EntityError> {
    let source = shader_source(shadows, atmosphere, gi)?;
    Ok(if ambient {
        crate::ambient_mrt::entity(&source, shadows, atmosphere, gi)
    } else {
        source
    })
}
