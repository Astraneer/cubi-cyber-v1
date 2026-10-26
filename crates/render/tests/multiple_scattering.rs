#![cfg(not(target_arch = "wasm32"))]
use render::multiple_scattering::{
    MultipleScatteringConfig, MultipleScatteringError, MultipleScatteringGpu,
};
use render::terrain_gpu::UploadBudget;
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
fn pixels(gpu: &render::Gpu, texture: &wgpu::Texture, size: [u32; 2]) -> Vec<[f32; 4]> {
    let stride = (size[0] * 8).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("multiple scattering numeric oracle"),
        size: u64::from(stride * size[1]),
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
                rows_per_image: Some(size[1]),
            },
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let output = mapped
        .chunks_exact(stride as usize)
        .flat_map(|line| {
            line[..size[0] as usize * 8].chunks_exact(8).map(|p| {
                std::array::from_fn(|c| {
                    half(u16::from_le_bytes(p[c * 2..c * 2 + 2].try_into().unwrap()))
                })
            })
        })
        .collect();
    drop(mapped);
    buffer.unmap();
    output
}
#[test]
fn homogeneous_isotropic_sphere_matches_independent_geometric_series_and_is_solar_independent() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut data: [[f32; 4]; 6] = [
        [0., 1., 1e9, 1e9],
        [0.5, 0.25, 0.125, 0.],
        [0., 0.5, 0.8, 20.],
        [0., 0., 0., 25.],
        [0., 1., 0., 0.],
        [0.; 4],
    ];
    let optics = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("analytic homogeneous sphere physical coefficients"),
        size: 96,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    optics
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(bytemuck::cast_slice(&data));
    optics.unmap();
    let transmittance = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unit incident sunlight for transfer-function oracle"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &transmittance,
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
    let view = transmittance.create_view(&Default::default());
    let config = MultipleScatteringConfig {
        size: [8, 4],
        directions: 64,
        steps: 32,
        ground_albedo: [0.; 3],
    };
    assert!(matches!(
        MultipleScatteringGpu::new(
            &gpu.device,
            &gpu.queue,
            &optics,
            &view,
            config,
            &mut UploadBudget::new(1055)
        ),
        Err(MultipleScatteringError::UploadBudget)
    ));
    let mut node = MultipleScatteringGpu::new(
        &gpu.device,
        &gpu.queue,
        &optics,
        &view,
        config,
        &mut UploadBudget::new(1056),
    )
    .unwrap();
    let startup = node.take_startup_work();
    assert_eq!(
        (
            startup.upload_bytes,
            startup.texture_bytes,
            startup.pipeline_compilations
        ),
        (1056, 512, 1)
    );
    let _views = (node.view(), node.feedback_view());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert_eq!(
        node.encode(&mut encoder, 1, &mut 0).unwrap_err(),
        MultipleScatteringError::WorkBudget
    );
    assert!(node.needs_redraw(1));
    let work = node.encode(&mut encoder, 1, &mut 65536).unwrap();
    assert_eq!(
        (
            work.rays,
            work.integration_steps,
            work.sunlight_lookups,
            work.passes
        ),
        (2048, 65536, 67584, 1)
    );
    assert_eq!(node.frame_graph().count, 1);
    assert!(node.timestamp_buffer().is_some());
    gpu.queue.submit([encoder.finish()]);
    let psi = pixels(&gpu, node.texture(), config.size);
    let feedback = pixels(&gpu, node.feedback_texture(), config.size);
    let mut error = 0f64;
    for c in 0..3 {
        let scatter: f64 = [0.5, 0.25, 0.125][c];
        let sigma = scatter + 0.5;
        let f = scatter / sigma * (1. - (-sigma).exp());
        let mut term = 1.;
        let mut series = 0.;
        for _ in 0..100 {
            series += term;
            term *= f;
        }
        let expected = f / (4. * std::f64::consts::PI) * series;
        for x in 0..8 {
            error = error.max((f64::from(psi[x][c]) - expected).abs());
            error = error.max((f64::from(feedback[x][c]) - f).abs());
        }
    }
    assert!(
        error < 0.00025,
        "finite-sphere geometric-series energy mismatch: {error}"
    );
    assert!(
        feedback
            .iter()
            .all(|p| p[..3].iter().all(|v| *v >= 0. && *v <= 0.5001))
    );
    assert!(
        psi.iter()
            .all(|p| p[..3].iter().all(|v| v.is_finite() && *v >= 0.))
    );
    data[2][3] = 100.;
    data[4] = [0.5, 0.5, 0.5, 0.8];
    gpu.queue
        .write_buffer(&optics, 0, bytemuck::cast_slice(&data));
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let idle = node.encode(&mut encoder, 1, &mut 0).unwrap();
    assert_eq!(
        (idle.passes, idle.integration_steps, idle.upload_bytes),
        (0, 0, 0)
    );
    assert_eq!(node.frame_graph().count, 0);
    gpu.queue.submit([encoder.finish()]);
    assert_eq!(
        pixels(&gpu, node.texture(), config.size),
        psi,
        "unit transfer must not depend on current solar intensity/direction/observer"
    );
    node.invalidate();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    node.encode(&mut encoder, 1, &mut 65536).unwrap();
    gpu.queue.submit([encoder.finish()]);
    assert_eq!(pixels(&gpu, node.texture(), config.size), psi);
    println!(
        "M10_MULTIPLE_SCATTER_GPU_OK sphere_f64_series_error={error} feedback_max<=0.5 directions=64 logical_steps=65536 solar_observer_changes=0work static_bitwise=true initial_upload_bytes=1056"
    );
}
#[test]
fn actual_earth_lut_is_finite_bounded_and_ground_reflection_changes_daylight() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut budget = UploadBudget::new(1_000_000);
    let mut atmo = render::atmosphere::AtmosphereGpu::new(
        &gpu.device,
        &gpu.queue,
        render::atmosphere::AtmosphereConfig {
            transmittance_size: [64, 32],
            sky_size: [32, 16],
            transmittance_steps: 64,
            sky_steps: 32,
        },
        render::atmosphere::AtmosphereState::default(),
        &mut budget,
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmo.encode_update(&mut encoder, &mut 1_000_000).unwrap();
    gpu.queue.submit([encoder.finish()]);
    let config = MultipleScatteringConfig {
        size: [8, 8],
        directions: 32,
        steps: 32,
        ground_albedo: [0.3; 3],
    };
    let mut ground = MultipleScatteringGpu::new(
        &gpu.device,
        &gpu.queue,
        atmo.optical_parameters(),
        atmo.transmittance_view(),
        config,
        &mut budget,
    )
    .unwrap();
    let mut black = MultipleScatteringGpu::new(
        &gpu.device,
        &gpu.queue,
        atmo.optical_parameters(),
        atmo.transmittance_view(),
        MultipleScatteringConfig {
            ground_albedo: [0.; 3],
            ..config
        },
        &mut budget,
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    ground.encode(&mut encoder, 1, &mut 65536).unwrap();
    black.encode(&mut encoder, 1, &mut 65536).unwrap();
    gpu.queue.submit([encoder.finish()]);
    let a = pixels(&gpu, ground.texture(), config.size);
    let b = pixels(&gpu, black.texture(), config.size);
    let f = pixels(&gpu, ground.feedback_texture(), config.size);
    assert!(
        a.iter()
            .flat_map(|p| p[..3].iter())
            .all(|v| v.is_finite() && *v >= 0.)
    );
    assert!(
        f.iter()
            .flat_map(|p| p[..3].iter())
            .all(|v| *v >= 0. && *v < 1.)
    );
    for y in 0..8 {
        assert_eq!(
            a[y * 8][..3],
            [0.; 3],
            "anti-solar planet shadow must stay dark"
        );
    }
    let daylight_ground_delta = (a[7][0] - b[7][0]).abs();
    assert!(daylight_ground_delta > 0.001);
    let maximum = a
        .iter()
        .flat_map(|p| p[..3].iter().copied())
        .fold(0f32, f32::max);
    println!(
        "M10_MULTIPLE_EARTH_GPU_OK texels=64 max_Psi={maximum} daylight_ground_delta={daylight_ground_delta} night_column_leaks=0 feedback_below1=true"
    );
}
