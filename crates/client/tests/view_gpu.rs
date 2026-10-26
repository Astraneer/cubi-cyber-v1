//! Actual-GPU scope: the static branch retains the submitted image and
//! therefore avoids creating wgpu encoders/passes. Dynamic draws are measured
//! separately and are not claimed allocation-free.
use client::{ClientScene, SceneConfig, native_pool, view::ClientView};
use core_types::Tick;
use render::terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget};
use std::{
    alloc::System,
    hint::black_box,
    time::{Duration, Instant},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<System> = telemetry::CountingAllocator::new(System);

#[test]
fn static_view_skips_gpu_dispatch_without_allocations_and_positive_control_is_observed() {
    let instance = render::instance();
    let gpu =
        pollster::block_on(render::Gpu::request(&instance, None, true)).expect("real GPU adapter");
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        format,
        128,
        96,
        render::OutputTransfer::Attachment,
        GpuConfig::default(),
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let mut config = SceneConfig::forest();
    config.horizontal_radius = 0;
    config.vertical_radius = 0;
    let mut view = ClientView::new(renderer, config).unwrap();
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !view.scene.is_ready() || view.stats().resident_pages != 1 {
        assert!(
            Instant::now() < deadline,
            "loaded CPU and GPU page must complete"
        );
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        std::thread::yield_now();
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual static-view frame"),
        size: wgpu::Extent3d {
            width: 128,
            height: 96,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(
        &gpu.queue,
        &mut encoder,
        &texture.create_view(&Default::default()),
        128,
        96,
    )
    .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(view.is_ready());
    let actual_work = view.take_frame_work();
    let details = view.last_frame_work_details();
    assert!(details.renderer.upload_bytes >= 192, "actual camera write");
    assert!(details.workers.cells_scanned > 0, "completed mesh work");
    assert!(details.workers.quads_emitted > 0, "completed mesh output");
    assert_eq!(
        actual_work.quads_emitted,
        details.workers.quads_emitted + details.foreground.quads_emitted,
        "GPU descriptor copies must not count as a second CPU emission"
    );
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
    assert!(!view.needs_redraw(128, 96));
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        view.advance_input(0.05).unwrap();
        let tick = view.tick();
        view.scene
            .pump_native(&workers, tick, &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        assert!(!black_box(view.needs_redraw(128, 96)));
    }
    let measured = scope.finish();
    assert_eq!(
        measured.allocating_calls(),
        0,
        "static client tick/pump/upload/dirty check"
    );
    assert_eq!(measured.dealloc_calls, 0);
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
    let scope = telemetry::AllocationScope::begin().unwrap();
    let control = black_box(vec![black_box(1_u8); 4096]);
    black_box(&control);
    drop(control);
    let positive = scope.finish();
    assert!(positive.allocating_calls() > 0 && positive.dealloc_calls > 0);
    view.invalidate();
    assert!(
        view.needs_redraw(128, 96),
        "positive dirty control must demand a real frame"
    );
}

#[test]
fn prepared_view_environment_reuses_hdr_for_exposure_and_captures_only_current_queries() {
    use client::view::EnvironmentProfile;
    use render::{
        atmosphere::AtmosphereConfig,
        environment_gpu::EnvironmentConfig,
        post::{Exposure, PostConfig},
    };
    use telemetry::flight::{FlightRecorder, Sample};

    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig::default(),
        &atlas,
    )
    .unwrap();
    let mut config = SceneConfig::forest();
    config.horizontal_radius = 0;
    config.vertical_radius = 0;
    let mut view = ClientView::new(renderer, config).unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        96,
        64,
        render::OutputTransfer::Attachment,
        EnvironmentProfile {
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                post: PostConfig {
                    exposure: Exposure::Automatic {
                        key: 0.18,
                        min_ev: -8.,
                        max_ev: 8.,
                        brighten_seconds: 0.01,
                        darken_seconds: 0.01,
                    },
                    bloom: None,
                    ..PostConfig::default()
                },
                ..EnvironmentConfig::default()
            },
            ..EnvironmentProfile::default()
        },
        &atlas,
    )
    .unwrap();
    view.prepare_entities(&gpu.device, &gpu.queue).unwrap();
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !view.scene.is_ready() || view.stats().resident_pages != 1 {
        assert!(Instant::now() < deadline);
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        std::thread::yield_now();
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shared environment actual output"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let mut capture = client::capture::FrameGpuCapture::new(&gpu.device);
    let mut recorder = FlightRecorder::new_full_capture(4, 4).unwrap();
    for frame in 1..=2 {
        view.advance_input(0.05).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        assert!(view.needs_redraw(96, 64));
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let terrain = view
            .encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        let work = view.environment_stats().unwrap().gpu;
        assert_eq!(view.stats().frame_pipeline_compilations, 0);
        assert_eq!(terrain.passes > 0, frame == 1);
        assert!(work.post.passes > 0 && work.post.tone_pixels == 96 * 64);
        assert_eq!(work.atmosphere.passes > 0, frame == 1);
        if frame == 2 {
            assert_eq!(work.post.histogram_samples, 0, "cached HDR histogram");
            assert_eq!(view.stats().terrain_frames, 1, "post-only image");
        }
        recorder
            .record_frame(Sample {
                frame,
                monotonic_ns: frame * 50_000_000,
                ..Sample::default()
            })
            .unwrap();
        let ticket = view.encode_gpu_capture(&mut capture, &mut encoder, frame);
        gpu.queue.submit([encoder.finish()]);
        if let Some(ticket) = ticket {
            capture.submitted(ticket);
        }
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        capture.collect(&mut recorder);
    }
    assert!(view.is_ready());
    assert!(!view.needs_redraw(96, 64), "exposure tail converged");
    assert_eq!(capture.pending(), 0);
    assert_eq!(capture.stats().failed, 0);
    if gpu
        .device
        .features()
        .contains(wgpu::Features::TIMESTAMP_QUERY)
    {
        let first = recorder.frames().find(|sample| sample.frame == 1).unwrap();
        let second = recorder.frames().find(|sample| sample.frame == 2).unwrap();
        assert!(first.gpu_ns.unwrap() > 0 && second.gpu_ns.unwrap() > 0);
        assert!(first.gpu_passes_ns[6].is_some());
        assert!(second.gpu_passes_ns[6].is_none(), "no stale terrain query");
        assert!(second.gpu_passes_ns[11].is_some());
        assert!(second.gpu_passes_ns[13].is_some());
    } else {
        assert!(recorder.frames().all(|sample| sample.gpu_ns.is_none()));
    }
    let _ = view.take_frame_work();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        view.advance_input(0.05).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        assert!(!black_box(view.needs_redraw(96, 64)));
    }
    let static_work = scope.finish();
    assert_eq!(static_work.allocating_calls(), 0);
    assert_eq!(static_work.dealloc_calls, 0);
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
    // The UI may acquire a fresh surface while the world remains entirely
    // clean. View.encode must still supply its background before the UI pass.
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let terrain = view
        .encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    let presentation = view.environment_stats().unwrap().gpu;
    assert_eq!(terrain.passes, 0);
    assert_eq!(presentation.passes, 1);
    assert_eq!(presentation.post.histogram_samples, 0);
    assert_eq!(presentation.post.bloom_texels, 0);
    assert_eq!(presentation.upload_bytes, 0);
    assert!(!view.needs_redraw(96, 64));
    let pipelines = view.stats().pipeline_compilations;
    view.resize(&gpu.device, 80, 60).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    assert!(view.needs_redraw(80, 60));
    assert_eq!(view.stats().pipeline_compilations, pipelines);
}

#[test]
fn shared_view_drives_known_sky_lighting_and_reuses_static_shadow_cache() {
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true)).unwrap();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        format,
        96,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig::default(),
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let mut config = SceneConfig::forest();
    config.horizontal_radius = 0;
    config.vertical_radius = 1;
    let mut view = ClientView::new(renderer, config).unwrap();
    let mut lighting = client::lighting::ClientLightingConfig::standalone();
    lighting.shadows.resolution = 64;
    lighting.shadows.max_updates_per_frame = 3;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shared view actual light and shadow path"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !view.is_ready() {
        assert!(
            Instant::now() < deadline,
            "bounded light owner must converge: {:?}",
            view.lighting_stats()
        );
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        std::thread::yield_now();
    }
    let lit = view.lighting_stats().unwrap();
    let sky = core_types::BlockPos {
        space: core_types::SpaceId(0),
        x: 96,
        y: 126,
        z: 96,
    };
    assert_eq!(
        view.scene
            .world()
            .page(sky.page())
            .unwrap()
            .light(sky.local_index())
            .sky,
        15,
        "the explicit profile ceiling supplies real skylight through known air"
    );
    assert!(lit.cpu_cells > 0 && lit.upload_bytes > 0 && lit.shadow_draws > 0);
    assert_eq!(lit.pending_shadow_cascades, 0);
    assert!(lit.ready);
    let _ = view.take_frame_work();
    assert!(!view.needs_redraw(96, 64));
    for _ in 0..4 {
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        assert!(!view.needs_redraw(96, 64));
    }
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
    assert_eq!(
        view.lighting_stats().unwrap().shadow_draws,
        lit.shadow_draws
    );
}

#[test]
fn authoritative_view_requires_light_bindings_and_cpu_ingest_never_writes_gpu() {
    use core_types::{BlockPos, Cell, SpaceId, Version};
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true)).unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig::default(),
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let at = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 8,
        z: 8,
    };
    let config = SceneConfig {
        center: at,
        horizontal_radius: 0,
        vertical_radius: 0,
        authoritative_remote: true,
        ..SceneConfig::forest()
    };
    let mut view = ClientView::new(renderer, config).unwrap();
    let mut lighting =
        client::lighting::ClientLightingConfig::default().with_authoritative_source();
    lighting.shadows.resolution = 32;
    lighting.shadows.max_updates_per_frame = 3;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
    let pages = view.scene.requested_pages().collect::<Vec<_>>();
    let cells = |page, version| {
        let mut values = Box::new([Cell::AIR; 4096]);
        if page == at.page() {
            values[usize::from(at.local_index().get())] = Cell::from_parts(rules::ids::STONE, 0);
        }
        net::PageUpdate::Snapshot(net::PageSnapshot {
            page,
            version,
            source_version: Version(1),
            applied_action: Default::default(),
            cells: values,
        })
    };
    for page in &pages {
        view.scene
            .import_authoritative_page(&cells(*page, 1), Tick(0))
            .unwrap();
    }
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !view.scene.is_ready() || view.stats().resident_pages != 1 {
        assert!(Instant::now() < deadline);
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        std::thread::yield_now();
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("authoritative LIGHT view binding"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(!view.is_ready());
    assert_eq!(view.lighting_stats().unwrap().pending_source_pages, 27);
    let _ = view.take_frame_work();
    let light = |page, cell_view, version| {
        net::light::Update::Snapshot(net::light::Snapshot {
            header: net::light::Header {
                page,
                cell_view,
                version,
                source: Version(10),
            },
            values: Box::new([net::light::pack(15, 3, [231, 17, 255]).unwrap(); 4096]),
        })
    };
    for page in &pages {
        view.scene
            .import_authoritative_light(&light(*page, 1, 1), Tick(0))
            .unwrap();
    }
    view.advance_input(0.).unwrap();
    assert_eq!(
        view.take_frame_work().upload_bytes,
        0,
        "CPU ingest is independent of in-flight GPU publication"
    );
    assert!(view.lighting_stats().unwrap().pending_pages > 0);
    while !view.is_ready() {
        assert!(
            Instant::now() < deadline,
            "certified GPU volume must settle: {:?}",
            view.lighting_stats()
        );
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
    }
    assert_eq!(view.lighting_stats().unwrap().pending_source_pages, 0);
    assert_eq!(
        view.lighting_stats().unwrap().cpu_cells,
        0,
        "authoritative mode never runs a client flood solver"
    );
    let meshes = view.stats().meshes_published;
    view.scene
        .import_authoritative_page(&cells(at.page(), 2), Tick(1))
        .unwrap();
    assert!(
        !view.is_ready(),
        "a current cell view without matching LIGHT is immediately unconfirmed"
    );
    view.advance_input(0.).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    assert_eq!(view.lighting_stats().unwrap().pending_source_pages, 1);
    view.scene
        .import_authoritative_light(&light(at.page(), 2, 2), Tick(1))
        .unwrap();
    view.advance_input(0.).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(view.is_ready());
    assert_eq!(
        view.stats().meshes_published,
        meshes,
        "independent light bindings never remesh cells"
    );
}

#[test]
fn shared_view_dispatches_public_horizon_plan_and_draws_the_complete_gpu_cover() {
    fn instances(gpu: &render::Gpu, source: &wgpu::Buffer) -> u32 {
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual height draw indirect result"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(source, 0, &readback, 0, 16);
        gpu.queue.submit([encoder.finish()]);
        let (send, recv) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                send.send(result).unwrap()
            });
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        recv.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        let mapped = readback.slice(..).get_mapped_range();
        let count = u32::from_le_bytes(mapped[4..8].try_into().unwrap());
        drop(mapped);
        readback.unmap();
        count
    }
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true)).unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig::default(),
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let mut config = SceneConfig::forest();
    config.horizontal_radius = 0;
    config.vertical_radius = 0;
    let mut view = ClientView::new(renderer, config).unwrap();
    view.prepare_horizon(
        &gpu.device,
        &gpu.queue,
        client::view::horizon::Config {
            horizon_level: 3,
            max_proxy_pages: 8,
            ..Default::default()
        },
    )
    .unwrap();
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !view.scene.is_ready()
        || view.stats().resident_pages != 1
        || view.horizon_wanted_height().next().is_none()
    {
        assert!(Instant::now() < deadline);
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        std::thread::yield_now();
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shared horizon complete GPU cover"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let before = view
        .encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert_eq!(
        instances(&gpu, view.horizon_mut().unwrap().height.indirect_buffer()),
        0,
        "no public height payload means zero actual height instances"
    );
    let keys = view.horizon_wanted_height().collect::<Vec<_>>();
    assert!(!keys.is_empty());
    for key in &keys {
        let mut known = [255; 37];
        known[36] = 1;
        view.ingest_remote_height(Box::new(net::height::HeightTile {
            key: *key,
            source: 1,
            error: None,
            heights: [64; 289],
            known,
            atlas: [0; 256],
            tint: [0xffff_ffff; 256],
            protected: [0; 32],
            replacement: net::height::Replacement::None,
        }))
        .unwrap();
    }
    while view.horizon_stats().unwrap().height_front != keys.len() {
        assert!(
            Instant::now() < deadline,
            "whole horizon cover must publish"
        );
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
    }
    let stats = view.horizon_stats().unwrap();
    assert!(stats.enabled && stats.height_front > 0);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let after = view
        .encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(
        after.draw_calls >= before.draw_calls,
        "height/proxy indirect draws are attached to actual HDR pass"
    );
    assert!(
        instances(&gpu, view.horizon_mut().unwrap().height.indirect_buffer()) > 0,
        "known complete cover produces actual GPU-selected height instances"
    );
    assert!(view.is_ready());
    let _ = view.take_frame_work();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        assert!(!black_box(view.needs_redraw(96, 64)));
    }
    let idle = scope.finish();
    assert_eq!(idle.allocating_calls(), 0);
    assert_eq!(idle.dealloc_calls, 0);
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
}

#[test]
fn unit_mesh_control_converts_initial_and_final_publications() {
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true)).unwrap();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        format,
        96,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig::default(),
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let mut config = SceneConfig::forest();
    config.horizontal_radius = 0;
    config.vertical_radius = 0;
    let mut view = ClientView::new_with_rasterization(
        renderer,
        config,
        client::view::NearRasterization::LegacyGreedy,
    )
    .unwrap();
    view.set_demo_unit_mesh(true).unwrap();
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !view.scene.is_ready() || view.stats().resident_pages != 1 {
        assert!(Instant::now() < deadline);
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        std::thread::yield_now();
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unit tessellation diagnostic"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    let initial = view.stats().resident_quads;
    assert!(initial as u64 > view.scene.stats().mesh_quads);
    let _ = view.take_frame_work();
    assert_eq!(
        view.last_frame_work_details().foreground.quads_emitted,
        initial as u64
    );
    assert!(
        view.set_demo_unit_mesh(false).is_err(),
        "mixed initial/final tessellation is forbidden"
    );
    let position = core_types::BlockPos {
        space: core_types::SpaceId(0),
        x: 100,
        y: 90,
        z: 100,
    };
    assert_eq!(
        view.scene.world().read(position),
        world::CellRead::Known(core_types::Cell::AIR)
    );
    let mut tx = world::Transaction::new(Tick(1), position.space);
    tx.expect_page(
        position.page(),
        view.scene.world().page_version(position.page()),
    );
    tx.set_cell(position, core_types::Cell::from_parts(rules::ids::STONE, 0));
    view.scene.apply_transaction(tx).unwrap();
    assert!(!view.is_ready());
    while !view.is_ready() {
        assert!(Instant::now() < deadline);
        view.scene
            .pump_native(&workers, Tick(1), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        std::thread::yield_now();
    }
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert_eq!(
        view.stats().resident_quads,
        initial + 6,
        "one isolated cube adds six unit faces"
    );
    let _ = view.take_frame_work();
    assert_eq!(
        view.last_frame_work_details().foreground.quads_emitted,
        (initial + 6) as u64
    );
}

#[test]
fn entity_body_animation_preserves_static_shadow_cache_and_uses_no_cpu_vertices() {
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true)).unwrap();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        format,
        96,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig::default(),
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let mut config = SceneConfig::forest();
    config.center.y = 80;
    config.horizontal_radius = 0;
    config.vertical_radius = 1;
    let mut view = ClientView::new(renderer, config).unwrap();
    let mut lighting = client::lighting::ClientLightingConfig::standalone();
    lighting.shadows.resolution = 64;
    lighting.shadows.max_updates_per_frame = 3;
    lighting.shadows.dynamic_casters = true;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
    view.prepare_entities(&gpu.device, &gpu.queue).unwrap();
    let workers = native_pool(2, 16).unwrap();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shared GPU body and independent shadow lists"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !view.is_ready() {
        assert!(
            Instant::now() < deadline,
            "entity loading must converge: {:?}, light {:?}",
            view.stats(),
            view.lighting_stats()
        );
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
    }
    view.activate_player();
    assert!(view.stats().player_spawned);
    for _ in 0..4 {
        view.advance_input(sim::TICK_SECONDS).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
    }
    let before = view.lighting_stats().unwrap();
    assert!(
        !view.stats().self_model_rendered,
        "first person primary body is hidden"
    );
    for _ in 0..6 {
        view.advance_input(sim::TICK_SECONDS).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
    }
    let after = view.lighting_stats().unwrap();
    assert_eq!(after.static_shadow_draws, before.static_shadow_draws);
    assert_eq!(after.shadow_updates, before.shadow_updates);
    assert!(after.dynamic_shadow_draws > before.dynamic_shadow_draws);
    assert!(after.dynamic_shadow_restore_pixels > before.dynamic_shadow_restore_pixels);
    assert!(after.dynamic_shadow_restore_draws > before.dynamic_shadow_restore_draws);
    assert_eq!(
        after.dynamic_shadow_copy_bytes, 0,
        "dynamic shadow damage never copies the whole atlas"
    );
    assert_eq!(view.entity_stats().unwrap().cpu_vertices, 0);
    assert!(view.entity_stats().unwrap().instance_records > 0);
    let mut movement_report = String::from("{}");
    view.movement_replay()
        .append_json(&mut movement_report)
        .unwrap();
    let replayed = sim::replay::Archive::from_report(&movement_report)
        .unwrap()
        .replay()
        .unwrap();
    assert_eq!(replayed.steps, 10);
    assert_eq!(
        replayed.final_hash,
        sim::prediction::PredictionState::capture(view.player())
            .unwrap()
            .hash()
    );
    view.set_key(client::view::InputKey::Camera, true);
    view.set_key(client::view::InputKey::Camera, false);
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(512 * 1024))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(
        view.stats().self_model_rendered,
        "shoulder view encoded the GPU catalog body"
    );
}
