//! Directional depth from the committed public height/proxy front. These lists
//! are independent of primary visibility and are reused sequentially per cascade.
use crate::{
    height_gpu::HeightShadowSource,
    lod_gpu::ProxyShadowSource,
    shadows::{
        DynamicShadowCaster, ShadowGpu, ShadowNode, ShadowNodeTimings, ShadowPrepareWork,
        ShadowRect, WorldBounds, crop_camera, footprint,
    },
    terrain_gpu::Camera,
};
use bytemuck::{Pod, Zeroable};
use std::num::NonZeroU64;

/// Explicit material provenance: unlisted height layers cast no shadow. Water,
/// alpha-cutout layers and unknown IDs must remain unset by the loading owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeightShadowPolicy {
    opaque: [u32; 8],
}
impl HeightShadowPolicy {
    pub fn from_opaque_layers(layers: impl IntoIterator<Item = u16>) -> Result<Self, Error> {
        let mut result = Self::default();
        for layer in layers {
            if layer >= 256 {
                return Err(Error::InvalidConfig);
            }
            result.opaque[usize::from(layer / 32)] |= 1 << (layer % 32);
        }
        Ok(result)
    }
    pub fn casts(self, layer: u16) -> bool {
        layer < 256 && self.opaque[usize::from(layer / 32)] & (1 << (layer % 32)) != 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    SourceNeedsReprepare,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    view_projection: [[f32; 4]; 4],
    eye: [f32; 4],
    direction: [f32; 4],
    params: [u32; 4],
    opaque: [u32; 8],
}
const CAMERA_BYTES: u64 = std::mem::size_of::<CameraGpu>() as u64;
const CAMERA_SHADER: &str = "struct Camera{view_projection:mat4x4<f32>,eye:vec4<f32>,direction:vec4<f32>,params:vec4<u32>,opaque:array<vec4<u32>,2>};";
const LOD_CAMERA: &str = "struct Camera{view_projection:mat4x4<f32>,eye:vec4<f32>,direction:vec4<f32>,params:vec4<u32>};";

pub struct DistantShadowGpu {
    height_headers: wgpu::Buffer,
    height_payload: wgpu::Buffer,
    near_cover: wgpu::Buffer,
    proxy_objects: wgpu::Buffer,
    proxy_payload: wgpu::Buffer,
    max_tiles: u32,
    max_objects: u32,
    max_payload_words: u32,
    atlas_layers: [u32; 2],
    source: [u64; 2],
    enabled: bool,
    policy: HeightShadowPolicy,
    cameras: [wgpu::Buffer; 3],
    cull_cameras: [wgpu::Buffer; 3],
    visible: wgpu::Buffer,
    indirect: wgpu::Buffer,
    fallback_bounds: Option<wgpu::Buffer>,
    cull_binds: [wgpu::BindGroup; 3],
    height_binds: [wgpu::BindGroup; 3],
    opaque_binds: [wgpu::BindGroup; 3],
    cutout_binds: [wgpu::BindGroup; 3],
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    height: wgpu::RenderPipeline,
    opaque: wgpu::RenderPipeline,
    cutout: wgpu::RenderPipeline,
    projected: bool,
    diagnostics: Option<ShadowNodeTimings>,
}
impl DistantShadowGpu {
    /// Prepare during loading, after the public render arenas and ShadowGpu.
    /// Newly prepared sources stay disabled until the owner certifies its front.
    pub fn new(
        device: &wgpu::Device,
        heights: HeightShadowSource<'_>,
        proxies: ProxyShadowSource<'_>,
        shadows: &ShadowGpu,
        policy: HeightShadowPolicy,
    ) -> Result<Self, Error> {
        let align = u64::from(device.limits().min_storage_buffer_offset_alignment);
        let span = |bytes: u64| bytes.div_ceil(align) * align;
        let height_span = span(u64::from(heights.max_tiles) * 4);
        let opaque_span = span(u64::from(proxies.max_objects) * 4);
        let cutout_span = u64::from(proxies.max_payload_words / 4) * 8;
        let total = height_span + opaque_span + cutout_span;
        if heights.max_tiles == 0
            || !shadows.cache().config().dynamic_casters
            || proxies.max_objects == 0
            || proxies.max_payload_words < 4
            || heights.atlas_layers == 0
            || proxies.atlas_layers == 0
            || heights.atlas_layers > 256
            || proxies.atlas_layers > 256
            || heights.headers.size() < u64::from(heights.max_tiles) * 64
            || heights.payload.size() < u64::from(heights.max_tiles) * crate::height_gpu::TILE_BYTES
            || heights.near_cover.size() < crate::near_cover::UPLOAD_BYTES
            || proxies.objects.size() < u64::from(proxies.max_objects) * 256
            || proxies.payload.size() < u64::from(proxies.max_payload_words) * 4
            || total > device.limits().max_buffer_size
            || total > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(Error::InvalidConfig);
        }
        let make = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let visible = make(
            "independent distant shadow lists",
            total,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = make(
            "distant height opaque cutout shadow draws",
            48,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
        );
        let cameras = std::array::from_fn(|_| {
            make(
                "distant full cascade camera",
                CAMERA_BYTES,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            )
        });
        let cull_cameras = std::array::from_fn(|_| {
            make(
                "distant independent cull camera",
                CAMERA_BYTES,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            )
        });
        let projected = shadows.footprint_binding(0).is_some();
        let fallback_bounds = (!projected).then(|| {
            make(
                "unused distant shadow reduction",
                footprint::STRIDE,
                wgpu::BufferUsages::STORAGE,
            )
        });
        let c = wgpu::ShaderStages::COMPUTE;
        let v = wgpu::ShaderStages::VERTEX;
        let f = wgpu::ShaderStages::FRAGMENT;
        let buffer = |binding, visibility, uniform, read_only| wgpu::BindGroupLayoutEntry {
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
        };
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("distant shadow compute inputs"),
            entries: &[
                buffer(0, c, true, true),
                buffer(1, c, false, true),
                buffer(2, c, false, true),
                buffer(3, c, false, false),
                buffer(4, c, false, false),
                buffer(5, c, false, false),
            ],
        });
        let height_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("public height shadow inputs"),
            entries: &[
                buffer(0, v | f, true, true),
                buffer(1, v, false, true),
                buffer(2, v, false, true),
                buffer(3, v, false, true),
                buffer(21, f, false, true),
            ],
        });
        let opaque_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("public opaque proxy shadow inputs"),
            entries: &[
                buffer(0, v | f, true, true),
                buffer(1, v | f, false, true),
                buffer(2, f, false, true),
                buffer(3, v, false, true),
            ],
        });
        let mut cutout_entries = vec![
            buffer(0, v, true, true),
            buffer(1, v, false, true),
            buffer(2, v, false, true),
            buffer(3, v, false, true),
            buffer(7, f, false, true),
        ];
        cutout_entries.extend([
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: f,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: f,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ]);
        let cutout_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("public quad alpha shadow inputs"),
            entries: &cutout_entries,
        });
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let portion = |offset, size| {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &visible,
                offset,
                size: NonZeroU64::new(size),
            })
        };
        let cull_binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("separate distant shadow cull"),
                layout: &cull_layout,
                entries: &[
                    entry(0, cull_cameras[i].as_entire_binding()),
                    entry(1, heights.headers.as_entire_binding()),
                    entry(2, proxies.objects.as_entire_binding()),
                    entry(3, visible.as_entire_binding()),
                    entry(4, indirect.as_entire_binding()),
                    entry(
                        5,
                        shadows.footprint_binding(i).unwrap_or_else(|| {
                            fallback_bounds
                                .as_ref()
                                .expect("fallback reduction buffer")
                                .as_entire_binding()
                        }),
                    ),
                ],
            })
        });
        let height_binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("public height directional depth"),
                layout: &height_layout,
                entries: &[
                    entry(0, cameras[i].as_entire_binding()),
                    entry(1, heights.headers.as_entire_binding()),
                    entry(2, heights.payload.as_entire_binding()),
                    entry(3, portion(0, height_span)),
                    entry(21, heights.near_cover.as_entire_binding()),
                ],
            })
        });
        let opaque_binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("public opaque directional depth"),
                layout: &opaque_layout,
                entries: &[
                    entry(0, cameras[i].as_entire_binding()),
                    entry(1, proxies.objects.as_entire_binding()),
                    entry(2, proxies.payload.as_entire_binding()),
                    entry(3, portion(height_span, opaque_span)),
                ],
            })
        });
        let cutout_binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("public cutout directional depth"),
                layout: &cutout_layout,
                entries: &[
                    entry(0, cameras[i].as_entire_binding()),
                    entry(1, proxies.objects.as_entire_binding()),
                    entry(2, proxies.payload.as_entire_binding()),
                    entry(3, portion(height_span + opaque_span, cutout_span)),
                    entry(4, wgpu::BindingResource::TextureView(proxies.colors)),
                    entry(6, wgpu::BindingResource::Sampler(proxies.sampler)),
                    entry(7, proxies.materials.as_entire_binding()),
                ],
            })
        });
        let [compute_source, height_source, opaque_source, cutout_source] = shader_sources(
            (height_span / 4) as u32,
            ((height_span + opaque_span) / 4) as u32,
        );
        let module = |label, source: String| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        };
        let compute = module("distant public shadow reduction", compute_source);
        let pipeline_layout = |label, layout: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[layout],
                push_constant_ranges: &[],
            })
        };
        let compute_layout = pipeline_layout("distant shadow compute layout", &cull_layout);
        let compute_pipeline = |name| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&compute_layout),
                module: &compute,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let depth_pipeline = |label, layout: &wgpu::BindGroupLayout, source, topology| {
            let module = module(label, source);
            let layout = pipeline_layout(label, layout);
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::GreaterEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
        };
        Ok(Self {
            height_headers: heights.headers.clone(),
            height_payload: heights.payload.clone(),
            near_cover: heights.near_cover.clone(),
            proxy_objects: proxies.objects.clone(),
            proxy_payload: proxies.payload.clone(),
            max_tiles: heights.max_tiles,
            max_objects: proxies.max_objects,
            max_payload_words: proxies.max_payload_words,
            atlas_layers: [heights.atlas_layers, proxies.atlas_layers],
            source: [heights.revision, proxies.revision],
            enabled: false,
            policy,
            cameras,
            cull_cameras,
            visible,
            indirect,
            fallback_bounds,
            cull_binds,
            height_binds,
            opaque_binds,
            cutout_binds,
            reset: compute_pipeline("reset"),
            cull: compute_pipeline("cull"),
            height: depth_pipeline(
                "public height shadow depth",
                &height_layout,
                height_source,
                wgpu::PrimitiveTopology::TriangleList,
            ),
            opaque: depth_pipeline(
                "public opaque shadow depth",
                &opaque_layout,
                opaque_source,
                wgpu::PrimitiveTopology::TriangleList,
            ),
            cutout: depth_pipeline(
                "public cutout shadow depth",
                &cutout_layout,
                cutout_source,
                wgpu::PrimitiveTopology::TriangleStrip,
            ),
            projected,
            diagnostics: None,
        })
    }
    /// Caller must additionally certify the actual committed front/near cover.
    /// Resource replacement requires loading-time reprepare; stale handles never
    /// silently redirect the existing bind groups.
    pub fn synchronize_sources(
        &mut self,
        heights: HeightShadowSource<'_>,
        proxies: ProxyShadowSource<'_>,
    ) -> Result<bool, Error> {
        if self.height_headers != *heights.headers
            || self.height_payload != *heights.payload
            || self.near_cover != *heights.near_cover
            || self.proxy_objects != *proxies.objects
            || self.proxy_payload != *proxies.payload
            || self.max_tiles != heights.max_tiles
            || self.max_objects != proxies.max_objects
            || self.max_payload_words != proxies.max_payload_words
            || self.atlas_layers != [heights.atlas_layers, proxies.atlas_layers]
        {
            return Err(Error::SourceNeedsReprepare);
        }
        let source = [heights.revision, proxies.revision];
        let changed = self.source != source;
        self.source = source;
        Ok(changed)
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    pub fn source_key(&self) -> [u64; 2] {
        if self.enabled { self.source } else { [0; 2] }
    }
    pub fn startup_pipeline_compilations(&self) -> u32 {
        5
    }
    /// Native diagnostic scopes only; default `None` owns no extra GPU resources.
    pub fn set_diagnostic_timings(&mut self, timings: Option<ShadowNodeTimings>) {
        self.diagnostics = timings;
    }
    /// Requested buffers only; source arenas, maps and driver pipeline memory are
    /// shared and are accounted by their respective owners.
    pub fn buffer_bytes(&self) -> u64 {
        self.visible.size()
            + self.indirect.size()
            + CAMERA_BYTES * 6
            + self.fallback_bounds.as_ref().map_or(0, wgpu::Buffer::size)
    }
    fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        region: (ShadowRect, u32),
        collect: bool,
    ) -> ShadowPrepareWork {
        let (rect, resolution) = region;
        let matrix = camera.view_projection;
        let direction = [-matrix[0][2], -matrix[1][2], -matrix[2][2], 0.];
        for (buffer, camera) in [
            (&self.cameras[cascade], camera),
            (
                &self.cull_cameras[cascade],
                crop_camera(camera, rect, resolution),
            ),
        ] {
            queue.write_buffer(
                buffer,
                0,
                bytemuck::bytes_of(&CameraGpu {
                    view_projection: camera.view_projection,
                    eye: [0., 0., 0., 1.],
                    direction,
                    params: [
                        if self.enabled { self.max_tiles } else { 0 },
                        if self.enabled { self.max_objects } else { 0 },
                        resolution,
                        u32::from(collect && self.projected),
                    ],
                    opaque: self.policy.opaque,
                }),
            );
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("independent public far shadow cull and bounds"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &self.cull_binds[cascade], &[]);
        pass.set_pipeline(&self.reset);
        pass.dispatch_workgroups(1, 1, 1);
        if self.enabled {
            pass.set_pipeline(&self.cull);
            pass.dispatch_workgroups(self.max_tiles.max(self.max_objects).div_ceil(64), 1, 1);
        }
        ShadowPrepareWork {
            dispatches: if self.enabled { 2 } else { 1 },
            upload_bytes: CAMERA_BYTES * 2,
            bounds_complete: true,
        }
    }
}
impl DynamicShadowCaster for DistantShadowGpu {
    fn available(&self) -> bool {
        true
    }
    fn shadow_revision(&self) -> u64 {
        0
    }
    fn shadow_key(&self) -> [u64; 4] {
        let key = self.source_key();
        [key[0], key[1], 0, 0]
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
        self.prepare(queue, encoder, cascade, camera, (rect, resolution), false)
    }
    fn prepare_shadow_sparse(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        resolution: u32,
    ) -> ShadowPrepareWork {
        self.prepare(
            queue,
            encoder,
            cascade,
            camera,
            (ShadowRect::full(resolution), resolution),
            true,
        )
    }
    fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, cascade: usize) -> u32 {
        if !self.enabled {
            return 0;
        }
        for (pipeline, bind, offset, node) in [
            (
                &self.height,
                &self.height_binds[cascade],
                0,
                ShadowNode::HeightDraw,
            ),
            (
                &self.opaque,
                &self.opaque_binds[cascade],
                16,
                ShadowNode::OpaqueDraw,
            ),
            (
                &self.cutout,
                &self.cutout_binds[cascade],
                32,
                ShadowNode::CutoutDraw,
            ),
        ] {
            if let Some(timing) = &self.diagnostics {
                timing.render_begin(pass, cascade, node);
            }
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw_indirect(&self.indirect, offset);
            if let Some(timing) = &self.diagnostics {
                timing.render_end(pass, cascade, node);
            }
        }
        3
    }
}

/// Four exact leaf versions. Nesting ShadowCasterPair would lose the pair's
/// internal versions because its scalar shadow_revision is deliberately zero.
pub struct SceneShadowCasters<'a> {
    pub entities: Option<&'a mut dyn DynamicShadowCaster>,
    pub props: Option<&'a mut dyn DynamicShadowCaster>,
    pub distant: &'a mut DistantShadowGpu,
}
impl DynamicShadowCaster for SceneShadowCasters<'_> {
    fn available(&self) -> bool {
        self.entities.as_ref().is_none_or(|c| c.available())
            && self.props.as_ref().is_none_or(|c| c.available())
    }
    fn shadow_revision(&self) -> u64 {
        0
    }
    fn shadow_key(&self) -> [u64; 4] {
        let far = self.distant.source_key();
        [
            self.entities.as_ref().map_or(0, |c| c.shadow_revision()),
            self.props.as_ref().map_or(0, |c| c.shadow_revision()),
            far[0],
            far[1],
        ]
    }
    fn visit_shadow_damage(&self, previous: [u64; 4], visit: &mut dyn FnMut(WorldBounds)) -> bool {
        let far = self.distant.source_key();
        if previous[2..] != far {
            return false;
        }
        self.entities.as_ref().map_or(previous[0] == 0, |c| {
            c.visit_shadow_damage([previous[0], 0, 0, 0], visit)
        }) && self.props.as_ref().map_or(previous[1] == 0, |c| {
            c.visit_shadow_damage([previous[1], 0, 0, 0], visit)
        })
    }
    fn commit_shadow_damage(&mut self) {
        for c in [&mut self.entities, &mut self.props].into_iter().flatten() {
            c.commit_shadow_damage();
        }
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
        let mut work = self
            .distant
            .prepare_shadow_region(queue, encoder, cascade, camera, rect, resolution);
        for c in [&mut self.entities, &mut self.props].into_iter().flatten() {
            add_work(
                &mut work,
                c.prepare_shadow_region(queue, encoder, cascade, camera, rect, resolution),
            );
        }
        work
    }
    fn prepare_shadow_sparse(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        camera: Camera,
        resolution: u32,
    ) -> ShadowPrepareWork {
        let mut work = self
            .distant
            .prepare_shadow_sparse(queue, encoder, cascade, camera, resolution);
        for c in [&mut self.entities, &mut self.props].into_iter().flatten() {
            add_work(
                &mut work,
                c.prepare_shadow_sparse(queue, encoder, cascade, camera, resolution),
            );
        }
        work
    }
    fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, cascade: usize) -> u32 {
        self.distant.draw_shadow(pass, cascade)
            + self
                .entities
                .as_ref()
                .map_or(0, |c| c.draw_shadow(pass, cascade))
            + self
                .props
                .as_ref()
                .map_or(0, |c| c.draw_shadow(pass, cascade))
    }
}
fn add_work(total: &mut ShadowPrepareWork, next: ShadowPrepareWork) {
    total.dispatches += next.dispatches;
    total.upload_bytes += next.upload_bytes;
    total.bounds_complete &= next.bounds_complete;
}

/// Exact loading sources in compute/height/opaque/cutout order, also used by
/// independent backend oracles. List starts are u32 offsets in the shared arena.
pub fn shader_sources(opaque_start: u32, cutout_start: u32) -> [String; 4] {
    let cull = format!(
        "{CAMERA_SHADER}\nconst OPAQUE_START:u32={opaque_start}u;const CUTOUT_START:u32={cutout_start}u;\n{}\n{}\n{}",
        footprint::COMMON,
        footprint::MODEL,
        include_str!("distant_shadow_cull.wgsl")
    );
    let height = include_str!("height_grid.wgsl")
        .split("fn illumination")
        .next()
        .expect("height shader prefix")
        .replace(
            "struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};",
            CAMERA_SHADER,
        );
    let height = format!(
        "{height}\n{}\n@fragment fn fragment(input:Out){{let layer=input.layer;if layer>=256u||(camera.opaque[layer/128u][(layer/32u)%4u]&(1u<<(layer%32u)))==0u{{discard;}}if covered_by_near(input.world-normalize(input.normal)*0.0001){{discard;}}}}",
        crate::near_cover::SHADER
    );
    let opaque = include_str!("lod_proxy.wgsl")
        .split("fn illumination")
        .next()
        .expect("opaque shader prefix")
        .replace(LOD_CAMERA, CAMERA_SHADER);
    let opaque = format!("{opaque}\n{}", include_str!("distant_shadow_trace.wgsl"));
    let cutout = include_str!("lod_cutout.wgsl")
        .split("fn illumination")
        .next()
        .expect("cutout shader prefix")
        .replace(LOD_CAMERA, CAMERA_SHADER);
    let cutout = format!(
        "{cutout}\n{}\n@fragment fn fragment(input:VertexOut){{if !shadow_alpha_pass(input.material,input.uv){{discard;}}}}",
        crate::shadow_alpha::WGSL
    );
    [cull, height, opaque, cutout]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn height_policy_requires_explicit_opaque_provenance() {
        let p = HeightShadowPolicy::from_opaque_layers([0, 31, 32, 255]).unwrap();
        for i in 0..=256 {
            assert_eq!(p.casts(i), [0, 31, 32, 255].contains(&i));
        }
        assert_eq!(
            HeightShadowPolicy::from_opaque_layers([256]),
            Err(Error::InvalidConfig)
        );
        assert!(!HeightShadowPolicy::default().casts(0));
    }
}
