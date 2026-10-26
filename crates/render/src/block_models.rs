//! Immutable block-model catalog and bounded GPU instance buckets. The catalog is
//! built once from allowed asset data; edits upload records, never model vertices.
use crate::{
    MSAA_SAMPLES,
    shadows::{
        DynamicShadowCaster, ShadowDamageTracker, ShadowGpu, ShadowPrepareWork, ShadowRect,
        WorldBounds, crop_camera,
    },
    terrain_gpu::{Camera, TerrainGeometry, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
pub const MODEL_KINDS: usize = 17;
pub mod kind {
    pub const CHEST: u32 = 0;
    pub const LADDER: u32 = 1;
    pub const SNOW: u32 = 2;
    pub const WALL_TORCH: u32 = 3;
    pub const CROSS: u32 = 4;
    pub const CUBE_MACHINE: u32 = 5;
    pub const INFUSER: u32 = 6;
    pub const SEPARATOR: u32 = 7;
    pub const HEAT_GENERATOR: u32 = 8;
    pub const WIND_GENERATOR: u32 = 9;
    pub const ENERGY_CUBE: u32 = 10;
    pub const CHEMICAL_TANK: u32 = 11;
    pub const PUMP: u32 = 12;
    pub const CABLE: u32 = 13;
    pub const TUBE: u32 = 14;
    pub const PIPE: u32 = 15;
    pub const TRANSPORTER: u32 = 16;
}
pub const ACTIVE: u32 = 1;
/// Reserved stable slots remain allocated but publish no primary or shadow draw.
pub const HIDDEN: u32 = 2;
/// Six annotations derived from the cube catalog; terrain remains the only body.
/// This record never contributes geometry to any shadow pass.
pub const PORT_MARKER_ONLY: u32 = 4;
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct BlockInstance {
    pub position: [f32; 3],
    pub yaw: f32,
    pub kind: u32,
    pub flags: u32,
    pub connections: u32,
    pub light: u32,
    pub phase: f32,
    pub amount: f32,
    pub tint: u32,
    pub layer: u32,
    /// First three words: cube face layers Down|Up<<16, North|South<<16,
    /// West|East<<16. Fourth: public machine presence bit31 plus World Face6
    /// modes (three bits per face); this annotation does not alter geometry.
    pub reserved: [u32; 4],
}
impl BlockInstance {
    pub fn hidden() -> Self {
        Self {
            flags: HIDDEN,
            ..Self::default()
        }
    }
}
impl Default for BlockInstance {
    fn default() -> Self {
        Self {
            position: [0.; 3],
            yaw: 0.,
            kind: 0,
            flags: 0,
            connections: 0,
            light: 15,
            phase: 0.,
            amount: 1.,
            tint: u32::MAX,
            layer: 0,
            reserved: [0; 4],
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct BlockVertex {
    pub position: [f32; 3],
    pub layer: u32,
    pub normal: [f32; 3],
    pub flags: u32,
    pub uv: [f32; 2],
    pub bone: u32,
    pub kind: u32,
    pub pivot: [f32; 3],
    pub frames: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ModelRange {
    pub draw: [u32; 4],
    pub minimum: [f32; 4],
    pub maximum: [f32; 4],
}
struct FaceSpec {
    kind: u32,
    points: [[f32; 3]; 4],
    normal: [f32; 3],
    uv: [[f32; 2]; 4],
    layer: u32,
    flags: u32,
    bone: u32,
    pivot: [f32; 3],
    frames: u32,
}
include!("block_model_data.rs");
pub struct BlockCatalog {
    pub vertices: Vec<BlockVertex>,
    pub ranges: [ModelRange; MODEL_KINDS],
}
impl Default for BlockCatalog {
    fn default() -> Self {
        Self::new()
    }
}
impl BlockCatalog {
    pub fn new() -> Self {
        let mut vertices = Vec::with_capacity(FACES.len() * 6);
        let mut ranges = [ModelRange::default(); MODEL_KINDS];
        for (kind, range) in ranges.iter_mut().enumerate() {
            range.draw[1] = vertices.len() as u32;
            range.minimum = [f32::INFINITY; 4];
            range.maximum = [f32::NEG_INFINITY; 4];
            for face in FACES.iter().filter(|f| f.kind == kind as u32) {
                for corner in [0, 1, 2, 0, 2, 3] {
                    for axis in 0..3 {
                        range.minimum[axis] = range.minimum[axis].min(face.points[corner][axis]);
                        range.maximum[axis] = range.maximum[axis].max(face.points[corner][axis]);
                    }
                    vertices.push(BlockVertex {
                        position: face.points[corner],
                        layer: face.layer,
                        normal: face.normal,
                        flags: face.flags,
                        uv: face.uv[corner],
                        bone: face.bone,
                        kind: face.kind,
                        // Bone0 has no animation pivot; retain canonical face
                        // coordinates for a procedural public-port annotation.
                        pivot: if face.bone == 0 {
                            let uv = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]][corner];
                            [uv[0], uv[1], 0.]
                        } else {
                            face.pivot
                        },
                        frames: face.frames,
                    });
                }
            }
            range.draw[0] = vertices.len() as u32 - range.draw[1];
            range.minimum[3] = 0.;
            range.maximum[3] = 0.;
            // All four facings, open chest lid and rotating tall rotor must fit.
            let radius = range.minimum[0]
                .abs()
                .max(range.maximum[0].abs())
                .max(range.minimum[2].abs())
                .max(range.maximum[2].abs())
                * 1.415;
            range.minimum[0] = -radius;
            range.minimum[2] = -radius;
            range.maximum[0] = radius;
            range.maximum[2] = radius;
            if kind == kind::CHEST as usize {
                range.maximum[1] = 1.6;
                range.minimum[2] = -0.8;
            }
            if kind == kind::WIND_GENERATOR as usize {
                range.minimum[0] = -2.2;
                range.maximum[0] = 2.2;
                range.minimum[2] = -2.2;
                range.maximum[2] = 2.2;
                range.maximum[1] = 6.6;
            }
        }
        Self { vertices, ranges }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct BlockConfig {
    pub max_instances: u32,
    pub atlas_layers: u32,
}
impl Default for BlockConfig {
    fn default() -> Self {
        Self {
            max_instances: 16384,
            atlas_layers: 186,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    InvalidConfig,
    InvalidInstance,
    InvalidCamera,
    UploadBudget,
    InvalidIndex,
}
impl std::fmt::Display for BlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for BlockError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockWork {
    pub upload_bytes: u64,
    pub instances: u32,
    pub cpu_vertices: u32,
    pub startup_vertices: u32,
    pub pipeline_compilations: u32,
    pub cull_dispatches: u32,
    pub camera_upload_bytes: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    view_projection: [[f32; 4]; 4],
    params: [u32; 4],
    reserved: [u32; 4],
}
struct ShadowResources {
    cull_cameras: [wgpu::Buffer; 3],
    cameras: [wgpu::Buffer; 3],
    binds: [wgpu::BindGroup; 3],
    cull_binds: [wgpu::BindGroup; 3],
    indirect: wgpu::Buffer,
    pipeline: wgpu::RenderPipeline,
}
pub struct BlockModelsGpu {
    config: BlockConfig,
    instances: wgpu::Buffer,
    vertices: wgpu::Buffer,
    indirect: wgpu::Buffer,
    camera: wgpu::Buffer,
    bind: wgpu::BindGroup,
    cull_bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    glass: wgpu::RenderPipeline,
    colored_transporters: u32,
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    previous: Vec<BlockInstance>,
    active: u32,
    kind_counts: [u32; MODEL_KINDS],
    shadow_kind_counts: [u32; MODEL_KINDS],
    revision: u64,
    shadow_revision: u64,
    shadow_damage: ShadowDamageTracker,
    model_bounds: [ModelRange; MODEL_KINDS],
    last: Option<(Camera, u64)>,
    shadow: Option<ShadowResources>,
    work: BlockWork,
}
impl BlockModelsGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: BlockConfig,
        shadows: Option<&ShadowGpu>,
    ) -> Result<Self, BlockError> {
        Self::new_prepared(device, queue, geometry, config, shadows, None)
    }
    /// Prepare shared atmospheric lighting once during loading.
    pub fn new_with_atmosphere(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: BlockConfig,
        shadows: Option<&ShadowGpu>,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
    ) -> Result<Self, BlockError> {
        Self::new_prepared(device, queue, geometry, config, shadows, Some(atmosphere))
    }
    fn new_prepared(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: BlockConfig,
        shadows: Option<&ShadowGpu>,
        atmosphere: Option<&crate::atmosphere::AtmosphereGpu>,
    ) -> Result<Self, BlockError> {
        if config.max_instances == 0
            || config.atlas_layers < 185
            || u64::from(config.max_instances) * 64
                > u64::from(device.limits().max_storage_buffer_binding_size)
            || u64::from(config.max_instances) * MODEL_KINDS as u64 * 4
                > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(BlockError::InvalidConfig);
        }
        let catalog = BlockCatalog::new();
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let instances = buffer(
            "64B block model records",
            u64::from(config.max_instances) * 64,
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let vertices = buffer(
            "immutable block model vertices",
            catalog.vertices.len() as u64 * 64,
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let ranges = buffer(
            "static block ranges and bounds",
            MODEL_KINDS as u64 * 48,
            storage,
        );
        queue.write_buffer(&vertices, 0, bytemuck::cast_slice(&catalog.vertices));
        queue.write_buffer(&ranges, 0, bytemuck::cast_slice(&catalog.ranges));
        let visible = buffer(
            "GPU block model compacted indices",
            u64::from(config.max_instances) * MODEL_KINDS as u64 * 4,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = buffer(
            "block model indirect buckets",
            MODEL_KINDS as u64 * 16,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
        );
        let camera = buffer(
            "block model camera",
            96,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let v = wgpu::ShaderStages::VERTEX;
        let f = wgpu::ShaderStages::FRAGMENT;
        let c = wgpu::ShaderStages::COMPUTE;
        let base_entries = [
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
        ];
        let mut entries = base_entries.to_vec();
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
        let render_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("static block atlas layout"),
            entries: &entries,
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("static block cull layout"),
            entries: &[
                buffer_layout(0, c, true, true),
                buffer_layout(1, c, false, true),
                buffer_layout(2, c, false, false),
                buffer_layout(3, c, false, false),
                buffer_layout(4, c, false, true),
            ],
        });
        let make_bind = |layout: &wgpu::BindGroupLayout,
                         camera: &wgpu::Buffer,
                         visible: &wgpu::Buffer,
                         lighting: bool| {
            let mut entries = vec![
                buffer_binding(0, camera),
                buffer_binding(1, &instances),
                buffer_binding(2, &vertices),
                buffer_binding(3, visible),
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(geometry.colors),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(geometry.sampler),
                },
            ];
            if lighting && let Some(s) = shadows {
                entries.extend([
                    buffer_binding(9, s.sampling_buffer()),
                    wgpu::BindGroupEntry {
                        binding: 10,
                        resource: wgpu::BindingResource::TextureView(s.depth_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 11,
                        resource: wgpu::BindingResource::Sampler(s.sampler()),
                    },
                ]);
            }
            if lighting && let Some(atmosphere) = atmosphere {
                entries.push(buffer_binding(20, atmosphere.lighting_buffer()));
            }
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("static block draw tables"),
                layout,
                entries: &entries,
            })
        };
        let make_cull = |camera: &wgpu::Buffer, visible: &wgpu::Buffer, indirect: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("static block cull tables"),
                layout: &cull_layout,
                entries: &[
                    buffer_binding(0, camera),
                    buffer_binding(1, &instances),
                    buffer_binding(2, visible),
                    buffer_binding(3, indirect),
                    buffer_binding(4, &ranges),
                ],
            })
        };
        let bind = make_bind(&render_layout, &camera, &visible, true);
        let cull_bind = make_cull(&camera, &visible, &indirect);
        let source = crate::atmosphere::surface_shader(
            &include_str!("block_models.wgsl")
                .replace("// PORTS", include_str!("block_ports.wgsl")),
            shadows.is_some(),
            atmosphere.is_some(),
            true,
            if shadows.is_some() {
                "return light.rgb+vec3(light.a*(0.15+0.85*cascade_shadow(world,normal)*max(dot(normal,normalize(shadow_sampling.sun.xyz)),0.)));"
            } else {
                "return vec3(0.65+0.35*max(dot(normal,normalize(vec3(0.35,0.8,0.2))),0.));"
            },
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU static block poses"),
            source: wgpu::ShaderSource::Wgsl(source.clone().into()),
        });
        let pipeline = render_pipeline(device, &render_layout, &shader, false, false);
        let glass = render_pipeline(device, &render_layout, &shader, false, true);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU static block frustum buckets"),
            source: wgpu::ShaderSource::Wgsl(include_str!("block_models_cull.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("block model compute"),
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
        let shadow = if shadows.is_some_and(|s| s.cache().config().dynamic_casters) {
            let cameras = std::array::from_fn(|_| {
                buffer(
                    "block caster camera",
                    96,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                )
            });
            let cull_cameras = std::array::from_fn(|_| {
                buffer(
                    "block caster cropped cull camera",
                    96,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                )
            });
            let visible = buffer(
                "independent block caster compacted indices",
                u64::from(config.max_instances) * MODEL_KINDS as u64 * 4,
                wgpu::BufferUsages::STORAGE,
            );
            let indirect = buffer(
                "independent block caster indirect",
                MODEL_KINDS as u64 * 16,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
            );
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("block caster draw layout"),
                entries: &base_entries,
            });
            let binds = std::array::from_fn(|i| make_bind(&layout, &cameras[i], &visible, false));
            let cull_binds =
                std::array::from_fn(|i| make_cull(&cull_cameras[i], &visible, &indirect));
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("same block pose for shadows"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            Some(ShadowResources {
                cull_cameras,
                cameras,
                binds,
                cull_binds,
                indirect,
                pipeline: render_pipeline(device, &layout, &shader, true, false),
            })
        } else {
            None
        };
        Ok(Self {
            config,
            instances,
            vertices,
            indirect,
            camera,
            bind,
            cull_bind,
            pipeline,
            glass,
            colored_transporters: 0,
            reset,
            cull,
            previous: Vec::with_capacity(config.max_instances as usize),
            active: 0,
            kind_counts: [0; MODEL_KINDS],
            shadow_kind_counts: [0; MODEL_KINDS],
            revision: 0,
            shadow_revision: 0,
            shadow_damage: ShadowDamageTracker::new(config.max_instances as usize),
            model_bounds: catalog.ranges,
            last: None,
            shadow,
            work: BlockWork {
                upload_bytes: catalog.vertices.len() as u64 * 64 + MODEL_KINDS as u64 * 48,
                startup_vertices: catalog.vertices.len() as u32,
                pipeline_compilations: 4 + u32::from(
                    shadows.is_some_and(|s| s.cache().config().dynamic_casters),
                ),
                ..BlockWork::default()
            },
        })
    }
    fn validate(&self, i: &BlockInstance) -> bool {
        i.position
            .iter()
            .chain([i.yaw, i.phase, i.amount].iter())
            .all(|x| x.is_finite())
            && i.kind < MODEL_KINDS as u32
            && i.flags & !(ACTIVE | HIDDEN | PORT_MARKER_ONLY) == 0
            && (i.flags & PORT_MARKER_ONLY == 0 || i.kind == kind::CUBE_MACHINE)
            && i.connections & !0xfff == 0
            && i.layer < self.config.atlas_layers
            && (0.0..=1.0).contains(&i.amount)
            && (i.kind != kind::CHEST || (0.0..=1.0).contains(&i.phase))
            && (i.kind != kind::CUBE_MACHINE
                || i.reserved[..3].iter().all(|p| {
                    p & 0xffff < self.config.atlas_layers && p >> 16 < self.config.atlas_layers
                }))
    }
    /// Loading-only fixed slot reservation. Subsequent additions/removals use
    /// one update() record and HIDDEN, without moving other live instance slots.
    pub fn reserve_slots(
        &mut self,
        queue: &wgpu::Queue,
        count: u32,
        budget: &mut UploadBudget,
    ) -> Result<BlockWork, BlockError> {
        if !self.previous.is_empty() || count == 0 || count > self.config.max_instances {
            return Err(BlockError::InvalidIndex);
        }
        let bytes = u64::from(count) * 64;
        if budget.remaining_bytes < bytes {
            return Err(BlockError::UploadBudget);
        }
        self.previous
            .resize(count as usize, BlockInstance::hidden());
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.previous));
        budget.remaining_bytes -= bytes;
        self.revision = self.revision.wrapping_add(1);
        self.work.upload_bytes += bytes;
        Ok(BlockWork {
            upload_bytes: bytes,
            instances: count,
            ..BlockWork::default()
        })
    }
    pub fn active_instances(&self) -> u32 {
        self.kind_counts.iter().sum()
    }
    pub fn reserved_slots(&self) -> usize {
        self.previous.len()
    }
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        instances: &[BlockInstance],
        budget: &mut UploadBudget,
    ) -> Result<BlockWork, BlockError> {
        if instances.len() > self.config.max_instances as usize
            || instances.iter().any(|i| !self.validate(i))
        {
            return Err(BlockError::InvalidInstance);
        }
        if self.previous == instances {
            return Ok(BlockWork::default());
        }
        let bytes = instances.len() as u64 * 64;
        if budget.remaining_bytes < bytes {
            return Err(BlockError::UploadBudget);
        }
        let visibility_changed = self.previous.len() != instances.len()
            || self.previous.iter().zip(instances).any(|(a, b)| {
                a.position != b.position || a.kind != b.kind || (a.flags ^ b.flags) & HIDDEN != 0
            });
        let shadow_changed = (0..self.previous.len().max(instances.len()))
            .any(|index| !same_shadow(self.previous.get(index), instances.get(index)));
        if shadow_changed {
            for index in 0..self.previous.len().max(instances.len()) {
                let old = self.previous.get(index);
                let new = instances.get(index);
                if !same_shadow(old, new) {
                    self.shadow_damage.record(
                        index,
                        old.and_then(|value| block_shadow_bounds(value, &self.model_bounds)),
                    );
                }
            }
        }
        if bytes > 0 {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));
        }
        self.previous.clear();
        self.previous.extend_from_slice(instances);
        self.refresh_active();
        if visibility_changed {
            self.revision = self.revision.wrapping_add(1);
        }
        if shadow_changed {
            self.shadow_revision = self.shadow_revision.wrapping_add(1)
        }
        budget.remaining_bytes -= bytes;
        let work = BlockWork {
            upload_bytes: bytes,
            instances: instances.len() as u32,
            ..BlockWork::default()
        };
        self.work.upload_bytes += bytes;
        Ok(work)
    }
    /// One cell/state edit is one fixed 64-byte record, without touching the mesh.
    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        index: u32,
        value: BlockInstance,
        budget: &mut UploadBudget,
    ) -> Result<BlockWork, BlockError> {
        if index as usize >= self.previous.len() {
            return Err(BlockError::InvalidIndex);
        }
        if !self.validate(&value) {
            return Err(BlockError::InvalidInstance);
        }
        let old = self.previous[index as usize];
        if old == value {
            return Ok(BlockWork::default());
        }
        if budget.remaining_bytes < 64 {
            return Err(BlockError::UploadBudget);
        }
        if !same_shadow(Some(&old), Some(&value)) {
            self.shadow_damage.record(
                index as usize,
                block_shadow_bounds(&old, &self.model_bounds),
            );
        }
        queue.write_buffer(
            &self.instances,
            u64::from(index) * 64,
            bytemuck::bytes_of(&value),
        );
        self.previous[index as usize] = value;
        self.count_instance(old, false);
        self.count_instance(value, true);
        if old.position != value.position
            || old.kind != value.kind
            || (old.flags ^ value.flags) & HIDDEN != 0
        {
            self.revision = self.revision.wrapping_add(1);
        }
        if !same_shadow(Some(&old), Some(&value)) {
            self.shadow_revision = self.shadow_revision.wrapping_add(1)
        }
        budget.remaining_bytes -= 64;
        self.work.upload_bytes += 64;
        Ok(BlockWork {
            upload_bytes: 64,
            instances: 1,
            ..BlockWork::default()
        })
    }
    fn refresh_active(&mut self) {
        self.kind_counts.fill(0);
        self.shadow_kind_counts.fill(0);
        self.colored_transporters = 0;
        self.active = 0;
        for index in 0..self.previous.len() {
            self.count_instance(self.previous[index], true);
        }
    }
    fn count_instance(&mut self, instance: BlockInstance, add: bool) {
        if instance.flags & HIDDEN != 0 {
            return;
        }
        if instance.kind == kind::TRANSPORTER
            && instance.reserved[3] & 0x40000000 != 0
            && (1..=18).contains(&(instance.reserved[3] & 31))
        {
            if add {
                self.colored_transporters += 1;
            } else {
                self.colored_transporters -= 1;
            }
        }
        if instance.flags & PORT_MARKER_ONLY == 0 {
            let count = &mut self.shadow_kind_counts[instance.kind as usize];
            if add {
                *count += 1;
            } else {
                *count -= 1;
            }
        }
        let count = &mut self.kind_counts[instance.kind as usize];
        if add {
            *count += 1;
        } else {
            *count -= 1;
        }
        if *count == 0 {
            self.active &= !(1 << instance.kind);
        } else {
            self.active |= 1 << instance.kind;
        }
    }
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: Camera,
    ) -> Result<BlockWork, BlockError> {
        if camera
            .view_projection
            .iter()
            .flatten()
            .any(|v| !v.is_finite())
        {
            return Err(BlockError::InvalidCamera);
        }
        if self.last == Some((camera, self.revision)) {
            return Ok(BlockWork::default());
        }
        self.prepare(queue, encoder, &self.camera, &self.cull_bind, camera);
        self.last = Some((camera, self.revision));
        Ok(BlockWork {
            cull_dispatches: 2,
            camera_upload_bytes: 96,
            ..BlockWork::default()
        })
    }
    fn prepare(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        buffer: &wgpu::Buffer,
        bind: &wgpu::BindGroup,
        camera: Camera,
    ) {
        queue.write_buffer(
            buffer,
            0,
            bytemuck::bytes_of(&CameraGpu {
                view_projection: camera.view_projection,
                params: [self.previous.len() as u32, self.config.max_instances, 0, 0],
                reserved: [0; 4],
            }),
        );
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("block model independent GPU cull"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, bind, &[]);
        pass.set_pipeline(&self.reset);
        pass.dispatch_workgroups(1, 1, 1);
        pass.set_pipeline(&self.cull);
        pass.dispatch_workgroups((self.previous.len() as u32).max(1).div_ceil(64), 1, 1);
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        let mut draws = self.draw_buckets(pass, &self.indirect);
        if self.colored_transporters != 0 {
            pass.set_pipeline(&self.glass);
            pass.draw_indirect(&self.indirect, u64::from(kind::TRANSPORTER) * 16);
            draws += 1;
        }
        draws
    }
    fn draw_buckets(&self, pass: &mut wgpu::RenderPass<'_>, indirect: &wgpu::Buffer) -> u32 {
        for kind in 0..MODEL_KINDS {
            if self.active & (1 << kind) != 0 {
                pass.draw_indirect(indirect, kind as u64 * 16);
            }
        }
        self.active.count_ones()
    }
    pub fn indirect_buffer(&self) -> &wgpu::Buffer {
        &self.indirect
    }
    pub fn model_vertex_buffer(&self) -> &wgpu::Buffer {
        &self.vertices
    }
    /// Read-only GPU capture source; each reserved stable slot is exactly 64 bytes.
    pub fn instance_buffer(&self) -> &wgpu::Buffer {
        &self.instances
    }
    pub fn take_work(&mut self) -> BlockWork {
        std::mem::take(&mut self.work)
    }
}
fn block_shadow_bounds(
    value: &BlockInstance,
    ranges: &[ModelRange; MODEL_KINDS],
) -> Option<WorldBounds> {
    if value.flags & (HIDDEN | PORT_MARKER_ONLY) != 0 {
        return None;
    }
    let range = &ranges[value.kind as usize];
    Some(WorldBounds {
        min: std::array::from_fn(|axis| value.position[axis] + range.minimum[axis]),
        max: std::array::from_fn(|axis| value.position[axis] + range.maximum[axis]),
    })
}
fn same_shadow(a: Option<&BlockInstance>, b: Option<&BlockInstance>) -> bool {
    let a = a.filter(|v| v.flags & PORT_MARKER_ONLY == 0);
    let b = b.filter(|v| v.flags & PORT_MARKER_ONLY == 0);
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_geometry(a, b),
        _ => false,
    }
}
fn same_geometry(a: &BlockInstance, b: &BlockInstance) -> bool {
    a.position == b.position
        && a.yaw == b.yaw
        && a.kind == b.kind
        && a.connections == b.connections
        && a.phase == b.phase
        && a.amount == b.amount
        && a.layer == b.layer
        && a.reserved[..3] == b.reserved[..3]
        && (a.flags ^ b.flags) & HIDDEN == 0
}
impl DynamicShadowCaster for BlockModelsGpu {
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
            |index| {
                self.previous
                    .get(index)
                    .and_then(|value| block_shadow_bounds(value, &self.model_bounds))
            },
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
        let Some(shadow) = &self.shadow else {
            return ShadowPrepareWork::default();
        };
        if self.shadow_kind_counts.iter().all(|n| *n == 0) {
            return ShadowPrepareWork::default();
        }
        let full = CameraGpu {
            view_projection: camera.view_projection,
            params: [self.previous.len() as u32, self.config.max_instances, 1, 0],
            reserved: [0; 4],
        };
        let cropped = CameraGpu {
            view_projection: crop_camera(camera, rect, resolution).view_projection,
            ..full
        };
        queue.write_buffer(&shadow.cameras[cascade], 0, bytemuck::bytes_of(&full));
        queue.write_buffer(
            &shadow.cull_cameras[cascade],
            0,
            bytemuck::bytes_of(&cropped),
        );
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("localized block model shadow cull"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &shadow.cull_binds[cascade], &[]);
        pass.set_pipeline(&self.reset);
        pass.dispatch_workgroups(1, 1, 1);
        pass.set_pipeline(&self.cull);
        pass.dispatch_workgroups((self.previous.len() as u32).div_ceil(64), 1, 1);
        ShadowPrepareWork {
            dispatches: 2,
            upload_bytes: 192,
        }
    }
    fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, cascade: usize) -> u32 {
        let Some(s) = &self.shadow else { return 0 };
        if self.previous.is_empty() {
            return 0;
        }
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, &s.binds[cascade], &[]);
        let mut draws = 0;
        for (kind, count) in self.shadow_kind_counts.iter().enumerate() {
            if *count != 0 {
                pass.draw_indirect(&s.indirect, kind as u64 * 16);
                draws += 1;
            }
        }
        draws
    }
}
fn render_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    shadow: bool,
    glass: bool,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("static block vertex pulling"),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    let color_targets = [Some(wgpu::ColorTargetState {
        format: wgpu::TextureFormat::Rgba16Float,
        blend: if glass {
            Some(wgpu::BlendState::ALPHA_BLENDING)
        } else {
            None
        },
        write_mask: wgpu::ColorWrites::ALL,
    })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("static blocks immutable model pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: !glass,
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: if shadow { 1 } else { MSAA_SAMPLES },
            alpha_to_coverage_enabled: !shadow && !glass,
            ..Default::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(if glass {
                "glass_fragment"
            } else if shadow {
                "shadow_fragment"
            } else {
                "fragment"
            }),
            compilation_options: Default::default(),
            targets: if shadow { &[] } else { &color_targets },
        }),
        multiview: None,
        cache: None,
    })
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_has_all_models_and_bounded_asset_indices() {
        let c = BlockCatalog::new();
        assert_eq!(std::mem::size_of::<BlockInstance>(), 64);
        assert_eq!(std::mem::size_of::<BlockVertex>(), 64);
        assert_eq!(std::mem::size_of::<ModelRange>(), 48);
        assert_eq!(c.vertices.len(), 7512);
        for (kind, r) in c.ranges.iter().enumerate() {
            assert!(r.draw[0] > 0);
            assert_eq!(r.draw[0] % 3, 0);
            assert!(
                c.vertices[r.draw[1] as usize..(r.draw[1] + r.draw[0]) as usize]
                    .iter()
                    .all(|v| v.kind == kind as u32
                        && v.layer < 185
                        && v.position
                            .iter()
                            .chain(v.normal.iter())
                            .all(|x| x.is_finite()))
            );
        }
        assert!(c.ranges[kind::WIND_GENERATOR as usize].maximum[1] > 6.);
    }
}
