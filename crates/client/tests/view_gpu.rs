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
