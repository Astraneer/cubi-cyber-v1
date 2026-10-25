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
