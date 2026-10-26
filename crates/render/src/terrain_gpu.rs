//! GPU terrain: descriptor arena, compact indirect buckets and conservative Hi-Z.
//! Terrain is page-local; this module never reads or generates the world.
//! Shadows, distant LOD and physical water are later milestones.

use crate::{
    MSAA_SAMPLES, OutputTransfer,
    terrain_mesh::{MAX_PAGE_QUADS, MeshClass, OwnerPatch, PageMesh},
};
use bytemuck::{Pod, Zeroable};
mod materials;
pub use materials::{atmosphere_material_shader_source, material_shader_source};

const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const OBJECT_BYTES: u64 = 128;
const EDIT_MASK_BYTES: u64 = 512;
const ENABLED: u32 = 1;
const OVERLAY: u32 = 2;
const EDIT_MASK: u32 = 4;
const FIXED_PATCH: u32 = 8;
const PATCH_STATE_BYTES: u64 = 4096 * 4;

#[derive(Clone, Copy, Debug)]
pub struct GpuConfig {
    pub max_pages: u32,
    pub max_quads: u32,
    pub occlusion: bool,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            max_pages: 256,
            max_quads: 262_144,
            occlusion: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageHandle {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct UploadBudget {
    pub remaining_bytes: u64,
}
impl UploadBudget {
    pub const fn new(bytes: u64) -> Self {
        Self {
            remaining_bytes: bytes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerrainError {
    Capacity,
    UploadBudget,
    StaleHandle,
    StaleVersion,
    InvalidTransform,
    InvalidMaterial,
    InvalidSize,
    OcclusionDisabled,
    InvalidEdit,
    LatePreparation,
    SceneSamplingDisabled,
    InvalidRasterMesh,
}

impl std::fmt::Display for TerrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for TerrainError {}

/// Clip transform is column-major, right-handed, with reversed depth: near 1/far 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub view_projection: [[f32; 4]; 4],
}

impl Camera {
    pub fn inverse_view_projection(&self) -> Option<[[f32; 4]; 4]> {
        crate::atmosphere::inverse(self.view_projection)
    }
    pub fn perspective(
        eye: [f32; 3],
        target: [f32; 3],
        up: [f32; 3],
        fov_y: f32,
        aspect: f32,
        near: f32,
        far: f32,
    ) -> Self {
        let f = 1.0 / (fov_y * 0.5).tan();
        let projection = [
            [f / aspect, 0.0, 0.0, 0.0],
            [0.0, f, 0.0, 0.0],
            [0.0, 0.0, near / (far - near), -1.0],
            [0.0, 0.0, far * near / (far - near), 0.0],
        ];
        Self {
            view_projection: multiply(projection, look_at(eye, target, up)),
        }
    }

    pub fn orthographic(
        eye: [f32; 3],
        target: [f32; 3],
        up: [f32; 3],
        half_height: f32,
        aspect: f32,
        near: f32,
        far: f32,
    ) -> Self {
        let projection = [
            [1.0 / (half_height * aspect), 0.0, 0.0, 0.0],
            [0.0, 1.0 / half_height, 0.0, 0.0],
            [0.0, 0.0, 1.0 / (far - near), 0.0],
            [0.0, 0.0, far / (far - near), 1.0],
        ];
        Self {
            view_projection: multiply(projection, look_at(eye, target, up)),
        }
    }
}

pub fn translation(origin: [f32; 3]) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [origin[0], origin[1], origin[2], 1.0],
    ]
}

fn multiply(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    std::array::from_fn(|c| std::array::from_fn(|r| (0..4).map(|k| a[k][r] * b[c][k]).sum()))
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalized(a: [f32; 3]) -> [f32; 3] {
    let n = dot(a, a).sqrt();
    [a[0] / n, a[1] / n, a[2] / n]
}
fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalized([target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]]);
    let s = normalized(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CullingMode {
    None,
    Frustum,
    FrustumAndHiZ,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TerrainFrameWork {
    pub objects_considered: u32,
    pub draw_calls: u32,
    pub passes: u32,
    pub camera_upload_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuPassKind {
    Unused,
    ResetVisibility,
    CullFrustum,
    DepthPrepass,
    HiZBase,
    HiZReduce,
    CullOcclusion,
    Color,
    Presentation,
}

#[derive(Clone, Copy, Debug)]
pub struct GpuFrameGraph {
    pub passes: [GpuPassKind; 32],
    pub count: u32,
    pub has_gpu_timestamps: bool,
}

struct PassQueries {
    set: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    period: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    view_projection: [[f32; 4]; 4],
    viewport: [f32; 4],
    params: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjectGpu {
    model: [[f32; 4]; 4],
    bounds_min: [f32; 4],
    bounds_max: [f32; 4],
    ranges: [[u32; 2]; 3],
    generation: u32,
    active: u32,
}
const _: () = assert!(std::mem::size_of::<ObjectGpu>() == 128);

#[derive(Clone, Copy)]
struct Span {
    start: u32,
    len: u32,
}
struct Slot {
    generation: u32,
    version: u64,
    span: Option<Span>,
    allocated: bool,
    object: ObjectGpu,
    neighbours: [[u32; 2]; 6],
}

struct Targets {
    sampled: bool,
    _color: wgpu::Texture,
    color: wgpu::TextureView,
    _resolved: wgpu::Texture,
    resolved: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth: wgpu::TextureView,
    tone_bind: wgpu::BindGroup,
}

struct HiZLevel {
    width: u32,
    height: u32,
    bind: wgpu::BindGroup,
}
struct HiZ {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    base_bind: wgpu::BindGroup,
    cull_bind: wgpu::BindGroup,
    levels: Vec<HiZLevel>,
    width: u32,
    height: u32,
}

/// Shared immutable terrain inputs for independent camera passes such as cascaded shadows.
/// Consumers must compact their own visibility rather than reuse the player's culled list.
pub struct TerrainGeometry<'a> {
    pub sample_count: u32,
    pub ambient_mrt: bool,
    pub objects: &'a wgpu::Buffer,
    pub quads: &'a wgpu::Buffer,
    pub materials: &'a wgpu::Buffer,
    pub colors: &'a wgpu::TextureView,
    pub tint_masks: &'a wgpu::TextureView,
    pub sampler: &'a wgpu::Sampler,
    pub shadow_alpha_state: &'a std::sync::Arc<crate::shadow_alpha::AtlasState>,
    pub edit_masks: &'a wgpu::Buffer,
    pub edit_neighbors: &'a wgpu::Buffer,
    pub patch_states: &'a wgpu::Buffer,
    pub max_pages: u32,
    pub max_quads: u32,
}

pub struct TerrainGpu {
    sample_count: u32,
    config: GpuConfig,
    unit_raster: bool,
    width: u32,
    height: u32,
    output_format: wgpu::TextureFormat,
    objects: wgpu::Buffer,
    quads: wgpu::Buffer,
    visible: wgpu::Buffer,
    indirect: wgpu::Buffer,
    edit_masks: wgpu::Buffer,
    edit_neighbors: wgpu::Buffer,
    patch_states: wgpu::Buffer,
    patch_clear: Box<[u32; 4096]>,
    patch_state_staging: Vec<u32>,
    materials: wgpu::Buffer,
    shadow_alpha_classification: Box<[u32]>,
    shadow_alpha_state: std::sync::Arc<crate::shadow_alpha::AtlasState>,
    colors: wgpu::TextureView,
    tint_masks: wgpu::TextureView,
    sampler: wgpu::Sampler,
    cameras: [wgpu::Buffer; 2],
    render_bind: wgpu::BindGroup,
    render_layout: wgpu::BindGroupLayout,
    lighting_bindings: Option<materials::LightingBindings>,
    local_materials: Option<materials::MaterialState>,
    frustum_bind: wgpu::BindGroup,
    cull_layout: wgpu::BindGroupLayout,
    hiz_base_layout: Option<wgpu::BindGroupLayout>,
    hiz_reduce_layout: Option<wgpu::BindGroupLayout>,
    tone_layout: wgpu::BindGroupLayout,
    opaque_pipeline: wgpu::RenderPipeline,
    cutout_pipeline: wgpu::RenderPipeline,
    masked_opaque_pipeline: wgpu::RenderPipeline,
    masked_cutout_pipeline: wgpu::RenderPipeline,
    depth_pipeline: Option<wgpu::RenderPipeline>,
    masked_depth_pipeline: Option<wgpu::RenderPipeline>,
    sky_pipeline: wgpu::RenderPipeline,
    atmosphere_sky: Option<crate::atmosphere::AtmosphereSkyGpu>,
    atmosphere_lighting: Option<wgpu::Buffer>,
    tone_pipeline: wgpu::RenderPipeline,
    reset_pipeline: wgpu::ComputePipeline,
    frustum_pipeline: wgpu::ComputePipeline,
    hiz_cull_pipeline: Option<wgpu::ComputePipeline>,
    hiz_base_pipeline: Option<wgpu::ComputePipeline>,
    hiz_reduce_pipeline: Option<wgpu::ComputePipeline>,
    targets: Targets,
    ambient: Option<(wgpu::Texture, wgpu::TextureView)>,
    hiz: Option<HiZ>,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    free_spans: Vec<Span>,
    allocation_scratch: Vec<Span>,
    staging: Vec<[u32; 4]>,
    atlas_layers: u32,
    last_camera: Option<(Camera, CullingMode)>,
    scene_revision: u64,
    hdr_revision: u64,
    last_render: Option<(Camera, CullingMode, u64)>,
    work: telemetry::WorkCounters,
    pass_queries: Option<PassQueries>,
    pass_count: std::cell::Cell<u32>,
    pass_kinds: std::cell::Cell<[GpuPassKind; 32]>,
}

impl TerrainGpu {
    /// CPU asset decoding and all pipeline creation belong to loading, before frames.
    /// Native callers can run this constructor in their loading worker; the async
    /// facade allows browser startup to retain its loading/error UI.
    #[allow(clippy::too_many_arguments)] // One startup operation mirrors surface/device initialization.
    pub async fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        transfer: OutputTransfer,
        config: GpuConfig,
        atlas: &AtlasImage,
    ) -> Result<Self, TerrainError> {
        Self::new(
            device, queue, format, width, height, transfer, config, atlas,
        )
    }

    #[allow(clippy::too_many_arguments)] // Keep the same stable constructor as the startup facade.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        transfer: OutputTransfer,
        config: GpuConfig,
        atlas: &AtlasImage,
    ) -> Result<Self, TerrainError> {
        Self::new_with_samples(
            device,
            queue,
            format,
            width,
            height,
            transfer,
            config,
            atlas,
            MSAA_SAMPLES,
        )
    }
    /// Select a prepared WebGPU sample variant during loading. Existing callers
    /// retain four samples; one sample never allocates a multisample color target.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        transfer: OutputTransfer,
        config: GpuConfig,
        atlas: &AtlasImage,
        sample_count: u32,
    ) -> Result<Self, TerrainError> {
        if !matches!(sample_count, 1 | 4) {
            return Err(TerrainError::InvalidSize);
        }
        let limits = device.limits();
        let padded_width = width.checked_next_power_of_two().unwrap_or(u32::MAX);
        let padded_height = height.checked_next_power_of_two().unwrap_or(u32::MAX);
        let largest_storage = (u64::from(config.max_pages) * PATCH_STATE_BYTES)
            .max(u64::from(config.max_quads) * 6 * 4);
        if width == 0
            || height == 0
            || config.max_pages == 0
            || config.max_quads == 0
            || padded_width > limits.max_texture_dimension_2d
            || padded_height > limits.max_texture_dimension_2d
            || largest_storage > u64::from(limits.max_storage_buffer_binding_size)
            || largest_storage > limits.max_buffer_size
            || atlas.layers == 0
            || atlas.layers > limits.max_texture_array_layers
            || atlas.tile_size > limits.max_texture_dimension_2d
        {
            return Err(TerrainError::InvalidSize);
        }
        assert!(transfer != OutputTransfer::ShaderSrgb || !format.is_srgb());
        let shadow_alpha_classification =
            crate::shadow_alpha::classify_mip0(atlas).map_err(|_| TerrainError::InvalidMaterial)?;
        let storage = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let objects = storage(
            "terrain object table",
            u64::from(config.max_pages) * OBJECT_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let quads = storage(
            "16-byte terrain descriptors",
            u64::from(config.max_quads) * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let visible = storage(
            "GPU compacted quad indices",
            u64::from(config.max_quads) * 6 * 4,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = storage(
            "ordinary and edited terrain indirect buckets",
            96,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
        );
        let edit_masks = storage(
            "old geometry owner-cell edit masks",
            u64::from(config.max_pages) * EDIT_MASK_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let edit_neighbors = storage(
            "generational page neighbor links",
            u64::from(config.max_pages) * 56,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let patch_states = storage(
            "six-face owner patch states",
            u64::from(config.max_pages) * PATCH_STATE_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let cameras = std::array::from_fn(|_| {
            storage(
                "terrain camera",
                96,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            )
        });
        let compute = wgpu::ShaderStages::COMPUTE;
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let render_layout = terrain_render_layout(device, false, false, false, false);
        let frustum_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frustum compute tables"),
            entries: &[
                buffer_entry(0, compute, true, true),
                buffer_entry(1, compute, false, true),
                buffer_entry(2, compute, false, false),
                buffer_entry(3, compute, false, false),
                buffer_entry(5, compute, false, true),
                buffer_entry(6, compute, false, true),
                buffer_entry(7, compute, false, true),
            ],
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Hi-Z cull tables"),
            entries: &[
                buffer_entry(0, compute, true, true),
                buffer_entry(1, compute, false, true),
                buffer_entry(2, compute, false, false),
                buffer_entry(3, compute, false, false),
                buffer_entry(5, compute, false, true),
                buffer_entry(6, compute, false, true),
                buffer_entry(7, compute, false, true),
                texture_entry(
                    4,
                    compute,
                    wgpu::TextureViewDimension::D2,
                    wgpu::TextureSampleType::Float { filterable: false },
                    false,
                ),
            ],
        });
        let tone_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("HDR presentation"),
            entries: &[texture_entry(
                0,
                fragment,
                wgpu::TextureViewDimension::D2,
                wgpu::TextureSampleType::Float { filterable: false },
                false,
            )],
        });
        let (atlas_view, mask_view, texture_bytes) = upload_atlas(device, queue, atlas);
        let materials: Vec<[u32; 4]> = (0..atlas.layers)
            .map(|layer| {
                crate::shadow_alpha::merge(
                    crate::material_pack::builtin_material(layer),
                    &shadow_alpha_classification,
                    true,
                )
            })
            .collect();
        let material_buffer = storage(
            "material table",
            u64::from(atlas.layers) * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        queue.write_buffer(&material_buffer, 0, bytemuck::cast_slice(&materials));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sharp repeating material texels"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let render_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("all terrain tables"),
            layout: &render_layout,
            entries: &[
                buffer_binding(0, &cameras[0]),
                buffer_binding(1, &objects),
                buffer_binding(2, &quads),
                buffer_binding(3, &visible),
                buffer_binding(4, &material_buffer),
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                buffer_binding(8, &edit_masks),
                buffer_binding(16, &edit_neighbors),
            ],
        });
        let frustum_bind = compute_bind(
            device,
            &frustum_layout,
            &cameras[0],
            &objects,
            &visible,
            &indirect,
            &edit_masks,
            &patch_states,
            &quads,
            None,
        );
        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("terrain pipeline layout"),
                bind_group_layouts: &[&render_layout],
                push_constant_ranges: &[],
            });
        let terrain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("descriptor vertex pulling terrain"),
            source: wgpu::ShaderSource::Wgsl(include_str!("terrain.wgsl").into()),
        });
        let opaque_pipeline = terrain_pipeline(
            device,
            &render_pipeline_layout,
            &terrain_shader,
            false,
            false,
            false,
            sample_count,
            false,
        );
        let cutout_pipeline = terrain_pipeline(
            device,
            &render_pipeline_layout,
            &terrain_shader,
            true,
            false,
            false,
            sample_count,
            false,
        );
        let masked_opaque_pipeline = terrain_pipeline(
            device,
            &render_pipeline_layout,
            &terrain_shader,
            false,
            false,
            true,
            sample_count,
            false,
        );
        let masked_cutout_pipeline = terrain_pipeline(
            device,
            &render_pipeline_layout,
            &terrain_shader,
            true,
            false,
            true,
            sample_count,
            false,
        );
        let depth_pipeline = config.occlusion.then(|| {
            terrain_pipeline(
                device,
                &render_pipeline_layout,
                &terrain_shader,
                false,
                true,
                false,
                sample_count,
                false,
            )
        });
        let masked_depth_pipeline = config.occlusion.then(|| {
            terrain_pipeline(
                device,
                &render_pipeline_layout,
                &terrain_shader,
                false,
                true,
                true,
                sample_count,
                false,
            )
        });
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shared sky in HDR graph"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sky.wgsl").into()),
        });
        let empty_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky graph layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("HDR sky MSAA4"),
            layout: Some(&empty_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: Some(depth_state(false, wgpu::CompareFunction::Always)),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: Some("fragment_hdr_sky"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let tone_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("HDR output transfer"),
            source: wgpu::ShaderSource::Wgsl(TONE_SHADER.into()),
        });
        let tone_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tone layout"),
            bind_group_layouts: &[&tone_layout],
            push_constant_ranges: &[],
        });
        let tone_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("HDR presentation"),
            layout: Some(&tone_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &tone_shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &tone_shader,
                entry_point: Some(match transfer {
                    OutputTransfer::Attachment => "linear_output",
                    OutputTransfer::ShaderSrgb => "srgb_output",
                }),
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
        let cull_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU frustum and conservative Hi-Z"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cull.wgsl").into()),
        });
        let frustum_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("frustum layout"),
            bind_group_layouts: &[&frustum_layout],
            push_constant_ranges: &[],
        });
        let cull_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("occlusion layout"),
            bind_group_layouts: &[&cull_layout],
            push_constant_ranges: &[],
        });
        let reset_pipeline = compute_pipeline(device, &frustum_pl, &cull_shader, "reset");
        let frustum_pipeline = compute_pipeline(device, &frustum_pl, &cull_shader, "frustum");
        let hiz_cull_pipeline = config
            .occlusion
            .then(|| compute_pipeline(device, &cull_pl, &cull_shader, "occlusion"));
        let targets = create_targets(device, width, height, &tone_layout, false, sample_count);
        let (hiz_base_layout, hiz_reduce_layout, hiz_base_pipeline, hiz_reduce_pipeline, hiz) =
            if config.occlusion {
                let base = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("MSAA reverse-Z to Hi-Z"),
                    entries: &[
                        texture_entry(
                            0,
                            compute,
                            wgpu::TextureViewDimension::D2,
                            wgpu::TextureSampleType::Depth,
                            sample_count > 1,
                        ),
                        storage_texture_entry(1, compute),
                    ],
                });
                let reduce = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("conservative Hi-Z reduction"),
                    entries: &[
                        texture_entry(
                            0,
                            compute,
                            wgpu::TextureViewDimension::D2,
                            wgpu::TextureSampleType::Float { filterable: false },
                            false,
                        ),
                        storage_texture_entry(1, compute),
                    ],
                });
                let base_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("reverse-Z coverage depth"),
                    source: wgpu::ShaderSource::Wgsl(crate::sampling::shader(
                        include_str!("depth.wgsl"),
                        sample_count,
                    )),
                });
                let reduce_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("minimum reverse-Z pyramid"),
                    source: wgpu::ShaderSource::Wgsl(include_str!("hiz.wgsl").into()),
                });
                let base_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("base depth layout"),
                    bind_group_layouts: &[&base],
                    push_constant_ranges: &[],
                });
                let reduce_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("reduce depth layout"),
                    bind_group_layouts: &[&reduce],
                    push_constant_ranges: &[],
                });
                let bp = compute_pipeline(device, &base_pl, &base_shader, "main");
                let rp = compute_pipeline(device, &reduce_pl, &reduce_shader, "main");
                let hiz = create_hiz(
                    device,
                    width,
                    height,
                    &targets.depth,
                    &base,
                    &reduce,
                    &cull_layout,
                    &cameras[1],
                    &objects,
                    &visible,
                    &indirect,
                    &edit_masks,
                    &patch_states,
                    &quads,
                );
                (Some(base), Some(reduce), Some(bp), Some(rp), Some(hiz))
            } else {
                (None, None, None, None, None)
            };
        let mut work = telemetry::WorkCounters::new();
        work.record(telemetry::WorkCounts {
            pipeline_compilations: if config.occlusion { 13 } else { 8 },
            upload_bytes: texture_bytes + u64::from(atlas.layers) * 16,
            ..telemetry::WorkCounts::ZERO
        });
        let mut free_spans = Vec::with_capacity(config.max_pages as usize + 2);
        free_spans.push(Span {
            start: 0,
            len: config.max_quads,
        });
        let pass_queries = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| PassQueries {
                set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("terrain pass timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 64,
                }),
                resolved: storage(
                    "terrain timestamp resolve",
                    512,
                    wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                ),
                period: queue.get_timestamp_period(),
            });
        Ok(Self {
            sample_count,
            config,
            unit_raster: false,
            width,
            height,
            output_format: format,
            objects,
            quads,
            visible,
            indirect,
            edit_masks,
            edit_neighbors,
            patch_states,
            patch_clear: Box::new([0; 4096]),
            patch_state_staging: Vec::with_capacity(4096),
            materials: material_buffer,
            shadow_alpha_classification,
            shadow_alpha_state: std::sync::Arc::new(crate::shadow_alpha::AtlasState::default()),
            colors: atlas_view,
            tint_masks: mask_view,
            sampler,
            cameras,
            render_bind,
            render_layout,
            lighting_bindings: None,
            local_materials: None,
            frustum_bind,
            cull_layout,
            hiz_base_layout,
            hiz_reduce_layout,
            tone_layout,
            opaque_pipeline,
            cutout_pipeline,
            masked_opaque_pipeline,
            masked_cutout_pipeline,
            depth_pipeline,
            masked_depth_pipeline,
            sky_pipeline,
            atmosphere_sky: None,
            atmosphere_lighting: None,
            tone_pipeline,
            reset_pipeline,
            frustum_pipeline,
            hiz_cull_pipeline,
            hiz_base_pipeline,
            hiz_reduce_pipeline,
            targets,
            ambient: None,
            hiz,
            slots: (0..config.max_pages)
                .map(|_| Slot {
                    generation: 0,
                    version: 0,
                    span: None,
                    allocated: false,
                    object: ObjectGpu::zeroed(),
                    neighbours: [[0; 2]; 6],
                })
                .collect(),
            free_slots: (0..config.max_pages).rev().collect(),
            free_spans,
            allocation_scratch: Vec::with_capacity(config.max_pages as usize + 2),
            staging: Vec::with_capacity(MAX_PAGE_QUADS),
            atlas_layers: atlas.layers,
            last_camera: None,
            scene_revision: 0,
            hdr_revision: 0,
            last_render: None,
            work,
            pass_queries,
            pass_count: std::cell::Cell::new(0),
            pass_kinds: std::cell::Cell::new([GpuPassKind::Unused; 32]),
        })
    }

    /// Keep CPU greedy descriptors, but rasterize each visible voxel face on
    /// the same unit grid as H1 overlays. No new arena, pipeline or visibility
    /// allocation is needed. Must precede uploads and the first encoded frame.
    /// A conservative slot bound makes every possible bucket fit its capacity:
    /// each allocated object contributes at most 4096*6 visible unit entries.
    pub fn prepare_unit_rasterization(&mut self) -> Result<u32, TerrainError> {
        if self.last_render.is_some() || self.free_slots.len() != self.slots.len() {
            return Err(TerrainError::LatePreparation);
        }
        if self.config.max_quads > (1 << 24) || self.config.max_quads < MAX_PAGE_QUADS as u32 {
            return Err(TerrainError::Capacity);
        }
        self.unit_raster = true;
        self.last_camera = None;
        self.invalidate();
        Ok(self.config.max_quads / MAX_PAGE_QUADS as u32)
    }
    pub fn unit_rasterization(&self) -> bool {
        self.unit_raster
    }
    pub fn unit_object_limit(&self) -> Option<u32> {
        self.unit_raster
            .then_some((self.config.max_quads / MAX_PAGE_QUADS as u32).min(self.config.max_pages))
    }
    fn can_allocate_slots(&self, count: usize) -> bool {
        self.free_slots.len() >= count
            && self.unit_object_limit().is_none_or(|limit| {
                self.slots.len() - self.free_slots.len() + count <= limit as usize
            })
    }
    fn validate_raster_mesh(&self, mesh: &PageMesh) -> Result<(), TerrainError> {
        if !self.unit_raster {
            return Ok(());
        }
        let mut units = 0usize;
        for &quad in mesh.iter() {
            crate::terrain_mesh::QuadDescriptor::from_le_bytes(quad.to_le_bytes())
                .map_err(|_| TerrainError::InvalidRasterMesh)?;
            let [width, height] = quad.size();
            let area = usize::from(width) * usize::from(height);
            if area > 1 && quad.ao().iter().any(|&ao| ao != quad.ao()[0]) {
                return Err(TerrainError::InvalidRasterMesh);
            }
            units += area;
            if units > MAX_PAGE_QUADS {
                return Err(TerrainError::Capacity);
            }
        }
        Ok(())
    }

    pub fn take_work(&mut self) -> telemetry::WorkCounts {
        self.work.take()
    }
    pub fn indirect_buffer(&self) -> &wgpu::Buffer {
        &self.indirect
    }

    /// Loading-screen configuration only: prepare lit variants and bind separate
    /// light/shadow resources before the first frame. No runtime shader compilation.
    pub fn attach_lighting(
        &mut self,
        device: &wgpu::Device,
        shadows: &crate::shadows::ShadowGpu,
        lights: &crate::light_volume::LightVolumeGpu,
    ) -> Result<(), TerrainError> {
        self.prepare_lighting(device, shadows, lights, None)
    }

    /// Optional reflected-light receiver. The GI source contains reflected
    /// outgoing radiance only; CPU block light and sunlight remain separate.
    pub fn attach_lighting_with_gi(
        &mut self,
        device: &wgpu::Device,
        shadows: &crate::shadows::ShadowGpu,
        lights: &crate::light_volume::LightVolumeGpu,
        gi: &crate::gi::GiGpu,
    ) -> Result<(), TerrainError> {
        self.prepare_lighting(device, shadows, lights, Some(gi))
    }

    fn prepare_lighting(
        &mut self,
        device: &wgpu::Device,
        shadows: &crate::shadows::ShadowGpu,
        lights: &crate::light_volume::LightVolumeGpu,
        gi: Option<&crate::gi::GiGpu>,
    ) -> Result<(), TerrainError> {
        if self.last_render.is_some() {
            return Err(TerrainError::LatePreparation);
        }
        if lights.buffer().size()
            < u64::from(self.config.max_pages) * crate::light_volume::LIGHT_PAGE_BYTES
        {
            return Err(TerrainError::InvalidSize);
        }
        self.lighting_bindings = Some(materials::LightingBindings::new(shadows, lights, gi));
        self.rebuild_material_pipelines(device);
        Ok(())
    }

    /// Select the prepared physical sky during loading. The owner updates its
    /// LUT before each affected frame and invalidates this renderer after changes.
    pub fn attach_atmosphere(
        &mut self,
        device: &wgpu::Device,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
    ) -> Result<(), TerrainError> {
        self.attach_environment_lighting(device, atmosphere, None, None)
    }
    /// Configure physical sky/surface illumination and optional M4/GI together,
    /// preserving local material bindings and compiling each final variant once.
    pub fn attach_environment_lighting(
        &mut self,
        device: &wgpu::Device,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
        lighting: Option<(
            &crate::shadows::ShadowGpu,
            &crate::light_volume::LightVolumeGpu,
        )>,
        gi: Option<&crate::gi::GiGpu>,
    ) -> Result<(), TerrainError> {
        if self.last_render.is_some() {
            return Err(TerrainError::LatePreparation);
        }
        if gi.is_some() && lighting.is_none() {
            return Err(TerrainError::InvalidSize);
        }
        if let Some((shadows, lights)) = lighting {
            if lights.buffer().size()
                < u64::from(self.config.max_pages) * crate::light_volume::LIGHT_PAGE_BYTES
            {
                return Err(TerrainError::InvalidSize);
            }
            self.lighting_bindings = Some(materials::LightingBindings::new(shadows, lights, gi));
        }
        self.atmosphere_sky = Some(
            crate::atmosphere::AtmosphereSkyGpu::new_with_samples_and_ambient(
                device,
                atmosphere.sky_view(),
                self.sample_count,
                self.ambient.is_some(),
            ),
        );
        self.atmosphere_lighting = Some(atmosphere.lighting_buffer().clone());
        self.work.record(telemetry::WorkCounts {
            pipeline_compilations: 1,
            ..telemetry::WorkCounts::ZERO
        });
        self.rebuild_material_pipelines(device);
        Ok(())
    }

    pub fn geometry(&self) -> TerrainGeometry<'_> {
        TerrainGeometry {
            sample_count: self.sample_count,
            ambient_mrt: self.ambient.is_some(),
            objects: &self.objects,
            quads: &self.quads,
            materials: &self.materials,
            colors: &self.colors,
            tint_masks: &self.tint_masks,
            sampler: &self.sampler,
            shadow_alpha_state: &self.shadow_alpha_state,
            edit_masks: &self.edit_masks,
            edit_neighbors: &self.edit_neighbors,
            patch_states: &self.patch_states,
            max_pages: self.config.max_pages,
            max_quads: self.config.max_quads,
        }
    }
    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }
    pub fn extent(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    /// Diagnostic readback of minimum opaque reverse-Z depth across all MSAA samples.
    /// Valid after a submitted `FrustumAndHiZ` frame; mip zero is padded to powers of two.
    pub fn conservative_depth_texture(&self) -> Option<&wgpu::Texture> {
        self.hiz.as_ref().map(|hiz| &hiz._texture)
    }

    /// Prepare read-only scene MSAA inputs for water/cloud composition during loading.
    /// Replaces only the color target; depth, resolved HDR and pipelines are retained.
    pub fn prepare_scene_sampling(&mut self, device: &wgpu::Device) -> Result<(), TerrainError> {
        if self.last_render.is_some() {
            return Err(TerrainError::LatePreparation);
        }
        if !self.targets.sampled {
            if self.sample_count > 1 {
                let color =
                    create_scene_color(device, self.width, self.height, true, self.sample_count);
                self.targets.color = color.create_view(&Default::default());
                self.targets._color = color;
            }
            self.targets.sampled = true;
        }
        Ok(())
    }

    /// Select a second signed ambient radiance-difference output before sky/model preparation or frames.
    pub fn prepare_ambient_mrt(&mut self, device: &wgpu::Device) -> Result<(), TerrainError> {
        if self.last_render.is_some() || (self.ambient.is_none() && self.atmosphere_sky.is_some()) {
            return Err(TerrainError::LatePreparation);
        }
        if self.ambient.is_none() {
            self.ambient = Some(create_ambient(
                device,
                self.width,
                self.height,
                self.sample_count,
            ));
            self.sky_pipeline = make_ambient_sky(device, self.sample_count);
            self.work.record(telemetry::WorkCounts {
                pipeline_compilations: 1,
                ..telemetry::WorkCounts::ZERO
            });
            self.rebuild_material_pipelines(device);
        }
        Ok(())
    }
    pub fn ambient_msaa_view(&self) -> Option<&wgpu::TextureView> {
        self.ambient.as_ref().map(|pair| &pair.1)
    }
    pub fn ambient_target_bytes(&self) -> u64 {
        u64::from(self.ambient.is_some())
            * u64::from(self.width)
            * u64::from(self.height)
            * u64::from(self.sample_count)
            * 8
    }
    pub fn scene_color_msaa_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.sampled.then_some(&self.targets.color)
    }
    pub fn scene_depth_msaa_view(&self) -> &wgpu::TextureView {
        &self.targets.depth
    }
    pub fn resolved_hdr_view(&self) -> &wgpu::TextureView {
        &self.targets.resolved
    }
    /// Color-input epoch, including geometry, textures and material bindings.
    pub fn visual_revision(&self) -> u64 {
        self.scene_revision
    }
    /// Changes after each newly encoded HDR scene, including camera movement.
    /// The caller must submit that encoder before using this revision as a cache key.
    pub fn hdr_revision(&self) -> u64 {
        self.hdr_revision
    }

    /// Linear HDR source. In the basic opaque/cutout graph, alpha records actual
    /// color-pass geometry coverage (sky zero), independent of prepass depth.
    pub fn resolved_hdr_texture(&self) -> &wgpu::Texture {
        &self.targets._resolved
    }

    /// Diagnostic readback preparation after color rendering: replace Hi-Z mip
    /// zero with minimum depth from the final color pass, rather than assuming
    /// the prepass and color pass produced the same covered samples. Higher mips
    /// are deliberately unused afterwards and rebuilt by the next frame.
    pub fn encode_final_depth_coverage(&self, encoder: &mut wgpu::CommandEncoder) -> bool {
        let (Some(hiz), Some(pipeline)) = (&self.hiz, &self.hiz_base_pipeline) else {
            return false;
        };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("diagnostic final color sample coverage"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &hiz.base_bind, &[]);
        pass.dispatch_workgroups(hiz.width.div_ceil(8), hiz.height.div_ceil(8), 1);
        true
    }
    pub fn output_format(&self) -> wgpu::TextureFormat {
        self.output_format
    }

    /// Call before acquiring a surface texture or creating an encoder. Unchanged
    /// scenes can retain their presented image while input/simulation still tick.
    /// A successful encode must be submitted/presented before using this cache.
    pub fn needs_redraw(&self, camera: &Camera, mode: CullingMode) -> bool {
        self.last_render != Some((*camera, mode, self.scene_revision))
    }

    /// Surface loss, output recreation or external visual inputs require redraw
    /// even if the camera and page meshes have not changed.
    pub fn invalidate(&mut self) {
        self.scene_revision = self.scene_revision.wrapping_add(1);
        self.last_render = None;
    }

    pub fn frame_graph(&self) -> GpuFrameGraph {
        GpuFrameGraph {
            passes: self.pass_kinds.get(),
            count: self.pass_count.get(),
            has_gpu_timestamps: self.pass_queries.is_some(),
        }
    }

    /// Copy this buffer to a caller-owned readback outside the hot path. Each
    /// recorded pass has two u64 timestamps; multiply their delta by period.
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.pass_queries
            .as_ref()
            .map(|queries| (&queries.resolved, queries.period))
    }

    fn record_pass(&self, kind: GpuPassKind) -> u32 {
        let index = self.pass_count.get();
        assert!(index < 32, "bounded frame graph");
        let mut kinds = self.pass_kinds.get();
        kinds[index as usize] = kind;
        self.pass_kinds.set(kinds);
        self.pass_count.set(index + 1);
        index
    }
    fn compute_timestamps(
        &self,
        kind: GpuPassKind,
    ) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let index = self.record_pass(kind);
        self.pass_queries
            .as_ref()
            .map(|queries| wgpu::ComputePassTimestampWrites {
                query_set: &queries.set,
                beginning_of_pass_write_index: Some(index * 2),
                end_of_pass_write_index: Some(index * 2 + 1),
            })
    }
    fn render_timestamps(&self, kind: GpuPassKind) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let index = self.record_pass(kind);
        self.pass_queries
            .as_ref()
            .map(|queries| wgpu::RenderPassTimestampWrites {
                query_set: &queries.set,
                beginning_of_pass_write_index: Some(index * 2),
                end_of_pass_write_index: Some(index * 2 + 1),
            })
    }

    pub fn upload_page(
        &mut self,
        queue: &wgpu::Queue,
        handle: Option<PageHandle>,
        version: u64,
        model: [[f32; 4]; 4],
        mesh: &PageMesh,
        budget: &mut UploadBudget,
    ) -> Result<PageHandle, TerrainError> {
        self.validate_raster_mesh(mesh)?;
        if handle.is_none() && !self.can_allocate_slots(1) {
            return Err(TerrainError::Capacity);
        }
        let len = mesh.opaque.len() + mesh.cutout.len() + mesh.water.len();
        if len > MAX_PAGE_QUADS || len > self.config.max_quads as usize {
            return Err(TerrainError::Capacity);
        }
        if mesh
            .iter()
            .any(|q| u32::from(q.material()) >= self.atlas_layers)
        {
            return Err(TerrainError::InvalidMaterial);
        }
        if model.iter().flatten().any(|v| !v.is_finite())
            || model[0][3] != 0.0
            || model[1][3] != 0.0
            || model[2][3] != 0.0
            || model[3][3] != 1.0
        {
            return Err(TerrainError::InvalidTransform);
        }
        let bytes = len as u64 * 16 + OBJECT_BYTES;
        if budget.remaining_bytes < bytes {
            return Err(TerrainError::UploadBudget);
        }
        let (slot, generation, old_span, flags) = if let Some(handle) = handle {
            let current = self
                .slots
                .get(handle.slot as usize)
                .ok_or(TerrainError::StaleHandle)?;
            if current.generation != handle.generation || !current.allocated {
                return Err(TerrainError::StaleHandle);
            }
            if version < current.version {
                return Err(TerrainError::StaleVersion);
            }
            (
                handle.slot,
                handle.generation,
                current.span,
                current.object.active & (ENABLED | OVERLAY),
            )
        } else {
            let slot = *self.free_slots.last().ok_or(TerrainError::Capacity)?;
            let generation = self.slots[slot as usize]
                .generation
                .checked_add(1)
                .ok_or(TerrainError::Capacity)?;
            (slot, generation, None, ENABLED)
        };
        let neighbours = if handle.is_some() {
            self.slots[slot as usize].neighbours
        } else {
            [[0; 2]; 6]
        };
        let span = if len == 0 {
            None
        } else if old_span.is_some_and(|span| span.len >= len as u32) {
            old_span
        } else {
            Some(self.allocate(len as u32).ok_or(TerrainError::Capacity)?)
        };
        if handle.is_none() {
            self.free_slots.pop();
        }
        if let Some(old) = old_span
            && span.is_none_or(|span| span.start != old.start)
        {
            self.free(old);
        }
        self.staging.clear();
        for quad in mesh.iter() {
            let mut words = quad.words;
            words[3] = slot;
            self.staging.push(words);
        }
        let mut cursor = span.map_or(0, |span| span.start);
        let ranges = std::array::from_fn(|bucket| {
            let count = [mesh.opaque.len(), mesh.cutout.len(), mesh.water.len()][bucket] as u32;
            let result = [cursor, count];
            cursor += count;
            result
        });
        let (bounds_min, bounds_max) = world_bounds(model);
        let object = ObjectGpu {
            model,
            bounds_min,
            bounds_max,
            ranges,
            generation,
            active: flags,
        };
        if let Some(span) = span {
            queue.write_buffer(
                &self.quads,
                u64::from(span.start) * 16,
                bytemuck::cast_slice(&self.staging),
            );
        }
        queue.write_buffer(
            &self.objects,
            u64::from(slot) * OBJECT_BYTES,
            bytemuck::bytes_of(&object),
        );
        self.slots[slot as usize] = Slot {
            generation,
            version,
            span,
            allocated: true,
            object,
            neighbours,
        };
        budget.remaining_bytes -= bytes;
        self.invalidate();
        self.work.record(telemetry::WorkCounts {
            upload_bytes: bytes,
            quads_emitted: len as u64,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(PageHandle { slot, generation })
    }

    pub fn release_page(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        budget: &mut UploadBudget,
    ) -> Result<(), TerrainError> {
        if budget.remaining_bytes < OBJECT_BYTES {
            return Err(TerrainError::UploadBudget);
        }
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .ok_or(TerrainError::StaleHandle)?;
        if slot.generation != handle.generation || !slot.allocated {
            return Err(TerrainError::StaleHandle);
        }
        let span = slot.span.take();
        slot.allocated = false;
        queue.write_buffer(
            &self.objects,
            u64::from(handle.slot) * OBJECT_BYTES,
            bytemuck::bytes_of(&ObjectGpu::zeroed()),
        );
        if let Some(span) = span {
            self.free(span);
        }
        self.free_slots.push(handle.slot);
        budget.remaining_bytes -= OBJECT_BYTES;
        self.invalidate();
        self.work.record(telemetry::WorkCounts {
            upload_bytes: OBJECT_BYTES,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(())
    }

    fn live_object(&self, handle: PageHandle) -> Result<ObjectGpu, TerrainError> {
        let slot = self
            .slots
            .get(handle.slot as usize)
            .ok_or(TerrainError::StaleHandle)?;
        if !slot.allocated || slot.generation != handle.generation {
            return Err(TerrainError::StaleHandle);
        }
        Ok(slot.object)
    }

    fn write_object(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        object: ObjectGpu,
        budget: &mut UploadBudget,
    ) -> Result<(), TerrainError> {
        if budget.remaining_bytes < OBJECT_BYTES {
            return Err(TerrainError::UploadBudget);
        }
        queue.write_buffer(
            &self.objects,
            u64::from(handle.slot) * OBJECT_BYTES,
            bytemuck::bytes_of(&object),
        );
        self.slots[handle.slot as usize].object = object;
        budget.remaining_bytes -= OBJECT_BYTES;
        self.work.record(telemetry::WorkCounts {
            upload_bytes: OBJECT_BYTES,
            ..telemetry::WorkCounts::ZERO
        });
        self.invalidate();
        Ok(())
    }

    /// Object animation changes only the 128-byte object record, never vertices.
    pub fn update_transform(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        model: [[f32; 4]; 4],
        budget: &mut UploadBudget,
    ) -> Result<(), TerrainError> {
        let mut object = self.live_object(handle)?;
        if model.iter().flatten().any(|v| !v.is_finite())
            || model[0][3] != 0.0
            || model[1][3] != 0.0
            || model[2][3] != 0.0
            || model[3][3] != 1.0
        {
            return Err(TerrainError::InvalidTransform);
        }
        if object.model == model {
            return Ok(());
        }
        object.model = model;
        let (low, high) = world_bounds(model);
        object.bounds_min = low;
        object.bounds_max = high;
        self.write_object(queue, handle, object, budget)
    }

    pub fn set_enabled(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        enabled: bool,
        budget: &mut UploadBudget,
    ) -> Result<(), TerrainError> {
        let mut object = self.live_object(handle)?;
        let flags = (object.active & !ENABLED) | if enabled { ENABLED } else { 0 };
        if flags == object.active {
            return Ok(());
        }
        object.active = flags;
        self.write_object(queue, handle, object, budget)
    }

    /// Page links use mesher Face order: Down, Up, North, South, West, East.
    /// Unchanged rows upload nothing. Header and target generations prevent ABA.
    pub fn set_page_neighbors(
        &mut self,
        queue: &wgpu::Queue,
        base: PageHandle,
        neighbors: [Option<PageHandle>; 6],
        budget: &mut UploadBudget,
    ) -> Result<u64, TerrainError> {
        if self.live_object(base)?.active & OVERLAY != 0 {
            return Err(TerrainError::InvalidEdit);
        }
        let mut links = [[0u32; 2]; 6];
        for (target, handle) in links.iter_mut().zip(neighbors) {
            if let Some(handle) = handle {
                if self.live_object(handle)?.active & OVERLAY != 0 {
                    return Err(TerrainError::InvalidEdit);
                }
                *target = [handle.slot, handle.generation];
            }
        }
        if self.slots[base.slot as usize].neighbours == links {
            return Ok(0);
        }
        if budget.remaining_bytes < 56 {
            return Err(TerrainError::UploadBudget);
        }
        let mut words = [[0u32; 2]; 7];
        words[0] = [base.generation, 0];
        words[1..].copy_from_slice(&links);
        queue.write_buffer(
            &self.edit_neighbors,
            u64::from(base.slot) * 56,
            bytemuck::cast_slice(&words),
        );
        self.slots[base.slot as usize].neighbours = links;
        budget.remaining_bytes -= 56;
        self.work.record(telemetry::WorkCounts {
            upload_bytes: 56,
            ..telemetry::WorkCounts::ZERO
        });
        self.invalidate();
        Ok(56)
    }

    /// Overlay geometry must not be clipped by the base page's edit mask.
    pub fn set_overlay(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        overlay: bool,
        budget: &mut UploadBudget,
    ) -> Result<(), TerrainError> {
        let mut object = self.live_object(handle)?;
        let flags = (object.active & !OVERLAY) | if overlay { OVERLAY } else { 0 };
        if flags == object.active {
            return Ok(());
        }
        object.active = flags;
        self.write_object(queue, handle, object, budget)
    }

    pub fn set_edit_mask(
        &mut self,
        queue: &wgpu::Queue,
        base: PageHandle,
        mask: &[u32; 128],
        budget: &mut UploadBudget,
    ) -> Result<(), TerrainError> {
        let mut object = self.live_object(base)?;
        if object.active & OVERLAY != 0 {
            return Err(TerrainError::InvalidEdit);
        }
        let nonempty = mask.iter().any(|word| *word != 0);
        if !nonempty && object.active & EDIT_MASK == 0 {
            return Ok(());
        }
        if budget.remaining_bytes < EDIT_MASK_BYTES + OBJECT_BYTES {
            return Err(TerrainError::UploadBudget);
        }
        queue.write_buffer(
            &self.edit_masks,
            u64::from(base.slot) * EDIT_MASK_BYTES,
            bytemuck::cast_slice(mask),
        );
        budget.remaining_bytes -= EDIT_MASK_BYTES;
        self.work.record(telemetry::WorkCounts {
            upload_bytes: EDIT_MASK_BYTES,
            ..telemetry::WorkCounts::ZERO
        });
        object.active = (object.active & !EDIT_MASK) | if nonempty { EDIT_MASK } else { 0 };
        self.write_object(queue, base, object, budget)
    }

    /// Mask and overlay are published together before the next submit. If the
    /// budget or arena cannot admit the operation, neither visible state changes.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_edit_overlay(
        &mut self,
        queue: &wgpu::Queue,
        base: PageHandle,
        mask: &[u32; 128],
        overlay: Option<PageHandle>,
        version: u64,
        model: [[f32; 4]; 4],
        mesh: &PageMesh,
        budget: &mut UploadBudget,
    ) -> Result<PageHandle, TerrainError> {
        let object = self.live_object(base)?;
        if object.active & OVERLAY != 0 || overlay == Some(base) {
            return Err(TerrainError::InvalidEdit);
        }
        if version < self.slots[base.slot as usize].version {
            return Err(TerrainError::StaleVersion);
        }
        if let Some(handle) = overlay
            && self.live_object(handle)?.active & OVERLAY == 0
        {
            return Err(TerrainError::InvalidEdit);
        }
        let price = mesh.quad_count() as u64 * 16 + OBJECT_BYTES * 3 + EDIT_MASK_BYTES;
        if budget.remaining_bytes < price {
            return Err(TerrainError::UploadBudget);
        }
        // upload_page performs all fallible mesh/arena/version validation before
        // its first queue write. The remaining handle/budget checks were prepaid.
        let handle = self.upload_page(queue, overlay, version, model, mesh, budget)?;
        self.set_overlay(queue, handle, true, budget)
            .expect("preflighted overlay flag");
        self.set_edit_mask(queue, base, mask, budget)
            .expect("preflighted base mask");
        Ok(handle)
    }

    /// Install a current full mesh and retire its overlays in the same submit
    /// epoch. Input-world versions must already have passed the caller's guard.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_edit(
        &mut self,
        queue: &wgpu::Queue,
        base: PageHandle,
        version: u64,
        model: [[f32; 4]; 4],
        mesh: &PageMesh,
        overlays: &[PageHandle],
        budget: &mut UploadBudget,
    ) -> Result<(), TerrainError> {
        let object = self.live_object(base)?;
        if object.active & OVERLAY != 0 {
            return Err(TerrainError::InvalidEdit);
        }
        for (index, overlay) in overlays.iter().enumerate() {
            if *overlay == base || overlays[..index].contains(overlay) {
                return Err(TerrainError::InvalidEdit);
            }
            if self.live_object(*overlay)?.active & OVERLAY == 0 {
                return Err(TerrainError::InvalidEdit);
            }
        }
        let price =
            mesh.quad_count() as u64 * 16 + OBJECT_BYTES + overlays.len() as u64 * OBJECT_BYTES;
        if budget.remaining_bytes < price {
            return Err(TerrainError::UploadBudget);
        }
        // A full replacement clears EDIT_MASK in its single object record.
        self.upload_page(queue, Some(base), version, model, mesh, budget)?;
        for overlay in overlays {
            self.release_page(queue, *overlay, budget)
                .expect("preflighted overlay retirement");
        }
        Ok(())
    }

    /// Rebase an existing page. See `publish_composite` for atomic admission.
    #[allow(clippy::too_many_arguments)]
    pub fn rebase_edit(
        &mut self,
        queue: &wgpu::Queue,
        base: PageHandle,
        version: u64,
        model: [[f32; 4]; 4],
        base_mesh: &PageMesh,
        overlay: Option<PageHandle>,
        mask: &[u32; 128],
        patch_mesh: &PageMesh,
        budget: &mut UploadBudget,
    ) -> Result<Option<PageHandle>, TerrainError> {
        self.publish_composite(
            queue,
            Some(base),
            version,
            model,
            base_mesh,
            overlay,
            mask,
            patch_mesh,
            budget,
        )
        .map(|(_, overlay)| overlay)
    }

    /// Publish a captured base and its entire current post-capture shell together.
    /// Both new slots and both arena allocations are admitted before any GPU write.
    /// `version` is the caller's monotonic GPU publication revision.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_composite(
        &mut self,
        queue: &wgpu::Queue,
        base: Option<PageHandle>,
        version: u64,
        model: [[f32; 4]; 4],
        base_mesh: &PageMesh,
        overlay: Option<PageHandle>,
        mask: &[u32; 128],
        patch_mesh: &PageMesh,
        budget: &mut UploadBudget,
    ) -> Result<(PageHandle, Option<PageHandle>), TerrainError> {
        if base.is_none() && overlay.is_some() {
            return Err(TerrainError::InvalidEdit);
        }
        if mask.iter().all(|word| *word == 0) {
            let handle = if let Some(base) = base {
                self.finish_edit(
                    queue,
                    base,
                    version,
                    model,
                    base_mesh,
                    overlay.as_slice(),
                    budget,
                )?;
                base
            } else {
                self.upload_page(queue, None, version, model, base_mesh, budget)?
            };
            return Ok((handle, None));
        }
        let old_base_span = if let Some(handle) = base {
            let object = self.live_object(handle)?;
            if object.active & OVERLAY != 0 || overlay == Some(handle) {
                return Err(TerrainError::InvalidEdit);
            }
            let slot = &self.slots[handle.slot as usize];
            if version < slot.version {
                return Err(TerrainError::StaleVersion);
            }
            slot.span
        } else {
            None
        };
        let old_overlay_span = if let Some(handle) = overlay {
            if self.live_object(handle)?.active & OVERLAY == 0 {
                return Err(TerrainError::InvalidEdit);
            }
            let slot = &self.slots[handle.slot as usize];
            if version < slot.version {
                return Err(TerrainError::StaleVersion);
            }
            slot.span
        } else {
            None
        };
        let new_slots = usize::from(base.is_none()) + usize::from(overlay.is_none());
        if !self.can_allocate_slots(new_slots) {
            return Err(TerrainError::Capacity);
        }
        for slot in self.free_slots.iter().rev().take(new_slots) {
            self.slots[*slot as usize]
                .generation
                .checked_add(1)
                .ok_or(TerrainError::Capacity)?;
        }
        for mesh in [base_mesh, patch_mesh] {
            self.validate_raster_mesh(mesh)?;
            if mesh.quad_count() > MAX_PAGE_QUADS
                || mesh.quad_count() > self.config.max_quads as usize
            {
                return Err(TerrainError::Capacity);
            }
            if mesh
                .iter()
                .any(|quad| u32::from(quad.material()) >= self.atlas_layers)
            {
                return Err(TerrainError::InvalidMaterial);
            }
        }
        if model.iter().flatten().any(|v| !v.is_finite())
            || model[0][3] != 0.0
            || model[1][3] != 0.0
            || model[2][3] != 0.0
            || model[3][3] != 1.0
        {
            return Err(TerrainError::InvalidTransform);
        }
        let price = (base_mesh.quad_count() as u64 + patch_mesh.quad_count() as u64) * 16
            + OBJECT_BYTES * 4
            + EDIT_MASK_BYTES;
        if budget.remaining_bytes < price {
            return Err(TerrainError::UploadBudget);
        }
        self.allocation_scratch.clone_from(&self.free_spans);
        let simulate =
            |free: &mut Vec<Span>, old: Option<Span>, len: u32| -> Result<(), TerrainError> {
                let next = if len == 0 {
                    None
                } else if old.is_some_and(|span| span.len >= len) {
                    old
                } else {
                    Some(allocate_span(free, len).ok_or(TerrainError::Capacity)?)
                };
                if let Some(old) = old
                    && next.is_none_or(|span| span.start != old.start)
                {
                    free_span(free, old);
                }
                Ok(())
            };
        simulate(
            &mut self.allocation_scratch,
            old_base_span,
            base_mesh.quad_count() as u32,
        )?;
        simulate(
            &mut self.allocation_scratch,
            old_overlay_span,
            patch_mesh.quad_count() as u32,
        )?;
        // This exact first-fit sequence cannot fail after admission. All writes
        // belong to the next single submit epoch, including a newly resident base.
        let base = self
            .upload_page(queue, base, version, model, base_mesh, budget)
            .expect("preflighted composite base");
        let overlay = self
            .publish_edit_overlay(
                queue, base, mask, overlay, version, model, patch_mesh, budget,
            )
            .expect("preflighted composite shell");
        Ok((base, Some(overlay)))
    }

    /// Replace only the dirty owner records. Owner order must be strictly
    /// increasing so adjacent records share geometry and state uploads.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_owner_patches(
        &mut self,
        queue: &wgpu::Queue,
        base: PageHandle,
        overlay: Option<PageHandle>,
        version: u64,
        model: [[f32; 4]; 4],
        mask: &[u32; 128],
        updates: &[OwnerPatch],
        budget: &mut UploadBudget,
    ) -> Result<PageHandle, TerrainError> {
        self.publish_fixed(
            queue,
            Some(base),
            version,
            model,
            None,
            overlay,
            mask,
            updates,
            budget,
        )?
        .1
        .ok_or(TerrainError::InvalidEdit)
    }

    /// Install a captured base with the current fixed owner shell in one submit
    /// epoch. Retired owner states need no clearing: the current base mask gates
    /// them before compaction. Surviving owner states remain current across rebases.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_owner_composite(
        &mut self,
        queue: &wgpu::Queue,
        base: Option<PageHandle>,
        version: u64,
        model: [[f32; 4]; 4],
        base_mesh: &PageMesh,
        overlay: Option<PageHandle>,
        mask: &[u32; 128],
        updates: &[OwnerPatch],
        budget: &mut UploadBudget,
    ) -> Result<(PageHandle, Option<PageHandle>), TerrainError> {
        self.publish_fixed(
            queue,
            base,
            version,
            model,
            Some(base_mesh),
            overlay,
            mask,
            updates,
            budget,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_fixed(
        &mut self,
        queue: &wgpu::Queue,
        base: Option<PageHandle>,
        version: u64,
        model: [[f32; 4]; 4],
        base_mesh: Option<&PageMesh>,
        overlay: Option<PageHandle>,
        mask: &[u32; 128],
        updates: &[OwnerPatch],
        budget: &mut UploadBudget,
    ) -> Result<(PageHandle, Option<PageHandle>), TerrainError> {
        if base.is_none() && (overlay.is_some() || base_mesh.is_none()) {
            return Err(TerrainError::InvalidEdit);
        }
        if mask.iter().all(|word| *word == 0) {
            if let Some(mesh) = base_mesh {
                return self.publish_composite(
                    queue,
                    base,
                    version,
                    model,
                    mesh,
                    overlay,
                    mask,
                    &PageMesh::default(),
                    budget,
                );
            }
            return Err(TerrainError::InvalidEdit);
        }
        let old_base_span = if let Some(handle) = base {
            if self.live_object(handle)?.active & OVERLAY != 0 || overlay == Some(handle) {
                return Err(TerrainError::InvalidEdit);
            }
            let slot = &self.slots[handle.slot as usize];
            if version < slot.version {
                return Err(TerrainError::StaleVersion);
            }
            slot.span
        } else {
            None
        };
        let old_overlay = if let Some(handle) = overlay {
            let object = self.live_object(handle)?;
            if object.active & OVERLAY == 0 {
                return Err(TerrainError::InvalidEdit);
            }
            let slot = &self.slots[handle.slot as usize];
            if version < slot.version {
                return Err(TerrainError::StaleVersion);
            }
            if object.active & FIXED_PATCH != 0
                && base.is_some_and(|base| {
                    object.ranges[1][0] != base.slot || object.ranges[1][1] != base.generation
                })
            {
                return Err(TerrainError::StaleHandle);
            }
            Some((handle, slot.span, object.active & FIXED_PATCH != 0))
        } else {
            None
        };
        let new_slots = usize::from(base.is_none()) + usize::from(overlay.is_none());
        if !self.can_allocate_slots(new_slots) {
            return Err(TerrainError::Capacity);
        }
        for slot in self.free_slots.iter().rev().take(new_slots) {
            self.slots[*slot as usize]
                .generation
                .checked_add(1)
                .ok_or(TerrainError::Capacity)?;
        }
        if let Some(mesh) = base_mesh {
            self.validate_raster_mesh(mesh)?;
            if mesh.quad_count() > MAX_PAGE_QUADS
                || mesh.quad_count() > self.config.max_quads as usize
            {
                return Err(TerrainError::Capacity);
            }
            if mesh
                .iter()
                .any(|q| u32::from(q.material()) >= self.atlas_layers)
            {
                return Err(TerrainError::InvalidMaterial);
            }
        }
        if model.iter().flatten().any(|v| !v.is_finite())
            || model[0][3] != 0.0
            || model[1][3] != 0.0
            || model[2][3] != 0.0
            || model[3][3] != 1.0
        {
            return Err(TerrainError::InvalidTransform);
        }
        let mut previous = None;
        for update in updates {
            if update.owner >= 4096 || previous.is_some_and(|owner| owner >= update.owner) {
                return Err(TerrainError::InvalidEdit);
            }
            previous = Some(update.owner);
            for face in 0..6 {
                if update.classes[face] == MeshClass::Empty {
                    continue;
                }
                let quad = update.quads[face];
                if quad.size() != [1, 1] || ((quad.words[0] >> 15) & 7) != face as u32 {
                    return Err(TerrainError::InvalidEdit);
                }
                if u32::from(quad.material()) >= self.atlas_layers {
                    return Err(TerrainError::InvalidMaterial);
                }
            }
        }
        let initialize = old_overlay.is_none_or(|(_, _, fixed)| !fixed);
        let state_clear = if initialize { PATCH_STATE_BYTES } else { 0 };
        let owner_bytes = updates.len() as u64 * 100;
        let overlay_bytes = OBJECT_BYTES + state_clear + owner_bytes;
        let base_bytes = base_mesh.map_or(0, |mesh| mesh.quad_count() as u64 * 16 + OBJECT_BYTES);
        if budget.remaining_bytes < base_bytes + overlay_bytes + EDIT_MASK_BYTES + OBJECT_BYTES {
            return Err(TerrainError::UploadBudget);
        }
        self.allocation_scratch.clone_from(&self.free_spans);
        if let Some(mesh) = base_mesh {
            let len = mesh.quad_count() as u32;
            let next = if len == 0 {
                None
            } else if old_base_span.is_some_and(|span| span.len >= len) {
                old_base_span
            } else {
                Some(
                    allocate_span(&mut self.allocation_scratch, len)
                        .ok_or(TerrainError::Capacity)?,
                )
            };
            if let Some(old) = old_base_span
                && next.is_none_or(|span| span.start != old.start)
            {
                free_span(&mut self.allocation_scratch, old);
            }
        }
        let old_span = old_overlay.and_then(|(_, span, _)| span);
        if old_span.is_none_or(|span| span.len < MAX_PAGE_QUADS as u32) {
            allocate_span(&mut self.allocation_scratch, MAX_PAGE_QUADS as u32)
                .ok_or(TerrainError::Capacity)?;
        }
        // Both slot and first-fit reservations have passed before this first write.
        let base = if let Some(mesh) = base_mesh {
            self.upload_page(queue, base, version, model, mesh, budget)
                .expect("preflighted fixed-shell base")
        } else {
            base.expect("validated existing base")
        };
        let (slot, generation) = if let Some((handle, _, _)) = old_overlay {
            (handle.slot, handle.generation)
        } else {
            let slot = self.free_slots.pop().expect("preflighted fixed-shell slot");
            (slot, self.slots[slot as usize].generation + 1)
        };
        let span = if old_span.is_some_and(|span| span.len >= MAX_PAGE_QUADS as u32) {
            old_span.expect("checked old span")
        } else {
            self.allocate(MAX_PAGE_QUADS as u32)
                .expect("preflighted fixed-shell arena")
        };
        if let Some(old) = old_span
            && old.start != span.start
        {
            self.free(old);
        }
        if initialize {
            queue.write_buffer(
                &self.patch_states,
                u64::from(slot) * PATCH_STATE_BYTES,
                bytemuck::cast_slice(&*self.patch_clear),
            );
        }
        self.staging.clear();
        self.patch_state_staging.clear();
        for update in updates {
            let mut state = 0u32;
            for face in 0..6 {
                let mut words = update.quads[face].words;
                words[3] = slot;
                self.staging.push(words);
                state |= (update.classes[face] as u32) << (face * 3);
            }
            self.patch_state_staging.push(state);
        }
        let mut first = 0;
        while first < updates.len() {
            let mut end = first + 1;
            while end < updates.len() && updates[end].owner == updates[end - 1].owner + 1 {
                end += 1;
            }
            let owner = u64::from(updates[first].owner);
            queue.write_buffer(
                &self.quads,
                (u64::from(span.start) + owner * 6) * 16,
                bytemuck::cast_slice(&self.staging[first * 6..end * 6]),
            );
            queue.write_buffer(
                &self.patch_states,
                u64::from(slot) * PATCH_STATE_BYTES + owner * 4,
                bytemuck::cast_slice(&self.patch_state_staging[first..end]),
            );
            first = end;
        }
        let (bounds_min, bounds_max) = world_bounds(model);
        let object = ObjectGpu {
            model,
            bounds_min,
            bounds_max,
            ranges: [
                [span.start, MAX_PAGE_QUADS as u32],
                [base.slot, base.generation],
                [0, 0],
            ],
            generation,
            active: ENABLED | OVERLAY | FIXED_PATCH,
        };
        queue.write_buffer(
            &self.objects,
            u64::from(slot) * OBJECT_BYTES,
            bytemuck::bytes_of(&object),
        );
        self.slots[slot as usize] = Slot {
            generation,
            version,
            span: Some(span),
            allocated: true,
            object,
            neighbours: [[0; 2]; 6],
        };
        budget.remaining_bytes -= overlay_bytes;
        self.work.record(telemetry::WorkCounts {
            upload_bytes: overlay_bytes,
            quads_emitted: updates.len() as u64 * 6,
            ..telemetry::WorkCounts::ZERO
        });
        self.set_edit_mask(queue, base, mask, budget)
            .expect("preflighted fixed-shell mask");
        self.invalidate();
        Ok((base, Some(PageHandle { slot, generation })))
    }

    fn allocate(&mut self, len: u32) -> Option<Span> {
        allocate_span(&mut self.free_spans, len)
    }
    fn free(&mut self, span: Span) {
        free_span(&mut self.free_spans, span)
    }

    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<(), TerrainError> {
        if width == 0
            || height == 0
            || width
                .checked_next_power_of_two()
                .is_none_or(|value| value > device.limits().max_texture_dimension_2d)
            || height
                .checked_next_power_of_two()
                .is_none_or(|value| value > device.limits().max_texture_dimension_2d)
        {
            return Err(TerrainError::InvalidSize);
        }
        if self.width == width && self.height == height {
            return Ok(());
        }
        self.targets = create_targets(
            device,
            width,
            height,
            &self.tone_layout,
            self.targets.sampled,
            self.sample_count,
        );
        self.hiz = match (&self.hiz_base_layout, &self.hiz_reduce_layout) {
            (Some(base), Some(reduce)) => Some(create_hiz(
                device,
                width,
                height,
                &self.targets.depth,
                base,
                reduce,
                &self.cull_layout,
                &self.cameras[1],
                &self.objects,
                &self.visible,
                &self.indirect,
                &self.edit_masks,
                &self.patch_states,
                &self.quads,
            )),
            _ => None,
        };
        if self.ambient.is_some() {
            self.ambient = Some(create_ambient(device, width, height, self.sample_count));
        }
        self.width = width;
        self.height = height;
        self.last_camera = None;
        self.invalidate();
        Ok(())
    }

    /// Explicit frame graph, with all large resources and pipelines retained.
    /// GPU work is never replaced by a fake CPU visibility result.
    pub fn encode_frame(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        camera: &Camera,
        mode: CullingMode,
    ) -> Result<TerrainFrameWork, TerrainError> {
        self.encode_frame_with_extra(queue, encoder, output, camera, mode, |_| 0)
    }

    /// Draw additional prepared scene batches into the shared HDR/depth pass.
    /// Return their actual draw-call count; their compute preparation precedes this call.
    pub fn encode_frame_with_extra(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        camera: &Camera,
        mode: CullingMode,
        extra: impl FnOnce(&mut wgpu::RenderPass<'_>) -> u32,
    ) -> Result<TerrainFrameWork, TerrainError> {
        self.encode_impl(queue, encoder, Some(output), camera, mode, false, extra)
    }

    /// Retain the four linear HDR/depth samples for separate water/cloud targets.
    /// This encodes no presentation pass. Sources must never also be attachments
    /// of a consuming pass. Prepare sampling once before the first frame.
    #[allow(clippy::too_many_arguments)] // Explicit graph inputs and prepared extra batches.
    pub fn encode_scene_with_extra(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        mode: CullingMode,
        skip_plain_water: bool,
        extra: impl FnOnce(&mut wgpu::RenderPass<'_>) -> u32,
    ) -> Result<TerrainFrameWork, TerrainError> {
        if !self.targets.sampled {
            return Err(TerrainError::SceneSamplingDisabled);
        }
        self.encode_impl(queue, encoder, None, camera, mode, skip_plain_water, extra)
    }

    #[allow(clippy::too_many_arguments)] // Shared opaque graph with optional presentation.
    fn encode_impl(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: Option<&wgpu::TextureView>,
        camera: &Camera,
        mode: CullingMode,
        skip_plain_water: bool,
        extra: impl FnOnce(&mut wgpu::RenderPass<'_>) -> u32,
    ) -> Result<TerrainFrameWork, TerrainError> {
        self.pass_count.set(0);
        self.pass_kinds.set([GpuPassKind::Unused; 32]);
        if mode == CullingMode::FrustumAndHiZ && !self.config.occlusion {
            return Err(TerrainError::OcclusionDisabled);
        }
        if camera
            .view_projection
            .iter()
            .flatten()
            .any(|n| !n.is_finite())
        {
            return Err(TerrainError::InvalidTransform);
        }
        let mut work = TerrainFrameWork {
            objects_considered: self.config.max_pages,
            draw_calls: 7 + u32::from(output.is_some()) - 2 * u32::from(skip_plain_water),
            passes: 4,
            camera_upload_bytes: 0,
        };
        if self.last_camera != Some((*camera, mode)) {
            for (phase, buffer) in self.cameras.iter().enumerate() {
                let viewport = [
                    self.width as f32,
                    self.height as f32,
                    self.width.next_power_of_two() as f32,
                    self.height.next_power_of_two() as f32,
                ];
                let uniform = CameraGpu {
                    view_projection: camera.view_projection,
                    viewport,
                    params: [
                        self.config.max_pages,
                        self.config.max_quads,
                        u32::from(mode != CullingMode::None) | (u32::from(self.unit_raster) << 1),
                        phase as u32,
                    ],
                };
                queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
            }
            self.last_camera = Some((*camera, mode));
            work.camera_upload_bytes = 192;
            self.work.record(telemetry::WorkCounts {
                upload_bytes: 192,
                ..telemetry::WorkCounts::ZERO
            });
        }
        if let Some(sky) = &mut self.atmosphere_sky {
            let bytes = sky
                .update_camera(queue, camera, &mut UploadBudget::new(u64::MAX))
                .map_err(|_| TerrainError::InvalidTransform)?;
            work.camera_upload_bytes += bytes;
            self.work.record(telemetry::WorkCounts {
                upload_bytes: bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        self.compute_visibility(
            encoder,
            &self.frustum_bind,
            &self.frustum_pipeline,
            GpuPassKind::CullFrustum,
        );
        if mode == CullingMode::FrustumAndHiZ {
            let hiz = self.hiz.as_ref().ok_or(TerrainError::OcclusionDisabled)?;
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("opaque reverse-Z occluders"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.targets.depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: self.render_timestamps(GpuPassKind::DepthPrepass),
                    occlusion_query_set: None,
                });
                pass.set_pipeline(
                    self.depth_pipeline
                        .as_ref()
                        .expect("prepared depth pipeline"),
                );
                pass.set_bind_group(0, &self.render_bind, &[]);
                pass.draw_indirect(&self.indirect, 0);
                pass.set_pipeline(
                    self.masked_depth_pipeline
                        .as_ref()
                        .expect("prepared masked depth pipeline"),
                );
                pass.draw_indirect(&self.indirect, 48);
            }
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("MSAA conservative reverse-Z base"),
                    timestamp_writes: self.compute_timestamps(GpuPassKind::HiZBase),
                });
                pass.set_pipeline(self.hiz_base_pipeline.as_ref().expect("prepared base"));
                pass.set_bind_group(0, &hiz.base_bind, &[]);
                pass.dispatch_workgroups(hiz.width.div_ceil(8), hiz.height.div_ceil(8), 1);
            }
            for level in &hiz.levels {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Hi-Z minimum mip"),
                    timestamp_writes: self.compute_timestamps(GpuPassKind::HiZReduce),
                });
                pass.set_pipeline(self.hiz_reduce_pipeline.as_ref().expect("prepared reduce"));
                pass.set_bind_group(0, &level.bind, &[]);
                pass.dispatch_workgroups(level.width.div_ceil(8), level.height.div_ceil(8), 1);
            }
            self.compute_visibility(
                encoder,
                &hiz.cull_bind,
                self.hiz_cull_pipeline.as_ref().expect("prepared occlusion"),
                GpuPassKind::CullOcclusion,
            );
            work.passes += 4 + hiz.levels.len() as u32;
            work.draw_calls += 2;
        }
        {
            let mut attachments = [None, None];
            attachments[0] = Some(wgpu::RenderPassColorAttachment {
                view: &self.targets.color,
                depth_slice: None,
                resolve_target: (self.sample_count > 1).then_some(&self.targets.resolved),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: if output.is_none() || self.sample_count == 1 {
                        wgpu::StoreOp::Store
                    } else {
                        wgpu::StoreOp::Discard
                    },
                },
            });
            if let Some((_, view)) = &self.ambient {
                attachments[1] = Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                });
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("HDR sky and terrain MSAA4"),
                color_attachments: &attachments[..if self.ambient.is_some() { 2 } else { 1 }],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: if mode == CullingMode::FrustumAndHiZ {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(0.0)
                        },
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.render_timestamps(GpuPassKind::Color),
                occlusion_query_set: None,
            });
            if let Some(sky) = &self.atmosphere_sky {
                sky.draw(&mut pass);
            } else {
                pass.set_pipeline(&self.sky_pipeline);
                pass.draw(0..3, 0..1);
            }
            pass.set_bind_group(0, &self.render_bind, &[]);
            for bucket in 0..6 {
                if skip_plain_water && bucket % 3 == 2 {
                    continue;
                }
                pass.set_pipeline(match bucket {
                    1 => &self.cutout_pipeline,
                    4 => &self.masked_cutout_pipeline,
                    3 | 5 => &self.masked_opaque_pipeline,
                    _ => &self.opaque_pipeline,
                });
                pass.draw_indirect(&self.indirect, bucket * 16);
            }
            work.draw_calls += extra(&mut pass);
        }
        if let Some(output) = output {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("HDR presentation transfer"),
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
                timestamp_writes: self.render_timestamps(GpuPassKind::Presentation),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.tone_pipeline);
            pass.set_bind_group(0, &self.targets.tone_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        work.passes = self.pass_count.get();
        if let Some(queries) = &self.pass_queries {
            encoder.resolve_query_set(&queries.set, 0..work.passes * 2, &queries.resolved, 0);
        }
        self.hdr_revision = self.hdr_revision.wrapping_add(1);
        self.last_render = Some((*camera, mode, self.scene_revision));
        Ok(work)
    }

    fn compute_visibility(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        pipeline: &wgpu::ComputePipeline,
        kind: GpuPassKind,
    ) {
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("reset indirect counts on GPU"),
                timestamp_writes: self.compute_timestamps(GpuPassKind::ResetVisibility),
            });
            pass.set_pipeline(&self.reset_pipeline);
            pass.set_bind_group(0, &self.frustum_bind, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compact visible terrain on GPU"),
                timestamp_writes: self.compute_timestamps(kind),
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(self.config.max_pages.div_ceil(64), 1, 1);
        }
    }
}

fn allocate_span(free: &mut Vec<Span>, len: u32) -> Option<Span> {
    let index = free.iter().position(|span| span.len >= len)?;
    let result = Span {
        start: free[index].start,
        len,
    };
    free[index].start += len;
    free[index].len -= len;
    if free[index].len == 0 {
        free.remove(index);
    }
    Some(result)
}
fn free_span(free: &mut Vec<Span>, span: Span) {
    let index = free.partition_point(|current| current.start < span.start);
    free.insert(index, span);
    let mut index = 0;
    while index + 1 < free.len() {
        if free[index].start + free[index].len == free[index + 1].start {
            free[index].len += free[index + 1].len;
            free.remove(index + 1);
        } else {
            index += 1;
        }
    }
}

fn world_bounds(model: [[f32; 4]; 4]) -> ([f32; 4], [f32; 4]) {
    let mut low = [f32::INFINITY; 4];
    let mut high = [f32::NEG_INFINITY; 4];
    for corner in 0..8 {
        let p = [
            if corner & 1 == 0 { 0.0 } else { 16.0 },
            if corner & 2 == 0 { 0.0 } else { 16.0 },
            if corner & 4 == 0 { 0.0 } else { 16.0 },
            1.0,
        ];
        for axis in 0..3 {
            let value = (0..4).map(|k| model[k][axis] * p[k]).sum::<f32>();
            low[axis] = low[axis].min(value);
            high[axis] = high[axis].max(value);
        }
    }
    low[3] = 1.0;
    high[3] = 1.0;
    (low, high)
}

fn terrain_render_layout(
    device: &wgpu::Device,
    lighting: bool,
    gi: bool,
    local_materials: bool,
    atmosphere: bool,
) -> wgpu::BindGroupLayout {
    let vertex = wgpu::ShaderStages::VERTEX;
    let fragment = wgpu::ShaderStages::FRAGMENT;
    let mut entries = vec![
        buffer_entry(0, vertex, true, true),
        buffer_entry(1, vertex | fragment, false, true),
        buffer_entry(2, vertex, false, true),
        buffer_entry(3, vertex, false, true),
        buffer_entry(4, fragment, false, true),
        texture_entry(
            5,
            fragment,
            wgpu::TextureViewDimension::D2Array,
            wgpu::TextureSampleType::Float { filterable: true },
            false,
        ),
        texture_entry(
            6,
            fragment,
            wgpu::TextureViewDimension::D2Array,
            wgpu::TextureSampleType::Float { filterable: true },
            false,
        ),
        wgpu::BindGroupLayoutEntry {
            binding: 7,
            visibility: fragment,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        buffer_entry(8, fragment, false, true),
        buffer_entry(16, fragment, false, true),
    ];
    if lighting {
        entries.extend([
            buffer_entry(9, fragment, true, true),
            texture_entry(
                10,
                fragment,
                wgpu::TextureViewDimension::D2Array,
                wgpu::TextureSampleType::Depth,
                false,
            ),
            wgpu::BindGroupLayoutEntry {
                binding: 11,
                visibility: fragment,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
            buffer_entry(12, fragment, false, true),
        ]);
    }
    if gi {
        entries.extend([
            buffer_entry(13, fragment, true, true),
            buffer_entry(14, fragment, false, true),
        ]);
    }
    if local_materials {
        for binding in [17, 18] {
            entries.push(texture_entry(
                binding,
                fragment,
                wgpu::TextureViewDimension::D2Array,
                wgpu::TextureSampleType::Float { filterable: true },
                false,
            ));
        }
        entries.push(buffer_entry(19, fragment, true, true));
    }
    if atmosphere {
        entries.push(buffer_entry(20, fragment, false, true));
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("terrain render tables"),
        entries: &entries,
    })
}

fn lit_terrain_shader(gi: bool) -> String {
    let source = include_str!("terrain.wgsl");
    let (before, rest) = source
        .split_once("// BEGIN_DEFAULT_LIGHTING")
        .expect("terrain lighting start");
    let (_, after) = rest
        .split_once("// END_DEFAULT_LIGHTING")
        .expect("terrain lighting end");
    let indirect = if gi {
        " + reflected_irradiance(world_position,normal)*0.3183098862"
    } else {
        ""
    };
    format!(
        "{before}\nlet light=sample_page_light(slot,local_position,local_normal);let sky=light.a;let visibility=cascade_shadow(world_position,normal);let direct=max(dot(normalize(normal),normalize(shadow_sampling.sun.xyz)),0.0)*visibility*sky*0.85;return albedo*((light.rgb+vec3(sky*0.15){indirect})*ao+vec3(direct));\n{after}\n{}\n{}\n{}",
        include_str!("shadow_sampling.wgsl"),
        include_str!("light_volume.wgsl"),
        if gi {
            include_str!("gi_sampling.wgsl")
        } else {
            ""
        },
    )
}

fn environment_terrain_shader(lit: bool, gi: bool) -> String {
    let source = include_str!("terrain.wgsl");
    let (before, rest) = source
        .split_once("// BEGIN_DEFAULT_LIGHTING")
        .expect("terrain lighting start");
    let (_, after) = rest
        .split_once("// END_DEFAULT_LIGHTING")
        .expect("terrain lighting end");
    let input = if lit {
        "let light=sample_page_light(slot,local_position,local_normal);let sky=light.a;let block=light.rgb;let visibility=cascade_shadow(world_position,normal);"
    } else {
        "let sky=1.0;let block=vec3(0.0);let visibility=1.0;"
    };
    let indirect = if gi {
        "+reflected_irradiance(world_position,normal)*0.3183098862"
    } else {
        ""
    };
    format!(
        "{before}\n{input}let sun=normalize(atmosphere_light.direction.xyz);let direct=atmosphere_light.sun.rgb*(0.3183098862*max(dot(normalize(normal),sun),0.0)*visibility*sky);return albedo*((block+sky*atmosphere_diffuse(normal){indirect})*ao+direct);\n{after}\n{}\n{}\n{}\n{}",
        include_str!("atmosphere_lighting.wgsl"),
        if lit {
            include_str!("shadow_sampling.wgsl")
        } else {
            ""
        },
        if lit {
            include_str!("light_volume.wgsl")
        } else {
            ""
        },
        if gi {
            include_str!("gi_sampling.wgsl")
        } else {
            ""
        }
    )
}

fn buffer_entry(
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
fn texture_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    dimension: wgpu::TextureViewDimension,
    sample_type: wgpu::TextureSampleType,
    multisampled: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: dimension,
            multisampled,
        },
        count: None,
    }
}
fn storage_texture_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::R32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
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
#[allow(clippy::too_many_arguments)]
fn compute_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    camera: &wgpu::Buffer,
    objects: &wgpu::Buffer,
    visible: &wgpu::Buffer,
    indirect: &wgpu::Buffer,
    edit_masks: &wgpu::Buffer,
    patch_states: &wgpu::Buffer,
    quads: &wgpu::Buffer,
    hiz: Option<&wgpu::TextureView>,
) -> wgpu::BindGroup {
    let mut entries = vec![
        buffer_binding(0, camera),
        buffer_binding(1, objects),
        buffer_binding(2, visible),
        buffer_binding(3, indirect),
        buffer_binding(5, edit_masks),
        buffer_binding(6, patch_states),
        buffer_binding(7, quads),
    ];
    if let Some(view) = hiz {
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: wgpu::BindingResource::TextureView(view),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute visibility bindings"),
        layout,
        entries: &entries,
    })
}
fn compute_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    entry: &str,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry),
        layout: Some(layout),
        module: shader,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    })
}
fn depth_state(write: bool, compare: wgpu::CompareFunction) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: write,
        depth_compare: compare,
        stencil: Default::default(),
        bias: Default::default(),
    }
}
#[allow(clippy::too_many_arguments)] // Loading selects depth/cutout/mask and independent MSAA/MRT output profiles.
fn terrain_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    cutout: bool,
    depth_only: bool,
    masked: bool,
    sample_count: u32,
    ambient_mrt: bool,
) -> wgpu::RenderPipeline {
    let color_targets = crate::ambient_mrt::targets(None, ambient_mrt);
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if depth_only {
            "reverse-Z opaque prepass"
        } else if cutout {
            "A2C cutout terrain"
        } else {
            "opaque terrain"
        }),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(depth_state(true, wgpu::CompareFunction::GreaterEqual)),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            alpha_to_coverage_enabled: sample_count > 1 && cutout && !depth_only,
            ..Default::default()
        },
        fragment: (!depth_only || masked).then_some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(if depth_only {
                "depth_fragment"
            } else if masked {
                "masked_fragment"
            } else {
                "fragment"
            }),
            compilation_options: Default::default(),
            targets: if depth_only { &[] } else { &color_targets },
        }),
        multiview: None,
        cache: None,
    })
}

fn create_scene_color(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    sampled: bool,
    sample_count: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("HDR MSAA4 color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | if sampled {
                wgpu::TextureUsages::TEXTURE_BINDING
            } else {
                wgpu::TextureUsages::empty()
            },
        view_formats: &[],
    })
}

fn create_targets(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    tone_layout: &wgpu::BindGroupLayout,
    sampled: bool,
    sample_count: u32,
) -> Targets {
    let texture = |label, format, count, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: count,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let resolved = texture(
        "resolved linear HDR",
        HDR_FORMAT,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    );
    let color = if sample_count == 1 {
        resolved.clone()
    } else {
        create_scene_color(device, width, height, sampled, sample_count)
    };
    let depth = texture(
        "actual reverse-Z MSAA4",
        DEPTH_FORMAT,
        sample_count,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let color_view = color.create_view(&Default::default());
    let resolved_view = resolved.create_view(&Default::default());
    let depth_view = depth.create_view(&Default::default());
    let tone_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("resolved HDR input"),
        layout: tone_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&resolved_view),
        }],
    });
    Targets {
        sampled,
        _color: color,
        color: color_view,
        _resolved: resolved,
        resolved: resolved_view,
        _depth: depth,
        depth: depth_view,
        tone_bind,
    }
}

#[allow(clippy::too_many_arguments)] // Startup/resize wires immutable GPU resources.
fn create_hiz(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    depth: &wgpu::TextureView,
    base_layout: &wgpu::BindGroupLayout,
    reduce_layout: &wgpu::BindGroupLayout,
    cull_layout: &wgpu::BindGroupLayout,
    camera: &wgpu::Buffer,
    objects: &wgpu::Buffer,
    visible: &wgpu::Buffer,
    indirect: &wgpu::Buffer,
    edit_masks: &wgpu::Buffer,
    patch_states: &wgpu::Buffer,
    quads: &wgpu::Buffer,
) -> HiZ {
    let width = width.next_power_of_two();
    let height = height.next_power_of_two();
    let mips = width.max(height).ilog2() + 1;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("padded conservative min Hi-Z"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let views: Vec<_> = (0..mips)
        .map(|level| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect();
    let base_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("MSAA depth base"),
        layout: base_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(depth),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&views[0]),
            },
        ],
    });
    let levels = (1..mips)
        .map(|level| HiZLevel {
            width: (width >> level).max(1),
            height: (height >> level).max(1),
            bind: device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("conservative min mip"),
                layout: reduce_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&views[level as usize - 1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&views[level as usize]),
                    },
                ],
            }),
        })
        .collect();
    let cull_bind = compute_bind(
        device,
        cull_layout,
        camera,
        objects,
        visible,
        indirect,
        edit_masks,
        patch_states,
        quads,
        Some(&view),
    );
    HiZ {
        _texture: texture,
        _view: view,
        base_bind,
        cull_bind,
        levels,
        width,
        height,
    }
}

fn upload_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &AtlasImage,
) -> (wgpu::TextureView, wgpu::TextureView, u64) {
    let texture = |label, format| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: atlas.tile_size,
                height: atlas.tile_size,
                depth_or_array_layers: atlas.layers,
            },
            mip_level_count: atlas.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    };
    let color = texture(
        "donor sRGB color array",
        wgpu::TextureFormat::Rgba8UnormSrgb,
    );
    let mask = texture("separate tint metadata array", wgpu::TextureFormat::R8Unorm);
    let mut bytes = 0;
    for (mip, level) in atlas.mips.iter().enumerate() {
        for (texture, data, channels) in [(&color, &level.rgba, 4), (&mask, &level.tint_mask, 1)] {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: mip as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(level.size * channels),
                    rows_per_image: Some(level.size),
                },
                wgpu::Extent3d {
                    width: level.size,
                    height: level.size,
                    depth_or_array_layers: atlas.layers,
                },
            );
            bytes += data.len() as u64;
        }
    }
    let descriptor = wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    };
    (
        color.create_view(&descriptor),
        mask.create_view(&descriptor),
        bytes,
    )
}

const TONE_SHADER: &str = r#"
@group(0) @binding(0) var hdr: texture_2d<f32>;
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
    let positions=array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    return vec4(positions[index],0.0,1.0);
}
fn color(position:vec4<f32>)->vec3<f32> {
    let value=max(textureLoad(hdr,vec2<i32>(position.xy),0).rgb,vec3(0.0));
    // Neutral output preserves ordinary sky/albedo, compressing only HDR peaks.
    return value/max(1.0,max(value.r,max(value.g,value.b)));
}
fn srgb(value:vec3<f32>)->vec3<f32> {
    return select(12.92*value,1.055*pow(value,vec3(1.0/2.4))-vec3(0.055),value>vec3(0.0031308));
}
@fragment fn linear_output(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {return vec4(color(p),1.0);}
@fragment fn srgb_output(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {return vec4(srgb(color(p)),1.0);}
"#;

impl AtlasImage {
    pub fn donor() -> Result<Self, String> {
        Self::from_png(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../donor/web/atlas.png"
        )))
    }
}

// CPU atlas import. The image bytes remain donor assets, not project-licensed code.
// donor/tools/atlas.py attributes FamiliarCraft to Scout339, CC BY-SA 4.0.

use std::io::Cursor;

const ATLAS_MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
const ATLAS_MAX_RGBA_BYTES: usize = 64 * 1024 * 1024;
const ATLAS_MAX_TILE_SIZE: u32 = 4096;
const ATLAS_MAX_LAYERS: u32 = 256;

#[derive(Clone, Debug)]
pub struct AtlasImage {
    pub tile_size: u32,
    pub layers: u32,
    pub mips: Vec<AtlasMip>,
}

#[derive(Clone, Debug)]
pub struct AtlasMip {
    pub size: u32,
    /// Contiguous square RGBA8 sRGB layers. Alpha is linear coverage.
    pub rgba: Vec<u8>,
    /// Contiguous square R8 layers. Full tint by default; material flags gate use.
    pub tint_mask: Vec<u8>,
}

impl AtlasImage {
    /// Decode a vertical stack of square tiles, then create each layer's mips.
    ///
    /// Layers 40/41 use donor alpha as tint metadata, not coverage. Their metadata
    /// is separated into R8 while their color alpha becomes fully opaque.
    pub fn from_png(encoded: &[u8]) -> Result<Self, String> {
        let (tile_size, height, layers, rgba_bytes) = atlas_dimensions(encoded)?;
        let mut decoder = png::Decoder::new(Cursor::new(encoded));
        decoder.set_limits(png::Limits {
            bytes: ATLAS_MAX_RGBA_BYTES,
        });
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        decoder.set_ignore_text_chunk(true);
        decoder.set_ignore_iccp_chunk(true);
        let mut reader = decoder
            .read_info()
            .map_err(|error| format!("atlas PNG header: {error}"))?;
        if reader.info().width != tile_size || reader.info().height != height {
            return Err("atlas PNG dimensions changed while decoding".into());
        }
        if reader.info().animation_control.is_some() {
            return Err("animated PNG is not a static texture atlas".into());
        }
        let (color_type, depth) = reader.output_color_type();
        if depth != png::BitDepth::Eight {
            return Err("atlas PNG did not expand to 8-bit channels".into());
        }
        let channels = match color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return Err("atlas PNG palette was not expanded".into()),
        };
        let expected_bytes = rgba_bytes / 4 * channels;
        if reader.output_buffer_size() != expected_bytes {
            return Err("atlas PNG decoded buffer dimensions are inconsistent".into());
        }
        let mut decoded = atlas_buffer(expected_bytes, 0)?;
        let frame = reader
            .next_frame(&mut decoded)
            .map_err(|error| format!("atlas PNG pixels: {error}"))?;
        if frame.width != tile_size
            || frame.height != height
            || frame.buffer_size() != expected_bytes
        {
            return Err("atlas PNG decoded frame dimensions are inconsistent".into());
        }
        reader
            .finish()
            .map_err(|error| format!("atlas PNG trailer: {error}"))?;
        let rgba = if color_type == png::ColorType::Rgba {
            decoded
        } else {
            let mut rgba = atlas_buffer(rgba_bytes, 0)?;
            for (source, target) in decoded.chunks_exact(channels).zip(rgba.chunks_exact_mut(4)) {
                match color_type {
                    png::ColorType::Grayscale => {
                        target.copy_from_slice(&[source[0], source[0], source[0], 255])
                    }
                    png::ColorType::GrayscaleAlpha => {
                        target.copy_from_slice(&[source[0], source[0], source[0], source[1]])
                    }
                    png::ColorType::Rgb => {
                        target.copy_from_slice(&[source[0], source[1], source[2], 255])
                    }
                    _ => unreachable!("validated PNG color type"),
                }
            }
            rgba
        };
        let mut base = AtlasMip {
            size: tile_size,
            rgba,
            tint_mask: atlas_buffer(rgba_bytes / 4, 255)?,
        };
        let layer_pixels = tile_size as usize * tile_size as usize;
        for layer in [40_usize, 41] {
            if layer >= layers as usize {
                continue;
            }
            for pixel in layer * layer_pixels..(layer + 1) * layer_pixels {
                let alpha = u16::from(base.rgba[pixel * 4 + 3]);
                base.tint_mask[pixel] = (alpha * 2).saturating_sub(255).min(255) as u8;
                base.rgba[pixel * 4 + 3] = 255;
            }
        }
        let mut mips = Vec::with_capacity(tile_size.ilog2() as usize + 1);
        mips.push(base);
        while mips.last().expect("base mip exists").size > 1 {
            let next = atlas_downsample(mips.last().expect("base mip exists"), layers)?;
            mips.push(next);
        }
        Ok(Self {
            tile_size,
            layers,
            mips,
        })
    }
}

fn atlas_dimensions(encoded: &[u8]) -> Result<(u32, u32, u32, usize), String> {
    if encoded.len() > ATLAS_MAX_INPUT_BYTES {
        return Err("atlas encoded PNG exceeds the input byte budget".into());
    }
    // Preflight IHDR before the decoder can allocate scanlines for untrusted dimensions.
    if encoded.len() < 33
        || &encoded[..8] != b"\x89PNG\r\n\x1a\n"
        || &encoded[8..12] != 13_u32.to_be_bytes().as_slice()
        || &encoded[12..16] != b"IHDR"
    {
        return Err("atlas requires a PNG with a standard IHDR".into());
    }
    let width = u32::from_be_bytes(encoded[16..20].try_into().expect("four-byte slice"));
    let height = u32::from_be_bytes(encoded[20..24].try_into().expect("four-byte slice"));
    if width == 0
        || !width.is_power_of_two()
        || width > ATLAS_MAX_TILE_SIZE
        || height == 0
        || height % width != 0
    {
        return Err("atlas dimensions require power-of-two square tiles up to 4096 pixels, stacked vertically".into());
    }
    let layers = height / width;
    if layers > ATLAS_MAX_LAYERS {
        return Err("atlas exceeds 256 texture layers".into());
    }
    let bytes = u64::from(width) * u64::from(height) * 4;
    if bytes > ATLAS_MAX_RGBA_BYTES as u64 {
        return Err("atlas decoded RGBA exceeds the memory budget".into());
    }
    Ok((width, height, layers, bytes as usize))
}

fn atlas_buffer(size: usize, value: u8) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(size)
        .map_err(|_| "atlas buffer allocation failed".to_owned())?;
    result.resize(size, value);
    Ok(result)
}

fn atlas_decode_srgb(channel: u8) -> f64 {
    let value = f64::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn atlas_encode_srgb(value: f64) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

fn atlas_downsample(source: &AtlasMip, layers: u32) -> Result<AtlasMip, String> {
    let size = source.size / 2;
    let output_pixels = size as usize * size as usize * layers as usize;
    let mut output = AtlasMip {
        size,
        rgba: atlas_buffer(output_pixels * 4, 0)?,
        tint_mask: atlas_buffer(output_pixels, 0)?,
    };
    for layer in 0..layers as usize {
        for y in 0..size as usize {
            for x in 0..size as usize {
                let input_base = layer * source.size as usize * source.size as usize
                    + y * 2 * source.size as usize
                    + x * 2;
                let inputs = [
                    input_base,
                    input_base + 1,
                    input_base + source.size as usize,
                    input_base + source.size as usize + 1,
                ];
                let target = layer * size as usize * size as usize + y * size as usize + x;
                for channel in 0..3 {
                    let linear = inputs
                        .iter()
                        .map(|&pixel| atlas_decode_srgb(source.rgba[pixel * 4 + channel]))
                        .sum::<f64>()
                        / 4.0;
                    output.rgba[target * 4 + channel] = atlas_encode_srgb(linear);
                }
                let alpha = inputs
                    .iter()
                    .map(|&pixel| u16::from(source.rgba[pixel * 4 + 3]))
                    .sum::<u16>();
                output.rgba[target * 4 + 3] = ((alpha + 2) / 4) as u8;
                let tint = inputs
                    .iter()
                    .map(|&pixel| u16::from(source.tint_mask[pixel]))
                    .sum::<u16>();
                output.tint_mask[target] = ((tint + 2) / 4) as u8;
            }
        }
    }
    Ok(output)
}

fn create_ambient(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    samples: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("signed removable ambient MRT"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

fn make_ambient_sky(device: &wgpu::Device, samples: u32) -> wgpu::RenderPipeline {
    let source = include_str!("sky.wgsl")
        .replace(
            "fn fragment_hdr_sky(input: VertexOutput) -> @location(0) vec4<f32>",
            "fn fragment_hdr_sky(input: VertexOutput) -> AmbientOutput",
        )
        .replace(
            "return vec4<f32>(sky_linear(input.uv), 0.0);",
            "return AmbientOutput(vec4<f32>(sky_linear(input.uv),0.0),vec4(0.0));",
        );
    let source = format!(
        "struct AmbientOutput{{@location(0)color:vec4<f32>,@location(1)ambient:vec4<f32>}};\n{source}"
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("unchanged fallback sky with zero ambient delta"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });
    let targets = crate::ambient_mrt::targets(None, true);
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fallback sky optional ambient MRT"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: Some(depth_state(false, wgpu::CompareFunction::Always)),
        multisample: wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment_hdr_sky"),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        multiview: None,
        cache: None,
    })
}

#[cfg(test)]
mod atlas_tests {
    use super::*;

    fn png_rgba(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("fixture header");
            writer.write_image_data(pixels).expect("fixture data");
            writer.finish().expect("fixture trailer");
        }
        bytes
    }

    #[test]
    fn black_white_mips_average_linear_light() {
        let fixture = png_rgba(
            2,
            2,
            &[
                0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
            ],
        );
        let image = AtlasImage::from_png(&fixture).unwrap();
        assert_eq!((image.tile_size, image.layers, image.mips.len()), (2, 1, 2));
        assert_eq!(image.mips[1].rgba, [188, 188, 188, 255]);
        assert_eq!(image.mips[1].tint_mask, [255]);
    }

    #[test]
    fn layer_mips_never_bleed_into_the_next_layer() {
        let mut pixels = [255, 0, 0, 255].repeat(4);
        pixels.extend([0, 0, 255, 255].repeat(4));
        let image = AtlasImage::from_png(&png_rgba(2, 4, &pixels)).unwrap();
        assert_eq!(image.layers, 2);
        assert_eq!(image.mips[1].rgba, [255, 0, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn checkerboard_alpha_stays_fractional_coverage() {
        let mut pixels = [255, 255, 255, 255].repeat(16);
        for y in 0..4 {
            for x in 0..4 {
                pixels[(y * 4 + x) * 4 + 3] = if (x + y) % 2 == 0 { 255 } else { 0 };
            }
        }
        let image = AtlasImage::from_png(&png_rgba(4, 4, &pixels)).unwrap();
        assert!(
            image.mips[1]
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel == [255, 255, 255, 128])
        );
        assert_eq!(image.mips[2].rgba, [255, 255, 255, 128]);
    }

    #[test]
    fn tint_metadata_is_separate_from_color_coverage() {
        let mut pixels = [11, 22, 33, 64].repeat(4 * 42);
        for (index, alpha) in [0_u8, 128, 192, 255].into_iter().enumerate() {
            pixels[(40 * 4 + index) * 4 + 3] = alpha;
            pixels[(41 * 4 + index) * 4 + 3] = alpha;
        }
        let image = AtlasImage::from_png(&png_rgba(2, 84, &pixels)).unwrap();
        assert_eq!(&image.mips[0].tint_mask[160..164], &[0, 1, 129, 255]);
        assert_eq!(&image.mips[0].tint_mask[164..168], &[0, 1, 129, 255]);
        assert!(
            image.mips[0].rgba[160 * 4..]
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 255)
        );
        assert!(
            image.mips[0].rgba[..160 * 4]
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 64)
        );
        assert_eq!(image.mips[1].tint_mask[40], 96);
        assert_eq!(image.mips[1].rgba[40 * 4 + 3], 255);
    }

    #[test]
    fn rejects_malformed_shapes_and_excess_layer_count() {
        assert!(AtlasImage::from_png(b"not png").is_err());
        assert!(
            AtlasImage::from_png(&png_rgba(3, 3, &[255; 3 * 3 * 4]))
                .unwrap_err()
                .contains("dimensions")
        );
        assert!(
            AtlasImage::from_png(&png_rgba(2, 3, &[255; 2 * 3 * 4]))
                .unwrap_err()
                .contains("dimensions")
        );
        assert!(
            AtlasImage::from_png(&png_rgba(1, 257, &[255; 257 * 4]))
                .unwrap_err()
                .contains("256")
        );
    }

    #[test]
    fn rejects_oversized_ihdr_before_decoder_allocations() {
        let original = png_rgba(1, 1, &[1, 2, 3, 255]);
        let mut huge = original.clone();
        huge[16..20].copy_from_slice(&65_536_u32.to_be_bytes());
        huge[20..24].copy_from_slice(&65_536_u32.to_be_bytes());
        assert!(
            AtlasImage::from_png(&huge)
                .unwrap_err()
                .contains("dimensions")
        );
        let mut budget = original;
        budget[16..20].copy_from_slice(&4096_u32.to_be_bytes());
        budget[20..24].copy_from_slice(&8192_u32.to_be_bytes());
        assert!(
            AtlasImage::from_png(&budget)
                .unwrap_err()
                .contains("memory budget")
        );
    }

    #[test]
    fn expands_rgb_grayscale_palette_and_sixteen_bit_inputs() {
        for (color, depth, source, expected) in [
            (
                png::ColorType::Rgb,
                png::BitDepth::Eight,
                vec![10, 20, 30],
                [10, 20, 30, 255],
            ),
            (
                png::ColorType::Grayscale,
                png::BitDepth::Eight,
                vec![40],
                [40, 40, 40, 255],
            ),
            (
                png::ColorType::GrayscaleAlpha,
                png::BitDepth::Eight,
                vec![50, 60],
                [50, 50, 50, 60],
            ),
            (
                png::ColorType::Grayscale,
                png::BitDepth::Sixteen,
                vec![70, 80],
                [70, 70, 70, 255],
            ),
            (
                png::ColorType::Indexed,
                png::BitDepth::One,
                vec![0],
                [10, 20, 30, 77],
            ),
        ] {
            let mut encoded = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
                encoder.set_color(color);
                encoder.set_depth(depth);
                if color == png::ColorType::Indexed {
                    encoder.set_palette(vec![10, 20, 30]);
                    encoder.set_trns(vec![77]);
                }
                let mut writer = encoder.write_header().unwrap();
                writer.write_image_data(&source).unwrap();
                writer.finish().unwrap();
            }
            let image = AtlasImage::from_png(&encoded).unwrap();
            assert_eq!(image.mips[0].rgba, expected, "{color:?}/{depth:?}");
        }
    }

    #[test]
    fn corrupted_pixel_stream_fails_crc_or_decode() {
        let mut encoded = png_rgba(2, 2, &[42; 16]);
        let idat = encoded
            .windows(4)
            .position(|bytes| bytes == b"IDAT")
            .unwrap();
        encoded[idat + 4] ^= 0xff;
        assert!(AtlasImage::from_png(&encoded).is_err());
    }
}
