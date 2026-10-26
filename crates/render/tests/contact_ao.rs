#![cfg(not(target_arch = "wasm32"))]
//! Standalone production implementation before optional MRT integration.
use render::{gpu_time, terrain_gpu};
#[path = "../src/contact_ao.rs"]
#[allow(dead_code)]
mod contact_ao;
#[path = "common/msaa.rs"]
mod msaa;
#[path = "common/contact_ao_reference.rs"]
mod reference;
use contact_ao::{ContactAoConfig, ContactAoGpu, ContactAoInputs};
use terrain_gpu::{Camera, UploadBudget};
const N: u32 = 32;
fn read(gpu: &render::Gpu, b: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let v = b.slice(..).get_mapped_range().to_vec();
    b.unmap();
    v
}
fn image(gpu: &render::Gpu, source: &wgpu::Texture) -> Vec<[f32; 4]> {
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256 * u64::from(N),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = gpu.device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &b,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(N),
            },
        },
        wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([e.finish()]);
    read(gpu, &b)
        .chunks_exact(8)
        .map(|v| {
            std::array::from_fn(|c| {
                let h = u16::from_le_bytes([v[c * 2], v[c * 2 + 1]]);
                let sign = if h & 32768 != 0 { -1. } else { 1. };
                let exp = (h >> 10) & 31;
                let m = f32::from(h & 1023);
                if exp == 0 {
                    sign * m * 2f32.powi(-24)
                } else {
                    sign * (1. + m / 1024.) * 2f32.powi(i32::from(exp) - 15)
                }
            })
        })
        .collect()
}
#[test]
fn same_frame_contact_ambient_preserves_direct_sky_and_msaa_samples() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let locations = msaa::locations(&gpu);
    for samples in [1, 4] {
        let camera =
            Camera::perspective([3., 3., 4.], [0., 0., 0.], [0., 1., 0.], 1., 1., 0.1, 100.);
        let mut matrices = Vec::new();
        matrices.extend(camera.view_projection.into_iter().flatten());
        matrices.extend(
            camera
                .inverse_view_projection()
                .unwrap()
                .into_iter()
                .flatten(),
        );
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 144,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&uniform, 0, bytemuck::cast_slice(&matrices));
        let texture = |format| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: N,
                    height: N,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | if samples == 1 {
                        wgpu::TextureUsages::COPY_SRC
                    } else {
                        wgpu::TextureUsages::empty()
                    },
                view_formats: &[],
            })
        };
        let hdr = texture(wgpu::TextureFormat::Rgba16Float);
        let ambient = texture(wgpu::TextureFormat::Rgba16Float);
        let depth = texture(wgpu::TextureFormat::Depth32Float);
        let baseline = if samples == 1 {
            hdr.clone()
        } else {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("unmodified actual HDR resolve"),
                size: wgpu::Extent3d {
                    width: N,
                    height: N,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let ambient_baseline = if samples == 1 {
            ambient.clone()
        } else {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("observed ambient MRT radiance format"),
                size: wgpu::Extent3d {
                    width: N,
                    height: N,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let ambient_baseline_view = ambient_baseline.create_view(&Default::default());
        let baseline_view = baseline.create_view(&Default::default());
        let hv = hdr.create_view(&Default::default());
        let av = ambient.create_view(&Default::default());
        let dv = depth.create_view(&Default::default());
        let source=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {label:Some("independent analytic floor/wall source"),source:wgpu::ShaderSource::Wgsl(r#"
struct Camera {vp:mat4x4<f32>,inverse:mat4x4<f32>,mode:vec4<u32>};@group(0)@binding(0)var<uniform>camera:Camera;
struct Vertex{@builtin(position)p:vec4<f32>,@location(0)@interpolate(linear,sample)uv:vec2<f32>};
@vertex fn vertex(@builtin(vertex_index)i:u32)->Vertex{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));var out:Vertex;out.p=vec4(p[i],0.,1.);out.uv=vec2(p[i].x*0.5+0.5,0.5-p[i].y*0.5);return out;}
fn world(uv:vec2<f32>,d:f32)->vec3<f32>{let p=camera.inverse*vec4(uv.x*2.-1.,1.-uv.y*2.,d,1.);return p.xyz/p.w;}
struct Out{@location(0)hdr:vec4<f32>,@location(1)ambient:vec4<f32>,@builtin(frag_depth)depth:f32};
@fragment fn fragment(input:Vertex,@builtin(sample_index)sample:u32)->Out {
 let origin=world(input.uv,1.);let direction=normalize(world(input.uv,0.)-origin);var best=1e9;
 let floor_t=-origin.y/direction.y;let floor_point=origin+floor_t*direction;if floor_t>0.&&abs(floor_point.x)<2.&&abs(floor_point.z)<2.{best=floor_t;}
 if camera.mode.x==1u {let wall_t=-origin.x/direction.x;let p=origin+wall_t*direction;if wall_t>0.&&p.y>=0.&&p.y<2.&&abs(p.z)<2.{best=min(best,wall_t);}}
 var out:Out;out.hdr=vec4(0.2,0.4,0.8,1.);out.ambient=vec4(0.);out.depth=0.;
 if best<1e8{let p=origin+best*direction;let clip=camera.vp*vec4(p,1.);out.depth=select(clip.z/clip.w,0.,camera.mode.x==2u);out.hdr=vec4(1.2,1.1,1.,1.);out.ambient=vec4(select(vec3(0.8,0.6,0.4),vec3(0.),camera.mode.y!=0u),1.);}
 if camera.mode.z!=0u {out.ambient=vec4(-out.ambient.rgb,out.ambient.a);}
 return out;
}
"#.into())});
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: None,
                vertex: wgpu::VertexState {
                    module: &source,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Always,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &source,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[const {
                        Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba16Float,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })
                    }; 2],
                }),
                multiview: None,
                cache: None,
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let stats = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let inputs = ContactAoInputs {
            hdr: &hv,
            ambient: &av,
            depth: &dv,
            statistics: Some(&stats),
        };
        let mut off_budget = UploadBudget::new(0);
        assert!(
            ContactAoGpu::new(
                &gpu.device,
                &gpu.queue,
                N,
                N,
                samples,
                ContactAoConfig {
                    quality: 0,
                    ..Default::default()
                },
                inputs,
                &mut off_budget
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(off_budget.remaining_bytes, 0);
        for quality in [1, 2] {
            let mut budget = UploadBudget::new(10000);
            let mut ao = ContactAoGpu::new(
                &gpu.device,
                &gpu.queue,
                N,
                N,
                samples,
                ContactAoConfig {
                    quality,
                    ..Default::default()
                },
                inputs,
                &mut budget,
            )
            .unwrap()
            .unwrap();
            assert_eq!(10000 - budget.remaining_bytes, 672);
            assert_eq!(
                ao.target_bytes(),
                u64::from(N * N) * if samples == 1 { 8 } else { 40 }
            );
            let mut corner = 0;
            for (revision, mode, direct_only, signed) in [
                (1, 0, 0, 0),
                (2, 1, 0, 0),
                (3, 2, 0, 0),
                (4, 1, 1, 0),
                (5, 1, 0, 1),
            ] {
                gpu.queue.write_buffer(
                    &uniform,
                    128,
                    bytemuck::cast_slice(&[mode, direct_only, signed, 0u32]),
                );
                gpu.queue.write_buffer(&stats, 0, &[0; 16]);
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("actual opaque MRT floor+wall"),
                        color_attachments: &[
                            Some(wgpu::RenderPassColorAttachment {
                                view: &hv,
                                depth_slice: None,
                                resolve_target: (samples > 1).then_some(&baseline_view),
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                    store: wgpu::StoreOp::Store,
                                },
                            }),
                            Some(wgpu::RenderPassColorAttachment {
                                view: &av,
                                depth_slice: None,
                                resolve_target: (samples > 1).then_some(&ambient_baseline_view),
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                    store: wgpu::StoreOp::Store,
                                },
                            }),
                        ],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &dv,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(0.),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    pass.set_pipeline(&pipeline);
                    pass.set_bind_group(0, &bind, &[]);
                    pass.draw(0..3, 0..1);
                }
                let planned = ao.pending_work(camera, revision).unwrap();
                let mut denied = UploadBudget::new(128);
                assert_eq!(
                    ao.encode(
                        &gpu.queue,
                        &mut encoder,
                        camera,
                        revision,
                        &mut denied,
                        planned.max_depth_reads - 1
                    ),
                    Err(contact_ao::ContactAoError::WorkBudget)
                );
                assert_eq!(denied.remaining_bytes, 128);
                assert_eq!(ao.pending_work(camera, revision).unwrap(), planned);
                if planned.upload_bytes != 0 {
                    let mut denied = UploadBudget::new(127);
                    assert_eq!(
                        ao.encode(
                            &gpu.queue,
                            &mut encoder,
                            camera,
                            revision,
                            &mut denied,
                            u64::MAX
                        ),
                        Err(contact_ao::ContactAoError::UploadBudget)
                    );
                    assert_eq!(denied.remaining_bytes, 127);
                }
                let work = ao
                    .encode(
                        &gpu.queue,
                        &mut encoder,
                        camera,
                        revision,
                        &mut UploadBudget::new(128),
                        u64::MAX,
                    )
                    .unwrap();
                gpu.queue.submit([encoder.finish()]);
                let pixels = image(&gpu, ao.resolved_texture());
                let read_stats = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 16,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut e = gpu.device.create_command_encoder(&Default::default());
                e.copy_buffer_to_buffer(&stats, 0, &read_stats, 0, 16);
                gpu.queue.submit([e.finish()]);
                let values = read(&gpu, &read_stats);
                let actual: [u32; 4] = std::array::from_fn(|i| {
                    u32::from_le_bytes(values[i * 4..i * 4 + 4].try_into().unwrap())
                });
                assert_eq!(u64::from(actual[0]), work.composition_samples);
                assert!(u64::from(actual[1]) <= work.max_depth_reads);
                assert!(actual[2] <= actual[0]);
                assert!(u64::from(actual[3]) <= work.max_kernel_samples);
                if direct_only != 0 {
                    assert_eq!(&actual[1..], &[0, 0, 0]);
                } else if mode == 1 {
                    assert!(actual[3] > 10);
                } else {
                    assert_eq!(actual[3], 0);
                }

                let original = image(&gpu, &baseline);
                let ambient_original = image(&gpu, &ambient_baseline);
                let dark = pixels
                    .iter()
                    .zip(&original)
                    .filter(|(p, o)| (p[0] - o[0]).abs() > 0.003)
                    .count();
                if mode == 1 && direct_only == 0 {
                    if signed == 0 {
                        corner = dark;
                    }
                    if signed != 0 {
                        assert!(
                            pixels
                                .iter()
                                .zip(&original)
                                .all(|(p, o)| p[..3].iter().zip(&o[..3]).all(|(p, o)| *p >= *o))
                        );
                    }
                    let sample_locations = if samples == 1 {
                        vec![[0.5, 0.5]]
                    } else {
                        locations.to_vec()
                    };
                    let mut witnesses = 0;
                    let mut max_error = 0f64;
                    let mut negative = 0f64;
                    for y in 1..N - 1 {
                        for x in 1..N - 1 {
                            let ao = sample_locations
                                .iter()
                                .map(|location| {
                                    reference::sample(
                                        camera,
                                        [x as i32, y as i32],
                                        *location,
                                        quality,
                                    )
                                })
                                .collect::<Option<Vec<_>>>();
                            let Some(ao) = ao else {
                                continue;
                            };
                            let expected = ao
                                .iter()
                                .map(|ao| {
                                    f64::from(original[(x + y * N) as usize][0])
                                        - f64::from(ambient_original[(x + y * N) as usize][0])
                                            * (1. - ao)
                                })
                                .sum::<f64>()
                                / f64::from(samples);
                            let actual = f64::from(pixels[(x + y * N) as usize][0]);
                            max_error = max_error.max((actual - expected).abs());
                            negative = negative.max(
                                (f64::from(original[(x + y * N) as usize][0]) - expected).abs(),
                            );
                            witnesses += 1;
                        }
                    }
                    assert!(
                        witnesses > 80,
                        "insufficient independent f64 witnesses {witnesses}"
                    );
                    assert!(
                        max_error < 0.002,
                        "f64 AO error {max_error} samples{samples}/q{quality}"
                    );
                    assert!(negative > 0.02, "constant/unoccluded negative escaped");
                    println!(
                        "AO f64 signed{signed} witnesses{witnesses} maxError{max_error:.8} constantNegative{negative:.6}"
                    );

                    assert!(
                        corner > 10,
                        "no contact shadows samples{samples}/q{quality}"
                    );
                    for (pixel, source) in pixels.iter().zip(&original) {
                        if signed == 0 && source[0] > 1.19 {
                            assert!(
                                pixel[0] >= 0.399 && pixel[1] >= 0.499 && pixel[2] >= 0.599,
                                "direct/emission floor changed {pixel:?}"
                            );
                        }
                    }
                } else {
                    assert_eq!(
                        pixels, original,
                        "flat/Unknown/direct source changed samples{samples}/q{quality}/mode{mode}/direct{direct_only}"
                    );
                }
                assert_eq!(work.composition_samples, u64::from(N * N * samples));
                assert_eq!(
                    work.max_kernel_samples,
                    u64::from(N * N * samples) * if quality == 1 { 16 } else { 32 }
                );
                assert_eq!(ao.pending_work(camera, revision).unwrap().passes, 0);
                let mut idle = gpu.device.create_command_encoder(&Default::default());
                assert_eq!(
                    ao.encode(
                        &gpu.queue,
                        &mut idle,
                        camera,
                        revision,
                        &mut UploadBudget::new(0),
                        0
                    )
                    .unwrap(),
                    contact_ao::ContactAoWork::default()
                );
                assert_eq!(ao.graph().count, 0);
            }
            let before_resize = image(&gpu, ao.resolved_texture());
            let mut denied = UploadBudget::new(16);
            assert_eq!(
                ao.resize_inputs(
                    &gpu.device,
                    &gpu.queue,
                    N,
                    N,
                    ContactAoInputs {
                        statistics: None,
                        ..inputs
                    },
                    &mut denied
                ),
                Err(contact_ao::ContactAoError::InvalidConfig)
            );
            assert_eq!(denied.remaining_bytes, 16);
            assert_eq!(before_resize, image(&gpu, ao.resolved_texture()));
            let mut denied = UploadBudget::new(15);
            assert_eq!(
                ao.resize_inputs(&gpu.device, &gpu.queue, N, N, inputs, &mut denied),
                Err(contact_ao::ContactAoError::UploadBudget)
            );
            assert_eq!(denied.remaining_bytes, 15);
            let mut resize = UploadBudget::new(16);
            ao.resize_inputs(&gpu.device, &gpu.queue, N, N, inputs, &mut resize)
                .unwrap();
            assert_eq!(resize.remaining_bytes, 0);
            assert_eq!(ao.startup_pipeline_compilations(), 1);
            assert_eq!(ao.extent(), [N, N]);
            assert_eq!(ao.sample_count(), samples);
            let mut e = gpu.device.create_command_encoder(&Default::default());
            ao.encode(
                &gpu.queue,
                &mut e,
                camera,
                5,
                &mut UploadBudget::new(128),
                u64::MAX,
            )
            .unwrap();
            gpu.queue.submit([e.finish()]);
            assert_eq!(before_resize, image(&gpu, ao.resolved_texture()));
            println!(
                "AO component samples{samples} quality{quality} contactPixels{corner} targetBytes{} off0 resources/pipelines; direct/flat/sky safe",
                ao.target_bytes()
            );
        }
    }
}
