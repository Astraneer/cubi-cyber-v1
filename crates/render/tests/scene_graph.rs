#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    cloud_gpu::{CloudConfig, CloudGpu, CloudInputs, CloudState},
    post::{Exposure, PostConfig, PostGpu},
    terrain_gpu::{
        AtlasImage, AtlasMip, Camera, CullingMode, GpuConfig, GpuPassKind, TerrainError,
        TerrainGpu, UploadBudget, translation,
    },
    terrain_mesh::{CellMaterial, MeshClass, MeshScratch, PageHalo, PageMesh, mesh_page},
};
const W: u32 = 96;
const H: u32 = 64;
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
fn output(gpu: &render::Gpu) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("integrated scene graph output"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
fn capture(
    gpu: &render::Gpu,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
) -> wgpu::Buffer {
    let row = (W * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual composed output"),
        size: u64::from(row) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
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
    buffer
}
fn cube(water: bool) -> PageMesh {
    let mut result = PageMesh::default();
    mesh_page(
        &PageHalo::from_page(&[1; 4096], 0),
        |raw| {
            if raw == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial {
                    class: if water {
                        MeshClass::Water
                    } else {
                        MeshClass::Opaque
                    },
                    ..CellMaterial::opaque(u16::from(water))
                }
            }
        },
        &mut MeshScratch::new(),
        &mut result,
    );
    result
}
fn post(gpu: &render::Gpu, source: &wgpu::TextureView) -> PostGpu {
    PostGpu::new(
        &gpu.device,
        &gpu.queue,
        source,
        W,
        H,
        wgpu::TextureFormat::Rgba8Unorm,
        OutputTransfer::ShaderSrgb,
        PostConfig {
            exposure: Exposure::Manual(1.),
            bloom: None,
            ..PostConfig::default()
        },
        &mut UploadBudget::new(1024),
    )
    .unwrap()
}
#[test]
fn retained_terrain_samples_compose_clouds_then_post_without_feedback_or_second_transfer() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true))
        .expect("actual software GPU required");
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 2,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![180, 90, 40, 255, 20, 70, 220, 255],
            tint_mask: vec![255; 2],
        }],
    };
    let mut terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 4,
            max_quads: 4096,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let mut upload = UploadBudget::new(1024 * 1024);
    terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([-16., 0., 0.]),
            &cube(false),
            &mut upload,
        )
        .unwrap();
    terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0., 0., 0.]),
            &cube(true),
            &mut upload,
        )
        .unwrap();
    let camera = Camera::perspective(
        [32., 27., 40.],
        [0., 7., 8.],
        [0., 1., 0.],
        0.9,
        W as f32 / H as f32,
        0.1,
        200.,
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert_eq!(
        terrain
            .encode_scene_with_extra(
                &gpu.queue,
                &mut encoder,
                &camera,
                CullingMode::FrustumAndHiZ,
                false,
                |_| 0
            )
            .unwrap_err(),
        TerrainError::SceneSamplingDisabled
    );
    terrain.prepare_scene_sampling(&gpu.device).unwrap();
    let lut = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unit atmospheric transmittance test input"),
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
            texture: &lut,
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
    let lut_view = lut.create_view(&Default::default());
    let mut clouds = CloudGpu::new(
        &gpu.device,
        &gpu.queue,
        CloudConfig {
            coverage: 0.,
            ..CloudConfig::default()
        },
        W,
        H,
        CloudInputs {
            transmittance: &lut_view,
            sky: &lut_view,
            scene_color_msaa: terrain.scene_color_msaa_view().unwrap(),
            scene_depth_msaa: terrain.scene_depth_msaa_view(),
        },
    )
    .unwrap();
    let mut direct = post(&gpu, terrain.resolved_hdr_view());
    let mut composed = post(&gpu, clouds.resolved_view());
    let a = output(&gpu);
    let b = output(&gpu);
    let scene = terrain
        .encode_scene_with_extra(
            &gpu.queue,
            &mut encoder,
            &camera,
            CullingMode::FrustumAndHiZ,
            false,
            |_| 0,
        )
        .unwrap();
    let graph = terrain.frame_graph();
    assert!(!graph.passes[..graph.count as usize].contains(&GpuPassKind::Presentation));
    assert_eq!(scene.draw_calls, 9); // sky, six color buckets, two opaque depth buckets
    let cloud = clouds
        .encode(
            &gpu.queue,
            &mut encoder,
            CloudState {
                camera,
                eye: [32., 27., 40.],
                time_seconds: 0.,
                sun: [0., 1., 0.],
                sun_intensity: 1.,
                scene_revision: terrain.hdr_revision(),
            },
            &mut upload,
            1_000_000,
        )
        .unwrap();
    assert_eq!(cloud.composition_samples, u64::from(W * H * 4));
    let pa = direct
        .encode(
            &mut encoder,
            &a.create_view(&Default::default()),
            terrain.hdr_revision(),
            &mut 1_000_000,
        )
        .unwrap();
    let pb = composed
        .encode(
            &mut encoder,
            &b.create_view(&Default::default()),
            terrain.hdr_revision(),
            &mut 1_000_000,
        )
        .unwrap();
    assert_eq!((pa.passes, pb.passes), (1, 1));
    let ca = capture(&gpu, &mut encoder, &a);
    let cb = capture(&gpu, &mut encoder, &b);
    let timing = direct.timestamp_buffer().map(|(source, period)| {
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual post timing integration oracle"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(source, 0, &buffer, 0, 16);
        (buffer, period)
    });
    gpu.queue.submit([encoder.finish()]);
    let before = read(&gpu, &ca);
    assert_eq!(
        before,
        read(&gpu, &cb),
        "zero-coverage cloud must preserve all four terrain/sky samples before one post transfer"
    );
    if let Some((buffer, period)) = timing {
        let raw = read(&gpu, &buffer);
        let start = u64::from_le_bytes(raw[..8].try_into().unwrap());
        let end = u64::from_le_bytes(raw[8..].try_into().unwrap());
        assert!(
            end > start && period > 0.,
            "timestamps must be actual GPU work"
        );
    }
    assert_eq!(
        terrain.prepare_scene_sampling(&gpu.device),
        Err(TerrainError::LatePreparation)
    );
    // Physical water mode must remove the placeholder bucket before water samples
    // immutable opaque attachments. A missing/ignored flag is a visible negative control.
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let dry = terrain
        .encode_scene_with_extra(
            &gpu.queue,
            &mut encoder,
            &camera,
            CullingMode::FrustumAndHiZ,
            true,
            |_| 0,
        )
        .unwrap();
    assert_eq!(dry.draw_calls, 7);
    direct
        .encode(
            &mut encoder,
            &a.create_view(&Default::default()),
            terrain.hdr_revision(),
            &mut 1_000_000,
        )
        .unwrap();
    let dry_image = capture(&gpu, &mut encoder, &a);
    gpu.queue.submit([encoder.finish()]);
    let changed = before
        .iter()
        .zip(read(&gpu, &dry_image))
        .filter(|(a, b)| **a != *b)
        .count();
    assert!(
        changed > 100,
        "physical-water graph must exclude visible blue placeholder geometry"
    );
    terrain.take_work();
    terrain.resize(&gpu.device, W * 2, H * 2).unwrap();
    assert!(terrain.scene_color_msaa_view().is_some());
    assert_eq!(terrain.take_work().pipeline_compilations, 0);
    println!(
        "M10_SCENE_GRAPH_GPU_OK pixels={} composition_samples={} scene_draws={} physical_water_scene_draws={} post_passes={} transparent_cloud_bitwise=true water_negative_changed_bytes={} resize_pipeline_compilations=0",
        W * H,
        cloud.composition_samples,
        scene.draw_calls,
        dry.draw_calls,
        pa.passes,
        changed
    );
}

#[test]
fn terrain_gi_receiver_uses_reflection_without_scalar_block_light_double_count() {
    use render::{
        gi::{GiConfig, GiGpu, GiVoxel},
        light_volume::{LightVolumeGpu, PackedLight},
        shadows::{ShadowConfig, ShadowGpu},
        terrain_mesh::{Face, QuadDescriptor},
    };
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 1,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![255; 4],
            tint_mask: vec![255],
        }],
    };
    let config = GpuConfig {
        max_pages: 2,
        max_quads: 1024,
        occlusion: true,
    };
    let make_terrain = || {
        TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            W,
            H,
            OutputTransfer::ShaderSrgb,
            config,
            &atlas,
        )
        .unwrap()
    };
    let mut baseline = make_terrain();
    let mut reflected = make_terrain();
    let plane = PageMesh {
        opaque: vec![
            QuadDescriptor::new([0, 8, 16], Face::Up, [16, 16], 0, [3; 4], u32::MAX).unwrap(),
        ],
        ..PageMesh::default()
    };
    let mut budget = UploadBudget::new(2_000_000);
    let base = baseline
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            &plane,
            &mut budget,
        )
        .unwrap();
    let other = reflected
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            &plane,
            &mut budget,
        )
        .unwrap();
    assert_eq!(base, other);
    let mut lights = LightVolumeGpu::new(&gpu.device, 2).unwrap();
    lights
        .upload_uniform(
            &gpu.queue,
            base,
            1,
            PackedLight::new(0, 15, [0; 3]).unwrap(),
            &mut budget,
        )
        .unwrap();
    let shadows = ShadowGpu::new(
        &gpu.device,
        &gpu.queue,
        baseline.geometry(),
        ShadowConfig {
            resolution: 32,
            ..ShadowConfig::default()
        },
        [8.; 3],
        [0., 1., 0.],
    )
    .unwrap();
    baseline
        .attach_lighting(&gpu.device, &shadows, &lights)
        .unwrap();
    let mut gi = GiGpu::new(
        &gpu.device,
        &gpu.queue,
        GiConfig {
            world_origin_page: [0; 3],
            world_pages: [1; 3],
            probe_origin: [2.; 3],
            probe_counts: [4; 3],
            probe_spacing: 4.,
            max_jobs: 64,
            max_distance: 24.,
            ..GiConfig::default()
        },
        &mut budget,
    )
    .unwrap();
    reflected
        .attach_lighting_with_gi(&gpu.device, &shadows, &lights, &gi)
        .unwrap();
    let room = |rgb| {
        std::array::from_fn(|index| {
            let x = index % 16;
            let z = index / 16 % 16;
            let y = index / 256;
            if x == 0 || x == 15 || y == 0 || y == 15 || z == 0 || z == 15 {
                GiVoxel::opaque_reflected(rgb).unwrap()
            } else {
                GiVoxel::EMPTY
            }
        })
    };
    gi.upload_page(&gpu.queue, [0; 3], 1, &room([0.5, 0., 0.]), 0, &mut budget)
        .unwrap();
    let camera = Camera::perspective(
        [8., 14., 20.],
        [8., 8., 8.],
        [0., 1., 0.],
        0.9,
        W as f32 / H as f32,
        0.1,
        100.,
    );
    let a = output(&gpu);
    let b = output(&gpu);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let initial = gi.trace_tick(&gpu.queue, &mut encoder, 0, &mut budget, &mut 10000);
    assert_eq!(initial.updated_probes, 64);
    baseline
        .encode_frame(
            &gpu.queue,
            &mut encoder,
            &a.create_view(&Default::default()),
            &camera,
            CullingMode::FrustumAndHiZ,
        )
        .unwrap();
    reflected
        .encode_frame(
            &gpu.queue,
            &mut encoder,
            &b.create_view(&Default::default()),
            &camera,
            CullingMode::FrustumAndHiZ,
        )
        .unwrap();
    let ca = capture(&gpu, &mut encoder, &a);
    let cb = capture(&gpu, &mut encoder, &b);
    gpu.queue.submit([encoder.finish()]);
    let dark = read(&gpu, &ca);
    let red = read(&gpu, &cb);
    let receivers: Vec<_> = dark
        .chunks_exact(4)
        .zip(red.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| a[..3] == [0; 3] && b[0] > 100 && b[1] == 0 && b[2] == 0)
        .map(|(i, _)| i)
        .collect();
    assert!(
        receivers.len() > 500,
        "GI must illuminate dark receivers with reflected red, despite scalar gameplay block=15"
    );
    reflected.take_work();
    gi.upload_page(&gpu.queue, [0; 3], 2, &room([0., 0., 0.5]), 1, &mut budget)
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let changed = gi.trace_tick(&gpu.queue, &mut encoder, 1, &mut budget, &mut 10000);
    assert_eq!(changed.updated_probes, 64);
    reflected.invalidate();
    reflected
        .encode_frame(
            &gpu.queue,
            &mut encoder,
            &b.create_view(&Default::default()),
            &camera,
            CullingMode::FrustumAndHiZ,
        )
        .unwrap();
    let cb = capture(&gpu, &mut encoder, &b);
    gpu.queue.submit([encoder.finish()]);
    let blue = read(&gpu, &cb);
    for &pixel in &receivers {
        let value = &blue[pixel * 4..pixel * 4 + 4];
        assert!(
            value[0] == 0 && value[1] == 0 && value[2] > 100,
            "updated reflection must replace previous red without residual or white scalar light"
        );
    }
    let work = reflected.take_work();
    assert_eq!(work.upload_bytes, 0);
    assert_eq!(work.pipeline_compilations, 0);
    println!(
        "M10_TERRAIN_GI_GPU_OK reflected_pixels={} red_to_blue=true scalar_block_level=15 scalar_emission=0 geometry_upload_bytes=0 runtime_pipeline_compilations=0",
        receivers.len()
    );
}

#[test]
fn environment_graph_preflights_budgets_reuses_static_scene_and_keeps_all_timing_sources() {
    environment_graph_sample_mode(4);
}
#[test]
fn one_sample_environment_keeps_water_aerial_clouds_celestials_and_static_reuse() {
    environment_graph_sample_mode(1);
}
fn environment_graph_sample_mode(sample_count: u32) {
    use render::{
        atmosphere::AtmosphereConfig,
        environment_gpu::{
            EnvironmentBudget, EnvironmentConfig, EnvironmentError, EnvironmentGpu,
            EnvironmentState,
        },
        terrain_mesh::Face,
        water_gpu::{WaterConfig, WaterDraw, WaterQuad},
    };
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 2,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![180, 90, 40, 255, 20, 70, 220, 255],
            tint_mask: vec![255; 2],
        }],
    };
    let mut terrain = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 4,
            max_quads: 65536,
            occlusion: true,
        },
        &atlas,
        sample_count,
    )
    .unwrap();
    let state = EnvironmentState {
        eye: [26., 31., 29.],
        sun: [0.35, 0.8, 0.2],
        sun_intensity: 1.,
        altitude_km: 0.072,
        time_seconds: 0.,
        delta_seconds: 1. / 60.,
        tick: 0,
        underwater: false,
    };
    let config = EnvironmentConfig {
        atmosphere: AtmosphereConfig {
            transmittance_size: [16, 8],
            sky_size: [16, 8],
            transmittance_steps: 16,
            sky_steps: 16,
        },
        water: Some(WaterConfig {
            max_objects: 4,
            max_quads: 16,
            reflection_steps: 0,
            ..WaterConfig::default()
        }),
        clouds: Some(CloudConfig {
            coverage: 0.,
            ..CloudConfig::default()
        }),
        gi: None,
        post: PostConfig {
            exposure: Exposure::Manual(1.),
            bloom: None,
            ..PostConfig::default()
        },
    };
    let mut upload = UploadBudget::new(1_000_000);
    let mut environment = EnvironmentGpu::new(
        &gpu.device,
        &gpu.queue,
        &mut terrain,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        config,
        state,
        None,
        &mut upload,
    )
    .unwrap();
    terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            &cube(false),
            &mut upload,
        )
        .unwrap();
    environment
        .prepare_multiple_scattering(
            &gpu.device,
            &gpu.queue,
            render::multiple_scattering::MultipleScatteringConfig {
                size: [8, 8],
                directions: 32,
                steps: 32,
                ..Default::default()
            },
            &mut upload,
        )
        .unwrap();
    environment
        .prepare_celestials(
            &gpu.device,
            &gpu.queue,
            render::celestial::CelestialConfig {
                stars: 0,
                ..Default::default()
            },
            &mut upload,
        )
        .unwrap();
    environment
        .prepare_aerial(
            &gpu.device,
            &gpu.queue,
            &terrain,
            W,
            H,
            render::aerial_gpu::AerialConfig {
                grid: [8, 8, 16],
                steps: 8,
                ..Default::default()
            },
            &mut upload,
        )
        .unwrap();
    let water = environment.water_mut().unwrap();
    let water_object = water
        .upload_object(&gpu.queue, None, translation([0., 0.5, 0.]), &mut upload)
        .unwrap();
    let quad = WaterQuad::new(
        [0, 16, 16],
        Face::Up,
        [16, 16],
        [1.; 4],
        [0.; 2],
        u32::MAX,
        0.,
    )
    .unwrap();
    water
        .upload_quads(
            &gpu.queue,
            &[WaterDraw {
                quad,
                object: water_object,
            }],
            &mut upload,
        )
        .unwrap();
    environment
        .set_water_cover_ready(&mut terrain, true)
        .unwrap();
    let camera = Camera::perspective(
        state.eye,
        [8.; 3],
        [0., 1., 0.],
        1.,
        W as f32 / H as f32,
        0.1,
        200.,
    );
    let output = output(&gpu);
    let view = output.create_view(&Default::default());
    let fresh_budget = || EnvironmentBudget {
        aerial_steps: 1_000_000,
        aerial_samples: 1_000_000,
        upload_bytes: 1_000_000,
        atmosphere_steps: 1_000_000,
        water_reflection_steps: 0,
        cloud_density_samples: 1_000_000,
        gi_rays: 0,
        post_pixels: 1_000_000,
    };
    let mut refused = fresh_budget();
    refused.cloud_density_samples = 0;
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert_eq!(
        environment
            .encode(
                &gpu.queue,
                &mut encoder,
                &mut terrain,
                &view,
                &camera,
                CullingMode::FrustumAndHiZ,
                state,
                &mut refused,
                |_| panic!("no scene work before complete preflight")
            )
            .unwrap_err(),
        EnvironmentError::WorkBudget
    );
    assert_eq!(refused.upload_bytes, 1_000_000);
    assert!(environment.needs_redraw(&terrain, &camera, CullingMode::FrustumAndHiZ, state));
    let mut refused = fresh_budget();
    refused.upload_bytes = 0;
    assert_eq!(
        environment
            .encode(
                &gpu.queue,
                &mut encoder,
                &mut terrain,
                &view,
                &camera,
                CullingMode::FrustumAndHiZ,
                state,
                &mut refused,
                |_| panic!("no upload before preflight")
            )
            .unwrap_err(),
        EnvironmentError::UploadBudget
    );
    let mut budget = fresh_budget();
    let first = environment
        .encode(
            &gpu.queue,
            &mut encoder,
            &mut terrain,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
            state,
            &mut budget,
            |_| 0,
        )
        .unwrap();
    assert_eq!(first.atmosphere.passes, 3);
    assert_eq!(first.celestial.draws, 1);
    assert_eq!(first.celestial.vertices, 8);
    assert_eq!(first.aerial.passes, 2);
    assert_eq!(first.aerial.integration_steps, 8192);
    assert_eq!(
        first.aerial.composition_samples,
        u64::from(W * H * sample_count)
    );
    assert_eq!(
        first.clouds.composition_samples,
        u64::from(W * H * sample_count)
    );
    assert!(matches!(
        environment.prepare_celestials(
            &gpu.device,
            &gpu.queue,
            render::celestial::CelestialConfig::default(),
            &mut upload
        ),
        Err(EnvironmentError::Terrain(
            render::terrain_gpu::TerrainError::LatePreparation
        ))
    ));
    assert!(
        first.terrain.passes > 0
            && first.water.passes > 0
            && first.clouds.passes > 0
            && first.post.passes > 0
    );
    let sources = environment.timing_sources(&terrain);
    let actual_passes = sources
        .iter()
        .flatten()
        .map(|source| source.count)
        .sum::<u32>();
    assert_eq!(
        actual_passes, first.passes,
        "every encoded pass, including atmosphere LUTs, must have an independent GPU timing source"
    );
    let timing = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual combined M10 pass timestamps"),
        size: u64::from(actual_passes) * 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut offset = 0;
    let mut mapped_kinds = std::collections::BTreeSet::new();
    for source in sources.into_iter().flatten() {
        encoder.copy_buffer_to_buffer(
            source.buffer,
            0,
            &timing,
            offset,
            u64::from(source.count) * 16,
        );
        offset += u64::from(source.count) * 16;
        for &kind in &source.kinds[..source.count as usize] {
            mapped_kinds.insert(kind);
        }
    }
    for kind in [5, 6, 8, 9, 13] {
        assert!(mapped_kinds.contains(&kind));
    }
    let pixels = capture(&gpu, &mut encoder, &output);
    gpu.queue.submit([encoder.finish()]);
    assert!(read(&gpu, &pixels).iter().any(|&value| value > 20));
    let timings = read(&gpu, &timing);
    let mut nonzero = 0;
    for pair in timings.chunks_exact(16) {
        let a = u64::from_le_bytes(pair[..8].try_into().unwrap());
        let b = u64::from_le_bytes(pair[8..].try_into().unwrap());
        assert!(b >= a);
        nonzero += usize::from(b > a);
    }
    assert!(nonzero > 5);
    assert!(!environment.needs_redraw(&terrain, &camera, CullingMode::FrustumAndHiZ, state));
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let mut budget = fresh_budget();
    let cached = environment
        .encode(
            &gpu.queue,
            &mut encoder,
            &mut terrain,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
            state,
            &mut budget,
            |_| panic!("static scene must reuse HDR"),
        )
        .unwrap();
    assert_eq!(cached.passes, 0);
    assert_eq!(cached.upload_bytes, 0);
    assert!(
        environment
            .timing_sources(&terrain)
            .iter()
            .all(Option::is_none)
    );
    environment.request_present();
    assert!(!environment.scene_needed(&terrain, &camera, CullingMode::FrustumAndHiZ, state));
    assert!(environment.needs_redraw(&terrain, &camera, CullingMode::FrustumAndHiZ, state));
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let mut budget = fresh_budget();
    let presented = environment
        .encode(
            &gpu.queue,
            &mut encoder,
            &mut terrain,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
            state,
            &mut budget,
            |_| panic!("output-only redraw must retain the cached scene"),
        )
        .unwrap();
    assert_eq!(presented.passes, 1);
    assert_eq!(presented.upload_bytes, 0);
    assert_eq!(presented.post.histogram_samples, 0);
    assert_eq!(
        (presented.celestial.draws, presented.celestial.vertices),
        (0, 0)
    );
    assert_eq!(presented.post.bloom_texels, 0);
    assert_eq!(presented.post.tone_pixels, u64::from(W) * u64::from(H));
    assert_eq!(
        environment
            .timing_sources(&terrain)
            .iter()
            .flatten()
            .count(),
        1
    );
    let repeated = capture(&gpu, &mut encoder, &output);
    gpu.queue.submit([encoder.finish()]);
    assert_eq!(read(&gpu, &repeated), read(&gpu, &pixels));
    assert!(!environment.needs_redraw(&terrain, &camera, CullingMode::FrustumAndHiZ, state));
    terrain.take_work();
    environment.water_mut().unwrap().take_work();
    environment
        .resize(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            W / 2,
            H / 2,
            &mut upload,
        )
        .unwrap();
    assert_eq!(terrain.take_work().pipeline_compilations, 0);
    assert_eq!(
        environment
            .water_mut()
            .unwrap()
            .take_work()
            .pipeline_compilations,
        0
    );
    assert!(environment.needs_redraw(&terrain, &camera, CullingMode::FrustumAndHiZ, state));
    println!(
        "M10_ENVIRONMENT_GPU_OK samples={} passes={} actual_timed_passes={} nonzero_GPU_durations={} static_passes=0 static_upload_bytes=0 budget_refusal_atomic=true resize_pipeline_compilations=0",
        sample_count, first.passes, actual_passes, nonzero
    );
}

#[test]
fn exposure_only_environment_frames_reuse_hdr_without_geometry_or_histogram_work() {
    use render::{
        atmosphere::AtmosphereConfig,
        environment_gpu::{EnvironmentBudget, EnvironmentConfig, EnvironmentGpu, EnvironmentState},
    };
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 1,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![255; 4],
            tint_mask: vec![255],
        }],
    };
    let mut terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 2,
            max_quads: 65536,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let state = EnvironmentState {
        eye: [26., 31., 29.],
        sun: [0.35, 0.8, 0.2],
        sun_intensity: 1.,
        altitude_km: 0.072,
        time_seconds: 0.,
        delta_seconds: 1. / 60.,
        tick: 0,
        underwater: false,
    };
    let config = EnvironmentConfig {
        atmosphere: AtmosphereConfig {
            transmittance_size: [16, 8],
            sky_size: [16, 8],
            transmittance_steps: 16,
            sky_steps: 16,
        },
        post: PostConfig {
            bloom: None,
            ..PostConfig::default()
        },
        ..EnvironmentConfig::default()
    };
    let mut upload = UploadBudget::new(1_000_000);
    let mut environment = EnvironmentGpu::new(
        &gpu.device,
        &gpu.queue,
        &mut terrain,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        config,
        state,
        None,
        &mut upload,
    )
    .unwrap();
    terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            &cube(false),
            &mut upload,
        )
        .unwrap();
    let camera = Camera::perspective(
        state.eye,
        [8.; 3],
        [0., 1., 0.],
        1.,
        W as f32 / H as f32,
        0.1,
        200.,
    );
    let output = output(&gpu);
    let view = output.create_view(&Default::default());
    let mut budget = EnvironmentBudget {
        aerial_steps: 1_000_000,
        aerial_samples: 1_000_000,
        upload_bytes: 10000,
        atmosphere_steps: 100000,
        water_reflection_steps: 0,
        cloud_density_samples: 0,
        gi_rays: 0,
        post_pixels: 100000,
    };
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let first = environment
        .encode(
            &gpu.queue,
            &mut encoder,
            &mut terrain,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
            state,
            &mut budget,
            |_| 0,
        )
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(first.terrain.passes > 0 && first.post.histogram_samples > 0);
    assert!(environment.needs_redraw(&terrain, &camera, CullingMode::FrustumAndHiZ, state));
    terrain.take_work();
    let mut budget = EnvironmentBudget {
        aerial_steps: 1_000_000,
        aerial_samples: 1_000_000,
        upload_bytes: 0,
        atmosphere_steps: 0,
        water_reflection_steps: 0,
        cloud_density_samples: 0,
        gi_rays: 0,
        post_pixels: 100000,
    };
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let second = environment
        .encode(
            &gpu.queue,
            &mut encoder,
            &mut terrain,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
            state,
            &mut budget,
            |_| panic!("exposure must not redraw geometry"),
        )
        .unwrap();
    let sources = environment.timing_sources(&terrain);
    assert_eq!(sources.iter().filter(|s| s.is_some()).count(), 1);
    assert!(
        sources[0].is_none(),
        "old terrain timestamps must not enter this frame's measurements"
    );
    assert!(sources.into_iter().flatten().all(|source| {
        source.kinds[..source.count as usize]
            .iter()
            .all(|&kind| kind == 11 || kind == 13)
    }));
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert_eq!(
        (
            second.terrain.passes,
            second.atmosphere.passes,
            second.upload_bytes,
            second.post.histogram_samples,
            second.post.bloom_texels
        ),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(second.post.passes, 2);
    assert_eq!(terrain.take_work().upload_bytes, 0);
    println!(
        "M10_POST_ONLY_GPU_OK first_histogram_samples={} cached_scene_passes=0 cached_histogram_samples=0 cached_upload_bytes=0 exposure_tone_passes=2 timing_sources=1 stale_terrain_timestamps=0",
        first.post.histogram_samples
    );
}

#[test]
fn physical_atmosphere_shares_surface_energy_and_removes_white_skylight_at_night() {
    use render::{
        atmosphere::{AtmosphereConfig, SOLAR_IRRADIANCE, reference_transmittance},
        environment_gpu::{EnvironmentBudget, EnvironmentConfig, EnvironmentGpu, EnvironmentState},
        light_volume::{LightVolumeGpu, PackedLight},
        material_pack::{Limits, MaterialPackGpu, PreparedMaterials},
        shadows::{ShadowConfig, ShadowGpu},
    };
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 1,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![255; 4],
            tint_mask: vec![255],
        }],
    };
    let make = || {
        TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            W,
            H,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 4,
                max_quads: 65536,
                occlusion: true,
            },
            &atlas,
        )
        .unwrap()
    };
    let mut terrain = make();
    let mut legacy = make();
    let mut upload = UploadBudget::new(2_000_000);
    let base = terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            &cube(false),
            &mut upload,
        )
        .unwrap();
    legacy
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            &cube(false),
            &mut upload,
        )
        .unwrap();
    let prepared = PreparedMaterials::prepare(&atlas, &[], 1, Limits::default()).unwrap();
    let pack = MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap();
    terrain
        .prepare_material_pack(&gpu.device, &gpu.queue, pack, true)
        .unwrap();
    let mut lights = LightVolumeGpu::new(&gpu.device, 4).unwrap();
    lights
        .upload_uniform(
            &gpu.queue,
            base,
            1,
            PackedLight::new(15, 0, [0; 3]).unwrap(),
            &mut upload,
        )
        .unwrap();
    let mut shadows = ShadowGpu::new(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        ShadowConfig {
            resolution: 32,
            max_updates_per_frame: 3,
            ..ShadowConfig::default()
        },
        [8.; 3],
        [0., 1., 0.],
    )
    .unwrap();
    legacy
        .attach_lighting(&gpu.device, &shadows, &lights)
        .unwrap();
    let state = EnvironmentState {
        eye: [26., 31., 29.],
        sun: [0., 1., 0.],
        sun_intensity: 1.,
        altitude_km: 0.072,
        time_seconds: 0.,
        delta_seconds: 1. / 60.,
        tick: 0,
        underwater: false,
    };
    terrain.set_material_view(&gpu.queue, state.eye, 0).unwrap();
    let config = EnvironmentConfig {
        atmosphere: AtmosphereConfig {
            transmittance_size: [64, 32],
            sky_size: [48, 32],
            transmittance_steps: 64,
            sky_steps: 48,
        },
        post: PostConfig {
            exposure: Exposure::Manual(0.15),
            bloom: None,
            ..PostConfig::default()
        },
        ..EnvironmentConfig::default()
    };
    let mut environment = EnvironmentGpu::new(
        &gpu.device,
        &gpu.queue,
        &mut terrain,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        config,
        state,
        Some((&shadows, &lights)),
        &mut upload,
    )
    .unwrap();
    let camera = Camera::perspective(
        state.eye,
        [8.; 3],
        [0., 1., 0.],
        1.,
        W as f32 / H as f32,
        0.1,
        200.,
    );
    let output = output(&gpu);
    let view = output.create_view(&Default::default());
    let budget = || EnvironmentBudget {
        aerial_steps: 1_000_000,
        aerial_samples: 1_000_000,
        upload_bytes: 100000,
        atmosphere_steps: 1_000_000,
        water_reflection_steps: 0,
        cloud_density_samples: 0,
        gi_rays: 0,
        post_pixels: 100000,
    };
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    shadows.encode(&gpu.queue, &mut encoder);
    environment
        .encode(
            &gpu.queue,
            &mut encoder,
            &mut terrain,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
            state,
            &mut budget(),
            |_| 0,
        )
        .unwrap();
    let day = capture(&gpu, &mut encoder, &output);
    let hdr = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent actual terrain coverage"),
        size: u64::from(W * H * 8),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: terrain.resolved_hdr_texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &hdr,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 8),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    let illumination = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent atmosphere shared-light oracle"),
        size: 80,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_buffer_to_buffer(
        environment.atmosphere_lighting_buffer(),
        0,
        &illumination,
        0,
        80,
    );
    gpu.queue.submit([encoder.finish()]);
    let rgba = |buffer: &wgpu::Buffer| {
        read(&gpu, buffer)
            .chunks_exact(((W * 4).div_ceil(256) * 256) as usize)
            .flat_map(|row| row[..(W * 4) as usize].iter().copied())
            .collect::<Vec<_>>()
    };
    let day = rgba(&day);
    let coverage = read(&gpu, &hdr);
    let raw = read(&gpu, &illumination);
    let values: Vec<_> = raw
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect();
    let expected = reference_transmittance(0.072, 1., 4096);
    let mut max_sun_error = 0f64;
    for channel in 0..3 {
        max_sun_error = max_sun_error.max(
            (f64::from(values[12 + channel]) - expected[channel] * f64::from(SOLAR_IRRADIANCE))
                .abs(),
        );
    }
    assert!(
        max_sun_error < 0.04,
        "solar irradiance mismatch={max_sun_error}, actual={:?}, reference={expected:?}",
        &values[12..15]
    );
    let red = (values[0] * 0.2820948 + values[1] * 0.48860252).max(0.);
    let blue = (values[8] * 0.2820948 + values[9] * 0.48860252).max(0.);
    assert!(
        blue > red && red > 0.,
        "surface ambient must come from the same colored sky integral"
    );
    let receivers: Vec<_> = coverage
        .chunks_exact(8)
        .enumerate()
        .filter(|(index, p)| {
            let _ = index;
            u16::from_le_bytes([p[6], p[7]]) == 0x3c00
        })
        .map(|(i, _)| i)
        .collect();
    assert!(
        receivers.len() > 1000,
        "fixture must cover a substantial receiver area: {}",
        receivers.len()
    );
    assert!(
        receivers
            .iter()
            .any(|&i| day[i * 4..i * 4 + 3].iter().any(|&c| c > 80)),
        "daylight must actually illuminate receivers"
    );
    let night = EnvironmentState {
        sun: [0., -1., 0.],
        tick: 1,
        ..state
    };
    shadows.update_view([8.; 3], night.sun).unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    shadows.encode(&gpu.queue, &mut encoder);
    environment
        .encode(
            &gpu.queue,
            &mut encoder,
            &mut terrain,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
            night,
            &mut budget(),
            |_| 0,
        )
        .unwrap();
    let dark = capture(&gpu, &mut encoder, &output);
    encoder.copy_buffer_to_buffer(
        environment.atmosphere_lighting_buffer(),
        0,
        &illumination,
        0,
        80,
    );
    legacy
        .encode_frame(
            &gpu.queue,
            &mut encoder,
            &view,
            &camera,
            CullingMode::FrustumAndHiZ,
        )
        .unwrap();
    let control = capture(&gpu, &mut encoder, &output);
    gpu.queue.submit([encoder.finish()]);
    let dark = rgba(&dark);
    let control = rgba(&control);
    let mut leaks = 0;
    let mut old_glow = 0;
    for &i in &receivers {
        leaks += usize::from(dark[i * 4..i * 4 + 3].iter().any(|&c| c != 0));
        old_glow += usize::from(control[i * 4..i * 4 + 3].iter().any(|&c| c > 20));
    }
    assert_eq!(
        leaks, 0,
        "skyVisibility15 alone must not illuminate a night-side material"
    );
    assert!(
        old_glow > 1000,
        "old white-skylight negative control must fail the same black-receiver criterion"
    );
    let raw = read(&gpu, &illumination);
    for i in (0..12).chain(12..15) {
        assert_eq!(
            f32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap()),
            0.
        );
    }
    println!(
        "M10_SHARED_ATMOSPHERE_GPU_OK receivers={} diffuse_sky_blue_gt_red=true max_solar_irradiance_error={:.8} solar_irradiance={} night_surface_leaks=0 old_white_skylight_negative_pixels={} PBR=true",
        receivers.len(),
        max_sun_error,
        SOLAR_IRRADIANCE,
        old_glow
    );
}
