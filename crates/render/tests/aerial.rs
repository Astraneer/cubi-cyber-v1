#![cfg(not(target_arch = "wasm32"))]
use render::aerial_gpu::{AerialConfig, AerialError, AerialGpu, AerialInputs, AerialState};
use render::terrain_gpu::{Camera, UploadBudget};
const W: u32 = 32;
const H: u32 = 16;
fn half(value: u16) -> f32 {
    let sign = if value & 0x8000 != 0 { -1. } else { 1. };
    let exponent = i32::from((value >> 10) & 31);
    let fraction = f32::from(value & 1023);
    if exponent == 0 {
        sign * fraction * 2f32.powi(-24)
    } else {
        sign * (1. + fraction / 1024.) * 2f32.powi(exponent - 15)
    }
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    data
}
fn pixels(gpu: &render::Gpu, texture: &wgpu::Texture, width: u32, height: u32) -> Vec<[f32; 4]> {
    let stride = (width * 8).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aerial physical numeric oracle"),
        size: u64::from(stride * height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
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
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
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
    read(gpu, &buffer)
        .chunks_exact(stride as usize)
        .flat_map(|line| {
            line[..width as usize * 8].chunks_exact(8).map(|p| {
                std::array::from_fn(|c| {
                    half(u16::from_le_bytes(p[c * 2..c * 2 + 2].try_into().unwrap()))
                })
            })
        })
        .collect()
}
fn scene(
    gpu: &render::Gpu,
    width: u32,
    height: u32,
    camera: Camera,
) -> (wgpu::Texture, wgpu::Texture) {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = |label, format| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 4,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
    };
    let color = texture("read-only scene color", wgpu::TextureFormat::Rgba16Float);
    let depth = texture(
        "read-only actual reverse depth",
        wgpu::TextureFormat::Depth32Float,
    );
    let clip_z = camera.view_projection[2][2] * -500. + camera.view_projection[3][2];
    let clip_w = camera.view_projection[2][3] * -500. + camera.view_projection[3][3];
    let source = format!(
        "@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}}struct Out{{@location(0)color:vec4<f32>,@builtin(frag_depth)depth:f32}};@fragment fn fragment()->Out{{return Out(vec4(1.,0.5,0.25,1.),{});}}",
        clip_z / clip_w
    );
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("analytic homogeneous medium plane"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("two exact foreground samples"),
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
                mask: 0b0101,
                alpha_to_coverage_enabled: false,
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
        let cv = color.create_view(&Default::default());
        let dv = depth.create_view(&Default::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sky plus actual half-covered plane"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &cv,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.1,
                        g: 0.2,
                        b: 0.3,
                        a: 0.,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
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
        pass.draw(0..3, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
    (color, depth)
}
fn aerial_gpu_shadow_refs<'a>(
    parameters: &'a wgpu::Buffer,
    depth: &'a wgpu::TextureView,
    sampler: &'a wgpu::Sampler,
) -> render::aerial_gpu::AerialShadows<'a> {
    render::aerial_gpu::AerialShadows {
        parameters,
        depth,
        sampler,
    }
}
#[test]
fn homogeneous_f64_energy_preserves_each_msaa_sample_and_resize_reuses_pipelines() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let camera = Camera::perspective([0.; 3], [0., 0., -1.], [0., 1., 0.], 0.1, 2., 1., 4000.);
    let (color, depth) = scene(&gpu, W, H, camera);
    let cv = color.create_view(&Default::default());
    let dv = depth.create_view(&Default::default());
    let optics = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("homogeneous physical limit of the shared atmosphere"),
        size: 96,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: true,
    });
    let data: [[f32; 4]; 6] = [
        [6360., 6460., 1e9, 1e9],
        [0.4, 0.2, 0.1, 0.],
        [0., 0., 0.8, 2.],
        [0., 0., 0., 25.],
        [0., 1., 0., 0.072],
        [0.; 4],
    ];
    optics
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(bytemuck::cast_slice(&data));
    optics.unmap();
    let white = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unattenuated external sun for analytic homogeneous oracle"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
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
            texture: &white,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255; 4],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let white = white.create_view(&Default::default());
    let inputs = || AerialInputs {
        color_msaa: &cv,
        depth_msaa: &dv,
        optics: &optics,
        transmittance: &white,
        shadows: None,
        multiple_scattering: None,
    };
    let mut aerial = AerialGpu::new(
        &gpu.device,
        &gpu.queue,
        AerialConfig {
            grid: [8, 8, 32],
            steps: 32,
            max_distance: 2048.,
            ..AerialConfig::default()
        },
        W,
        H,
        inputs(),
    )
    .unwrap();
    let startup = aerial.take_startup_work();
    assert_eq!(startup.pipeline_compilations, 2);
    assert_eq!(startup.volume_bytes, 32768);
    assert_eq!(startup.target_bytes, u64::from(W) * u64::from(H) * 40);
    let state = AerialState {
        camera,
        eye: [0.; 3],
        source_revision: 1,
        atmosphere_revision: 1,
        shadow_revision: 0,
    };
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let mut budget = UploadBudget::new(112);
    assert_eq!(
        aerial
            .encode(
                &gpu.queue,
                &mut encoder,
                state,
                &mut budget,
                &mut 0,
                &mut 2048
            )
            .unwrap_err(),
        AerialError::WorkBudget
    );
    assert_eq!(budget.remaining_bytes, 112);
    let work = aerial
        .encode(
            &gpu.queue,
            &mut encoder,
            state,
            &mut budget,
            &mut 65536,
            &mut 2048,
        )
        .unwrap();
    assert_eq!(
        (
            work.integration_steps,
            work.composition_samples,
            work.passes,
            work.draws
        ),
        (65536, 2048, 2, 1)
    );
    assert_eq!(aerial.frame_graph().count, 2);
    assert!(aerial.timestamp_buffer().is_some());
    gpu.queue.submit([encoder.finish()]);
    let image = pixels(&gpu, aerial.resolved_texture(), W, H);
    let mut max_error = 0f64;
    for y in 0..H {
        for x in 0..W {
            let dx = (2. * (f64::from(x) + 0.5) / f64::from(W) - 1.) * (0.05f64).tan() * 2.;
            let dy = -(2. * (f64::from(y) + 0.5) / f64::from(H) - 1.) * (0.05f64).tan();
            let distance = 0.5 * (1. + dx * dx + dy * dy).sqrt();
            let cosine = dy / (1. + dx * dx + dy * dy).sqrt();
            let source = 2. * 3. * (1. + cosine * cosine) / (16. * std::f64::consts::PI);
            for c in 0..3 {
                let t = (-[0.4, 0.2, 0.1][c] * distance).exp();
                let lit = [1., 0.5, 0.25][c] * t + source * (1. - t);
                let expected = 0.5 * lit + 0.5 * [0.1, 0.2, 0.3][c];
                max_error =
                    max_error.max((f64::from(image[(x + y * W) as usize][c]) - expected).abs());
            }
            assert_eq!(image[(x + y * W) as usize][3], 0.5);
        }
    }
    assert!(
        max_error < 0.002,
        "independent homogeneous Beer/scattering or MSAA mismatch: {max_error}"
    );
    // Controlled M4 comparison atlas: the first cascade shadows the whole
    // ray segment, while an empty depth map is an independent lit control.
    let shadow_uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("analytic fully-occluded aerial shadow atlas mapping"),
        size: 224,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: true,
    });
    let mut shadow_words = [0u32; 56];
    for cascade in 0..3 {
        shadow_words[cascade * 16 + 14] = 0.5f32.to_bits();
        shadow_words[cascade * 16 + 15] = 1f32.to_bits();
    }
    shadow_words[49] = 1f32.to_bits();
    shadow_words[52] = 1;
    shadow_words[53] = 1;
    shadow_uniform
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(bytemuck::cast_slice(&shadow_words));
    shadow_uniform.unmap();
    let shadow_depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("controlled actual reverse-Z comparison depth"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 3,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let shadow_view = shadow_depth.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let shadow_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("actual M4 reverse-Z comparison"),
        compare: Some(wgpu::CompareFunction::GreaterEqual),
        ..Default::default()
    });
    let clear_shadow = |encoder: &mut wgpu::CommandEncoder, value: f32| {
        for layer in 0..3 {
            let view = shadow_depth.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            });
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("controlled shadow plane clear"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(value),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
    };
    let mut shadowed = AerialGpu::new(
        &gpu.device,
        &gpu.queue,
        AerialConfig {
            grid: [8, 8, 32],
            steps: 32,
            max_distance: 2048.,
            ..Default::default()
        },
        W,
        H,
        AerialInputs {
            shadows: Some(aerial_gpu_shadow_refs(
                &shadow_uniform,
                &shadow_view,
                &shadow_sampler,
            )),
            ..inputs()
        },
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    clear_shadow(&mut encoder, 1.);
    let work = shadowed
        .encode(
            &gpu.queue,
            &mut encoder,
            state,
            &mut UploadBudget::new(112),
            &mut 65536,
            &mut 2048,
        )
        .unwrap();
    assert_eq!(work.shadow_samples, 262144);
    gpu.queue.submit([encoder.finish()]);
    let blocked = pixels(&gpu, shadowed.resolved_texture(), W, H);
    let darkened = image
        .iter()
        .zip(&blocked)
        .filter(|(a, b)| a[0] - b[0] > 0.005)
        .count();
    assert_eq!(
        darkened,
        (W * H) as usize,
        "sun fog must not leak through the controlled occluder"
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    clear_shadow(&mut encoder, 0.);
    let opened = AerialState {
        shadow_revision: 1,
        ..state
    };
    let work = shadowed
        .encode(
            &gpu.queue,
            &mut encoder,
            opened,
            &mut UploadBudget::new(112),
            &mut 65536,
            &mut 2048,
        )
        .unwrap();
    assert_eq!(work.integration_steps, 65536);
    gpu.queue.submit([encoder.finish()]);
    let restored = pixels(&gpu, shadowed.resolved_texture(), W, H);
    let max_restore = restored
        .iter()
        .zip(&image)
        .flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| (a - b).abs()))
        .fold(0f32, f32::max);
    assert!(
        max_restore < 0.0005,
        "empty shadow atlas must match no-shadow source: {max_restore}"
    );
    println!(
        "M10_AERIAL_SHADOW_GPU_OK blocked_receivers={darkened} max_restore_error={max_restore} shadow_reads_upper_bound=262144 stale_shadow_cache_rebuilt=true"
    );
    let original = image;
    let source_only = AerialState {
        source_revision: 2,
        ..state
    };
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let recolor = aerial
        .encode(
            &gpu.queue,
            &mut encoder,
            source_only,
            &mut UploadBudget::new(0),
            &mut 0,
            &mut 2048,
        )
        .unwrap();
    assert_eq!(
        (
            recolor.passes,
            recolor.integration_steps,
            recolor.upload_bytes
        ),
        (1, 0, 0)
    );
    gpu.queue.submit([encoder.finish()]);
    assert_eq!(pixels(&gpu, aerial.resolved_texture(), W, H), original);
    let state = source_only;
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let idle = aerial
        .encode(
            &gpu.queue,
            &mut encoder,
            state,
            &mut UploadBudget::new(0),
            &mut 0,
            &mut 0,
        )
        .unwrap();
    assert_eq!((idle.passes, idle.upload_bytes), (0, 0));
    gpu.queue.submit([encoder.finish()]);
    assert_eq!(pixels(&gpu, aerial.resolved_texture(), W, H), original);
    let _sample_view = aerial.color_msaa();
    let _resolved = aerial.resolved_view();
    let (c2, d2) = scene(&gpu, W / 2, H / 2, camera);
    aerial
        .resize_inputs(
            &gpu.device,
            W / 2,
            H / 2,
            AerialInputs {
                color_msaa: &c2.create_view(&Default::default()),
                depth_msaa: &d2.create_view(&Default::default()),
                optics: &optics,
                transmittance: &white,
                shadows: None,
                multiple_scattering: None,
            },
        )
        .unwrap();
    aerial.invalidate();
    assert!(aerial.needs_redraw(state));
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    aerial
        .encode(
            &gpu.queue,
            &mut encoder,
            state,
            &mut UploadBudget::new(112),
            &mut 65536,
            &mut 512,
        )
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    assert_eq!(aerial.take_startup_work().pipeline_compilations, 0);
    assert!(!pixels(&gpu, aerial.resolved_texture(), W / 2, H / 2).is_empty());
    println!(
        "M10_AERIAL_GPU_OK homogeneous_f64_max_error={max_error} MSAA_foreground_samples=2 MSAA_sky_samples=2 integration_steps=65536 composition_samples=2048 static_bitwise=true static_upload_bytes=0 resize_pipeline_compilations=0"
    );
}
