//! Single prepared UI pipeline and bounded retained geometry. Repeated rendering
//! of an unchanged UI issues no CPU vertex construction or uploads.
use crate::{
    atlas::{ATLAS_SIZE, UiAtlas},
    paint::{MAX_UI_QUADS, PaintList},
};
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiWork {
    pub pipeline_compilations: u64,
    pub upload_bytes: u64,
    pub vertex_builds: u64,
    pub draws: u64,
}
pub struct UiRenderer {
    pub atlas: UiAtlas,
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    uniform: wgpu::Buffer,
    scratch: Vec<Vertex>,
    count: u32,
    stamp: Option<(u64, u32, u32)>,
    work: UiWork,
    shader_srgb: bool,
}
impl UiRenderer {
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        shader_srgb: bool,
    ) -> Result<Self, &'static str> {
        if shader_srgb && format.is_srgb() {
            return Err("UI output transfer would encode twice");
        }
        let mut atlas = UiAtlas::load()?;
        let pixels = atlas.take_pixels();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("UI pixel atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
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
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE * 4),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("UI nearest texels"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UI viewport"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("UI layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let view = texture.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("UI atlas bindings"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Rust UI"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ui.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("UI pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("UI prepared pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x2,1=>Float32x2,2=>Unorm8x4],
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UI retained vertices"),
            size: (MAX_UI_QUADS * 6 * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            atlas,
            pipeline,
            bind,
            vertices,
            uniform,
            scratch: Vec::with_capacity(MAX_UI_QUADS * 6),
            count: 0,
            stamp: None,
            work: UiWork {
                pipeline_compilations: 1,
                upload_bytes: pixels.len() as u64,
                ..Default::default()
            },
            shader_srgb,
        })
    }
    pub fn upload(&mut self, queue: &wgpu::Queue, list: &PaintList) -> Result<bool, &'static str> {
        let stamp = (list.revision, list.width, list.height);
        if self.stamp == Some(stamp) {
            return Ok(false);
        }
        if list.width == 0 || list.height == 0 || list.quads.len() > MAX_UI_QUADS {
            return Err("UI frame budget");
        }
        self.scratch.clear();
        for quad in &list.quads {
            let r = quad.rect;
            let u = quad.uv;
            let color = [
                (quad.color >> 16) as u8,
                (quad.color >> 8) as u8,
                quad.color as u8,
                (quad.color >> 24) as u8,
            ];
            for (x, y) in [
                (0.0, 0.0),
                (0.0, 1.0),
                (1.0, 0.0),
                (1.0, 0.0),
                (0.0, 1.0),
                (1.0, 1.0),
            ] {
                self.scratch.push(Vertex {
                    position: [r.x + x * r.w, r.y + y * r.h],
                    uv: [u.x + x * u.w, u.y + y * u.h],
                    color,
                });
            }
        }
        let uniform = [
            list.width as f32,
            list.height as f32,
            u8::from(self.shader_srgb) as f32,
            0.0,
        ];
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&uniform));
        let bytes = bytemuck::cast_slice(&self.scratch);
        if !bytes.is_empty() {
            queue.write_buffer(&self.vertices, 0, bytes);
        }
        self.count = self.scratch.len() as u32;
        self.stamp = Some(stamp);
        self.work.upload_bytes += bytes.len() as u64 + 16;
        self.work.vertex_builds += self.count as u64;
        Ok(true)
    }
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        if self.count == 0 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("UI overlay"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.count, 0..1);
        self.work.draws += 1;
    }
    pub fn take_work(&mut self) -> UiWork {
        std::mem::take(&mut self.work)
    }
}
