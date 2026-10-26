use client::props_source::{PropsLimits, PropsSource, VisibleRegion};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use render::{
    block_models::{BlockConfig, BlockModelsGpu},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn point(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 4,
        z: 8,
    }
}
fn region(page: PageId) -> Option<VisibleRegion> {
    Some(VisibleRegion {
        min: page,
        max: page,
    })
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    for x in [0, 16] {
        tx.create_page(point(x).page(), vec![Cell::AIR; 4096]);
        tx.set_cell(point(x), Cell::from_parts(rules::ids::CHEST, 0));
    }
    world.commit(tx).unwrap();
    world
}
fn limits() -> PropsLimits {
    PropsLimits {
        max_pages: 4,
        max_instances: 16,
        max_running: 2,
        max_ready: 2,
        max_age_ticks: 40,
    }
}
fn finish(source: &mut PropsSource, world: &World) {
    while let Some(task) = source.take_job(world, |_| Box::new([u32::MAX; 256])) {
        assert!(
            source
                .complete(world, task.execute(|| false).unwrap())
                .is_ok()
        );
        source.pump_ready(world, 16).unwrap();
    }
}
#[test]
fn outside_interest_worker_completion_cannot_publish_and_unchanged_policy_is_allocation_free() {
    let mut world = fixture();
    let mut source = PropsSource::attach(&mut world, limits()).unwrap();
    let mut tasks = Vec::new();
    while let Some(task) = source.take_job(&world, |_| Box::new([u32::MAX; 256])) {
        tasks.push(task);
    }
    assert_eq!(tasks.len(), 2);
    source
        .set_visible_region(region(point(0).page()), Tick(2))
        .unwrap();
    for task in tasks {
        assert!(
            source
                .complete(&world, task.execute(|| false).unwrap())
                .is_ok()
        );
    }
    source.ingest(&mut world, 4, |_| u32::MAX).unwrap();
    source.pump_ready(&world, 16).unwrap();
    assert_eq!(source.stats(&world).models, 1);
    assert_eq!(source.take_work().jobs_stale, 1);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert!(
            !source
                .set_visible_region(region(point(0).page()), Tick(3))
                .unwrap()
        );
        assert_eq!(source.ingest(&mut world, 8, |_| u32::MAX).unwrap(), 0);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
}
#[test]
fn actual_gpu_retires_only_indexed_outside_models_then_reuses_slots_for_new_visible_page() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            64,
            64,
            render::OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mut models = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            BlockConfig {
                max_instances: 16,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        models
            .reserve_slots(&gpu.queue, 16, &mut UploadBudget::new(1024))
            .unwrap();
        let mut world = fixture();
        let mut source = PropsSource::attach(&mut world, limits()).unwrap();
        source
            .set_visible_region(region(point(0).page()), Tick(1))
            .unwrap();
        source.ingest(&mut world, 4, |_| u32::MAX).unwrap();
        finish(&mut source, &world);
        source
            .flush(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(1024),
                16,
            )
            .unwrap();
        assert_eq!(models.active_instances(), 1);
        source.take_work();
        models.take_work();
        source
            .set_visible_region(region(point(16).page()), Tick(2))
            .unwrap();
        source.watch_page(point(16).page(), Tick(2)).unwrap();
        assert_eq!(source.ingest(&mut world, 1, |_| u32::MAX).unwrap(), 1);
        assert_eq!(source.take_work().visibility_members, 1);
        finish(&mut source, &world);
        source.ingest(&mut world, 4, |_| u32::MAX).unwrap();
        assert_eq!(
            source
                .flush(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(128),
                    16
                )
                .unwrap(),
            2
        );
        assert_eq!(models.active_instances(), 1);
        source.take_work();
        models.take_work();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            assert_eq!(source.ingest(&mut world, 8, |_| u32::MAX).unwrap(), 0);
            assert_eq!(
                source
                    .flush(
                        &gpu.queue,
                        &mut models,
                        [0; 3],
                        &mut UploadBudget::new(1024),
                        16
                    )
                    .unwrap(),
                0
            );
        }
        let counts = scope.finish();
        assert_eq!(counts.allocating_calls(), 0);
        assert_eq!(counts.dealloc_calls, 0);
        assert_eq!(models.take_work().upload_bytes, 0);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
    });
}
