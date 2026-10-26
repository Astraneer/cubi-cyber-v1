pub use client::water;
#[path = "../src/water_source.rs"]
#[allow(dead_code)]
mod source;
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use source::*;
use world::{Transaction, World};
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    }
}
fn world() -> World {
    let mut w = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(page(), vec![Cell::AIR; 4096]);
    tx.set_cell(
        BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 4,
            z: 8,
        },
        Cell::from_parts(rules::ids::WATER, 0),
    );
    w.commit(tx).unwrap();
    w
}
fn tints(_: PageId) -> Box<[u32; 256]> {
    Box::new([u32::MAX; 256])
}
fn limits() -> WaterSourceLimits {
    WaterSourceLimits {
        max_pages: 16,
        max_running: 2,
        max_ready: 4,
        ..WaterSourceLimits::default()
    }
}
fn drain_cpu(owner: &mut WaterSource, w: &World) {
    while let Some(job) = owner.take_job(w, tints).unwrap() {
        owner
            .complete(w, job.execute(|| false))
            .map_err(|_| ())
            .unwrap();
    }
}
#[test]
fn exact_dirty_journal_rejects_stale_fluid_result_and_coalesces_work() {
    let mut w = world();
    let mut owner = WaterSource::attach(&mut w, limits()).unwrap();
    let job = owner.take_job(&w, tints).unwrap().unwrap();
    assert!(owner.take_job(&w, tints).unwrap().is_none());
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(
        BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 4,
            z: 8,
        },
        Cell::from_parts(rules::ids::WATER, 7),
    );
    w.commit(tx).unwrap();
    owner.ingest(&mut w, 64).unwrap();
    assert_eq!(
        owner
            .complete(&w, job.execute(|| false))
            .map_err(|_| ())
            .unwrap(),
        WaterCompletion::Stale
    );
    drain_cpu(&mut owner, &w);
    assert_eq!(owner.stats(&w).ready, 1);
    let work = owner.take_work();
    assert_eq!(work.jobs_submitted, 2);
    assert_eq!(work.jobs_stale, 1);
    assert_eq!(work.worker_cells, 8192);
    assert_eq!(work.cpu_frame_vertices, 0);
    owner.detach(&mut w);
}
#[test]
fn component_only_records_are_budgeted_and_do_not_queue_geometry() {
    let mut w = world();
    let mut owner = WaterSource::attach(&mut w, limits()).unwrap();
    drain_cpu(&mut owner, &w);
    owner.take_work();
    for n in 0..20 {
        let mut tx = Transaction::new(Tick(2 + n), SpaceId(0));
        tx.event(world::GameplayEvent {
            kind: 1,
            subject: None,
            position: None,
            payload: Vec::new(),
        });
        w.commit(tx).unwrap();
    }
    assert_eq!(owner.ingest(&mut w, 3).unwrap(), 3);
    assert!(!owner.stats(&w).caught_up);
    assert_eq!(owner.take_work().jobs_submitted, 0);
    owner.ingest(&mut w, 100).unwrap();
    assert!(owner.stats(&w).caught_up);
    assert!(owner.take_job(&w, tints).unwrap().is_none());
    owner.detach(&mut w);
}
#[test]
fn ready_backpressure_retains_owned_completion_and_missing_center_never_generates() {
    let mut w = world();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    let p = PageId { x: 1, ..page() };
    tx.create_page(p, vec![Cell::AIR; 4096]);
    tx.set_cell(
        BlockPos {
            space: SpaceId(0),
            x: 24,
            y: 4,
            z: 8,
        },
        Cell::from_parts(rules::ids::WATER, 0),
    );
    w.commit(tx).unwrap();
    let mut owner = WaterSource::attach(
        &mut w,
        WaterSourceLimits {
            max_ready: 1,
            ..limits()
        },
    )
    .unwrap();
    let a = owner.take_job(&w, tints).unwrap().unwrap();
    let b = owner.take_job(&w, tints).unwrap().unwrap();
    owner
        .complete(&w, a.execute(|| false))
        .map_err(|_| ())
        .unwrap();
    let blocked = owner
        .complete(&w, b.execute(|| false))
        .expect_err("ready cap");
    assert_eq!(owner.stats(&w).ready, 1);
    assert_eq!(owner.stats(&w).running, 1);
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.remove_page(p);
    w.commit(tx).unwrap();
    owner.ingest(&mut w, 100).unwrap();
    assert_eq!(
        owner.complete(&w, blocked.built).map_err(|_| ()).unwrap(),
        WaterCompletion::Stale
    );
    assert_eq!(w.loaded_pages(), 1);
    owner.detach(&mut w);
}
#[test]
fn empty_cached_masks_skip_all_cell_jobs_and_idle_ingest_is_empty() {
    let mut w = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    for x in 0..8 {
        tx.create_page(PageId { x, ..page() }, vec![Cell::AIR; 4096]);
    }
    w.commit(tx).unwrap();
    let mut owner = WaterSource::attach(&mut w, limits()).unwrap();
    assert_eq!(owner.take_work().mask_words, 8 * 64);
    assert!(owner.take_job(&w, tints).unwrap().is_none());
    assert!(owner.stats(&w).settled);
    for _ in 0..100 {
        assert_eq!(owner.ingest(&mut w, 64).unwrap(), 0);
    }
    assert_eq!(owner.take_work(), WaterSourceWork::default());
    owner.detach(&mut w);
}
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
struct GpuInputs {
    _color: wgpu::Texture,
    color: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth: wgpu::TextureView,
    _sky: wgpu::Texture,
    sky: wgpu::TextureView,
}
fn gpu_inputs(gpu: &render::Gpu) -> GpuInputs {
    let texture = |label, format| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 4,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    let color_texture = texture(
        "actual opaque source for live fluid owner",
        wgpu::TextureFormat::Rgba16Float,
    );
    let color = color_texture.create_view(&Default::default());
    let depth_texture = texture(
        "known opaque depth for live fluid owner",
        wgpu::TextureFormat::Depth32Float,
    );
    let depth = depth_texture.create_view(&Default::default());
    let sky_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unit environment for live fluid source oracle"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &sky_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0u8, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63],
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
    let sky = sky_texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("actual MSAA opaque HDR input clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.001),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    gpu.queue.submit([encoder.finish()]);
    GpuInputs {
        _color: color_texture,
        color,
        _depth: depth_texture,
        depth,
        _sky: sky_texture,
        sky,
    }
}
fn gpu_image(gpu: &render::Gpu, water: &render::water_gpu::WaterGpu) -> Vec<u8> {
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("real live fluid HDR image"),
        size: 64 * 64 * 8,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: water.resolved_texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &b,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(512),
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
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = b.slice(..).get_mapped_range().to_vec();
    b.unmap();
    bytes
}
#[test]
fn actual_m8_fluid_plans_drive_live_gpu_pages_and_idle_owner_allocates_nothing() {
    pollster::block_on(async {
        use render::{
            terrain_gpu::{Camera, UploadBudget},
            water_gpu::{WaterConfig, WaterGpu, WaterInputs, WaterState},
        };
        use world::CellRead;
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let inputs = gpu_inputs(&gpu);
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            WaterConfig {
                max_objects: 16,
                max_quads: 1024,
                reflection_steps: 0,
                wave_amplitude: 0.,
                ..WaterConfig::default()
            },
            64,
            64,
            WaterInputs {
                color_msaa: &inputs.color,
                depth_msaa: &inputs.depth,
                sky: &inputs.sky,
                transmittance: &inputs.sky,
            },
            None,
        )
        .unwrap();
        let mut w = world();
        let mut owner = WaterSource::attach(&mut w, limits()).unwrap();
        drain_cpu(&mut owner, &w);
        assert_eq!(
            owner
                .publish_ready(
                    &gpu.queue,
                    &mut water,
                    &w,
                    [0; 3],
                    &mut UploadBudget::new(100000),
                    4
                )
                .unwrap(),
            1
        );
        assert_eq!(water.active_quad_count(), 6);
        let state = WaterState {
            camera: Camera::perspective(
                [8.5, 6., 12.],
                [8.5, 3.5, 8.5],
                [0., 1., 0.],
                0.8,
                1.,
                0.1,
                100.,
            ),
            eye: [8.5, 6., 12.],
            time_seconds: 0.,
            sun: [0., 1., 0.],
            sun_intensity: 0.,
            underwater: false,
            scene_revision: 1,
        };
        let render = |water: &mut WaterGpu| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            water
                .encode(
                    &gpu.queue,
                    &mut encoder,
                    state,
                    &mut UploadBudget::new(256),
                    0,
                )
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
        };
        render(&mut water);
        let before = gpu_image(&gpu, &water);
        owner.take_work();
        let tick_fluid = |world: &mut World, at: [i32; 3], tick: u64| {
            let plan = rules::survival::fluid::plan_tick(
                |[x, y, z]| match world.read(BlockPos {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                }) {
                    CellRead::Known(c) => Some(c),
                    CellRead::Unknown => None,
                },
                at,
            )
            .unwrap();
            assert!(!plan.changes().is_empty());
            let mut tx = Transaction::new(Tick(tick), SpaceId(0));
            for c in plan.changes() {
                tx.set_cell(
                    BlockPos {
                        space: SpaceId(0),
                        x: c.position[0],
                        y: c.position[1],
                        z: c.position[2],
                    },
                    c.after,
                );
            }
            world.commit(tx).unwrap();
            plan.reads
        };
        let reads1 = tick_fluid(&mut w, [8, 4, 8], 5);
        owner.ingest(&mut w, 100).unwrap();
        let stale = owner.take_job(&w, tints).unwrap().unwrap();
        let reads2 = tick_fluid(&mut w, [8, 3, 8], 10);
        owner.ingest(&mut w, 100).unwrap();
        assert_eq!(
            owner
                .complete(&w, stale.execute(|| false))
                .map_err(|_| ())
                .unwrap(),
            WaterCompletion::Stale
        );
        drain_cpu(&mut owner, &w);
        assert_eq!(
            owner
                .publish_ready(
                    &gpu.queue,
                    &mut water,
                    &w,
                    [0; 3],
                    &mut UploadBudget::new(100000),
                    4
                )
                .unwrap(),
            1
        );
        assert_eq!(water.active_quad_count(), 14);
        render(&mut water);
        let after = gpu_image(&gpu, &water);
        assert_ne!(
            before, after,
            "real M8 fluid column did not reach physical water renderer"
        );
        let work = owner.take_work();
        assert_eq!(work.jobs_stale, 1);
        assert_eq!(work.cpu_frame_vertices, 0);
        assert_eq!(work.pages_published, 1);
        assert_eq!(work.gpu_upload_bytes, 14 * 16 + 8);
        assert_eq!(work.jobs_submitted, 2);
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            assert_eq!(owner.ingest(&mut w, 64).unwrap(), 0);
            assert!(owner.take_job(&w, tints).unwrap().is_none());
            assert_eq!(
                owner
                    .publish_ready(
                        &gpu.queue,
                        &mut water,
                        &w,
                        [0; 3],
                        &mut UploadBudget::new(0),
                        4
                    )
                    .unwrap(),
                0
            );
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
        assert_eq!(owner.take_work(), WaterSourceWork::default());
        assert!(owner.stats(&w).settled);
        println!(
            "M10 actualM8fluid reads{}+{},GPUquads6→14,232Bsinglepage,stale1,idle100cycles0cells0uploads0allocations",
            reads1, reads2
        );
        water.take_work();
        owner.request_model_update();
        let shifted = |_| Some(render::terrain_gpu::translation([-1024., 0., 0.]));
        let revision = water.revision();
        assert_eq!(
            owner
                .refresh_models(&gpu.queue, &mut water, &mut UploadBudget::new(79), shifted)
                .unwrap(),
            None
        );
        assert_eq!(water.revision(), revision);
        assert!(!owner.stats(&w).settled);
        owner.take_work();
        assert_eq!(
            owner
                .refresh_models(&gpu.queue, &mut water, &mut UploadBudget::new(80), shifted)
                .unwrap(),
            Some(1)
        );
        assert_eq!(water.active_quad_count(), 14);
        assert_eq!(owner.take_work().gpu_upload_bytes, 80);
        assert_eq!(water.take_work().quad_uploads, 0);
        assert!(owner.stats(&w).settled);
        owner.invalidate_tints(&w).unwrap();
        drain_cpu(&mut owner, &w);
        // Same immutable tint values change the certificate, without reuploading quads.
        assert_eq!(
            owner
                .publish_ready_with_model(
                    &gpu.queue,
                    &mut water,
                    &w,
                    &mut UploadBudget::new(0),
                    4,
                    shifted
                )
                .unwrap(),
            1
        );
        assert_eq!(owner.take_work().gpu_upload_bytes, 0);
        let revision = water.revision();
        assert_eq!(
            owner
                .retire_all(&gpu.queue, &mut water, &mut UploadBudget::new(11))
                .unwrap(),
            None
        );
        assert_eq!(water.revision(), revision);
        assert_eq!(water.active_quad_count(), 14);
        assert_eq!(
            owner
                .retire_all(&gpu.queue, &mut water, &mut UploadBudget::new(12))
                .unwrap(),
            Some(1)
        );
        assert_eq!(water.active_quad_count(), 0);
        assert_eq!(owner.stats(&w).tracked_pages, 0);
        assert!(owner.stats(&w).settled);
        for n in 0..64 {
            let new_page = PageId {
                x: 32 + n,
                ..page()
            };
            let mut tx = Transaction::new(Tick(20 + n as u64 * 2), SpaceId(0));
            tx.create_page(new_page, vec![Cell::AIR; 4096]);
            tx.set_cell(
                BlockPos {
                    space: SpaceId(0),
                    x: new_page.x * 16 + 8,
                    y: 4,
                    z: 8,
                },
                Cell::from_parts(rules::ids::WATER, 0),
            );
            w.commit(tx).unwrap();
            owner.ingest(&mut w, 100).unwrap();
            let job = owner.take_job(&w, tints).unwrap().unwrap();
            let mut tx = Transaction::new(Tick(21 + n as u64 * 2), SpaceId(0));
            tx.remove_page(new_page);
            w.commit(tx).unwrap();
            owner.ingest(&mut w, 100).unwrap();
            assert_eq!(
                owner
                    .complete(&w, job.execute(|| false))
                    .map_err(|_| ())
                    .unwrap(),
                WaterCompletion::Stale
            );
            assert!(owner.take_job(&w, tints).unwrap().is_none());
            owner
                .publish_ready(
                    &gpu.queue,
                    &mut water,
                    &w,
                    [0; 3],
                    &mut UploadBudget::new(0),
                    8,
                )
                .unwrap();
            assert_eq!(
                owner.stats(&w).tracked_pages,
                0,
                "retired metadata leaked at {n}"
            );
            assert!(owner.stats(&w).settled);
        }
        assert_eq!(water.active_quad_count(), 0);
        owner.detach(&mut w);
    });
}
#[test]
fn browser_water_packets_match_native_and_reject_forged_inputs_and_lengths() {
    let mut w = world();
    let mut owner = WaterSource::attach(&mut w, limits()).unwrap();
    let job = owner.take_job(&w, tints).unwrap().unwrap();
    let request = job.encode().unwrap();
    assert_eq!(request.len(), 7104);
    for n in 0..request.len() {
        assert!(WaterJob::decode(&request[..n]).is_err());
    }
    let mut malformed = request.clone();
    malformed[248] = 253;
    assert!(matches!(
        WaterJob::decode(&malformed),
        Err(WaterWireError::Cell)
    ));
    let response = execute_water_request(&request, || false).unwrap();
    let native = job.execute(|| false);
    let wire = BuiltWater::decode(&response).unwrap();
    assert_eq!(native.id, wire.id);
    assert_eq!(
        native.result.as_ref().unwrap().mesh.quads,
        wire.result.as_ref().unwrap().mesh.quads
    );
    assert_eq!(wire.result.as_ref().unwrap().mesh.work.snapshot_lookups, 0);
    assert_eq!(
        native.result.as_ref().unwrap().mesh.work.snapshot_lookups,
        27
    );
    for n in 0..response.len() {
        assert!(BuiltWater::decode(&response[..n]).is_err());
    }
    let mut bad = response.clone();
    bad[253] = 0;
    assert!(matches!(
        BuiltWater::decode(&bad),
        Err(WaterWireError::Work)
    ));
    let mut forged = BuiltWater::decode(&response).unwrap();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(
        BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 4,
            z: 8,
        },
        Cell::from_parts(rules::ids::WATER, 7),
    );
    w.commit(tx).unwrap();
    let current = water::WaterTask::capture(&w, page(), tints(page())).unwrap();
    forged.result.as_mut().unwrap().inputs = current.inputs;
    forged.result.as_mut().unwrap().version = current.version;
    assert_eq!(
        owner.complete(&w, forged).map_err(|_| ()).unwrap(),
        WaterCompletion::Stale,
        "worker cannot relabel old geometry with newer live inputs"
    );
    assert_eq!(owner.stats(&w).ready, 0);
    owner.detach(&mut w);
    println!(
        "M10 browser water transport7104Brequest nativequadsBITWISEsame,5832publicclasses,norawIDs,forgedcurrentcertificateRejected"
    );
}
