//! Standalone production helper before its surface attachment is integrated.
#[path = "common/msaa.rs"]
mod msaa;
use render::terrain_gpu::{Camera, UploadBudget};
use render::water_reflection::{
    ReflectionConfig, ReflectionError, ReflectionInputs, WaterReflectionGpu,
};
const SIZE: u32 = 32;
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
fn image(gpu: &render::Gpu, renderer: &WaterReflectionGpu) -> Vec<[f32; 4]> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("deferred SSR HDR oracle"),
        size: 256 * u64::from(SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: renderer.resolved_texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
        .chunks_exact(8)
        .map(|p| {
            std::array::from_fn(|c| {
                let h = u16::from_le_bytes([p[c * 2], p[c * 2 + 1]]);
                let sign = if h & 32768 != 0 { -1. } else { 1. };
                let exponent = (h >> 10) & 31;
                let mantissa = f32::from(h & 1023);
                if exponent == 0 {
                    sign * mantissa * 2f32.powi(-24)
                } else if exponent == 31 {
                    f32::NAN
                } else {
                    sign * (1. + mantissa / 1024.) * 2f32.powi(i32::from(exponent) - 15)
                }
            })
        })
        .collect()
}
fn transform(matrix: [[f32; 4]; 4], value: [f64; 4]) -> [f64; 4] {
    std::array::from_fn(|r| (0..4).map(|c| f64::from(matrix[c][r]) * value[c]).sum())
}
fn counters(gpu: &render::Gpu, source: &wgpu::Buffer) -> [u32; 4] {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual deferred SSR counters"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(source, 0, &buffer, 0, 16);
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(gpu, &buffer);
    std::array::from_fn(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
}
#[test]
fn deferred_ssr_obeys_viewport_bound_independent_rays_and_exact_unknown_fallback() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let locations = msaa::locations(&gpu);
    for samples in [1, 4] {
        let camera =
            Camera::perspective([8., 6., 24.], [8., 0., 8.], [0., 1., 0.], 1., 1., 0.1, 100.);
        let inverse = camera.inverse_view_projection().unwrap();
        let mut params = [0u32; 64];
        for (index, value) in camera
            .view_projection
            .into_iter()
            .flatten()
            .chain(inverse.into_iter().flatten())
            .enumerate()
        {
            params[index] = value.to_bits();
        }
        for (index, value) in [8f32, 6., 24.].into_iter().enumerate() {
            params[32 + index] = value.to_bits();
        }
        params[44] = 1.333f32.to_bits();
        params[46] = 128f32.to_bits();
        params[52] = SIZE;
        params[53] = SIZE;
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("exact water shared 256B params"),
            size: 256,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&uniform, 0, bytemuck::cast_slice(&params));
        let texture = |label, format, count| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: count,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let opaque_color = texture(
            "independent red wall MSAA color",
            wgpu::TextureFormat::Rgba16Float,
            samples,
        );
        let opaque_resolved = texture(
            "independent red wall resolved color",
            wgpu::TextureFormat::Rgba16Float,
            1,
        );
        let opaque_depth = texture(
            "independent red wall depth",
            wgpu::TextureFormat::Depth32Float,
            samples,
        );
        let water_color = texture(
            "water original HDR samples",
            wgpu::TextureFormat::Rgba16Float,
            samples,
        );
        let water_depth = texture(
            "water plane original depth",
            wgpu::TextureFormat::Depth32Float,
            samples,
        );
        let normal = texture(
            "water plane normal foam optional MRT",
            wgpu::TextureFormat::Rgba16Float,
            samples,
        );
        let sky = texture(
            "black environment exact SSR replacement",
            wgpu::TextureFormat::Rgba16Float,
            1,
        );
        let views = [
            &opaque_color,
            &opaque_resolved,
            &opaque_depth,
            &water_color,
            &water_depth,
            &normal,
            &sky,
        ]
        .map(|t| t.create_view(&Default::default()));
        let stats = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual bounded SSR steps"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let source = "struct P{vp:mat4x4<f32>};@group(0)@binding(0)var<uniform>p:P;@vertex fn wall(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let a=array<vec3<f32>,6>(vec3(0.,0.,0.),vec3(16.,0.,0.),vec3(0.,16.,0.),vec3(0.,16.,0.),vec3(16.,0.,0.),vec3(16.,16.,0.));return p.vp*vec4(a[i],1.);}@fragment fn red()->@location(0)vec4<f32>{return vec4(1.,0.,0.,1.);}@vertex fn plane(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let a=array<vec3<f32>,6>(vec3(0.,0.,16.),vec3(16.,0.,16.),vec3(0.,0.,0.),vec3(0.,0.,0.),vec3(16.,0.,16.),vec3(16.,0.,0.));return p.vp*vec4(a[i],1.);}struct Result{@location(0)color:vec4<f32>,@location(1)normal:vec4<f32>};@fragment fn water()->Result{var r:Result;r.color=vec4(0.125,0.25,0.5,1.);r.normal=vec4(0.,1.,0.,0.);return r;}";
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("independent SSR wall/plane fixture"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let make = |vertex, fragment, targets: &[Option<wgpu::ColorTargetState>]| {
            gpu.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("analytic SSR scene"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some(vertex),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState {
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
                    multisample: wgpu::MultisampleState {
                        count: samples,
                        ..Default::default()
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(fragment),
                        compilation_options: Default::default(),
                        targets,
                    }),
                    multiview: None,
                    cache: None,
                })
        };
        let target = Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba16Float,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        });
        let wall = make("wall", "red", std::slice::from_ref(&target));
        let plane = make("plane", "water", &[target.clone(), target]);
        let bind = |pipeline: &wgpu::RenderPipeline| {
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                }],
            })
        };
        let wall_bind = bind(&wall);
        let plane_bind = bind(&plane);
        let render_scene = |overdraw: u32, known: bool| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            {
                let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("analytic opaque red wall"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &views[0],
                        depth_slice: None,
                        resolve_target: (samples > 1).then_some(&views[1]),
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &views[2],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                if known {
                    p.set_pipeline(&wall);
                    p.set_bind_group(0, &wall_bind, &[]);
                    p.draw(0..6, 0..1);
                }
            }
            {
                let attachments = [
                    Some(wgpu::RenderPassColorAttachment {
                        view: &views[3],
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.125,
                                g: 0.25,
                                b: 0.5,
                                a: 1.,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &views[5],
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ];
                let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("water plane optional MRT"),
                    color_attachments: &attachments,
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &views[4],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                p.set_pipeline(&plane);
                p.set_bind_group(0, &plane_bind, &[]);
                p.draw(0..6, 0..overdraw);
            }
            {
                let _p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("black environment fixture"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &views[6],
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }
            encoder.clear_buffer(&stats, 0, None);
            gpu.queue.submit([encoder.finish()]);
        };
        render_scene(1, true);
        let inputs = || ReflectionInputs {
            params: &uniform,
            water_color: &views[3],
            water_depth: &views[4],
            normal_foam: &views[5],
            opaque_color: if samples == 1 { &views[0] } else { &views[1] },
            opaque_depth: &views[2],
            sky: &views[6],
            statistics: Some(&stats),
        };
        let config = ReflectionConfig {
            steps: 64,
            max_distance: 32.,
            tolerance: 1.2,
        };
        assert!(matches!(
            WaterReflectionGpu::new(
                &gpu.device,
                &gpu.queue,
                samples,
                SIZE,
                SIZE,
                config,
                inputs(),
                &mut UploadBudget::new(15)
            ),
            Err(ReflectionError::UploadBudget)
        ));
        let mut renderer = WaterReflectionGpu::new(
            &gpu.device,
            &gpu.queue,
            samples,
            SIZE,
            SIZE,
            config,
            inputs(),
            &mut UploadBudget::new(16),
        )
        .unwrap();
        assert_eq!(
            renderer.target_bytes(),
            u64::from(SIZE * SIZE) * if samples == 1 { 8 } else { 40 }
        );
        let _ = renderer.color_msaa();
        let _ = renderer.resolved_view();
        let bound = u64::from(SIZE * SIZE * samples * 64);
        let encode = |renderer: &WaterReflectionGpu| {
            let mut e = gpu.device.create_command_encoder(&Default::default());
            assert_eq!(
                renderer.encode(&mut e, bound - 1, None),
                Err(ReflectionError::WorkBudget)
            );
            let work = renderer.encode(&mut e, bound, None).unwrap();
            assert_eq!(work.max_steps, bound);
            gpu.queue.submit([e.finish()]);
        };
        encode(&renderer);
        let actual = image(&gpu, &renderer);
        let actual_counters = counters(&gpu, &stats);
        assert!(u64::from(actual_counters[1]) <= bound);
        assert!(actual_counters[2] > 30);
        assert_eq!(actual_counters[2] + actual_counters[3], actual_counters[0]);
        let samples_xy: Vec<_> = if samples == 1 {
            vec![[0.5, 0.5]]
        } else {
            locations.to_vec()
        };
        let mut witnesses = 0;
        let mut matched = 0;
        let mut max_error = 0f64;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let mut expected = 0.;
                let mut certified = true;
                for sample in &samples_xy {
                    let q = transform(
                        inverse,
                        [
                            (f64::from(x) + sample[0]) / f64::from(SIZE) * 2. - 1.,
                            1. - (f64::from(y) + sample[1]) / f64::from(SIZE) * 2.,
                            0.,
                            1.,
                        ],
                    );
                    let v = [q[0] / q[3] - 8., q[1] / q[3] - 6., q[2] / q[3] - 24.];
                    let length = v.iter().map(|x| x * x).sum::<f64>().sqrt();
                    let ray = v.map(|x| x / length);
                    if ray[1] >= 0. {
                        certified = false;
                        break;
                    }
                    let t = -6. / ray[1];
                    let surface = [8. + ray[0] * t, 0., 24. + ray[2] * t];
                    let r = [ray[0], -ray[1], ray[2]];
                    if surface[0] < 1.
                        || surface[0] > 15.
                        || surface[2] < 1.
                        || surface[2] > 15.
                        || r[2] >= 0.
                    {
                        certified = false;
                        break;
                    }
                    let along = -surface[2] / r[2];
                    let wall = [surface[0] + r[0] * along, r[1] * along, 0.];
                    let clip = transform(camera.view_projection, [wall[0], wall[1], wall[2], 1.]);
                    let screen = [clip[0] / clip[3] * 0.5 + 0.5, 0.5 - clip[1] / clip[3] * 0.5];
                    if along > 30.
                        || wall[0] < 1.
                        || wall[0] > 15.
                        || wall[1] < 1.
                        || wall[1] > 15.
                        || clip[3] <= 0.
                        || screen
                            .iter()
                            .any(|v| *v < 2. / f64::from(SIZE) || *v > 1. - 2. / f64::from(SIZE))
                    {
                        certified = false;
                        break;
                    }
                    let c = -ray[1];
                    let ct = (1. - (1. - c * c) / (1.333 * 1.333)).sqrt();
                    let rs = (c - 1.333 * ct) / (c + 1.333 * ct);
                    let rp = (1.333 * c - ct) / (1.333 * c + ct);
                    expected += (rs * rs + rp * rp) * 0.5 / f64::from(samples);
                }
                if certified {
                    witnesses += 1;
                    let value = actual[(x + y * SIZE) as usize];
                    if value[0] > 0.13 {
                        matched += 1;
                    }
                    max_error = max_error.max((f64::from(value[0]) - (0.125 + expected)).abs());
                }
            }
        }
        assert!(witnesses > 30);
        assert_eq!(matched, witnesses);
        assert!(
            max_error < 0.003,
            "independent Fresnel/linear replacement error {max_error}"
        );
        render_scene(257, true);
        encode(&renderer);
        assert_eq!(
            counters(&gpu, &stats),
            actual_counters,
            "actual traces cannot multiply with surface overdraw"
        );
        assert_eq!(
            image(&gpu, &renderer),
            actual,
            "surface overdraw cannot repeat SSR traces or change its image"
        );
        renderer
            .resize_inputs(&gpu.device, SIZE, SIZE, inputs())
            .unwrap();
        encode(&renderer);
        assert_eq!(image(&gpu, &renderer), actual);
        render_scene(1, false);
        encode(&renderer);
        let unknown = image(&gpu, &renderer);
        assert_eq!(counters(&gpu, &stats)[2], 0);
        assert!(
            unknown.iter().all(|p| *p == [0.125, 0.25, 0.5, 1.]),
            "Unknown miss must preserve source samples exactly"
        );
        eprintln!(
            "M10_DEFERRED_SSR_GPU_OK samples={samples} wall_rays={matched}/{witnesses} maxHDRerror={max_error} maxSteps={bound} actualSteps={} overdraw257_bitwise=true Unknown_bitwise=true resize_bitwise=true",
            actual_counters[1]
        );
    }
}
