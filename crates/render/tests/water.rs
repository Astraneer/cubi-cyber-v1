#![cfg(not(target_arch = "wasm32"))]
use render::{
    terrain_gpu::{Camera, UploadBudget, translation},
    terrain_mesh::Face,
    water_gpu::{WaterConfig, WaterDraw, WaterError, WaterGpu, WaterInputs, WaterQuad, WaterState},
};
const SIZE: u32 = 32;
#[path = "common/msaa.rs"]
mod msaa;
fn read_buffer(gpu: &render::Gpu, b: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let out = b.slice(..).get_mapped_range().to_vec();
    b.unmap();
    out
}
fn half(h: u16) -> f32 {
    let sign = if h & 0x8000 == 0 { 1. } else { -1. };
    let e = (h >> 10) & 31;
    let m = h & 1023;
    if e == 0 {
        sign * f32::from(m) * 2f32.powi(-24)
    } else {
        sign * (1. + f32::from(m) / 1024.) * 2f32.powi(i32::from(e) - 15)
    }
}
fn image(gpu: &render::Gpu, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    let dimensions = texture.size();
    let width = dimensions.width;
    let height = dimensions.height;
    let row = (width * 8).div_ceil(256) * 256;
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual HDR cloud readback"),
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
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
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read_buffer(gpu, &b)
        .chunks_exact(row as usize)
        .flat_map(|r| {
            r[..(width * 8) as usize].chunks_exact(8).map(|p| {
                std::array::from_fn(|c| {
                    half(u16::from_le_bytes(p[c * 2..c * 2 + 2].try_into().unwrap()))
                })
            })
        })
        .collect()
}
fn stats(gpu: &render::Gpu, cloud: &WaterGpu) -> [u32; 4] {
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual cloud work count readback"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(cloud.statistics_buffer(), 0, &b, 0, 16);
    gpu.queue.submit([encoder.finish()]);
    let b = read_buffer(gpu, &b);
    std::array::from_fn(|i| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap()))
}
struct Sources {
    _color: wgpu::Texture,
    color: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth: wgpu::TextureView,
    _sky: wgpu::Texture,
    sky: wgpu::TextureView,
}
fn camera() -> Camera {
    Camera::perspective(
        [8., 12., 8.],
        [8., 0., 8.],
        [0., 0., -1.],
        0.2,
        1.,
        0.1,
        100.,
    )
}
fn sources(gpu: &render::Gpu, known_bottom: bool, foreground_mask: u32) -> Sources {
    sources_size(gpu, known_bottom, foreground_mask, SIZE)
}
fn sources_size(gpu: &render::Gpu, known_bottom: bool, foreground_mask: u32, size: u32) -> Sources {
    let texture = |label, format| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 4,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    let color_texture = texture(
        "independent known white floor sample radiance",
        wgpu::TextureFormat::Rgba16Float,
    );
    let color = color_texture.create_view(&Default::default());
    let depth_texture = texture(
        "independent known floor or Unknown sentinel",
        wgpu::TextureFormat::Depth32Float,
    );
    let depth = depth_texture.create_view(&Default::default());
    let sky_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("zero sky radiance analytic water oracle"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &sky_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&[0f32; 4]),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(16),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let sky = sky_texture.create_view(&Default::default());
    let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("red foreground independent samplemask"),source:wgpu::ShaderSource::Wgsl(format!("@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}}struct Out{{@location(0)color:vec4<f32>,@builtin(frag_depth)depth:f32,@builtin(sample_mask)mask:u32}};@fragment fn fragment()->Out{{var o:Out;o.color=vec4(1.,0.,0.,1.);o.depth=0.019019019;o.mask={foreground_mask}u;return o;}}").into())});
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("opaque foreground sample oracle"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
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
                count: 4,
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
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("real MSAA4 opaque scene before water"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(if known_bottom {
                        0.1 * (100. - 16.) / (99.9 * 16.)
                    } else {
                        0.
                    }),
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
    gpu.queue.submit([encoder.finish()]);
    Sources {
        _color: color_texture,
        color,
        _depth: depth_texture,
        depth,
        _sky: sky_texture,
        sky,
    }
}
fn inputs(s: &Sources) -> WaterInputs<'_> {
    WaterInputs {
        color_msaa: &s.color,
        depth_msaa: &s.depth,
        sky: &s.sky,
        transmittance: &s.sky,
    }
}
fn config() -> WaterConfig {
    WaterConfig {
        max_objects: 2,
        max_quads: 8,
        absorption: [0.1, 0.2, 0.3],
        turbidity: 0.,
        scattering: [0.; 3],
        wave_amplitude: 0.,
        refraction_scale: 0.,
        reflection_steps: 0,
        instrument: true,
        ..WaterConfig::default()
    }
}
fn state() -> WaterState {
    WaterState {
        camera: camera(),
        eye: [8., 12., 8.],
        time_seconds: 0.,
        sun: [0., 1., 0.],
        sun_intensity: 0.,
        underwater: false,
        scene_revision: 1,
    }
}
fn draw(
    gpu: &render::Gpu,
    water: &mut WaterGpu,
    state: WaterState,
) -> render::water_gpu::WaterWork {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = water
        .encode(
            &gpu.queue,
            &mut encoder,
            state,
            &mut UploadBudget::new(256),
            u64::MAX,
        )
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    work
}
fn populate(gpu: &render::Gpu, water: &mut WaterGpu) -> WaterDraw {
    let object = water
        .upload_object(
            &gpu.queue,
            None,
            translation([0.; 3]),
            &mut UploadBudget::new(80),
        )
        .unwrap();
    let quad = WaterQuad::new(
        [0, 0, 16],
        Face::Up,
        [16; 2],
        [1.; 4],
        [0.; 2],
        u32::MAX,
        0.,
    )
    .unwrap();
    let record = WaterDraw { quad, object };
    assert_eq!(std::mem::size_of::<WaterQuad>(), 16);
    assert_eq!(
        water
            .upload_quads(&gpu.queue, &[record], &mut UploadBudget::new(16))
            .unwrap()
            .upload_bytes,
        16
    );
    record
}
#[test]
fn staged_object_flags_publish_atomically_and_never_expose_failed_or_reused_ranges() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let s = sources(&gpu, true, 0);
    let mut water = WaterGpu::new(
        &gpu.device,
        &gpu.queue,
        WaterConfig {
            max_quads: 2,
            ..config()
        },
        SIZE,
        SIZE,
        inputs(&s),
        None,
    )
    .unwrap();
    draw(&gpu, &mut water, state());
    let opaque = image(&gpu, water.resolved_texture());
    let white = WaterQuad::new(
        [0, 0, 16],
        Face::Up,
        [16; 2],
        [1.; 4],
        [0.; 2],
        u32::MAX,
        0.,
    )
    .unwrap();
    let red = WaterQuad::new(
        [0, 0, 16],
        Face::Up,
        [16; 2],
        [1.; 4],
        [0.; 2],
        0xff0000ff,
        0.,
    )
    .unwrap();
    let old = water
        .upload_object(
            &gpu.queue,
            None,
            translation([0.; 3]),
            &mut UploadBudget::new(80),
        )
        .unwrap();
    water
        .upload_page_quads(&gpu.queue, old, &[white], &mut UploadBudget::new(24))
        .unwrap();
    draw(&gpu, &mut water, state());
    let previous = image(&gpu, water.resolved_texture());
    assert_ne!(previous, opaque);
    let staged = water
        .upload_object_inactive(
            &gpu.queue,
            None,
            translation([0.; 3]),
            &mut UploadBudget::new(80),
        )
        .unwrap();
    assert!(matches!(
        water.upload_page_quads(&gpu.queue, staged, &[red, red], &mut UploadBudget::new(40)),
        Err(WaterError::Capacity)
    ));
    water
        .upload_object(
            &gpu.queue,
            Some(staged),
            translation([0.25, 0., 0.]),
            &mut UploadBudget::new(80),
        )
        .unwrap();
    draw(&gpu, &mut water, state());
    assert_eq!(
        image(&gpu, water.resolved_texture()),
        previous,
        "failed staged range and later transform stay hidden"
    );
    let revision = water.revision();
    let mut denied = UploadBudget::new(23);
    assert!(matches!(
        water.upload_page_quads(&gpu.queue, staged, &[red], &mut denied),
        Err(WaterError::UploadBudget)
    ));
    assert_eq!((denied.remaining_bytes, water.revision()), (23, revision));
    water
        .upload_page_quads(&gpu.queue, staged, &[red], &mut UploadBudget::new(24))
        .unwrap();
    draw(&gpu, &mut water, state());
    assert_eq!(
        image(&gpu, water.resolved_texture()),
        previous,
        "ready replacement is not published early"
    );
    let _ = water.take_work();
    let revision = water.revision();
    let mut denied = UploadBudget::new(7);
    assert_eq!(
        water.set_enabled_batch(&gpu.queue, &[(old, false), (staged, true)], &mut denied),
        Err(WaterError::UploadBudget)
    );
    assert_eq!((denied.remaining_bytes, water.revision()), (7, revision));
    assert_eq!(
        water.set_enabled_batch(
            &gpu.queue,
            &[(old, false), (old, true)],
            &mut UploadBudget::new(8)
        ),
        Err(WaterError::InvalidState)
    );
    draw(&gpu, &mut water, state());
    assert_eq!(image(&gpu, water.resolved_texture()), previous);
    assert_eq!(
        water
            .set_enabled_batch(
                &gpu.queue,
                &[(old, false), (staged, true)],
                &mut UploadBudget::new(8)
            )
            .unwrap(),
        8
    );
    let work = water.take_work();
    assert_eq!((work.upload_bytes, work.quad_uploads), (8, 0));
    draw(&gpu, &mut water, state());
    let replacement = image(&gpu, water.resolved_texture());
    assert!(
        replacement
            .iter()
            .zip(&previous)
            .filter(|(a, b)| a != b)
            .count()
            > 100
    );
    let revision = water.revision();
    assert_eq!(
        water
            .set_enabled_batch(
                &gpu.queue,
                &[(old, false), (staged, true)],
                &mut UploadBudget::new(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(water.revision(), revision);
    water
        .upload_page_quads(&gpu.queue, old, &[], &mut UploadBudget::new(8))
        .unwrap();
    water
        .release_object(&gpu.queue, old, &mut UploadBudget::new(4))
        .unwrap();
    let reused = water
        .upload_object_inactive(
            &gpu.queue,
            None,
            translation([0.; 3]),
            &mut UploadBudget::new(80),
        )
        .unwrap();
    assert_eq!(old.slot, reused.slot);
    assert!(reused.generation > old.generation);
    assert_eq!(
        water.set_enabled_batch(
            &gpu.queue,
            &[(staged, false), (old, true)],
            &mut UploadBudget::new(8)
        ),
        Err(WaterError::StaleHandle)
    );
    draw(&gpu, &mut water, state());
    assert_eq!(
        image(&gpu, water.resolved_texture()),
        replacement,
        "stale generation cannot partially hide the current cover"
    );
    water
        .set_enabled_batch(
            &gpu.queue,
            &[(staged, false), (reused, true)],
            &mut UploadBudget::new(8),
        )
        .unwrap();
    draw(&gpu, &mut water, state());
    assert_eq!(
        image(&gpu, water.resolved_texture()),
        opaque,
        "reused object cannot resurrect retired quad spans"
    );
    println!(
        "M10_STAGED_WATER_GPU_OK inactive_transform_hidden=true capacity_retry_hidden=true denied7B_atomic=true publish8B_noquads=true duplicate_rejected=true stale_batch_atomic=true reused_slot_no_ghost=true"
    );
}
#[test]
fn physical_absorption_fresnel_match_independent_f64_and_preserve_msaa() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        for mask in [0, 5] {
            let s = sources(&gpu, true, mask);
            let mut water = WaterGpu::new(
                &gpu.device,
                &gpu.queue,
                config(),
                SIZE,
                SIZE,
                inputs(&s),
                None,
            )
            .unwrap();
            let record = populate(&gpu, &mut water);
            let work = draw(&gpu, &mut water, state());
            assert_eq!(work.cpu_vertices, 0);
            let actual = image(&gpu, water.resolved_texture());
            let mut max_error = 0f64;
            for y in 0..SIZE {
                for x in 0..SIZE {
                    let ndc = [
                        (x as f64 + 0.5) / f64::from(SIZE) * 2. - 1.,
                        (y as f64 + 0.5) / f64::from(SIZE) * 2. - 1.,
                    ];
                    let ray_length =
                        (1. + (ndc[0] * ndc[0] + ndc[1] * ndc[1]) * 0.1f64.tan().powi(2)).sqrt();
                    let cosine = 1. / ray_length;
                    let transmitted_cosine =
                        (1. - (1. - cosine * cosine) / (1.333f64 * 1.333)).sqrt();
                    let rs = (cosine - 1.333 * transmitted_cosine)
                        / (cosine + 1.333 * transmitted_cosine);
                    let rp = (transmitted_cosine - 1.333 * cosine)
                        / (transmitted_cosine + 1.333 * cosine);
                    let fresnel = (rs * rs + rp * rp) * 0.5;
                    let thickness = 4. * ray_length;
                    let p = actual[(x + SIZE * y) as usize];
                    for c in 0..3 {
                        let behind = (-[0.1, 0.2, 0.3][c] * thickness).exp() * (1. - fresnel);
                        let expected = if mask == 0 {
                            behind
                        } else {
                            behind * 0.5 + if c == 0 { 0.5 } else { 0. }
                        };
                        max_error = max_error.max((f64::from(p[c]) - expected).abs());
                    }
                    assert_eq!(p[3], 1.);
                }
            }
            assert!(
                max_error < 0.003,
                "water Beer/Fresnel oracle error {max_error}"
            );
            let counters = stats(&gpu, &water);
            assert!(counters[0] >= SIZE * SIZE * 2);
            assert_eq!(counters[1], 0);
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            assert_eq!(
                water
                    .encode(
                        &gpu.queue,
                        &mut encoder,
                        state(),
                        &mut UploadBudget::new(0),
                        0
                    )
                    .unwrap()
                    .passes,
                0
            );
            assert_eq!(
                water.release_object(&gpu.queue, record.object, &mut UploadBudget::new(4)),
                Err(WaterError::ObjectInUse)
            );
            println!(
                "M10 water16Bquad mask{mask} BeerFresnelError{max_error} actualFragments{} MSAApreserved",
                counters[0]
            );
        }
    });
}
#[test]
fn incomplete_physical_cover_preserves_opaque_samples_and_retains_its_arena() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let s = sources(&gpu, true, 5);
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            config(),
            SIZE,
            SIZE,
            inputs(&s),
            None,
        )
        .unwrap();
        populate(&gpu, &mut water);
        water.set_surface_enabled(false);
        assert_eq!(water.worst_reflection_steps(), 0);
        draw(&gpu, &mut water, state());
        let fallback = image(&gpu, water.resolved_texture());
        assert!(fallback.iter().all(|pixel| *pixel == [1., 0.5, 0.5, 1.]));
        assert_eq!(stats(&gpu, &water)[0], 0);
        assert_eq!(water.active_quad_count(), 1);
        water.set_surface_enabled(true);
        draw(&gpu, &mut water, state());
        let physical = image(&gpu, water.resolved_texture());
        assert!(physical.iter().zip(&fallback).all(|(a, b)| a != b));
        assert!(stats(&gpu, &water)[0] > 0);
        water.set_surface_enabled(false);
        draw(&gpu, &mut water, state());
        assert_eq!(image(&gpu, water.resolved_texture()), fallback);
        assert_eq!(water.active_quad_count(), 1);
    });
}
#[test]
fn underwater_interface_has_the_independent_snell_critical_angle_and_no_transmission_above_it() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let s = sources(&gpu, false, 0);
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &s._sky,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&[1f32; 4]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            WaterConfig {
                absorption: [0.; 3],
                ..config()
            },
            SIZE,
            SIZE,
            inputs(&s),
            None,
        )
        .unwrap();
        populate(&gpu, &mut water);
        let eye = [8., -2., 8.];
        let camera = Camera::perspective(eye, [11., 1., 8.], [0., 0., -1.], 1., 1., 0.1, 100.);
        let state = WaterState {
            camera,
            eye,
            underwater: true,
            ..state()
        };
        draw(&gpu, &mut water, state);
        let actual = image(&gpu, water.resolved_texture());
        let inverse = camera.inverse_view_projection().unwrap();
        let samples = msaa::locations(&gpu);
        let mut max_error = 0f64;
        let mut transmitted = 0;
        let mut totally_reflected = Vec::new();
        for y in 0..SIZE {
            for x in 0..SIZE {
                let mut expected = 0.;
                let mut valid = true;
                let mut all_tir = true;
                for sample in samples {
                    let q = transform(
                        inverse,
                        [
                            (f64::from(x) + sample[0]) / f64::from(SIZE) * 2. - 1.,
                            1. - (f64::from(y) + sample[1]) / f64::from(SIZE) * 2.,
                            0.,
                            1.,
                        ],
                    );
                    let ray = [q[0] / q[3] - 8., q[1] / q[3] + 2., q[2] / q[3] - 8.];
                    let length = ray.iter().map(|v| v * v).sum::<f64>().sqrt();
                    let direction = ray.map(|v| v / length);
                    let distance = 2. / direction[1];
                    let surface = [8. + direction[0] * distance, 8. + direction[2] * distance];
                    if direction[1] <= 0. || surface.iter().any(|v| *v < 0.1 || *v > 15.9) {
                        valid = false;
                        break;
                    }
                    let cosine = direction[1];
                    let sine2 = 1.333f64.powi(2) * (1. - cosine * cosine);
                    if sine2 >= 1. {
                        continue;
                    }
                    all_tir = false;
                    let transmitted_cosine = (1. - sine2).sqrt();
                    let rs = (1.333 * cosine - transmitted_cosine)
                        / (1.333 * cosine + transmitted_cosine);
                    let rp = (1.333 * transmitted_cosine - cosine)
                        / (1.333 * transmitted_cosine + cosine);
                    expected += (1. - (rs * rs + rp * rp) * 0.5) * 0.25;
                }
                if !valid {
                    continue;
                }
                let index = (x + SIZE * y) as usize;
                max_error = max_error.max((f64::from(actual[index][0]) - expected).abs());
                if all_tir {
                    totally_reflected.push(index);
                    assert_eq!(
                        actual[index],
                        [0., 0., 0., 1.],
                        "TIR pixel{x},{y}, samples{samples:?}"
                    );
                } else {
                    transmitted += 1;
                }
            }
        }
        assert!(transmitted > 100 && totally_reflected.len() > 100);
        assert!(
            max_error < 0.003,
            "Snell/Fresnel 4-sample oracle error {max_error}"
        );
        draw(
            &gpu,
            &mut water,
            WaterState {
                underwater: false,
                ..state
            },
        );
        let wrong_medium = image(&gpu, water.resolved_texture());
        assert!(
            totally_reflected
                .iter()
                .all(|&i| wrong_medium[i][0] > 0.005),
            "air→water negative control must miss total internal reflection"
        );
        println!(
            "M10 underwater Snell criticalAngle={}deg transmitted={transmitted} totalInternal={} HDRerror={max_error} wrongMediumNegative=true",
            (1f64 / 1.333).asin().to_degrees(),
            totally_reflected.len()
        );
    });
}
#[test]
fn unknown_bottom_never_leaks_fake_floor_and_stale_or_budget_updates_are_atomic() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let s = sources(&gpu, false, 0);
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            WaterConfig {
                reflection_steps: 8,
                ..config()
            },
            SIZE,
            SIZE,
            inputs(&s),
            None,
        )
        .unwrap();
        let record = populate(&gpu, &mut water);
        draw(&gpu, &mut water, state());
        let baseline = image(&gpu, water.resolved_texture());
        assert!(
            baseline.iter().all(|p| *p == [0., 0., 0., 1.]),
            "Unknown depth became a visible white floor"
        );
        let counters = stats(&gpu, &water);
        assert_eq!(counters[2], 0);
        assert!(counters[3] > 0);
        assert!(counters[1] <= counters[0] * 8);
        let mut changed = record;
        changed.quad = WaterQuad::new(
            [0, 0, 16],
            Face::Up,
            [16; 2],
            [0.5; 4],
            [1., 0.],
            u32::MAX,
            0.,
        )
        .unwrap();
        assert_eq!(
            water.upload_quads(&gpu.queue, &[changed], &mut UploadBudget::new(15)),
            Err(WaterError::UploadBudget)
        );
        draw(&gpu, &mut water, state());
        assert_eq!(image(&gpu, water.resolved_texture()), baseline);
        water
            .upload_quads(&gpu.queue, &[], &mut UploadBudget::new(0))
            .unwrap();
        water
            .release_object(&gpu.queue, record.object, &mut UploadBudget::new(4))
            .unwrap();
        let new = water
            .upload_object(
                &gpu.queue,
                None,
                translation([0.; 3]),
                &mut UploadBudget::new(80),
            )
            .unwrap();
        assert_eq!(new.slot, record.object.slot);
        assert_ne!(new.generation, record.object.generation);
        assert_eq!(
            water.upload_quads(&gpu.queue, &[record], &mut UploadBudget::new(16)),
            Err(WaterError::StaleHandle)
        );
        println!(
            "M10 unknownBottom opaque conservative, SSRsteps{} <=fragments{}*8 hits0 fallback{};16Bbudgetatomic+ABAguard",
            counters[1], counters[0], counters[3]
        );
    });
}
fn reflection_scene(gpu: &render::Gpu, camera: Camera) -> Sources {
    let s = sources(gpu, false, 0);
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &s._sky,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&[0f32, 0., 1., 1.]),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(16),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent physical scene camera"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    gpu.queue
        .write_buffer(&uniform, 0, bytemuck::cast_slice(&camera.view_projection));
    let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("actual green floor and red reflection wall"),source:wgpu::ShaderSource::Wgsl("struct Camera{vp:mat4x4<f32>};@group(0)@binding(0)var<uniform>camera:Camera;struct Out{@builtin(position)p:vec4<f32>,@location(0)color:vec3<f32>};@vertex fn vertex(@builtin(vertex_index)i:u32)->Out{let points=array<vec3<f32>,12>(vec3(-40.,-2.,40.),vec3(40.,-2.,40.),vec3(-40.,-2.,-40.),vec3(-40.,-2.,-40.),vec3(40.,-2.,40.),vec3(40.,-2.,-40.),vec3(0.,0.,0.),vec3(16.,0.,0.),vec3(0.,16.,0.),vec3(0.,16.,0.),vec3(16.,0.,0.),vec3(16.,16.,0.));var o:Out;o.p=camera.vp*vec4(points[i],1.);o.color=select(vec3(0.,1.,0.),vec3(1.,0.,0.),i>=6u);return o;}@fragment fn fragment(o:Out)->@location(0)vec4<f32>{return vec4(o.color,1.);}".into())});
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("independent opaque scene for SSR oracle"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
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
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: 4,
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
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("independent scene camera bind"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        }],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("opaque floor wall actual known reverse depth"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &s.color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.,
                        g: 0.,
                        b: 1.,
                        a: 1.,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &s.depth,
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
        pass.draw(0..12, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
    s
}
fn transform(m: [[f32; 4]; 4], v: [f64; 4]) -> [f64; 4] {
    std::array::from_fn(|r| (0..4).map(|c| f64::from(m[c][r]) * v[c]).sum())
}
#[test]
fn bounded_ssr_hits_actual_red_wall_certified_by_independent_reflected_rays() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let camera =
            Camera::perspective([8., 6., 24.], [8., 0., 8.], [0., 1., 0.], 1., 1., 0.1, 100.);
        let s = reflection_scene(&gpu, camera);
        for (steps, range, tolerance) in [(32, 16., 0.75), (64, 32., 1.2)] {
            let config = WaterConfig {
                absorption: [0.; 3],
                reflection_steps: steps,
                reflection_distance: range,
                reflection_tolerance: tolerance,
                ..config()
            };
            let mut water = WaterGpu::new(
                &gpu.device,
                &gpu.queue,
                config,
                SIZE,
                SIZE,
                inputs(&s),
                None,
            )
            .unwrap();
            populate(&gpu, &mut water);
            let current = WaterState {
                camera,
                eye: [8., 6., 24.],
                ..state()
            };
            draw(&gpu, &mut water, current);
            let actual = image(&gpu, water.resolved_texture());
            let counters = stats(&gpu, &water);
            assert!(
                counters[2] > 20,
                "SSR never sampled known reflected geometry: {counters:?}"
            );
            assert!(u64::from(counters[1]) <= water.worst_reflection_steps());
            let inverse = camera.inverse_view_projection().unwrap();
            let mut certified = 0;
            let mut reflected = 0;
            for y in 0..SIZE {
                for x in 0..SIZE {
                    let q = transform(
                        inverse,
                        [
                            (f64::from(x) + 0.5) / f64::from(SIZE) * 2. - 1.,
                            1. - (f64::from(y) + 0.5) / f64::from(SIZE) * 2.,
                            0.,
                            1.,
                        ],
                    );
                    let v = [q[0] / q[3] - 8., q[1] / q[3] - 6., q[2] / q[3] - 24.];
                    let length = (v.iter().map(|x| x * x).sum::<f64>()).sqrt();
                    let ray = v.map(|x| x / length);
                    if ray[1] >= 0. {
                        continue;
                    }
                    let t = -6. / ray[1];
                    let surface = [8. + ray[0] * t, 0., 24. + ray[2] * t];
                    if surface[0] < 1. || surface[0] > 15. || surface[2] < 1. || surface[2] > 15. {
                        continue;
                    }
                    let r = [ray[0], -ray[1], ray[2]];
                    if r[2] >= 0. {
                        continue;
                    }
                    let distance = -surface[2] / r[2];
                    let wall = [surface[0] + r[0] * distance, r[1] * distance, 0.];
                    if distance > f64::from(config.reflection_distance) - 2.
                        || wall[0] < 1.
                        || wall[0] > 15.
                        || wall[1] < 1.
                        || wall[1] > 15.
                    {
                        continue;
                    }
                    let clip = transform(camera.view_projection, [wall[0], wall[1], wall[2], 1.]);
                    if clip[3] <= 0. {
                        continue;
                    }
                    let screen = [clip[0] / clip[3] * 0.5 + 0.5, 0.5 - clip[1] / clip[3] * 0.5];
                    if screen
                        .iter()
                        .any(|v| *v < 2. / f64::from(SIZE) || *v > 1. - 2. / f64::from(SIZE))
                    {
                        continue;
                    }
                    certified += 1;
                    if actual[(x + SIZE * y) as usize][0] > 0.005 {
                        reflected += 1;
                    }
                }
            }
            assert!(certified > 30);
            assert!(
                reflected == certified,
                "independent reflected wall witnesses missing {reflected}/{certified}"
            );
            let _ = water.take_work();
            water
                .resize_inputs(&gpu.device, SIZE, SIZE, inputs(&s))
                .unwrap();
            let resize_work = water.take_work();
            assert_eq!(
                (resize_work.pipeline_compilations, resize_work.upload_bytes),
                (0, 0)
            );
            draw(&gpu, &mut water, current);
            assert_eq!(
                image(&gpu, water.resolved_texture()),
                actual,
                "resized deferred water input bindings must retain the same image"
            );
            println!(
                "M10 SSR quality steps={steps} range={range} actual wall rays{reflected}/{certified} counters fragments{} steps{} hits{} fallback{} resizeBITWISE=true",
                counters[0], counters[1], counters[2], counters[3]
            );
        }
    });
}
fn verify_water_timestamps(gpu: &render::Gpu, water: &WaterGpu) {
    let graph = water.graph();
    assert!(graph.count > 0 && graph.count <= 3);
    assert!(
        graph.kinds[..graph.count as usize]
            .iter()
            .all(|k| (1..=3).contains(k))
    );
    if let Some((_, period)) = water.timestamp_buffer() {
        assert!(period > 0.);
        let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual water GPU interval oracle"),
            size: u64::from(graph.count) * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        water.copy_timestamps_into(&mut encoder, &b, 0);
        gpu.queue.submit([encoder.finish()]);
        let bytes = read_buffer(gpu, &b);
        for p in bytes.chunks_exact(16) {
            let start = u64::from_le_bytes(p[..8].try_into().unwrap());
            let end = u64::from_le_bytes(p[8..].try_into().unwrap());
            assert!(end >= start);
        }
    }
}
#[test]
fn water_cloud_post_graph_and_resize_reuse_resources_without_pipeline_compilation() {
    pollster::block_on(async {
        use render::{
            OutputTransfer,
            cloud_gpu::{CloudConfig, CloudDensity, CloudGpu, CloudInputs, CloudState},
            post::{Exposure, PostConfig, PostGpu},
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let s = sources(&gpu, true, 0);
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            config(),
            SIZE,
            SIZE,
            inputs(&s),
            None,
        )
        .unwrap();
        populate(&gpu, &mut water);
        water.take_work();
        let config = CloudConfig {
            downsample: 1,
            bottom: 4.,
            thickness: 4.,
            extinction: 0.05,
            sky_strength: 0.,
            max_distance: 100.,
            density: CloudDensity::Uniform(1.),
            ..CloudConfig::default()
        };
        let mut clouds = CloudGpu::new(
            &gpu.device,
            &gpu.queue,
            config,
            SIZE,
            SIZE,
            CloudInputs {
                transmittance: &s.sky,
                sky: &s.sky,
                scene_color_msaa: water.color_msaa(),
                scene_depth_msaa: water.depth_msaa(),
            },
        )
        .unwrap();
        let mut post = PostGpu::new(
            &gpu.device,
            &gpu.queue,
            clouds.resolved_view(),
            SIZE,
            SIZE,
            wgpu::TextureFormat::Rgba8Unorm,
            OutputTransfer::Attachment,
            PostConfig {
                exposure: Exposure::Manual(1.),
                bloom: None,
                histogram_stride: 1,
            },
            &mut UploadBudget::new(10000),
        )
        .unwrap();
        let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("water cloud post complete actual graph"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let bytes = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("full effects final tone oracle"),
            size: 256 * u64::from(SIZE),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let water_work = water
            .encode(
                &gpu.queue,
                &mut encoder,
                state(),
                &mut UploadBudget::new(256),
                0,
            )
            .unwrap();
        let cloud_work = clouds
            .encode(
                &gpu.queue,
                &mut encoder,
                CloudState {
                    camera: camera(),
                    eye: [8., 12., 8.],
                    time_seconds: 0.,
                    sun: [0., 1., 0.],
                    sun_intensity: 0.,
                    scene_revision: 1,
                },
                &mut UploadBudget::new(176),
                100000,
            )
            .unwrap();
        let post_work = post
            .encode(
                &mut encoder,
                &output.create_view(&Default::default()),
                1,
                &mut 100000,
            )
            .unwrap();
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &bytes,
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
        let final_bytes = read_buffer(&gpu, &bytes);
        let hdr = image(&gpu, clouds.resolved_texture());
        let mut hdr_error = 0f64;
        let mut byte_error = 0i32;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let ndc = [
                    (f64::from(x) + 0.5) / f64::from(SIZE) * 2. - 1.,
                    (f64::from(y) + 0.5) / f64::from(SIZE) * 2. - 1.,
                ];
                let ray_length =
                    (1. + (ndc[0] * ndc[0] + ndc[1] * ndc[1]) * 0.1f64.tan().powi(2)).sqrt();
                let cos = 1. / ray_length;
                let ct = (1. - (1. - cos * cos) / (1.333f64 * 1.333)).sqrt();
                let f = 0.5
                    * (((cos - 1.333 * ct) / (cos + 1.333 * ct)).powi(2)
                        + ((ct - 1.333 * cos) / (ct + 1.333 * cos)).powi(2));
                for c in 0..3 {
                    let expected = (-[0.1, 0.2, 0.3][c] * 4. * ray_length).exp()
                        * (1. - f)
                        * (-0.05 * 4. * ray_length).exp();
                    hdr_error = hdr_error
                        .max((f64::from(hdr[(x + y * SIZE) as usize][c]) - expected).abs());
                    let mapped = (expected * (2.51 * expected + 0.03)
                        / (expected * (2.43 * expected + 0.59) + 0.14))
                        .clamp(0., 1.);
                    let actual = i32::from(final_bytes[(y * 256 + x * 4) as usize + c]);
                    byte_error = byte_error.max((actual - (mapped * 255.).round() as i32).abs());
                }
            }
        }
        assert!(hdr_error < 0.002);
        assert!(byte_error <= 2);
        assert_eq!(water_work.passes + cloud_work.passes + post_work.passes, 6);
        assert_eq!(water.graph().count, 3);
        assert_eq!(clouds.graph().count, 2);
        verify_water_timestamps(&gpu, &water);
        let resized = sources_size(&gpu, true, 0, 16);
        water
            .resize_inputs(&gpu.device, 16, 16, inputs(&resized))
            .unwrap();
        clouds
            .resize_inputs(
                &gpu.device,
                16,
                16,
                CloudInputs {
                    transmittance: &resized.sky,
                    sky: &resized.sky,
                    scene_color_msaa: water.color_msaa(),
                    scene_depth_msaa: water.depth_msaa(),
                },
            )
            .unwrap();
        post.resize_input(
            &gpu.device,
            &gpu.queue,
            clouds.resolved_view(),
            16,
            16,
            &mut UploadBudget::new(4096),
        )
        .unwrap();
        assert_eq!(water.take_work().pipeline_compilations, 0);
        assert_eq!(water.graph().count, 0);
        assert_eq!(clouds.graph().count, 0);
        assert_eq!(water.resolved_texture().size().width, 16);
        let work = draw(&gpu, &mut water, state());
        assert_eq!(work.passes, 3);
        assert_eq!(image(&gpu, water.resolved_texture()).len(), 256);
        verify_water_timestamps(&gpu, &water);
        assert_eq!(water.take_work().quad_uploads, 0);
        println!(
            "M10 actualWater→Cloud→Post6passes HDRerror{hdr_error} byteerror{byte_error}; realtimestamps; resize16reusespipelines+16Bquads"
        );
    });
}
#[test]
fn underwater_medium_attenuates_actual_opaque_depth_and_unknown_stays_scattering_only() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        for known in [true, false] {
            let s = sources(&gpu, known, 0);
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &s._sky,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&[1f32; 4]),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(16),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            let mut water = WaterGpu::new(
                &gpu.device,
                &gpu.queue,
                WaterConfig {
                    scattering: [0.2; 3],
                    ..config()
                },
                SIZE,
                SIZE,
                inputs(&s),
                None,
            )
            .unwrap();
            draw(
                &gpu,
                &mut water,
                WaterState {
                    underwater: true,
                    ..state()
                },
            );
            let actual = image(&gpu, water.resolved_texture());
            let mut error = 0f64;
            for y in 0..SIZE {
                for x in 0..SIZE {
                    let ndc = [
                        (f64::from(x) + 0.5) / f64::from(SIZE) * 2. - 1.,
                        (f64::from(y) + 0.5) / f64::from(SIZE) * 2. - 1.,
                    ];
                    let ray =
                        (1. + (ndc[0] * ndc[0] + ndc[1] * ndc[1]) * 0.1f64.tan().powi(2)).sqrt();
                    for c in 0..3 {
                        let t = if known {
                            (-[0.1, 0.2, 0.3][c] * 16. * ray).exp()
                        } else {
                            0.
                        };
                        let expected = t + (1. - t) * 0.2;
                        error = error
                            .max((f64::from(actual[(x + y * SIZE) as usize][c]) - expected).abs());
                    }
                }
            }
            assert!(error < 0.0015);
            println!(
                "M10 underwater known{known} independentmediumError{error} nofakeUnknownfloor"
            );
        }
    });
}
fn raw_quads(gpu: &render::Gpu, water: &WaterGpu) -> Vec<u8> {
    let size = water.quad_buffer().size();
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("water stable arena raw bytes oracle"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(water.quad_buffer(), 0, &b, 0, size);
    gpu.queue.submit([encoder.finish()]);
    read_buffer(gpu, &b)
}
#[test]
fn live_page_ranges_upload_only_changed_page_and_reclaim_without_stale_faces() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let s = sources(&gpu, true, 0);
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            config(),
            SIZE,
            SIZE,
            inputs(&s),
            None,
        )
        .unwrap();
        let a = water
            .upload_object(
                &gpu.queue,
                None,
                translation([0.; 3]),
                &mut UploadBudget::new(80),
            )
            .unwrap();
        let b = water
            .upload_object(
                &gpu.queue,
                None,
                translation([1000., 0., 0.]),
                &mut UploadBudget::new(80),
            )
            .unwrap();
        let q = WaterQuad::new(
            [0, 0, 16],
            Face::Up,
            [16; 2],
            [1.; 4],
            [0.; 2],
            u32::MAX,
            0.,
        )
        .unwrap();
        assert_eq!(
            water.upload_page_quads(&gpu.queue, a, &[q], &mut UploadBudget::new(23)),
            Err(WaterError::UploadBudget)
        );
        assert_eq!(water.active_quad_count(), 0);
        assert_eq!(
            water
                .upload_page_quads(&gpu.queue, a, &[q], &mut UploadBudget::new(24))
                .unwrap()
                .upload_bytes,
            24
        );
        water
            .upload_page_quads(&gpu.queue, b, &[q], &mut UploadBudget::new(24))
            .unwrap();
        assert_eq!(water.active_quad_count(), 2);
        draw(&gpu, &mut water, state());
        let initial = image(&gpu, water.resolved_texture());
        let before = raw_quads(&gpu, &water);
        let changed = WaterQuad::new(
            [0, 0, 16],
            Face::Up,
            [16; 2],
            [0.5; 4],
            [0.5, 0.],
            u32::MAX,
            0.,
        )
        .unwrap();
        assert_eq!(
            water
                .upload_page_quads(&gpu.queue, a, &[changed], &mut UploadBudget::new(16))
                .unwrap()
                .upload_bytes,
            16
        );
        draw(&gpu, &mut water, state());
        assert_ne!(initial, image(&gpu, water.resolved_texture()));
        let after = raw_quads(&gpu, &water);
        assert_ne!(&before[..16], &after[..16]);
        assert_eq!(
            &before[16..],
            &after[16..],
            "one fluid page rewrote an unrelated page"
        );
        water
            .upload_page_quads(&gpu.queue, a, &[], &mut UploadBudget::new(8))
            .unwrap();
        draw(&gpu, &mut water, state());
        assert!(
            image(&gpu, water.resolved_texture())
                .iter()
                .all(|p| *p == [1.; 4]),
            "retired page retained old water faces"
        );
        water
            .release_object(&gpu.queue, a, &mut UploadBudget::new(4))
            .unwrap();
        water
            .upload_page_quads(&gpu.queue, b, &[q, q], &mut UploadBudget::new(40))
            .unwrap();
        assert_eq!(water.active_quad_count(), 2);
        assert_eq!(
            water.upload_page_quads(&gpu.queue, a, &[q], &mut UploadBudget::new(24)),
            Err(WaterError::StaleHandle)
        );
        println!(
            "M10 livewater arena2pages onePageEdit16B unrelatedBytesBITWISEsame retire8B freesrange growReusesSpan ABArejected"
        );
    });
}
