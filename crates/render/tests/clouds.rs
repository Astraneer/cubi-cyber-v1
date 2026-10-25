#![cfg(not(target_arch = "wasm32"))]
use render::{
    cloud_gpu::{
        CloudConfig, CloudDensity, CloudError, CloudGpu, CloudInputs, CloudQuality, CloudState,
    },
    terrain_gpu::{Camera, UploadBudget},
};
const SIZE: u32 = 32;
struct Sources {
    _color: wgpu::Texture,
    color: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth: wgpu::TextureView,
    _lut: wgpu::Texture,
    lut: wgpu::TextureView,
}
fn camera() -> Camera {
    Camera::perspective([0.; 3], [0., 1., 0.], [0., 0., -1.], 0.2, 1., 0.1, 100.)
}
fn sources(gpu: &render::Gpu, sample_mask: u32) -> Sources {
    let texture = |label, format| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
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
        "actual blue sky with half red foreground samples",
        wgpu::TextureFormat::Rgba16Float,
    );
    let color = color_texture.create_view(&Default::default());
    let depth_texture = texture(
        "actual MSAA4 reverse depth",
        wgpu::TextureFormat::Depth32Float,
    );
    let depth = depth_texture.create_view(&Default::default());
    let lut_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unit radiance physical oracle LUT"),
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
            texture: &lut_texture,
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
    let lut = lut_texture.create_view(&Default::default());
    let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("independent MSAA coverage oracle source"),source:wgpu::ShaderSource::Wgsl(format!("@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}} struct Out{{@location(0)color:vec4<f32>,@builtin(frag_depth)depth:f32,@builtin(sample_mask)mask:u32}};@fragment fn fragment()->Out{{var o:Out;o.color=vec4(1.,0.,0.,1.);o.depth=0.019019019;o.mask={sample_mask}u;return o;}}").into())});
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("red foreground exactly two of four samples"),
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
            label: Some("known foreground before cloud layer"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color,
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
                view: &depth,
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
        pass.draw(0..3, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
    Sources {
        _color: color_texture,
        color,
        _depth: depth_texture,
        depth,
        _lut: lut_texture,
        lut,
    }
}
fn inputs(s: &Sources) -> CloudInputs<'_> {
    CloudInputs {
        transmittance: &s.lut,
        sky: &s.lut,
        scene_color_msaa: &s.color,
        scene_depth_msaa: &s.depth,
    }
}
fn state() -> CloudState {
    CloudState {
        camera: camera(),
        eye: [0.; 3],
        time_seconds: 0.,
        sun: [0., 1., 0.],
        sun_intensity: 0.,
        scene_revision: 1,
    }
}
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
    let row = (SIZE * 8).div_ceil(256) * 256;
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual HDR cloud readback"),
        size: u64::from(row * SIZE),
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
    read_buffer(gpu, &b)
        .chunks_exact(row as usize)
        .flat_map(|r| {
            r[..(SIZE * 8) as usize].chunks_exact(8).map(|p| {
                std::array::from_fn(|c| {
                    half(u16::from_le_bytes(p[c * 2..c * 2 + 2].try_into().unwrap()))
                })
            })
        })
        .collect()
}
fn stats(gpu: &render::Gpu, cloud: &CloudGpu) -> [u32; 4] {
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
#[test]
fn beer_lambert_clouds_preserve_individual_msaa_foreground_samples() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let s = sources(&gpu, 5);
        let mut previous = None;
        for quality in [CloudQuality::LayerA, CloudQuality::VolumeB] {
            let config = CloudConfig {
                quality,
                downsample: 1,
                steps: 32,
                light_steps: 0,
                bottom: 10.,
                thickness: 10.,
                extinction: 0.1,
                sky_strength: 0.,
                max_distance: 100.,
                density: CloudDensity::Uniform(1.),
                ..CloudConfig::default()
            };
            let mut cloud =
                CloudGpu::new(&gpu.device, &gpu.queue, config, SIZE, SIZE, inputs(&s)).unwrap();
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = cloud
                .encode(
                    &gpu.queue,
                    &mut encoder,
                    state(),
                    &mut UploadBudget::new(176),
                    100000,
                )
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
            assert_eq!(work.upload_bytes, 176);
            let actual = image(&gpu, cloud.resolved_texture());
            let mut error = 0f32;
            for y in 0..SIZE {
                for x in 0..SIZE {
                    let ndc = [
                        (x as f64 + 0.5) / f64::from(SIZE) * 2. - 1.,
                        (y as f64 + 0.5) / f64::from(SIZE) * 2. - 1.,
                    ];
                    let length =
                        (1. + (ndc[0] * ndc[0] + ndc[1] * ndc[1]) * 0.1f64.tan().powi(2)).sqrt();
                    let expected = (-length).exp() as f32 * 0.5;
                    let p = actual[(x + SIZE * y) as usize];
                    error = error.max((p[2] - expected).abs());
                    assert!(
                        (p[0] - 0.5).abs() < 0.001,
                        "foreground coverage sample changed {p:?}"
                    );
                    assert!(p[1].abs() < 0.001);
                    assert_eq!(p[3], 1.);
                }
            }
            assert!(
                error < 0.002,
                "Beer-Lambert independent oracle maxerror {error}"
            );
            let actual_work = stats(&gpu, &cloud);
            let steps = if quality == CloudQuality::LayerA {
                1
            } else {
                32
            };
            assert_eq!(
                actual_work,
                [
                    SIZE * SIZE * steps,
                    SIZE * SIZE * steps,
                    SIZE * SIZE,
                    SIZE * SIZE
                ]
            );
            if let Some(p) = previous {
                assert_eq!(
                    p, actual,
                    "homogeneous slab must be integration-step invariant"
                );
            }
            previous = Some(actual);
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            assert_eq!(
                cloud
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
            let mut changed = state();
            changed.time_seconds = 1.;
            assert_eq!(
                cloud.encode(
                    &gpu.queue,
                    &mut encoder,
                    changed,
                    &mut UploadBudget::new(176),
                    0
                ),
                Err(CloudError::WorkBudget)
            );
            println!(
                "M10 clouds{quality:?} actualsteps{} density{} pixels{} BeerError{error} MSAAforeground0.5exact",
                actual_work[0], actual_work[1], actual_work[2]
            );
        }
    });
}
#[test]
fn procedural_volume_uses_actual_atmosphere_and_bounded_light_steps() {
    pollster::block_on(async {
        use render::atmosphere::{AtmosphereConfig, AtmosphereGpu, AtmosphereState};
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let s = sources(&gpu, 0);
        let mut atmo = AtmosphereGpu::new(
            &gpu.device,
            &gpu.queue,
            AtmosphereConfig {
                transmittance_size: [16, 8],
                sky_size: [16, 8],
                transmittance_steps: 16,
                sky_steps: 16,
            },
            AtmosphereState::default(),
            &mut UploadBudget::new(100000),
        )
        .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        atmo.encode_update(&mut encoder, &mut 1000000).unwrap();
        gpu.queue.submit([encoder.finish()]);
        let source_inputs = CloudInputs {
            transmittance: atmo.transmittance_view(),
            sky: atmo.sky_view(),
            scene_color_msaa: &s.color,
            scene_depth_msaa: &s.depth,
        };
        let config = CloudConfig {
            quality: CloudQuality::VolumeB,
            downsample: 2,
            steps: 32,
            light_steps: 4,
            bottom: 10.,
            thickness: 10.,
            coverage: 0.8,
            wind: [8., 2.],
            noise_scale: 0.2,
            extinction: 0.1,
            max_distance: 100.,
            ..CloudConfig::default()
        };
        let mut cloud =
            CloudGpu::new(&gpu.device, &gpu.queue, config, SIZE, SIZE, source_inputs).unwrap();
        let mut sun = state();
        sun.sun_intensity = 5.;
        let encode = |cloud: &mut CloudGpu, state| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = cloud
                .encode(
                    &gpu.queue,
                    &mut encoder,
                    state,
                    &mut UploadBudget::new(176),
                    1000000,
                )
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
            work
        };
        encode(&mut cloud, sun);
        let initial = image(&gpu, cloud.resolved_texture());
        assert!(initial.iter().flatten().all(|v| v.is_finite() && *v >= 0.));
        let actual_work = stats(&gpu, &cloud);
        assert_eq!(actual_work[0], 16 * 16 * 32);
        assert!(actual_work[1] > actual_work[0]);
        assert!(actual_work[1] <= actual_work[0] * 5);
        sun.time_seconds = 17.;
        encode(&mut cloud, sun);
        let moved = image(&gpu, cloud.resolved_texture());
        assert_ne!(
            initial, moved,
            "wind did not advect actual procedural density"
        );
        let mut clear = CloudGpu::new(
            &gpu.device,
            &gpu.queue,
            CloudConfig {
                coverage: 0.,
                ..config
            },
            SIZE,
            SIZE,
            source_inputs,
        )
        .unwrap();
        encode(&mut clear, sun);
        assert!(
            image(&gpu, clear.resolved_texture())
                .iter()
                .all(|p| *p == [0., 0., 1., 1.]),
            "zero procedural coverage invented cloud opacity"
        );
        let foreground = sources(&gpu, 15);
        let mut occluded = CloudGpu::new(
            &gpu.device,
            &gpu.queue,
            config,
            SIZE,
            SIZE,
            CloudInputs {
                scene_color_msaa: &foreground.color,
                scene_depth_msaa: &foreground.depth,
                ..source_inputs
            },
        )
        .unwrap();
        encode(&mut occluded, sun);
        assert!(
            image(&gpu, occluded.resolved_texture())
                .iter()
                .all(|p| *p == [1., 0., 0., 1.]),
            "clouds behind known opaque plane changed foreground"
        );
        println!(
            "M10 volume actualAtmoLUTs pixels{} viewSteps{} density+light{} bounded<=40960 windChangesHDR zeroCoverageSkyExact opaqueForegroundExact",
            actual_work[2], actual_work[0], actual_work[1]
        );
    });
}
