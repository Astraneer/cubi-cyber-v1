//! Prepared opaque wall cards. CPU data/assets enter only at explicit loading;
//! one grouped draw shares the real scene depth/MSAA and radiometric receivers.
use crate::{
    atmosphere::AtmosphereGpu,
    gi::GiGpu,
    light_volume::PackedLight,
    shadows::ShadowGpu,
    terrain_gpu::{Camera, TerrainGeometry, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
pub const INSTANCE_BYTES: u64 = 64;
pub const CAMERA_BYTES: u64 = 80;
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub max_boards: u32,
    pub memory_budget_bytes: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_boards: 9,
            memory_budget_bytes: 4 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy)]
pub struct Images<'a> {
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub rgba: &'a [u8],
}
#[derive(Clone, Copy, Default)]
pub struct Lighting<'a> {
    pub shadows: Option<&'a ShadowGpu>,
    pub atmosphere: Option<&'a AtmosphereGpu>,
    pub gi: Option<&'a GiGpu>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Instance {
    pub center: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub size: [f32; 2],
    pub layer: u32,
    pub light: PackedLight,
    pub enabled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidImage,
    InvalidInstance,
    MemoryBudget,
    UploadBudget,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub pipeline_compilations: u32,
    pub upload_bytes: u64,
    pub instance_uploads: u32,
    pub camera_uploads: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct BoardGpu {
    center: [f32; 4],
    right_width: [f32; 4],
    up_height: [f32; 4],
    codes: [u32; 4],
}
const _: () = assert!(std::mem::size_of::<BoardGpu>() == INSTANCE_BYTES as usize);
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    matrix: [[f32; 4]; 4],
    params: [u32; 4],
}
pub struct RecipeBoardGpu {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    instances: wgpu::Buffer,
    camera: wgpu::Buffer,
    current: Vec<BoardGpu>,
    staging: Vec<BoardGpu>,
    last_camera: Option<Camera>,
    layers: u32,
    active: u32,
    texture_bytes: u64,
    sample_count: u32,
    work: Work,
}
impl RecipeBoardGpu {
    pub fn required_allocation_bytes(
        width: u32,
        height: u32,
        layers: u32,
        capacity: u32,
    ) -> Option<u64> {
        u64::from(width)
            .checked_mul(u64::from(height))?
            .checked_mul(u64::from(layers))?
            .checked_mul(4)?
            .checked_add(u64::from(capacity) * INSTANCE_BYTES + CAMERA_BYTES)
    }
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        images: Images<'_>,
        config: Config,
        lighting: Lighting<'_>,
    ) -> Result<Self, Error> {
        let limits = device.limits();
        if config.max_boards == 0
            || config.max_boards > 32
            || ![1, 4].contains(&geometry.sample_count)
            || images.width == 0
            || images.height == 0
            || images.layers == 0
            || images.width > limits.max_texture_dimension_2d
            || images.height > limits.max_texture_dimension_2d
            || images.layers > limits.max_texture_array_layers
            || lighting.gi.is_some() && lighting.atmosphere.is_none()
        {
            return Err(Error::InvalidConfig);
        }
        let bytes = Self::required_allocation_bytes(
            images.width,
            images.height,
            images.layers,
            config.max_boards,
        )
        .ok_or(Error::MemoryBudget)?;
        if bytes > config.memory_budget_bytes {
            return Err(Error::MemoryBudget);
        }
        let texture_bytes = bytes - u64::from(config.max_boards) * INSTANCE_BYTES - CAMERA_BYTES;
        if u64::try_from(images.rgba.len()).ok() != Some(texture_bytes)
            || images.rgba.chunks_exact(4).any(|p| p[3] != 255)
        {
            return Err(Error::InvalidImage);
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("prepared physical recipe cards"),
            size: wgpu::Extent3d {
                width: images.width,
                height: images.height,
                depth_or_array_layers: images.layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            images.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(images.width * 4),
                rows_per_image: Some(images.height),
            },
            texture.size(),
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("isolated recipe-card pixels"),
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
            "physical card64B records",
            u64::from(config.max_boards) * INSTANCE_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let camera = buffer(
            "physical card camera",
            CAMERA_BYTES,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let v = wgpu::ShaderStages::VERTEX;
        let f = wgpu::ShaderStages::FRAGMENT;
        let mut entries = vec![
            buffer_layout(0, v, true),
            buffer_layout(1, v, false),
            texture_layout(
                2,
                wgpu::TextureViewDimension::D2Array,
                wgpu::TextureSampleType::Float { filterable: true },
            ),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: f,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        if lighting.shadows.is_some() {
            entries.extend([
                buffer_layout(9, f, true),
                texture_layout(
                    10,
                    wgpu::TextureViewDimension::D2Array,
                    wgpu::TextureSampleType::Depth,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: f,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ]);
        }
        if lighting.atmosphere.is_some() {
            entries.push(buffer_layout(20, f, false));
        }
        if lighting.gi.is_some() {
            crate::gi_model::layout(&mut entries);
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("physical recipe-card receivers"),
            entries: &entries,
        });
        let mut entries = vec![
            binding(0, &camera),
            binding(1, &instances),
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ];
        if let Some(shadows) = lighting.shadows {
            entries.extend([
                binding(9, shadows.sampling_buffer()),
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
        if let Some(atmosphere) = lighting.atmosphere {
            entries.push(binding(20, atmosphere.lighting_buffer()));
        }
        if let Some(gi) = lighting.gi {
            crate::gi_model::bindings(&mut entries, gi);
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("physical recipe-card atlas and lighting"),
            layout: &layout,
            entries: &entries,
        });
        let source = shader_source(
            lighting.shadows.is_some(),
            lighting.atmosphere.is_some(),
            lighting.gi.is_some(),
            geometry.ambient_mrt,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("occluded physical recipe cards"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("physical recipe cards"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("opaque world recipe cards"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
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
                count: geometry.sample_count,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &crate::ambient_mrt::targets(None, geometry.ambient_mrt),
            }),
            multiview: None,
            cache: None,
        });
        Ok(Self {
            pipeline,
            bind,
            instances,
            camera,
            current: vec![BoardGpu::zeroed(); config.max_boards as usize],
            staging: vec![BoardGpu::zeroed(); config.max_boards as usize],
            last_camera: None,
            layers: images.layers,
            active: 0,
            texture_bytes,
            sample_count: geometry.sample_count,
            work: Work {
                pipeline_compilations: 1,
                upload_bytes: texture_bytes,
                ..Default::default()
            },
        })
    }
    /// Validate and reserve the complete update before either camera or records
    /// are queued. No partial plane publication is possible on budget failure.
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        camera: Camera,
        instances: &[Instance],
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        self.prepare(queue, Some(camera), instances, budget)
    }
    pub fn prepare_records(
        &mut self,
        queue: &wgpu::Queue,
        instances: &[Instance],
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        self.prepare(queue, None, instances, budget)
    }
    fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        camera: Option<Camera>,
        instances: &[Instance],
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if instances.len() > self.staging.len()
            || camera.is_some_and(|c| c.view_projection.iter().flatten().any(|v| !v.is_finite()))
        {
            return Err(Error::InvalidInstance);
        }
        self.staging.fill(BoardGpu::zeroed());
        let mut active = 0;
        for (row, item) in self.staging.iter_mut().zip(instances) {
            if !item.enabled {
                continue;
            }
            let dot =
                |a: [f32; 3], b: [f32; 3]| a.into_iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
            if item.layer >= self.layers
                || item
                    .center
                    .iter()
                    .chain(&item.right)
                    .chain(&item.up)
                    .chain(&item.size)
                    .any(|v| !v.is_finite())
                || item.size.iter().any(|v| *v <= 0.)
                || (dot(item.right, item.right) - 1.).abs() > 1e-4
                || (dot(item.up, item.up) - 1.).abs() > 1e-4
                || dot(item.right, item.up).abs() > 1e-4
            {
                return Err(Error::InvalidInstance);
            }
            *row = BoardGpu {
                center: [item.center[0], item.center[1], item.center[2], 0.],
                right_width: [item.right[0], item.right[1], item.right[2], item.size[0]],
                up_height: [item.up[0], item.up[1], item.up[2], item.size[1]],
                codes: [item.layer, item.light.raw(), 1, 0],
            };
            active += 1;
        }
        let records = self.staging != self.current;
        let camera_changed = active != 0 && camera.is_some() && self.last_camera != camera;
        let bytes =
            u64::from(records) * self.instances.size() + u64::from(camera_changed) * CAMERA_BYTES;
        if budget.remaining_bytes < bytes {
            return Err(Error::UploadBudget);
        }
        if records {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.staging));
            self.current.copy_from_slice(&self.staging);
        }
        if camera_changed {
            queue.write_buffer(
                &self.camera,
                0,
                bytemuck::bytes_of(&CameraGpu {
                    matrix: camera.expect("changed camera").view_projection,
                    params: [self.current.len() as u32, 0, 0, 0],
                }),
            );
            self.last_camera = camera;
        }
        budget.remaining_bytes -= bytes;
        self.active = active;
        let work = Work {
            upload_bytes: bytes,
            instance_uploads: u32::from(records),
            camera_uploads: u32::from(camera_changed),
            ..Default::default()
        };
        self.work.upload_bytes += bytes;
        self.work.instance_uploads += work.instance_uploads;
        self.work.camera_uploads += work.camera_uploads;
        Ok(work)
    }
    pub fn prepare_camera(
        &mut self,
        queue: &wgpu::Queue,
        camera: Camera,
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if camera
            .view_projection
            .iter()
            .flatten()
            .any(|v| !v.is_finite())
        {
            return Err(Error::InvalidInstance);
        }
        if self.active == 0 || self.last_camera == Some(camera) {
            return Ok(Work::default());
        }
        if budget.remaining_bytes < CAMERA_BYTES {
            return Err(Error::UploadBudget);
        }
        queue.write_buffer(
            &self.camera,
            0,
            bytemuck::bytes_of(&CameraGpu {
                matrix: camera.view_projection,
                params: [self.current.len() as u32, 0, 0, 0],
            }),
        );
        budget.remaining_bytes -= CAMERA_BYTES;
        self.last_camera = Some(camera);
        self.work.upload_bytes += CAMERA_BYTES;
        self.work.camera_uploads += 1;
        Ok(Work {
            upload_bytes: CAMERA_BYTES,
            camera_uploads: 1,
            ..Default::default()
        })
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        if self.active == 0 {
            return 0;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..6, 0..self.current.len() as u32);
        1
    }
    pub fn allocation_bytes(&self) -> u64 {
        self.texture_bytes + self.instances.size() + self.camera.size()
    }
    pub fn texture_bytes(&self) -> u64 {
        self.texture_bytes
    }
    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }
    pub fn active_boards(&self) -> u32 {
        self.active
    }
    pub fn take_work(&mut self) -> Work {
        std::mem::take(&mut self.work)
    }
}
fn buffer_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    uniform: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
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
fn texture_layout(
    binding: u32,
    view_dimension: wgpu::TextureViewDimension,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension,
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
/// Exact loading-selected source for portable backend validation.
pub fn shader_source(shadows: bool, atmosphere: bool, gi: bool, ambient_mrt: bool) -> String {
    let legacy = if shadows {
        "return light.rgb+vec3(light.a*(.15+.85*cascade_shadow(world,normal)*max(dot(normal,normalize(shadow_sampling.sun.xyz)),0.)));"
    } else {
        "return light.rgb+vec3(light.a*(.65+.35*max(dot(normal,normalize(vec3(.35,.8,.2))),0.)));"
    };
    let source = crate::atmosphere::surface_shader(
        include_str!("recipe_boards.wgsl"),
        shadows,
        atmosphere,
        true,
        legacy,
    );
    let source = if gi {
        crate::gi_model::shader(source)
    } else {
        source
    };
    let base = if atmosphere {
        "light.rgb+light.a*atmosphere_diffuse(normal)"
    } else if shadows {
        "light.rgb+vec3(light.a*.15)"
    } else {
        "light.rgb+vec3(light.a*.65)"
    };
    let ambient = format!(
        "{base}{}",
        if gi {
            "+model_reflected_irradiance(world,normal)*0.3183098862"
        } else {
            ""
        }
    );
    if ambient_mrt {
        source.replace("// OUTPUT","struct BoardOutput{@location(0)color:vec4<f32>,@location(1)ambient:vec4<f32>};").replace("// RETURN",&format!("return BoardOutput(vec4(color,1.),vec4(albedo*board_ambient(input.world,input.normal,input.light),1.));}}\nfn board_ambient(world:vec3<f32>,normal:vec3<f32>,light:vec4<f32>)->vec3<f32>{{return {ambient};"))
    } else {
        source
            .replace("// OUTPUT", "alias BoardOutput=vec4<f32>;")
            .replace("->BoardOutput", "->@location(0)vec4<f32>")
            .replace("// RETURN", "return vec4(color,1.);")
    }
}
