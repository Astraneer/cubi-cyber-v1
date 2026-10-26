#![cfg(not(target_arch = "wasm32"))]
//! Completed pixels and presentation stamps remain old during a UI-only frame.
use client::{
    ClientScene, SceneConfig, native_pool,
    view::{ClientView, EnvironmentProfile},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    post::{Exposure, PostConfig},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};

fn pixels(gpu: &render::Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cached presentation completed pixels"),
        size: 64 * 64 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (send, receive) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            send.send(result).unwrap();
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receive.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
#[test]
fn cached_tone_preserves_physical_pixels_camera_world_and_edit_stamps() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 32,
            max_quads: 262144,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let center = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 8,
        z: 8,
    };
    let mut view = ClientView::new(
        renderer,
        SceneConfig {
            center,
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 32,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    let mut lighting =
        client::lighting::ClientLightingConfig::default().with_authoritative_source();
    lighting.shadows.resolution = 32;
    lighting.shadows.max_updates_per_frame = 3;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        64,
        64,
        OutputTransfer::Attachment,
        EnvironmentProfile {
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                post: PostConfig {
                    exposure: Exposure::Manual(1.),
                    bloom: None,
                    ..Default::default()
                },
                ..Default::default()
            },
            celestials: None,
            aerial: None,
            multiple_scattering: None,
            ..Default::default()
        },
        &atlas,
    )
    .unwrap();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cached presentation target"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(!view.encode_cached_present(&mut encoder, &output).unwrap());
    assert!(!view.has_cached_present());
    assert!(
        !view.initial_scene_inputs_ready(),
        "Unknown near inputs cannot start HDR"
    );
    for _ in 0..20 {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode_loading_clear(&mut encoder, &output);
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        assert!(!view.is_ready());
        assert!(!view.has_cached_present());
        assert_eq!(view.stats().terrain_frames, 0);
        assert_eq!(view.rendered_world_version(), Version(0));
    }
    assert!(
        pixels(&gpu, &texture)
            .chunks_exact(4)
            .all(|p| p == [5, 6, 8, 255]),
        "completed Loading clear is the only output"
    );
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if page == center.page() {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * z] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
        }
        view.scene
            .import_authoritative_page(
                &net::PageUpdate::Snapshot(net::PageSnapshot {
                    page,
                    version: 1,
                    source_version: Version(1),
                    applied_action: Default::default(),
                    cells,
                }),
                Tick(1),
            )
            .unwrap();
        view.scene
            .import_authoritative_light(
                &net::light::Update::Snapshot(net::light::Snapshot {
                    header: net::light::Header {
                        page,
                        cell_view: 1,
                        version: 1,
                        source: Version(2),
                    },
                    values: Box::new([net::light::pack(0, 15, [0; 3]).unwrap(); 4096]),
                }),
                Tick(2),
            )
            .unwrap();
    }
    let pool = native_pool(2, 8).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut first_scene = false;
    while !view.is_ready() {
        assert!(
            Instant::now() < deadline,
            "baseline never became ready: {:?}",
            view.stats()
        );
        view.scene
            .pump_native(&pool, Tick(2), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        if view.initial_scene_inputs_ready() {
            if !first_scene {
                assert!(
                    !view.is_ready(),
                    "CPU inputs do not replace actual shadow/content presentation"
                );
                assert!(!view.has_cached_present());
                first_scene = true;
            }
            view.encode(&gpu.queue, &mut encoder, &output, 64, 64)
                .unwrap();
        } else {
            view.encode_loading_clear(&mut encoder, &output);
        }
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
    }
    assert!(
        first_scene,
        "initial CPU gate must not wait on its first shadow encode"
    );
    let baseline = pixels(&gpu, &texture);
    assert!(view.has_cached_present());
    let stamps = (
        view.rendered_camera_signature(),
        view.rendered_world_version(),
        view.rendered_upload_bytes(),
        view.rendered_edit_revision(),
        view.stats().terrain_frames,
    );
    let mut settings = ui::settings::Settings::default();
    settings.set(ui::settings::Setting::Fov, 90).unwrap();
    settings.set(ui::settings::Setting::Exposure, 100).unwrap();
    view.apply_settings(&settings);
    let mut cells = Box::new([Cell::AIR; 4096]);
    cells[8 + 16 * 8] = Cell::from_parts(rules::ids::STONE, 0);
    view.scene
        .import_authoritative_page(
            &net::PageUpdate::Snapshot(net::PageSnapshot {
                page: center.page(),
                version: 2,
                source_version: Version(3),
                applied_action: Default::default(),
                cells,
            }),
            Tick(3),
        )
        .unwrap();
    view.scene
        .import_authoritative_light(
            &net::light::Update::Snapshot(net::light::Snapshot {
                header: net::light::Header {
                    page: center.page(),
                    cell_view: 2,
                    version: 2,
                    source: Version(4),
                },
                values: Box::new([net::light::pack(0, 15, [0; 3]).unwrap(); 4096]),
            }),
            Tick(3),
        )
        .unwrap();
    assert_ne!(view.current_camera_signature(64, 64), stamps.0);
    assert_ne!(view.scene.world().version(), stamps.1);
    for _ in 0..20 {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert!(view.encode_cached_present(&mut encoder, &output).unwrap());
        gpu.queue.submit([encoder.finish()]);
        assert_eq!(
            pixels(&gpu, &texture),
            baseline,
            "cached tone changed old HDR pixels"
        );
        assert_eq!(
            (
                view.rendered_camera_signature(),
                view.rendered_world_version(),
                view.rendered_upload_bytes(),
                view.rendered_edit_revision(),
                view.stats().terrain_frames
            ),
            stamps
        );
        let work = view.environment_stats().unwrap().gpu;
        assert_eq!(
            (
                work.passes,
                work.post.tone_pixels,
                work.upload_bytes,
                work.terrain.passes
            ),
            (1, 4096, 0, 0)
        );
        assert_eq!(
            (work.post.histogram_samples, work.post.bloom_texels),
            (0, 0)
        );
        assert!(
            view.needs_redraw(64, 64),
            "cached tone consumed unshown scene dirtiness"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while !view.is_ready() || view.rendered_world_version() != view.scene.world().version() {
        assert!(
            Instant::now() < deadline,
            "new scene did not replace the old presentation"
        );
        view.scene
            .pump_native(&pool, Tick(3), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 64, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
    }
    assert_ne!(
        pixels(&gpu, &texture),
        baseline,
        "negative control did not draw the changed scene"
    );
    assert_eq!(
        view.rendered_camera_signature(),
        view.current_camera_signature(64, 64)
    );
    let settled = view.stats().terrain_frames;
    view.invalidate();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 64, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert_eq!(
        view.stats().terrain_frames,
        settled,
        "post-only frame counted stale terrain work"
    );
    assert!(
        !view.needs_redraw(64, 64),
        "post-only request caused an endless redraw loop"
    );
    let unscaled = pixels(&gpu, &texture);
    settings.set(ui::settings::Setting::Scale, 85).unwrap();
    let receipt = view.apply_settings(&settings);
    let scale_bit = 1 << ui::settings::Setting::Scale as u32;
    assert_eq!(receipt.applied & scale_bit, 0);
    assert_ne!(receipt.reload_required & scale_bit, 0);
    view.resize(&gpu.device, 64, 64).unwrap();
    assert_eq!(view.render_scale(), 85);
    assert_eq!(view.render_extent(), [55, 55]);
    assert_eq!(view.render_sample_count(), 4);
    assert_ne!(view.settings_application().applied & scale_bit, 0);
    assert!(!view.has_cached_present());
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 64, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    let scaled = pixels(&gpu, &texture);
    assert!(scaled.chunks_exact(4).all(|p| p[3] == 255));
    assert_ne!(
        scaled, unscaled,
        "scale did not change the rendered scene sampling"
    );
    for _ in 0..4 {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert!(view.encode_cached_present(&mut encoder, &output).unwrap());
        gpu.queue.submit([encoder.finish()]);
        assert_eq!(pixels(&gpu, &texture), scaled);
        let work = view.environment_stats().unwrap().gpu;
        assert_eq!(
            work.post.tone_pixels, 4096,
            "native output must remain charged"
        );
        assert_eq!(work.terrain.passes, 0);
        assert_eq!(work.upload_bytes, 0);
    }
    settings.set(ui::settings::Setting::Scale, 100).unwrap();
    view.apply_settings(&settings);
    view.resize(&gpu.device, 64, 64).unwrap();
    assert_eq!(view.render_extent(), [64, 64]);
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 64, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert_eq!(
        pixels(&gpu, &texture),
        unscaled,
        "100% roundtrip changed scene"
    );
    view.resize(&gpu.device, 32, 32).unwrap();
    assert!(!view.has_cached_present());
}
