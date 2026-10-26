//! Optional bounded contact ambient visibility. Quality0 allocates nothing.
//! Inputs must be the same complete opaque/model frame and its ambient-only MRT.
use crate::terrain_gpu::{Camera, UploadBudget};
use bytemuck::{Pod, Zeroable};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactAoConfig {
    pub quality: u8,
    pub radius: f32,
    pub bias: f32,
    pub strength: f32,
}
impl Default for ContactAoConfig {
    fn default() -> Self {
        Self {
            quality: 1,
            radius: 1.25,
            bias: 0.025,
            strength: 1.,
        }
    }
}
#[derive(Clone, Copy)]
pub struct ContactAoInputs<'a> {
    pub hdr: &'a wgpu::TextureView,
    pub ambient: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    pub statistics: Option<&'a wgpu::Buffer>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactAoError {
    InvalidConfig,
    InvalidCamera,
    UploadBudget,
    WorkBudget,
}
impl std::fmt::Display for ContactAoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ContactAoError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContactAoWork {
    pub passes: u32,
    pub draws: u32,
    pub composition_samples: u64,
    pub max_depth_reads: u64,
    pub max_kernel_samples: u64,
    pub upload_bytes: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    matrix: [[f32; 4]; 4],
    inverse: [[f32; 4]; 4],
    image: [u32; 4],
    optics: [f32; 4],
    kernel: [[f32; 4]; 32],
}
const PARAM_BYTES: u64 = std::mem::size_of::<Params>() as u64;
// Fixed IEEE754 values keep native/WASM loading kernels identical. The f64
// geometric oracle independently derives the ideal golden-angle hemisphere.
const KERNEL_BITS: [[u32; 4]; 32] = [
    [0x3dcd3996, 0x00000000, 0x3acd4000, 0x00000000],
    [0xbd99d489, 0x3d8cebcb, 0x3b9ca333, 0x00000000],
    [0x3c16a510, 0xbdd690a0, 0x3c070800, 0x00000000],
    [0x3d893240, 0x3db2f2c1, 0x3c467e66, 0x00000000],
    [0xbdeb3393, 0xbca66a51, 0x3c87b400, 0x00000000],
    [0x3dd77d07, 0xbd891379, 0x3cb23c00, 0x00000000],
    [0xbd0ec7a1, 0x3e04c890, 0x3ce430cd, 0x00000000],
    [0xbd8921ac, 0xbe0404eb, 0x3d0f7600, 0x00000000],
    [0x3e179dc7, 0x3d5d7aeb, 0x3d31e39a, 0x00000000],
    [0xbe2214ad, 0x3d85cf26, 0x3d5a0e00, 0x00000000],
    [0x3da173f9, 0xbe2c8203, 0x3d845100, 0x00000000],
    [0x3d775deb, 0x3e452940, 0x3d9f2633, 0x00000000],
    [0xbe418fe9, 0xbde058b0, 0x3dbddd00, 0x00000000],
    [0x3e6bebd4, 0xbd4f776a, 0x3de0cbcd, 0x00000000],
    [0xbe1583f0, 0x3e54ab95, 0x3e042480, 0x00000000],
    [0xbd0f4a69, 0xbe8a3874, 0x3e1a5580, 0x00000000],
    [0x3e639ea7, 0x3e3fd692, 0x3e33241a, 0x00000000],
    [0xbe9e1bdc, 0x3c5139ba, 0x3e4ebb80, 0x00000000],
    [0x3e6d6071, 0xbe6c38b7, 0x3e6d46e6, 0x00000000],
    [0xbc8245dc, 0x3eb01458, 0x3e8778c0, 0x00000000],
    [0xbe6c741d, 0xbe8dacda, 0x3e99f340, 0x00000000],
    [0x3ebe4662, 0x3d4ccf4e, 0x3eae288d, 0x00000000],
    [0xbea2daf6, 0x3e629ef8, 0x3ec42e40, 0x00000000],
    [0x3db2983c, 0xbec677c2, 0x3edc19f3, 0x00000000],
    [0x3e4d87e2, 0x3eb356de, 0x3ef60140, 0x00000000],
    [0xbec5e2d4, 0xbdfc8630, 0x3f08fce0, 0x00000000],
    [0x3ebadbc2, 0xbe2caaab, 0x3f180c86, 0x00000000],
    [0xbe1a93da, 0x3eb8ad75, 0x3f283a60, 0x00000000],
    [0xbe00452a, 0xbeb24fd8, 0x3f39913a, 0x00000000],
    [0x3e97e274, 0x3e1fa70e, 0x3f4c1be0, 0x00000000],
    [0xbe897582, 0x3d90ef5e, 0x3f5fe520, 0x00000000],
    [0x3dbd9861, 0xbe136e9c, 0x3f74f7c6, 0x00000000],
];
pub struct ContactAoGpu {
    width: u32,
    height: u32,
    samples: u32,
    config: ContactAoConfig,
    statistics: bool,
    uniform: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    texture: wgpu::Texture,
    resolved: wgpu::Texture,
    msaa: wgpu::TextureView,
    resolved_view: wgpu::TextureView,
    last: Option<(Camera, u64)>,
    timer: crate::gpu_time::GpuPassTimer<1>,
}
impl ContactAoGpu {
    #[allow(clippy::too_many_arguments)] // Loading owns immutable views, explicit extent, sample profile and upload credits.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        samples: u32,
        config: ContactAoConfig,
        inputs: ContactAoInputs<'_>,
        budget: &mut UploadBudget,
    ) -> Result<Option<Self>, ContactAoError> {
        if config.quality == 0 {
            return Ok(None);
        }
        if !matches!(config.quality, 1 | 2)
            || !matches!(samples, 1 | 4)
            || width == 0
            || height == 0
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
            || !config.radius.is_finite()
            || !(0.001..=64.).contains(&config.radius)
            || !config.bias.is_finite()
            || config.bias < 0.
            || config.bias >= config.radius
            || !config.strength.is_finite()
            || !(0.0..=1.0).contains(&config.strength)
        {
            return Err(ContactAoError::InvalidConfig);
        }
        let reads = u64::from(width)
            * u64::from(height)
            * u64::from(samples)
            * (5 + if config.quality == 1 { 16 } else { 32 });
        if inputs.statistics.is_some() && reads > u64::from(u32::MAX) {
            return Err(ContactAoError::InvalidConfig);
        }
        if budget.remaining_bytes < PARAM_BYTES {
            return Err(ContactAoError::UploadBudget);
        }
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("contact AO fixed world-radius kernel"),
            size: PARAM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &uniform,
            0,
            bytemuck::bytes_of(&params(config, width, height, samples)),
        );
        budget.remaining_bytes -= PARAM_BYTES;
        let mut entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        for binding in 1..=3 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: if binding == 3 {
                        wgpu::TextureSampleType::Depth
                    } else {
                        wgpu::TextureSampleType::Float { filterable: false }
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: samples > 1,
                },
                count: None,
            });
        }
        if inputs.statistics.is_some() {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("contact AO same-frame opaque inputs"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded per-sample contact ambient visibility"),
            source: wgpu::ShaderSource::Wgsl(shader(samples, inputs.statistics.is_some()).into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ambient-only contact AO composite"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: samples,
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
        let bind = bind(device, &layout, &uniform, inputs);
        let (texture, resolved, msaa, resolved_view) = targets(device, width, height, samples);
        Ok(Some(Self {
            width,
            height,
            samples,
            config,
            statistics: inputs.statistics.is_some(),
            uniform,
            layout,
            bind,
            pipeline,
            texture,
            resolved,
            msaa,
            resolved_view,
            last: None,
            timer: crate::gpu_time::GpuPassTimer::new(device, queue, "contact ambient visibility"),
        }))
    }
    pub fn resize_inputs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        inputs: ContactAoInputs<'_>,
        budget: &mut UploadBudget,
    ) -> Result<(), ContactAoError> {
        if inputs.statistics.is_some() != self.statistics
            || width == 0
            || height == 0
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            return Err(ContactAoError::InvalidConfig);
        }
        let reads = u64::from(width)
            * u64::from(height)
            * u64::from(self.samples)
            * (5 + if self.config.quality == 1 { 16 } else { 32 });
        if self.statistics && reads > u64::from(u32::MAX) {
            return Err(ContactAoError::InvalidConfig);
        }
        if budget.remaining_bytes < 16 {
            return Err(ContactAoError::UploadBudget);
        }
        let (texture, resolved, msaa, resolved_view) = targets(device, width, height, self.samples);
        let bind = bind(device, &self.layout, &self.uniform, inputs);
        queue.write_buffer(
            &self.uniform,
            128,
            bytemuck::bytes_of(&[
                width,
                height,
                self.samples,
                if self.config.quality == 1 { 16u32 } else { 32 },
            ]),
        );
        budget.remaining_bytes -= 16;
        self.width = width;
        self.height = height;
        self.texture = texture;
        self.resolved = resolved;
        self.msaa = msaa;
        self.resolved_view = resolved_view;
        self.bind = bind;
        self.last = None;
        Ok(())
    }
    pub fn worst_work(&self) -> ContactAoWork {
        let pixels = u64::from(self.width) * u64::from(self.height) * u64::from(self.samples);
        let kernel = if self.config.quality == 1 { 16 } else { 32 };
        ContactAoWork {
            passes: 1,
            draws: 1,
            composition_samples: pixels,
            max_depth_reads: pixels * (5 + kernel),
            max_kernel_samples: pixels * kernel,
            upload_bytes: 128,
        }
    }
    pub fn pending_work(
        &self,
        camera: Camera,
        source_revision: u64,
    ) -> Result<ContactAoWork, ContactAoError> {
        if self.last == Some((camera, source_revision)) {
            return Ok(ContactAoWork::default());
        }
        if camera.inverse_view_projection().is_none() {
            return Err(ContactAoError::InvalidCamera);
        }
        let mut work = self.worst_work();
        if self.last.is_some_and(|(old, _)| old == camera) {
            work.upload_bytes = 0;
        }
        Ok(work)
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: Camera,
        source_revision: u64,
        budget: &mut UploadBudget,
        max_depth_reads: u64,
    ) -> Result<ContactAoWork, ContactAoError> {
        let work = self.pending_work(camera, source_revision)?;
        self.timer.begin();
        if work.passes == 0 {
            return Ok(work);
        }
        if work.max_depth_reads > max_depth_reads {
            return Err(ContactAoError::WorkBudget);
        }
        if budget.remaining_bytes < work.upload_bytes {
            return Err(ContactAoError::UploadBudget);
        }
        if work.upload_bytes != 0 {
            let matrices = [
                camera.view_projection,
                camera
                    .inverse_view_projection()
                    .ok_or(ContactAoError::InvalidCamera)?,
            ];
            queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&matrices));
        }
        budget.remaining_bytes -= work.upload_bytes;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("same-frame ambient-only contact visibility"),
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
                timestamp_writes: self.timer.render(1),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.timer.finish(encoder);
        self.last = Some((camera, source_revision));
        Ok(work)
    }
    pub fn invalidate(&mut self) {
        self.last = None;
    }
    pub fn config(&self) -> ContactAoConfig {
        self.config
    }
    pub fn sample_count(&self) -> u32 {
        self.samples
    }
    pub fn extent(&self) -> [u32; 2] {
        [self.width, self.height]
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
        u64::from(self.width) * u64::from(self.height) * if self.samples == 1 { 8 } else { 40 }
    }
    pub fn uniform_bytes(&self) -> u64 {
        PARAM_BYTES
    }
    pub fn startup_pipeline_compilations(&self) -> u32 {
        1
    }
    pub fn graph(&self) -> crate::gpu_time::PassGraph<1> {
        self.timer.graph()
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.timer.timestamp_buffer()
    }
}
fn params(config: ContactAoConfig, width: u32, height: u32, samples: u32) -> Params {
    Params {
        matrix: [[0.; 4]; 4],
        inverse: [[0.; 4]; 4],
        image: [
            width,
            height,
            samples,
            if config.quality == 1 { 16 } else { 32 },
        ],
        optics: [config.radius, config.bias, config.strength, 0.],
        kernel: KERNEL_BITS.map(|sample| sample.map(f32::from_bits)),
    }
}
pub fn shader(samples: u32, statistics: bool) -> String {
    let source=include_str!("contact_ao.wgsl").replace("// STATS",if statistics {"struct Stats {fragments:atomic<u32>,loads:atomic<u32>,receivers:atomic<u32>,hits:atomic<u32>};@group(0)@binding(4)var<storage,read_write>stats:Stats;fn record_fragment(){atomicAdd(&stats.fragments,1u);}fn record_load(){atomicAdd(&stats.loads,1u);}fn record_receiver(){atomicAdd(&stats.receivers,1u);}fn record_hit(){atomicAdd(&stats.hits,1u);}"}else{"fn record_fragment(){}fn record_load(){}fn record_receiver(){}fn record_hit(){}"});
    if samples == 1 {
        source
            .replace("texture_depth_multisampled_2d", "texture_depth_2d")
            .replace("texture_multisampled_2d", "texture_2d")
    } else {
        source
    }
}
fn bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    inputs: ContactAoInputs<'_>,
) -> wgpu::BindGroup {
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::TextureView(inputs.hdr),
        },
        wgpu::BindGroupEntry {
            binding: 2,
            resource: wgpu::BindingResource::TextureView(inputs.ambient),
        },
        wgpu::BindGroupEntry {
            binding: 3,
            resource: wgpu::BindingResource::TextureView(inputs.depth),
        },
    ];
    if let Some(stats) = inputs.statistics {
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: stats.as_entire_binding(),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
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
    let make = |count| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("contact ambient visibility HDR output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: count,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | if count == 1 {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
    };
    let texture = make(samples);
    let resolved = if samples == 1 {
        texture.clone()
    } else {
        make(1)
    };
    let msaa = texture.create_view(&Default::default());
    let resolved_view = resolved.create_view(&Default::default());
    (texture, resolved, msaa, resolved_view)
}
