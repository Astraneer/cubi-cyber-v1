#![cfg(not(target_arch = "wasm32"))]
//! Independent color/orientation oracle for the next public-port shader helper.
//! Live BlockModels integration is verified separately once attached.
#[test]
fn public_port_helper_matches_enum_colors_and_world_face_orientation() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let source = format!(
            "{}\n{}",
            include_str!("../src/block_ports.wgsl"),
            r#"
@group(0)@binding(0)var<uniform>light:vec4<f32>;
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let a=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(a[i],0.,1.);}
@fragment fn fragment(@builtin(position)p:vec4<f32>)->@location(0)vec4<f32>{
 let pixel=u32(p.x);if pixel>=192u{if pixel>=210u{return vec4(0.,0.,0.,1.);}return vec4(connector_linear(pixel-191u)*light.x,1.);}let face=pixel%6u;let yaw=(pixel/6u)%4u;
 var word=0x80000000u;
 if pixel<144u{let permutation=pixel/24u;for(var i=0u;i<6u;i++){word|=((i+permutation)%6u)<<(3u*i);}}
 else if pixel<168u{word=0u;}
 let normals=array<vec3<f32>,6>(vec3(0.,-1.,0.),vec3(0.,1.,0.),vec3(0.,0.,-1.),vec3(0.,0.,1.),vec3(-1.,0.,0.),vec3(1.,0.,0.));
 let n=normals[face];let angle=-f32(yaw)*1.57079632679;let c=cos(angle);let s=sin(angle);
 let world_normal=vec3(n.x*c+n.z*s,n.y,-n.x*s+n.z*c);
 let mode=machine_port_mode(word,world_normal);
 return vec4(machine_port_overlay(vec3(.2),mode,vec2(.5))*light.x,1.);
}
"#
        );
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("public ports independent float oracle"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let light = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: light.as_entire_binding(),
            }],
        });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            });
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 224,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 224 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let colors = [
            [207u8, 207, 207],
            [201, 7, 31],
            [54, 107, 208],
            [164, 96, 217],
            [89, 193, 95],
            [255, 221, 79],
        ];
        // Exact cardinal rotation table, independent of shader trig/branching.
        let connector_colors = [
            [64u8, 64, 64],
            [54, 107, 208],
            [89, 193, 95],
            [0, 243, 208],
            [201, 7, 31],
            [164, 96, 217],
            [255, 161, 96],
            [207, 207, 207],
            [122, 122, 122],
            [85, 158, 255],
            [117, 255, 137],
            [48, 255, 249],
            [255, 56, 60],
            [213, 94, 203],
            [255, 221, 79],
            [255, 255, 255],
            [161, 118, 73],
            [255, 188, 196],
        ];
        let world_faces = [
            [0, 1, 2, 3, 4, 5],
            [0, 1, 5, 4, 2, 3],
            [0, 1, 3, 2, 5, 4],
            [0, 1, 4, 5, 3, 2],
        ];
        let mut max_error = 0f64;
        for illumination in [1f32, 0.] {
            gpu.queue
                .write_buffer(&light, 0, bytemuck::bytes_of(&[illumination, 0., 0., 0.]));
            let view = texture.create_view(&Default::default());
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
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
                pass.draw(0..3, 0..1);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &read,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(224 * 16),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 224,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            read.slice(..)
                .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            rx.recv().unwrap().unwrap();
            let bytes = read.slice(..).get_mapped_range();
            for pixel in 0..224 {
                let mode = if pixel < 144 {
                    Some((world_faces[pixel / 6 % 4][pixel % 6] + pixel / 24) % 6)
                } else if pixel < 168 {
                    None
                } else {
                    Some(0)
                };
                for channel in 0..3 {
                    let albedo = if pixel >= 210 {
                        0.
                    } else if pixel >= 192 {
                        let s = f64::from(connector_colors[pixel - 192][channel]) / 255.;
                        if s <= 0.04045 {
                            s / 12.92
                        } else {
                            ((s + 0.055) / 1.055).powf(2.4)
                        }
                    } else {
                        mode.map_or(0.2, |mode| {
                            let s = f64::from(colors[mode][channel]) / 255.;
                            let linear = if s <= 0.04045 {
                                s / 12.92
                            } else {
                                ((s + 0.055) / 1.055).powf(2.4)
                            };
                            0.2 * 0.4 + linear * 0.6
                        })
                    };
                    let offset = pixel * 16 + channel * 4;
                    let actual = f64::from(f32::from_le_bytes(
                        bytes[offset..offset + 4].try_into().unwrap(),
                    ));
                    let error = (actual - albedo * f64::from(illumination)).abs();
                    max_error = max_error.max(error);
                    assert!(
                        error <= 0.0000003,
                        "port pixel{pixel} channel{channel}:actual{actual} expected{albedo} light{illumination}"
                    );
                }
            }
            drop(bytes);
            read.unmap();
        }
        println!(
            "publicPorts_helper_GPU144mode/facingCases+24Absent+24ConfirmedNone+18ConnectorColors maxError{max_error:.9} darkness0;liveCatalogStillSeparate"
        );
    });
}
