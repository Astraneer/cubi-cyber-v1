#![cfg(not(target_arch = "wasm32"))]
use cloud_gpu::{CloudConfig, CloudDensity, CloudGpu, CloudInputs, CloudQuality, CloudState};
use render::temporal_optics_mask::{Error, TemporalOpticsMask};
use render::{cloud_gpu, water_gpu};
use render::{
    terrain_gpu::{Camera, UploadBudget, translation},
    terrain_mesh::Face,
};
use water_gpu::{WaterConfig, WaterDraw, WaterGpu, WaterInputs, WaterQuad, WaterState};
const W: u32 = 64;
const H: u32 = 32;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn texture(gpu: &render::Gpu, format: wgpu::TextureFormat) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("optical mask oracle input/target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
struct Inputs {
    color: wgpu::Texture,
    depth: wgpu::Texture,
    sky: wgpu::Texture,
    motion: wgpu::Texture,
    surface: wgpu::Texture,
}
impl Inputs {
    fn new(gpu: &render::Gpu) -> Self {
        Self {
            color: texture(gpu, wgpu::TextureFormat::Rgba16Float),
            depth: texture(gpu, wgpu::TextureFormat::Depth32Float),
            sky: texture(gpu, wgpu::TextureFormat::Rgba16Float),
            motion: texture(gpu, wgpu::TextureFormat::Rgba16Float),
            surface: texture(gpu, wgpu::TextureFormat::Rgba32Uint),
        }
    }
    fn clear(&self, gpu: &render::Gpu, depth: f32) {
        let mut e = gpu.device.create_command_encoder(&Default::default());
        let cv = self.color.create_view(&Default::default());
        let dv = self.depth.create_view(&Default::default());
        {
            let _p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("known opaque optical input"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &cv,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(depth),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        for (t, color) in [
            (
                &self.sky,
                wgpu::Color {
                    r: 1.,
                    g: 1.,
                    b: 1.,
                    a: 1.,
                },
            ),
            (
                &self.motion,
                wgpu::Color {
                    r: 1.,
                    g: 2.,
                    b: 3.,
                    a: 4.,
                },
            ),
            (
                &self.surface,
                wgpu::Color {
                    r: 17.,
                    g: 23.,
                    b: 31.,
                    a: 41.,
                },
            ),
        ] {
            let v = t.create_view(&Default::default());
            let _p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("independent nonzero motion/identity sentinel"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &v,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        gpu.queue.submit([e.finish()]);
    }
}
fn bytes(gpu: &render::Gpu, texture: &wgpu::Texture, bpp: u32) -> Vec<u8> {
    let row = (W * bpp).div_ceil(256) * 256;
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual mask readback"),
        size: u64::from(row * H),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut e = gpu.device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &b,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([e.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |v| tx.send(v).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let padded = b.slice(..).get_mapped_range();
    let out = padded
        .chunks(row as usize)
        .flat_map(|row| row[..(W * bpp) as usize].iter().copied())
        .collect();
    drop(padded);
    b.unmap();
    out
}
fn half(bytes: &[u8], pixel: usize, channel: usize) -> f32 {
    let at = pixel * 8 + channel * 2;
    let b = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let sign = if b & 32768 != 0 { -1. } else { 1. };
    let e = (b >> 10) & 31;
    let m = f32::from(b & 1023);
    if e == 0 {
        sign * m * 2f32.powi(-24)
    } else {
        sign * (1. + m / 1024.) * 2f32.powi(i32::from(e) - 15)
    }
}
fn masked(gpu: &render::Gpu, inputs: &Inputs) -> Vec<bool> {
    let ids = bytes(gpu, &inputs.surface, 16);
    let motion = bytes(gpu, &inputs.motion, 8);
    ids.chunks_exact(16)
        .enumerate()
        .map(|(pixel, p)| {
            let values: Vec<_> = p
                .chunks_exact(4)
                .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
                .collect();
            let zero = values == [0; 4];
            assert!(zero || values == [17, 23, 31, 41], "partial/corrupted ID");
            assert_eq!(half(&motion, pixel, 0), if zero { 0. } else { 1. });
            assert_eq!(half(&motion, pixel, 3), if zero { 0. } else { 4. });
            zero
        })
        .collect()
}
fn camera() -> Camera {
    Camera::orthographic([8., 10., 8.], [8., 0., 8.], [0., 0., -1.], 8., 2., 0.1, 32.)
}
fn make_water(gpu: &render::Gpu, inputs: &Inputs, waves: f32) -> WaterGpu {
    let mut w = WaterGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        WaterConfig {
            max_objects: 2,
            max_quads: 8,
            reflection_steps: 0,
            wave_amplitude: waves,
            ..Default::default()
        },
        W,
        H,
        WaterInputs {
            color_msaa: &inputs.color.create_view(&Default::default()),
            depth_msaa: &inputs.depth.create_view(&Default::default()),
            sky: &inputs.sky.create_view(&Default::default()),
            transmittance: &inputs.sky.create_view(&Default::default()),
        },
        None,
        1,
    )
    .unwrap();
    let object = w
        .upload_object(
            &gpu.queue,
            None,
            translation([0.; 3]),
            &mut UploadBudget::new(1000),
        )
        .unwrap();
    w.upload_quads(
        &gpu.queue,
        &[WaterDraw {
            quad: WaterQuad::new([4, 0, 12], Face::Up, [8, 8], [1.; 4], [0.; 2], u32::MAX, 0.)
                .unwrap(),
            object,
        }],
        &mut UploadBudget::new(1000),
    )
    .unwrap();
    w
}
fn water_frame(gpu: &render::Gpu, water: &mut WaterGpu, time: f32) {
    let mut e = gpu.device.create_command_encoder(&Default::default());
    water
        .encode(
            &gpu.queue,
            &mut e,
            WaterState {
                camera: camera(),
                eye: [8., 10., 8.],
                time_seconds: time,
                sun: [0., 1., 0.],
                sun_intensity: 0.,
                underwater: false,
                scene_revision: time.to_bits() as u64 + 1,
            },
            &mut UploadBudget::new(10000),
            0,
        )
        .unwrap();
    gpu.queue.submit([e.finish()]);
}
#[test]
fn water_exact_visible_occluded_coplanar_and_wave_coverage_invalidates_only_real_surfaces() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true))
        .expect("actual GPU required");
    let inputs = Inputs::new(&gpu);
    inputs.clear(&gpu, 0.2);
    let mut water = make_water(&gpu, &inputs, 0.);
    let mut mask = TemporalOpticsMask::new(
        &gpu.device,
        &gpu.queue,
        true,
        Some(&water),
        None,
        &inputs.motion.create_view(&Default::default()),
        &inputs.surface.create_view(&Default::default()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(mask.pipeline_compilations(), 1);
    assert_eq!(mask.target_bytes(), 0);
    assert!(mask.allocation_bytes() <= 256);
    assert_eq!(mask.extent(), [W, H]);
    assert!(
        TemporalOpticsMask::new(
            &gpu.device,
            &gpu.queue,
            false,
            Some(&water),
            None,
            &inputs.sky.create_view(&Default::default()),
            &inputs.sky.create_view(&Default::default())
        )
        .unwrap()
        .is_none()
    );
    assert!(matches!(
        TemporalOpticsMask::new(
            &gpu.device,
            &gpu.queue,
            true,
            Some(&water),
            None,
            &inputs.sky.create_view(&Default::default()),
            &inputs.sky.create_view(&Default::default())
        ),
        Err(Error::InvalidTargets)
    ));
    let mut e = gpu.device.create_command_encoder(&Default::default());
    assert_eq!(
        mask.encode(&mut e, Some(&water), None, u64::MAX),
        Err(Error::NotPrepared)
    );
    assert_eq!(mask.graph().count, 0);
    drop(e);
    water_frame(&gpu, &mut water, 0.);
    let plan = mask.pending_work(Some(&water), None).unwrap();
    assert_eq!(plan.texture_loads_bound, 0);
    assert_eq!(plan.vertex_invocations_bound, 4);
    assert_eq!(plan.pixels, u64::from(W * H));
    let motion = inputs.motion.create_view(&Default::default());
    let surface = inputs.surface.create_view(&Default::default());
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        assert_eq!(mask.pending_work(Some(&water), None).unwrap(), plan);
        assert!(
            TemporalOpticsMask::new(
                &gpu.device,
                &gpu.queue,
                false,
                Some(&water),
                None,
                &motion,
                &surface
            )
            .unwrap()
            .is_none()
        );
    }
    let idle = scope.finish();
    assert_eq!(idle.allocating_calls(), 0);
    assert_eq!(idle.dealloc_calls, 0);
    let scope = telemetry::AllocationScope::begin().unwrap();
    let control = std::hint::black_box(vec![1u8; 4096]);
    std::hint::black_box(&control);
    drop(control);
    assert!(scope.finish().allocating_calls() > 0);
    let mut e = gpu.device.create_command_encoder(&Default::default());
    assert_eq!(
        mask.encode(&mut e, Some(&water), None, plan.pixels - 1),
        Err(Error::WorkBudget)
    );
    drop(e);
    assert!(!masked(&gpu, &inputs).into_iter().any(|v| v));
    let mut e = gpu.device.create_command_encoder(&Default::default());
    assert_eq!(
        mask.encode(&mut e, Some(&water), None, plan.pixels)
            .unwrap(),
        plan
    );
    gpu.queue.submit([e.finish()]);
    let hit = masked(&gpu, &inputs);
    let color = bytes(&gpu, water.resolved_texture(), 8);
    let mut visible = 0;
    for (p, hit) in hit.iter().enumerate() {
        let reference = half(&color, p, 0) > 0.0001;
        assert_eq!(*hit, reference, "visible coverage pixel{p}");
        visible += usize::from(*hit);
    }
    assert_eq!(visible, 256);
    assert_eq!(mask.graph().count, 1);
    assert_eq!(
        mask.timing_source().is_some(),
        gpu.device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
    );
    mask.clear_timing();
    assert!(mask.timing_source().is_none());
    // Equal-depth is independently generated from the same planar projection;
    // a depth-difference-only mask would incorrectly retain all256 histories.
    let depth = camera().view_projection[3][2];
    inputs.clear(&gpu, depth);
    water_frame(&gpu, &mut water, 1.);
    let mut e = gpu.device.create_command_encoder(&Default::default());
    mask.encode(&mut e, Some(&water), None, plan.pixels)
        .unwrap();
    gpu.queue.submit([e.finish()]);
    let coplanar = masked(&gpu, &inputs);
    let color = bytes(&gpu, water.resolved_texture(), 8);
    let actual = (0..(W * H) as usize)
        .filter(|p| half(&color, *p, 0) > 0.0001)
        .count();
    assert_eq!(
        actual, 256,
        "coplanar positive control must draw actual water"
    );
    assert_eq!(coplanar.iter().filter(|v| **v).count(), actual);
    inputs.clear(&gpu, 0.95);
    water_frame(&gpu, &mut water, 2.);
    let mut e = gpu.device.create_command_encoder(&Default::default());
    mask.encode(&mut e, Some(&water), None, plan.pixels)
        .unwrap();
    gpu.queue.submit([e.finish()]);
    assert!(
        !masked(&gpu, &inputs).into_iter().any(|v| v),
        "occluded water must retain foreground IDs"
    );
    mask.resize_inputs(
        &inputs.motion.create_view(&Default::default()),
        &inputs.surface.create_view(&Default::default()),
    )
    .unwrap();
    assert_eq!(mask.pipeline_compilations(), 1);
    for time in [0., 0.3, 1.7] {
        inputs.clear(&gpu, 0.2);
        let mut wavy = make_water(&gpu, &inputs, 0.24);
        water_frame(&gpu, &mut wavy, time);
        let mut e = gpu.device.create_command_encoder(&Default::default());
        mask.encode(&mut e, Some(&wavy), None, plan.pixels).unwrap();
        gpu.queue.submit([e.finish()]);
        let hits = masked(&gpu, &inputs);
        let color = bytes(&gpu, wavy.resolved_texture(), 8);
        for (p, hit) in hits.iter().enumerate() {
            assert_eq!(
                *hit,
                half(&color, p, 0) > 0.0001,
                "wave/time{time} pixel{p}"
            );
        }
    }
}
fn mixed_depth(gpu: &render::Gpu, inputs: &Inputs) {
    let source = "@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}@fragment fn fragment(@builtin(position)p:vec4<f32>)->@builtin(frag_depth)f32{let hole=u32(p.x)==50u&&u32(p.y)==8u;return select(.95,.005,p.x<33.||hole);}";
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("independent mixed near/far opaque planes"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mixed foreground clips cloud transport"),
            layout: None,
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
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
    let mut e = gpu.device.create_command_encoder(&Default::default());
    let depth = inputs.depth.create_view(&Default::default());
    {
        let mut pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("asymmetric mixed depth boundary including sub-lowres hole"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    gpu.queue.submit([e.finish()]);
}
#[test]
fn cloud_actual_layer_and_volume_transport_masks_visible_clouds_but_not_hidden_or_clear() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let inputs = Inputs::new(&gpu);
    for quality in [CloudQuality::LayerA, CloudQuality::VolumeB] {
        for (depth, density, expected) in [
            (0.005f32, 1., Some(true)),
            (0.95, 1., Some(false)),
            (0.005, 0., Some(false)),
            (-1., 1., None),
        ] {
            inputs.clear(&gpu, depth.max(0.));
            if depth < 0. {
                mixed_depth(&gpu, &inputs);
            }
            let mut cloud = CloudGpu::new_with_samples(
                &gpu.device,
                &gpu.queue,
                CloudConfig {
                    quality,
                    bottom: 4.,
                    thickness: 2.,
                    density: CloudDensity::Uniform(density),
                    extinction: 0.4,
                    anisotropy: 0.,
                    ..Default::default()
                },
                W,
                H,
                CloudInputs {
                    transmittance: &inputs.sky.create_view(&Default::default()),
                    sky: &inputs.sky.create_view(&Default::default()),
                    scene_color_msaa: &inputs.color.create_view(&Default::default()),
                    scene_depth_msaa: &inputs.depth.create_view(&Default::default()),
                },
                1,
            )
            .unwrap();
            let mask = TemporalOpticsMask::new(
                &gpu.device,
                &gpu.queue,
                true,
                None,
                Some(&cloud),
                &inputs.motion.create_view(&Default::default()),
                &inputs.surface.create_view(&Default::default()),
            )
            .unwrap()
            .unwrap();
            let plan = mask.pending_work(None, Some(&cloud)).unwrap();
            assert_eq!(plan.pixels, u64::from(W * H));
            assert_eq!(plan.texture_loads_bound, u64::from(W * H) * 10);
            let mut e = gpu.device.create_command_encoder(&Default::default());
            cloud
                .encode(
                    &gpu.queue,
                    &mut e,
                    CloudState {
                        camera: Camera::perspective(
                            [8., 10., 8.],
                            [8., 0., 8.],
                            [0., 0., -1.],
                            0.6,
                            2.,
                            0.1,
                            32.,
                        ),
                        eye: [8., 10., 8.],
                        time_seconds: 0.,
                        sun: [0., 1., 0.],
                        sun_intensity: 0.,
                        scene_revision: 1,
                    },
                    &mut UploadBudget::new(10000),
                    10_000_000,
                )
                .unwrap();
            mask.encode(&mut e, None, Some(&cloud), plan.pixels)
                .unwrap();
            gpu.queue.submit([e.finish()]);
            let bits = masked(&gpu, &inputs);
            if let Some(expected) = expected {
                assert!(
                    bits.iter().all(|hit| *hit == expected),
                    "{quality:?} depth{depth} density{density}"
                );
            } else {
                let count = bits.iter().filter(|v| **v).count();
                assert!(count > 512 && count < (W * H - 512) as usize);
                assert!(
                    !bits[50 + 8 * W as usize],
                    "unrepresented far hole must use same composite sum fallback"
                );
            }
            let color = bytes(&gpu, cloud.resolved_texture(), 8);
            for (p, hit) in bits.iter().enumerate() {
                assert_eq!(
                    half(&color, p, 0) > 0.0001,
                    *hit,
                    "color-composite coverage must agree with actual transport"
                );
            }
            assert_eq!(mask.pipeline_compilations(), 1);
            assert_eq!(mask.graph().count, 1);
            assert_eq!(mask.target_bytes(), 0);
        }
    }
}
