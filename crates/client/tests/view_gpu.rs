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
    config.vertical_radius = 0;
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
    config.center.y = 72;
    config.horizontal_radius = 0;
    config.vertical_radius = 0;
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
        assert!(Instant::now() < deadline, "entity loading must converge");
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
    assert!(after.dynamic_shadow_copy_bytes > before.dynamic_shadow_copy_bytes);
    assert_eq!(view.entity_stats().unwrap().cpu_vertices, 0);
    assert!(view.entity_stats().unwrap().instance_records > 0);
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
