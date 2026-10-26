#![cfg(not(target_arch = "wasm32"))]
//! Independent sample-ray witness for homogeneous near Height interpolation.
use render::terrain_gpu::Camera;
#[path = "common/msaa.rs"]
mod sample_locations;

fn ground_point(camera: Camera, sample: [f64; 2]) -> [f64; 3] {
    let ndc = [2. * sample[0] / 128. - 1., 1. - 2. * sample[1] / 128.];
    let a: [f64; 3] = std::array::from_fn(|axis| {
        f64::from(camera.view_projection[axis][1])
            - ndc[1] * f64::from(camera.view_projection[axis][3])
    });
    let b: [f64; 3] = std::array::from_fn(|axis| {
        f64::from(camera.view_projection[axis][0])
            - ndc[0] * f64::from(camera.view_projection[axis][3])
    });
    let ray = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let t = (8. - 55.) / ray[1];
    [t * ray[0], 8., 100. + t * ray[2]]
}

fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}

#[test]
fn homogeneous_height_world_matches_independent_sample_rays() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let offsets = sample_locations::locations(&gpu);
        let camera = Camera::perspective(
            [0., 55., 100.],
            [0., 8., 0.],
            [0., 1., 0.],
            1.,
            1.,
            0.1,
            300.,
        );
        let mut uniform = Vec::new();
        for col in camera.view_projection {
            for value in col {
                uniform.extend(value.to_le_bytes());
            }
        }
        uniform.extend([0u8; 16]);
        uniform.extend(4u32.to_le_bytes());
        uniform.extend([0u8; 12]);
        let camera_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 96,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&camera_buffer, 0, &uniform);
        let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {label:Some("independent Height sample position probe"),source:wgpu::ShaderSource::Wgsl(r#"
struct Camera {matrix:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};
@group(0)@binding(0)var<uniform> camera:Camera;
@group(0)@binding(1)var atlas:texture_2d_array<f32>;
@group(0)@binding(2)var texels:sampler;
@group(0)@binding(3)var<storage,read_write> measured:array<vec4<f32>,12>;
struct Out {@builtin(position)position:vec4<f32>,@location(0)uv:vec2<f32>,@location(1)normal:vec3<f32>,@location(2)@interpolate(perspective,sample)world:vec4<f32>,@location(3)@interpolate(flat)layer:u32,@location(4)@interpolate(flat)tint:vec3<f32>};
@vertex fn vertex(@builtin(vertex_index) vi:u32)->Out {
let cell=vi/6u;let corners=array<vec2<u32>,6>(vec2(0u,0u),vec2(0u,1u),vec2(1u,0u),vec2(1u,0u),vec2(0u,1u),vec2(1u,1u));let corner=corners[vi%6u];
let p=vec3(f32(cell%80u+corner.x)-32.,8.,f32(cell/80u+corner.y)-32.);var out:Out;out.position=camera.matrix*vec4(p,1.);out.world=vec4(p,1.);out.uv=p.xz;out.normal=vec3(0.,1.,0.);out.layer=3u;out.tint=vec3(1.);return out;
}
@fragment fn fragment(input:Out,@builtin(sample_index) sample:u32)->@location(0)vec4<f32>{
if sample>=camera.reserved.x {discard;}let texel=textureSample(atlas,texels,input.uv,i32(input.layer));let world=input.world.xyz/input.world.w;let value=vec4((world.x+16.)*texel.r*input.tint.x*input.normal.y,world.z,input.world.w,fract(input.position.y));let pixel=vec2<u32>(input.position.xy);if all(pixel==vec2(46u,66u)){measured[sample]=value;}if all(pixel==vec2(45u,69u)){measured[4u+sample]=value;}if all(pixel==vec2(41u,82u)){measured[8u+sample]=value;}return value;
}
"#.into())});
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
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
                primitive: wgpu::PrimitiveState {
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 4,
                    ..Default::default()
                },
                multiview: None,
                cache: None,
            });
        let atlas = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 4,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255; 16],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 4,
            },
        );
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = gpu.device.create_sampler(&Default::default());
        let rows = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("exact f32 sample coordinate witnesses"),
            size: 192,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: rows.as_entire_binding(),
                },
            ],
        });
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 128,
                height: 128,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 4,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 192,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..80 * 80 * 6, 0..1);
        }
        encoder.copy_buffer_to_buffer(&rows, 0, &output, 0, 192);
        gpu.queue.submit([encoder.finish()]);
        let rows: Vec<[f32; 4]> = read(&gpu, &output)
            .chunks_exact(16)
            .map(|row| {
                std::array::from_fn(|i| {
                    f32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap())
                })
            })
            .collect();
        for (pixel, rows) in [[46, 66], [45, 69], [41, 82]]
            .into_iter()
            .zip(rows.chunks_exact(4))
        {
            println!(
                "height_sample pixel{pixel:?} actual_worldXplus16_Z_homogeneousW_fragY={rows:?}"
            );
            for (sample, row) in rows.iter().enumerate() {
                let expected = ground_point(
                    camera,
                    [
                        f64::from(pixel[0]) + offsets[sample][0],
                        f64::from(pixel[1]) + offsets[sample][1],
                    ],
                );
                for (channel, expected) in [expected[0] + 16., expected[2]].into_iter().enumerate()
                {
                    // Direct f32 storage avoids any attachment conversion.
                    // The original wrong-sample errors were 0.082..0.104.
                    let tolerance = 0.0001;
                    assert!(
                        (f64::from(row[channel]) - expected).abs() < tolerance,
                        "pixel{pixel:?} sample{sample} channel{channel}: actual{} expected{expected} tolerance{tolerance}",
                        row[channel]
                    );
                }
                assert!(row[2].is_finite() && row[2] > 0.);
            }
        }
    });
}
