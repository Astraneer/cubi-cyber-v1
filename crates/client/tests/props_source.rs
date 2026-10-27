pub use client::block_models;
#[path = "../src/props_source.rs"]
#[allow(dead_code)]
mod source;
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use source::*;
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 4,
        z: 8,
    }
}
fn page() -> PageId {
    p(0).page()
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(page(), vec![Cell::AIR; 4096]);
    for (x, id) in [
        (2, rules::ids::CHEST),
        (4, rules::ids::DANDELION),
        (6, rules::ids::WIND_GENERATOR),
        (8, rules::ids::STONE),
    ] {
        tx.set_cell(p(x), Cell::from_parts(id, 3));
    }
    world.commit(tx).unwrap();
    world
}
fn limits() -> PropsLimits {
    PropsLimits {
        max_pages: 8,
        max_instances: 16,
        max_running: 2,
        max_ready: 2,
        max_age_ticks: 40,
    }
}
fn tints(_: PageId) -> Box<[u32; 256]> {
    Box::new([u32::MAX; 256])
}
fn finish(owner: &mut PropsSource, world: &World) {
    while let Some(job) = owner.take_job(world, tints) {
        owner
            .complete(world, job.execute(|| false).unwrap())
            .map_err(|_| ())
            .unwrap();
        owner.pump_ready(world, 4096).unwrap();
    }
}
#[test]
fn bootstrap_reads_only_occupied_cells_and_rejects_changed_snapshot() {
    let mut world = fixture();
    let mut source = PropsSource::attach(&mut world, limits()).unwrap();
    let job = source.take_job(&world, tints).unwrap();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(p(2), Cell::AIR);
    world.commit(tx).unwrap();
    source.ingest(&mut world, 64, |_| u32::MAX).unwrap();
    assert!(
        !source
            .complete(&world, job.execute(|| false).unwrap())
            .map_err(|_| ())
            .unwrap()
    );
    finish(&mut source, &world);
    assert_eq!(source.stats(&world).models, 2);
    let work = source.take_work();
    assert_eq!(work.jobs_stale, 1);
    assert_eq!(work.worker_mask_words, 128);
    assert_eq!(work.worker_cells, 7);
    assert_eq!(work.cpu_vertices, 0);
    source.detach(&mut world);
}
#[test]
fn contents_events_and_unknown_pages_never_invent_models() {
    let mut world = fixture();
    let mut source = PropsSource::attach(&mut world, limits()).unwrap();
    finish(&mut source, &world);
    source.take_work();
    for n in 0..20 {
        let mut tx = Transaction::new(Tick(2 + n), SpaceId(0));
        tx.event(world::GameplayEvent {
            kind: 1,
            subject: None,
            position: None,
            payload: Vec::new(),
        });
        world.commit(tx).unwrap();
    }
    assert_eq!(source.ingest(&mut world, 3, |_| u32::MAX).unwrap(), 3);
    assert!(!source.stats(&world).caught_up);
    source.ingest(&mut world, 100, |_| u32::MAX).unwrap();
    assert!(source.take_job(&world, tints).is_none());
    assert_eq!(source.take_work().worker_cells, 0);
    assert_eq!(world.loaded_pages(), 1);
    source.detach(&mut world);
}
#[test]
fn temporal_publication_tracks_only_successful_gpu_slots_and_never_reuses_a_lifetime() {
    use render::{
        block_models::{BlockConfig, BlockModelsGpu},
        terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
    };
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
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
            atlas_layers: 186,
        },
        None,
    )
    .unwrap();
    models
        .reserve_slots(&gpu.queue, 16, &mut UploadBudget::new(1024))
        .unwrap();
    let mut world = fixture();
    let mut source = PropsSource::attach(&mut world, limits()).unwrap();
    assert_eq!(source.temporal_registration_bytes(), 0);
    assert!(source.prepare_temporal_publications().unwrap());
    assert_eq!(
        source.temporal_registration_bytes(),
        limits().max_instances * 8
    );
    finish(&mut source, &world);
    let mut events = Vec::with_capacity(16);
    assert_eq!(
        source
            .flush_sampled_with_publications(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(0),
                16,
                |_| None,
                |event| events.push(event)
            )
            .unwrap(),
        0
    );
    assert!(events.is_empty());
    assert_eq!(
        source
            .flush_sampled_with_publications(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(192),
                16,
                |_| None,
                |event| events.push(event)
            )
            .unwrap(),
        3
    );
    assert_eq!(events.len(), 3);
    let original = *events
        .iter()
        .find(|event| event.position == Some(p(2)))
        .unwrap();
    assert_ne!(original.registration, 0);
    let original_gpu = models.instance_records()[original.slot as usize];
    events.clear();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(p(2), Cell::AIR);
    world.commit(tx).unwrap();
    source.ingest(&mut world, 64, |_| u32::MAX).unwrap();
    source
        .flush_sampled_with_publications(
            &gpu.queue,
            &mut models,
            [0; 3],
            &mut UploadBudget::new(0),
            16,
            |_| None,
            |event| events.push(event),
        )
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(
        models.instance_records()[original.slot as usize],
        original_gpu
    );
    source
        .flush_sampled_with_publications(
            &gpu.queue,
            &mut models,
            [0; 3],
            &mut UploadBudget::new(64),
            16,
            |_| None,
            |event| events.push(event),
        )
        .unwrap();
    assert_eq!(
        events,
        &[PropsPublication {
            slot: original.slot,
            position: None,
            registration: original.registration
        }]
    );
    events.clear();
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.set_cell(p(2), Cell::from_parts(rules::ids::CHEST, 3));
    world.commit(tx).unwrap();
    source.ingest(&mut world, 64, |_| u32::MAX).unwrap();
    source
        .flush_sampled_with_publications(
            &gpu.queue,
            &mut models,
            [0; 3],
            &mut UploadBudget::new(64),
            16,
            |_| None,
            |event| events.push(event),
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].slot, original.slot);
    assert_eq!(events[0].position, original.position);
    assert_ne!(events[0].registration, original.registration);
    let second = events[0];
    // A complete CPU remove/recreate may occur while the GPU still shows the
    // previous chest. Its next publication must change lifetime even when the
    // eventual64B pose happens to be exactly equal to the old visible record.
    for (tick, cell) in [(4, Cell::AIR), (5, Cell::from_parts(rules::ids::CHEST, 3))] {
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.set_cell(p(2), cell);
        world.commit(tx).unwrap();
        source.ingest(&mut world, 64, |_| u32::MAX).unwrap();
    }
    events.clear();
    source
        .flush_sampled_with_publications(
            &gpu.queue,
            &mut models,
            [0; 3],
            &mut UploadBudget::new(64),
            16,
            |_| None,
            |event| events.push(event),
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].position, original.position);
    assert_ne!(events[0].registration, second.registration);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        assert_eq!(
            source
                .flush_sampled_with_publications(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(0),
                    16,
                    |_| None,
                    |_| panic!("idle publication")
                )
                .unwrap(),
            0
        );
    }
    let allocations = scope.finish();
    assert_eq!(allocations.allocating_calls(), 0);
    assert_eq!(allocations.dealloc_calls, 0);
    source.detach(&mut world);
}
#[test]
fn actual_world_models_publish_stable_gpu_records_light_and_public_animation() {
    pollster::block_on(async {
        use render::{
            block_models::{BlockConfig, BlockModelsGpu},
            terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
        };
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
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        models
            .reserve_slots(&gpu.queue, 16, &mut UploadBudget::new(1024))
            .unwrap();
        let mut world = fixture();
        let mut source = PropsSource::attach(&mut world, limits()).unwrap();
        finish(&mut source, &world);
        assert_eq!(
            source
                .flush(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(192),
                    16
                )
                .unwrap(),
            3
        );
        assert_eq!(models.active_instances(), 3);
        source.take_work();
        models.take_work();
        assert!(
            source
                .set_animation(
                    p(2),
                    block_models::Animation {
                        chest_open: 0.75,
                        ..block_models::Animation::default()
                    },
                    Tick(2)
                )
                .unwrap()
        );
        assert_eq!(
            source
                .flush(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(63),
                    16
                )
                .unwrap(),
            0
        );
        assert_eq!(
            source
                .flush(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(64),
                    16
                )
                .unwrap(),
            1
        );
        assert_eq!(source.take_work().upload_bytes, 64);
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_light(
            p(2),
            world::LightSample {
                sky: 15,
                block: 0,
                rgb: [0; 3],
            },
        );
        world.commit(tx).unwrap();
        source.ingest(&mut world, 64, |_| u32::MAX).unwrap();
        assert!(source.take_job(&world, tints).is_none());
        source
            .flush(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(64),
                16,
            )
            .unwrap();
        let work = source.take_work();
        assert_eq!(work.records_uploaded, 1);
        assert_eq!(work.worker_cells, 0);
        assert_eq!(work.upload_bytes, 64);
        // A certificate can change without a World LIGHT transaction (zero
        // snapshot or raw-view revocation). Only indexed members are revisited.
        assert!(source.mark_light_binding(page(), Tick(2)));
        assert_eq!(
            source
                .flush_sampled(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(63),
                    16,
                    |_| panic!("no sampler before budget preflight")
                )
                .unwrap(),
            0
        );
        for _ in 0..16 {
            if source.stats(&world).settled {
                break;
            }
            assert!(
                source
                    .flush_sampled(
                        &gpu.queue,
                        &mut models,
                        [0; 3],
                        &mut UploadBudget::new(64),
                        1,
                        |_| None
                    )
                    .unwrap()
                    <= 1
            );
        }
        assert!(source.stats(&world).settled);
        let active = read_records(&gpu, &models);
        assert!(active.iter().all(|i| *i == 0));
        let work = source.take_work();
        assert_eq!(work.binding_members, 3);
        assert_eq!(work.worker_cells, 0);
        let full = world::LightSample {
            sky: 15,
            block: 3,
            rgb: [231, 17, 255],
        };
        assert!(source.mark_light_binding(page(), Tick(2)));
        for _ in 0..16 {
            if source.stats(&world).settled {
                break;
            }
            source
                .flush_sampled(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(64),
                    1,
                    |_| Some(full),
                )
                .unwrap();
        }
        let active = read_records(&gpu, &models);
        assert_eq!(active.len(), 3);
        assert!(active.iter().all(|i| *i == 0xff11e73f));
        let work = source.take_work();
        assert_eq!(
            (work.binding_members, work.worker_cells, work.upload_bytes),
            (3, 0, 192)
        );
        let frame = std::sync::Arc::new(public_frame(7, 10));
        assert!(!source.publish_public_frame(frame.clone(), 11).unwrap());
        assert!(source.publish_public_frame(frame.clone(), 10).unwrap());
        assert!(!source.publish_public_frame(frame, 10).unwrap());
        while source.pump_public(Tick(2), 0.5, 1) > 0 {}
        source
            .flush_sampled(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(128),
                16,
                |_| Some(full),
            )
            .unwrap();
        let records = read_raw_records(&gpu, &models);
        let chest = records
            .iter()
            .find(|r| r[4] == render::block_models::kind::CHEST)
            .unwrap();
        assert!((f32::from_bits(chest[8]) - 0.25).abs() < 0.00002);
        let wind = records
            .iter()
            .find(|r| r[4] == render::block_models::kind::WIND_GENERATOR)
            .unwrap();
        assert_eq!(wind[15], 0x80000000 | 0o543210);
        assert!((f32::from_bits(wind[8]) - 1.29).abs() < 0.002);
        assert_eq!(source.animate_public(Tick(12), 0.5, 1), 1);
        source
            .flush_sampled(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(64),
                16,
                |_| Some(full),
            )
            .unwrap();
        let records = read_raw_records(&gpu, &models);
        let wind = records
            .iter()
            .find(|r| r[4] == render::block_models::kind::WIND_GENERATOR)
            .unwrap();
        assert!((f32::from_bits(wind[8]) - 2.09).abs() < 0.002);
        assert!(source.revoke_public_frame(page(), Tick(13)).unwrap());
        while source.pump_public(Tick(13), 0., 1) > 0 {}
        assert_eq!(source.animate_public(Tick(14), 0., 16), 0);
        source
            .flush_sampled(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(128),
                16,
                |_| Some(full),
            )
            .unwrap();
        let records = read_raw_records(&gpu, &models);
        assert!(
            records
                .iter()
                .filter(|r| r[4] != render::block_models::kind::CROSS)
                .all(|r| r[8] == 0 && r[15] == 0)
        );
        assert_eq!(source.take_work().worker_cells, 0);
        // One-credit journal removal interleaved with GPU flush cannot skip a
        // member when stable slot bookkeeping contains holes.
        let mut tx = Transaction::new(Tick(3), SpaceId(0));
        tx.remove_page(page());
        world.commit(tx).unwrap();
        while !source.stats(&world).caught_up {
            source.ingest(&mut world, 1, |_| u32::MAX).unwrap();
            source
                .flush(
                    &gpu.queue,
                    &mut models,
                    [0; 3],
                    &mut UploadBudget::new(64),
                    1,
                )
                .unwrap();
        }
        source
            .flush(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(1024),
                16,
            )
            .unwrap();
        assert_eq!(models.active_instances(), 0);
        assert_eq!(source.stats(&world).models, 0);
        source.take_work();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            assert_eq!(source.ingest(&mut world, 64, |_| u32::MAX).unwrap(), 0);
            assert!(source.take_job(&world, tints).is_none());
            assert_eq!(source.pump_ready(&world, 16).unwrap(), 0);
            assert_eq!(
                source
                    .flush(
                        &gpu.queue,
                        &mut models,
                        [0; 3],
                        &mut UploadBudget::new(0),
                        16
                    )
                    .unwrap(),
                0
            );
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
        assert_eq!(source.take_work(), PropsWork::default());
        assert!(source.stats(&world).settled);
        source.detach(&mut world);
    });
}

#[test]
fn native_props_snapshot_really_executes_off_owner_thread() {
    let mut world = fixture();
    let mut source = PropsSource::attach(&mut world, limits()).unwrap();
    let task = source.take_job(&world, tints).unwrap();
    let id = task.id;
    let pool = native_pool(2, 2).unwrap();
    let spec = jobs::JobSpec::derived(
        jobs::JobKey {
            page: page(),
            kind: 0x5052,
        },
        jobs::Priority::Normal,
        jobs::WorkCost {
            cells: 4096,
            bytes: 262144,
            ..jobs::WorkCost::ZERO
        },
        262144,
        world.tick(),
    );
    assert!(
        pool.try_submit(jobs::Dispatch {
            id: jobs::JobId(id),
            spec,
            payload: task,
            cancellation: jobs::CancellationToken::new()
        })
        .is_ok()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let done = loop {
        if let Some(done) = pool.try_recv() {
            break done;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    let jobs::JobOutcome::Success(built) = done.outcome else {
        panic!("actual model worker failed")
    };
    assert_ne!(built.worker_thread, std::thread::current().id());
    assert_eq!(built.samples.len(), 3);
    assert_eq!(built.cells, 4);
    assert!(source.complete(&world, built).map_err(|_| ()).unwrap());
    source.detach(&mut world);
}

#[test]
fn browser_props_transport_matches_native_adaptive_storage_and_retries() {
    for mode in 0..3 {
        let mut world = if mode == 1 {
            fixture()
        } else {
            let mut world = World::new(rules::properties, 1024);
            let cells: Vec<_> = (0..4096)
                .map(|i| Cell::from_parts(rules::ids::CHEST, if mode == 0 { 3 } else { i as u16 }))
                .collect();
            let mut tx = Transaction::new(Tick(1), SpaceId(0));
            tx.create_page(page(), cells);
            world.commit(tx).unwrap();
            world
        };
        let mut owner = PropsSource::attach(&mut world, limits()).unwrap();
        let task = owner.take_job(&world, tints).unwrap();
        let id = task.id;
        let request = task.encode().unwrap();
        let result = execute_props_request(&request, || false).unwrap();
        let browser = PropsBuilt::decode(&result).unwrap();
        let native = task.execute(|| false).unwrap();
        assert_eq!(browser.samples, native.samples);
        assert_eq!(
            (
                browser.id,
                browser.source,
                browser.cells,
                browser.mask_words
            ),
            (native.id, native.source, native.cells, native.mask_words)
        );
        assert!(browser.samples.iter().all(|s| s.light == 0));
        for end in [0, 3, 35, 100, request.len() - 1] {
            assert!(execute_props_request(&request[..end], || false).is_err());
        }
        for end in [0, 3, 35, 41, result.len() - 1] {
            assert!(PropsBuilt::decode(&result[..end]).is_err());
        }
        assert_eq!(
            execute_props_request(&request, || true).unwrap_err(),
            PropsWireError::Cancelled
        );
        if mode == 1 {
            let mut invalid = request.clone();
            assert_eq!(invalid[1572], 1); // Native paletted input, before any expansion.
            let end = invalid.len();
            invalid[end - 8..].fill(255);
            assert_eq!(
                execute_props_request(&invalid, || false).unwrap_err(),
                PropsWireError::Storage
            );
            let mut duplicate = result.clone();
            let first: [u8; 2] = duplicate[42..44].try_into().unwrap();
            duplicate[52..54].copy_from_slice(&first);
            assert!(PropsBuilt::decode(&duplicate).is_err());
            let mut tx = Transaction::new(Tick(2), SpaceId(0));
            tx.set_cell(p(2), Cell::AIR);
            world.commit(tx).unwrap();
            assert!(!owner.complete(&world, browser).map_err(|_| ()).unwrap());
            let retry = owner.take_job(&world, tints).unwrap();
            assert!(retry.id > id);
            let retry_id = retry.id;
            owner.fail_job(&world, retry_id);
            assert!(owner.take_job(&world, tints).unwrap().id > retry_id);
        }
        println!(
            "Props adaptive mode{mode}: request{}B result{}B models{} CPUvertices0",
            request.len(),
            result.len(),
            native.samples.len()
        );
        owner.detach(&mut world);
    }
}

fn read_raw_records(
    gpu: &render::Gpu,
    models: &render::block_models::BlockModelsGpu,
) -> Vec<[u32; 16]> {
    let size = models.instance_buffer().size();
    let capture = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("public prop LIGHT certificate GPU record oracle"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(models.instance_buffer(), 0, &capture, 0, size);
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    capture
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let result = capture
        .slice(..)
        .get_mapped_range()
        .chunks_exact(64)
        .filter(|r| {
            u32::from_le_bytes(r[20..24].try_into().unwrap()) & render::block_models::HIDDEN == 0
        })
        .map(|r| {
            std::array::from_fn(|i| u32::from_le_bytes(r[i * 4..i * 4 + 4].try_into().unwrap()))
        })
        .collect();
    capture.unmap();
    result
}

fn read_records(gpu: &render::Gpu, models: &render::block_models::BlockModelsGpu) -> Vec<u32> {
    read_raw_records(gpu, models)
        .into_iter()
        .map(|r| r[7])
        .collect()
}
fn public_frame(view: u64, cell_view: u64) -> net::props::Frame {
    use net::props::{Frame, Hint, Kind};
    Frame {
        page: page(),
        cell_view,
        view,
        source: core_types::Version(view),
        tick: Tick(2),
        hints: vec![
            Hint::from_visual(
                p(2).local_index(),
                Kind::Chest,
                None,
                false,
                Some(0.25),
                None,
            )
            .unwrap(),
            Hint::from_visual(
                p(6).local_index(),
                Kind::Wind,
                Some(0o543210),
                true,
                None,
                Some((1.25, 0.08)),
            )
            .unwrap(),
        ],
    }
}
#[test]
fn public_props_require_exact_raw_view_and_bound_total_cached_hints() {
    let mut world = fixture();
    let mut source = PropsSource::attach(&mut world, limits()).unwrap();
    // State can precede the asynchronous geometry result.
    let frame = std::sync::Arc::new(public_frame(7, 10));
    assert!(source.publish_public_frame(frame.clone(), 10).unwrap());
    assert_eq!(source.public_binding(page()), Some((10, 7)));
    assert!(
        !source
            .publish_public_frame(std::sync::Arc::new(public_frame(6, 10)), 10)
            .unwrap()
    );
    assert!(
        !source
            .publish_public_frame(std::sync::Arc::new(public_frame(8, 11)), 10)
            .unwrap()
    );
    let mut too_many = public_frame(8, 10);
    too_many.hints = (0..17)
        .map(|i| {
            net::props::Hint::from_visual(
                core_types::LocalIdx::new(i).unwrap(),
                net::props::Kind::Chest,
                None,
                false,
                Some(0.),
                None,
            )
            .unwrap()
        })
        .collect();
    assert!(matches!(
        source.publish_public_frame(std::sync::Arc::new(too_many), 10),
        Err(PropsError::Capacity)
    ));
    assert_eq!(source.public_binding(page()), Some((10, 7)));
    finish(&mut source, &world);
    assert_eq!(source.model_pages().collect::<Vec<_>>(), vec![page()]);
    source.pump_public(Tick(2), 0., 16);
    assert_eq!(source.animate_public(Tick(3), 0., 16), 1);
    assert!(source.revoke_public_frame(page(), Tick(4)).unwrap());
    source.pump_public(Tick(4), 0., 16);
    assert_eq!(source.animate_public(Tick(5), 0., 16), 0);
    source.detach(&mut world);
}
