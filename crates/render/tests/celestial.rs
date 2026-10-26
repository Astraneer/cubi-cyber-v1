#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
use render::{
    atmosphere,
    celestial::{CelestialConfig, CelestialGpu, CelestialState},
};
const SIZE: u32 = 128;
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
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    celestial: Option<&mut CelestialGpu>,
    state: CelestialState,
) -> Vec<u8> {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual celestial HDR tone output"),
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
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual celestial image oracle"),
        size: u64::from(SIZE * SIZE * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    if let Some(celestial) = celestial {
        celestial
            .prepare_frame(&gpu.queue, state, &mut UploadBudget::new(144))
            .unwrap();
        terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &texture.create_view(&Default::default()),
                &state.camera,
                CullingMode::Frustum,
                |pass| celestial.draw(pass),
            )
            .unwrap();
    } else {
        terrain
            .encode_frame(
                &gpu.queue,
                &mut encoder,
                &texture.create_view(&Default::default()),
                &state.camera,
                CullingMode::Frustum,
            )
            .unwrap();
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 4),
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
}
fn brightness(image: &[u8]) -> u64 {
    image
        .chunks_exact(4)
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum()
}
fn changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count()
}
fn setup(gpu: &render::Gpu) -> (TerrainGpu, atmosphere::AtmosphereGpu, UploadBudget) {
    let mut budget = UploadBudget::new(1_000_000);
    let mut atmo = atmosphere::AtmosphereGpu::new(
        &gpu.device,
        &gpu.queue,
        atmosphere::AtmosphereConfig {
            transmittance_size: [32, 16],
            sky_size: [32, 16],
            transmittance_steps: 32,
            sky_steps: 24,
        },
        atmosphere::AtmosphereState {
            sun_direction: [0., -0.8, 0.6],
            altitude_km: 0.072,
        },
        &mut budget,
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmo.encode_update(&mut encoder, &mut 1_000_000).unwrap();
    gpu.queue.submit([encoder.finish()]);
    let mut terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        SIZE,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 4,
            max_quads: 4096,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    terrain.attach_atmosphere(&gpu.device, &atmo).unwrap();
    (terrain, atmo, budget)
}
fn state() -> CelestialState {
    CelestialState {
        camera: Camera::perspective([0.; 3], [0., 0.8, -0.6], [0., 1., 0.], 0.3, 1., 0.05, 100.),
        sun: [0., -0.8, 0.6],
        moon: [0., 0.8, -0.6],
        rotation_radians: 0.,
        time_seconds: 0.,
        moon_phase: 0,
        sun_enabled: true,
        moon_radiance: 0.2,
        star_radiance: 1.,
        altitude_km: 0.072,
    }
}
#[test]
fn lunar_phases_reuse_catalog_and_real_depth_preserves_opaque_samples() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let (mut terrain, atmo, mut budget) = setup(&gpu);
    let mut bodies = CelestialGpu::new(
        &gpu.device,
        &gpu.queue,
        &atmo,
        CelestialConfig {
            stars: 0,
            ..Default::default()
        },
        &mut budget,
    )
    .unwrap();
    let startup = bodies.take_work();
    assert_eq!(startup.pipeline_compilations, 1);
    assert_eq!(startup.catalog_bytes, 32);
    assert_eq!(bodies.vertex_count(), 8);
    let mut s = state();
    let full = frame(&gpu, &mut terrain, Some(&mut bodies), s);
    let full_energy = brightness(&full);
    s.moon_phase = 2;
    let quarter = frame(&gpu, &mut terrain, Some(&mut bodies), s);
    s.moon_phase = 4;
    let new = frame(&gpu, &mut terrain, Some(&mut bodies), s);
    let energies = [full_energy, brightness(&quarter), brightness(&new)];
    assert!(
        energies[0] > energies[1] * 3 / 2 && energies[1] > energies[2] * 5,
        "physical lunar phase order: {energies:?}"
    );
    assert_eq!(new, frame(&gpu, &mut terrain, Some(&mut bodies), s));
    assert_eq!(
        bodies
            .prepare_frame(&gpu.queue, s, &mut UploadBudget::new(0))
            .unwrap()
            .upload_bytes,
        0
    );
    assert_eq!(bodies.take_work().pipeline_compilations, 0);
    use render::terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh};
    let mut halo = PageHalo::filled(0);
    for y in 0..2 {
        for z in 0..2 {
            for x in 0..2 {
                halo.set(x, y, z, 1);
            }
        }
    }
    let mut mesh = PageMesh::default();
    render::terrain_mesh::mesh_page(
        &halo,
        |id| {
            if id == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial::opaque(3)
            }
        },
        &mut MeshScratch::default(),
        &mut mesh,
    );
    terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            render::terrain_gpu::translation([-1., 7., -7.]),
            &mesh,
            &mut budget,
        )
        .unwrap();
    s.moon_phase = 0;
    let opaque = frame(&gpu, &mut terrain, None, s);
    let occluded = frame(&gpu, &mut terrain, Some(&mut bodies), s);
    assert_eq!(
        opaque, occluded,
        "celestial draw must preserve actual reverse-Z foreground coverage"
    );
    println!(
        "M10_CELESTIAL_PHASE_GPU_OK phase_RGB8_energy={energies:?} static_bitwise=true static_upload_bytes=0 foreground_changed_pixels=0 frame_compilations=0"
    );
}
#[test]
fn donor_scene_variants_and_rotating_twinkle_are_prepared_with_bounded_work() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let (mut terrain, mut atmo, mut budget) = setup(&gpu);
    let mut star_frames = Vec::new();
    let mut solar_frames = Vec::new();
    let mut state = state();
    state.camera = Camera::perspective([0.; 3], [0., 0.8, -0.6], [0., 1., 0.], 1.2, 1., 0.05, 100.);
    state.moon = [0., -0.8, 0.6];
    for style in 0..3 {
        let mut bodies = CelestialGpu::new(
            &gpu.device,
            &gpu.queue,
            &atmo,
            CelestialConfig {
                stars: 4096,
                sun_style: style,
                moon_style: style.min(1),
                star_style: style,
                ..Default::default()
            },
            &mut budget,
        )
        .unwrap();
        assert_eq!(bodies.vertex_count(), 16392);
        bodies.take_work();
        let initial = frame(&gpu, &mut terrain, Some(&mut bodies), state);
        assert!(brightness(&initial) > 10);
        assert_eq!(initial, frame(&gpu, &mut terrain, Some(&mut bodies), state));
        let energy = brightness(&initial);
        let mut max_delta = 0.;
        for time in [0.1, 0.2, 0.3, 0.4] {
            let mut time_state = state;
            time_state.time_seconds = time;
            let next = frame(&gpu, &mut terrain, Some(&mut bodies), time_state);
            max_delta = f64::max(
                max_delta,
                (brightness(&next) as f64 - energy as f64).abs() / energy.max(1) as f64,
            );
        }
        assert!(
            max_delta < 0.2,
            "twinkle must remain bounded, style{style}: {max_delta}"
        );
        let mut rotated = state;
        rotated.rotation_radians = 0.3;
        let rotation_pixels = changed(
            &initial,
            &frame(&gpu, &mut terrain, Some(&mut bodies), rotated),
        );
        assert!(rotation_pixels > 10);
        assert_eq!(bodies.take_work().pipeline_compilations, 0);
        star_frames.push(initial);
        println!(
            "M10_STARS_GPU_OK style={style} stars=4096 vertices=16392 max_relative_total_twinkle={max_delta} rotation_changed_pixels={rotation_pixels} frame_compilations=0"
        );
    }
    assert!(
        changed(&star_frames[0], &star_frames[1]) > 10
            && changed(&star_frames[1], &star_frames[2]) > 10
    );
    state.sun = [0., 0.8, -0.6];
    atmo.set_state(
        &gpu.queue,
        atmosphere::AtmosphereState {
            sun_direction: state.sun,
            altitude_km: 0.072,
        },
        &mut budget,
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmo.encode_update(&mut encoder, &mut 1_000_000).unwrap();
    gpu.queue.submit([encoder.finish()]);
    for style in 0..3 {
        let mut bodies = CelestialGpu::new(
            &gpu.device,
            &gpu.queue,
            &atmo,
            CelestialConfig {
                stars: 0,
                sun_style: style,
                ..Default::default()
            },
            &mut budget,
        )
        .unwrap();
        let image = frame(&gpu, &mut terrain, Some(&mut bodies), state);
        let optical = atmosphere::reference_transmittance(0.072, 0.8, 4096);
        let maximum = optical.into_iter().fold(0.0_f64, f64::max);
        let center = &image[((SIZE / 2 + SIZE * (SIZE / 2)) * 4) as usize..][..3];
        let mut error = 0;
        for c in 0..3 {
            let linear = optical[c] / maximum;
            let srgb = if linear <= 0.0031308 {
                12.92 * linear
            } else {
                1.055 * linear.powf(1. / 2.4) - 0.055
            };
            error = error.max(center[c].abs_diff((srgb * 255.).round() as u8));
        }
        assert!(
            error <= 3,
            "solar style{style} actual{center:?} f64 T{optical:?} error{error}"
        );
        solar_frames.push(image);
    }
    assert!(
        changed(&solar_frames[0], &solar_frames[1]) > 10
            && changed(&solar_frames[1], &solar_frames[2]) > 10
    );
}
