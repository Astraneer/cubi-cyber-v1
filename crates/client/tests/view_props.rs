use client::{
    ClientScene, SceneConfig,
    audio_bridge::{MachineCommand, MachineSounds},
    props_source::PropsLimits,
    view::ViewProps,
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use net::{
    PageSnapshot, PageUpdate,
    light::{Header, Snapshot, Update, pack},
    props::{Frame, Hint, Kind},
};
use render::{
    block_models::{MODEL_KINDS, kind},
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn point(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 4,
        z: 0,
    }
}
fn scene() -> ClientScene {
    let mut scene = ClientScene::new(SceneConfig {
        center: point(0),
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        authoritative_remote: true,
        ..SceneConfig::forest()
    })
    .unwrap();
    for x in [0, 16] {
        let mut cells = Box::new([Cell::AIR; 4096]);
        cells[point(x).local_index().get() as usize] =
            Cell::from_parts(rules::ids::HEAT_GENERATOR, 0);
        if x == 0 {
            cells[point(1).local_index().get() as usize] = Cell::from_parts(rules::ids::CHEST, 0);
        }
        scene
            .import_authoritative_page(
                &PageUpdate::Snapshot(PageSnapshot {
                    page: point(x).page(),
                    version: 10,
                    source_version: Version(100),
                    cells,
                }),
                Tick(1),
            )
            .unwrap();
    }
    scene
        .import_authoritative_light(
            &Update::Snapshot(Snapshot {
                header: Header {
                    page: point(0).page(),
                    cell_view: 10,
                    version: 1,
                    source: Version(101),
                },
                values: Box::new([pack(15, 7, [12, 5, 1]).unwrap(); 4096]),
            }),
            Tick(2),
        )
        .unwrap();
    scene
}
fn public(view: u64, active: bool) -> Arc<Frame> {
    Arc::new(Frame {
        page: point(0).page(),
        cell_view: 10,
        view,
        source: Version(view),
        tick: Tick(10),
        hints: vec![
            Hint::from_visual(
                point(0).local_index(),
                Kind::Machine,
                Some(0o543210),
                active,
                None,
                None,
            )
            .unwrap(),
        ],
    })
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    data
}
#[test]
fn native_worker_to_certified_gpu_records_public_ports_draw_and_audible_machine_loop() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = TerrainGpu::new(
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
        let mut scene = scene();
        let mut props = ViewProps::prepare(
            &gpu.device,
            &gpu.queue,
            &terrain,
            &mut scene,
            None,
            None,
            PropsLimits {
                max_pages: 4,
                max_instances: 16,
                max_running: 2,
                max_ready: 2,
                max_age_ticks: 40,
            },
        )
        .unwrap();
        assert!(props.publish_public_frame(&scene, public(1, true)).unwrap());
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                Instant::now() < deadline,
                "native model worker did not settle"
            );
            props.advance_cpu(&mut scene, Tick(10)).unwrap();
            props
                .upload_ready(&scene, &gpu.queue, &mut UploadBudget::new(4096), Tick(10))
                .unwrap();
            if props.stats.ready {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(
            props.gpu.active_instances(),
            2,
            "halo machine must not duplicate near/far ownership"
        );
        assert_eq!(props.stats.worker_cells, 2);
        assert!(props.stats.startup_vertices > 0 && props.stats.pipeline_compilations > 0);
        assert_eq!(props.stats.cpu_vertices, 0);
        let records = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prepared model records oracle"),
            size: 1024,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let counts = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual public model draw counts"),
            size: MODEL_KINDS as u64 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("actual near block model output"),
            size: wgpu::Extent3d {
                width: 64,
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
        let camera =
            Camera::perspective([3., 9., 22.], [0., 4., 0.], [0., 1., 0.], 1., 1., 0.1, 200.);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        props
            .prepare_camera(&gpu.queue, &mut encoder, camera)
            .unwrap();
        terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &output.create_view(&Default::default()),
                &camera,
                CullingMode::Frustum,
                |pass| props.draw(pass),
            )
            .unwrap();
        encoder.copy_buffer_to_buffer(props.gpu.instance_buffer(), 0, &records, 0, 1024);
        encoder.copy_buffer_to_buffer(
            props.gpu.indirect_buffer(),
            0,
            &counts,
            0,
            MODEL_KINDS as u64 * 16,
        );
        gpu.queue.submit([encoder.finish()]);
        let bytes = read(&gpu, &records);
        let words = bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect::<Vec<_>>();
        let machine = words
            .chunks_exact(16)
            .find(|words| {
                words[4] == kind::HEAT_GENERATOR && words[5] & render::block_models::HIDDEN == 0
            })
            .unwrap();
        assert_ne!(machine[5] & render::block_models::ACTIVE, 0);
        assert_eq!(machine[7], pack(15, 7, [12, 5, 1]).unwrap());
        assert_eq!(machine[15], 0o543210 | (1 << 31));
        let bytes = read(&gpu, &counts);
        let count = |kind: u32| {
            u32::from_le_bytes(
                bytes[(kind as usize * 16 + 4)..(kind as usize * 16 + 8)]
                    .try_into()
                    .unwrap(),
            )
        };
        assert_eq!(count(kind::HEAT_GENERATOR), 1);
        assert_eq!(count(kind::CHEST), 1);
        let prepared = audio::scene::PreparedSounds::prepare(None, None).unwrap();
        let (mut mixer, events) = prepared.into_parts();
        let mut sounds = MachineSounds::default();
        let mut handle = None;
        let work = sounds.visit(7, &scene, &props.source, [0., 5., 0.], |command| {
            if let MachineCommand::Start(cue) = command {
                let mut request = events
                    .request(cue.event, cue.emitter, Some(cue.position), cue.seed)
                    .unwrap();
                request.gain = cue.gain;
                assert_eq!(request.category, audio::Category::Machines);
                handle = Some(mixer.play(request).unwrap());
            }
        });
        assert_eq!(work.started, 1);
        let handle = handle.unwrap();
        let mut pcm = [0.; 1024];
        mixer.mix_stereo(&mut pcm);
        assert!(pcm.iter().any(|sample| sample.abs() > 1e-6));
        let cursor = mixer.cursor(handle).unwrap();
        sounds.visit(7, &scene, &props.source, [0., 5., 0.], |_| {
            panic!("unchanged public source must not restart loop")
        });
        assert_eq!(mixer.cursor(handle), Some(cursor));
        sounds.set_time(Tick(20));
        let refresh = sounds.visit(7, &scene, &props.source, [0., 5., 0.], |command| {
            if let MachineCommand::Start(cue) = command {
                let request = events
                    .request(cue.event, cue.emitter, Some(cue.position), cue.seed)
                    .unwrap();
                assert_eq!(mixer.play(request).unwrap(), handle);
            }
        });
        assert_eq!(refresh.refreshed, 1);
        assert_eq!(
            mixer.cursor(handle),
            Some(cursor),
            "refresh preserves loop phase"
        );
        assert!(mixer.stop(handle));
        mixer.mix_stereo(&mut pcm);
        assert_eq!(mixer.active_voices(), 0);
        sounds.set_time(Tick(40));
        let mut recovered = None;
        let refresh = sounds.visit(7, &scene, &props.source, [0., 5., 0.], |command| {
            if let MachineCommand::Start(cue) = command {
                let request = events
                    .request(cue.event, cue.emitter, Some(cue.position), cue.seed)
                    .unwrap();
                recovered = Some(mixer.play(request).unwrap());
            }
        });
        assert_eq!(refresh.refreshed, 1);
        assert_ne!(
            recovered,
            Some(handle),
            "a lost voice obtains a fresh handle"
        );
        mixer.mix_stereo(&mut pcm);
        assert!(pcm.iter().any(|sample| sample.abs() > 1e-6));
        props
            .publish_public_frame(&scene, public(2, false))
            .unwrap();
        props.advance_cpu(&mut scene, Tick(10)).unwrap();
        props
            .upload_ready(&scene, &gpu.queue, &mut UploadBudget::new(4096), Tick(10))
            .unwrap();
        assert_eq!(
            sounds
                .visit(7, &scene, &props.source, [0., 5., 0.], |command| {
                    if let MachineCommand::Stop(emitter) = command {
                        mixer.stop_emitter(emitter);
                    }
                })
                .stopped,
            1
        );
        mixer.mix_stereo(&mut pcm);
        assert!(
            pcm[256..].iter().all(|sample| *sample == 0.),
            "128-frame stop fade must terminate"
        );
        assert_eq!(mixer.active_voices(), 0);
        // Drain bookkeeping before measuring a static product iteration.
        for _ in 0..4 {
            props.advance_cpu(&mut scene, Tick(10)).unwrap();
            props
                .upload_ready(&scene, &gpu.queue, &mut UploadBudget::new(4096), Tick(10))
                .unwrap();
        }
        props.gpu.take_work();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            let cpu = props.advance_cpu(&mut scene, Tick(10)).unwrap();
            let upload = props
                .upload_ready(&scene, &gpu.queue, &mut UploadBudget::new(4096), Tick(10))
                .unwrap();
            assert_eq!(cpu.worker_cells + cpu.cpu_vertices + upload.upload_bytes, 0);
        }
        let allocations = scope.finish();
        assert_eq!(allocations.allocating_calls(), 0);
        assert_eq!(allocations.dealloc_calls, 0);
        assert_eq!(props.gpu.take_work().pipeline_compilations, 0);
    });
}

#[test]
fn shared_view_composes_near_models_hdr_and_dynamic_shadows_then_remains_idle() {
    use client::{
        native_pool,
        view::{ClientView, EnvironmentProfile},
    };
    use render::{
        atmosphere::AtmosphereConfig,
        environment_gpu::EnvironmentConfig,
        post::{Exposure, PostConfig},
    };
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 8,
            max_quads: 262_144,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let mut view = ClientView::new(
        terrain,
        SceneConfig {
            center: point(0),
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 27,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    let mut lighting =
        client::lighting::ClientLightingConfig::default().with_authoritative_source();
    lighting.shadows.resolution = 32;
    lighting.shadows.dynamic_casters = true;
    lighting.shadows.max_updates_per_frame = 3;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
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
                    exposure: Exposure::Manual(0.),
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
    view.prepare_entities(&gpu.device, &gpu.queue).unwrap();
    view.prepare_props(
        &gpu.device,
        &gpu.queue,
        PropsLimits {
            max_pages: 4,
            max_instances: 16,
            max_running: 2,
            max_ready: 2,
            max_age_ticks: 40,
        },
    )
    .unwrap();
    let pages = view.scene.requested_pages().collect::<Vec<_>>();
    for page in &pages {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if *page == point(0).page() {
            cells[usize::from(point(0).local_index().get())] =
                Cell::from_parts(rules::ids::HEAT_GENERATOR, 0);
            cells[usize::from(point(1).local_index().get())] =
                Cell::from_parts(rules::ids::CHEST, 0);
        } else if *page == point(16).page() {
            cells[usize::from(point(16).local_index().get())] =
                Cell::from_parts(rules::ids::HEAT_GENERATOR, 0);
        }
        view.scene
            .import_authoritative_page(
                &PageUpdate::Snapshot(PageSnapshot {
                    page: *page,
                    version: 10,
                    source_version: Version(100),
                    cells,
                }),
                Tick(0),
            )
            .unwrap();
        view.scene
            .import_authoritative_light(
                &Update::Snapshot(Snapshot {
                    header: Header {
                        page: *page,
                        cell_view: 10,
                        version: 1,
                        source: Version(101),
                    },
                    values: Box::new([pack(15, 7, [12, 5, 1]).unwrap(); 4096]),
                }),
                Tick(0),
            )
            .unwrap();
    }
    assert!(view.publish_remote_props(public(1, true)).unwrap());
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shared near models HDR presentation"),
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
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        assert!(
            Instant::now() < deadline,
            "shared model owner did not settle: {:?} light {:?} scene {:?}",
            view.props_stats(),
            view.lighting_stats(),
            view.stats()
        );
        view.advance_input(0.).unwrap();
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        if view.is_ready() && !view.needs_redraw(96, 64) {
            break;
        }
    }
    let props = view.props_stats().unwrap();
    assert_eq!(view.stats().resident_quads, 0);
    assert_eq!(
        view.stats().terrain_frames,
        0,
        "sky-only terrain does not fake content readiness"
    );
    assert!(
        view.stats().props_frames > 0,
        "a real model HDR draw establishes model-only readiness"
    );
    assert_eq!(props.instances, 2, "halo model has no near GPU ownership");
    assert!(
        (2..=16).contains(&props.worker_cells),
        "bounded retries inspect only occupied candidates"
    );
    assert_eq!(props.cpu_vertices, 0);
    assert_eq!(view.stats().frame_pipeline_compilations, 0);
    assert!(view.environment_stats().unwrap().gpu.post.tone_pixels > 0);
    let light = view.lighting_stats().unwrap();
    assert!(
        light.dynamic_shadow_draws > 0,
        "model shadow list is composed with the entity list"
    );
    assert!(light.authoritative_source && light.ready);
    view.take_frame_work();
    assert_eq!(
        view.last_frame_work_details().workers.cells_scanned,
        view.scene.stats().mesh_cells_scanned + props.worker_cells,
        "completed model kernels belong beside scene workers, not owner light reads"
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        assert!(!view.needs_redraw(96, 64));
    }
    let idle = scope.finish();
    assert_eq!(idle.allocating_calls(), 0);
    assert_eq!(idle.dealloc_calls, 0);
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
    assert_eq!(
        view.lighting_stats().unwrap().dynamic_shadow_draws,
        light.dynamic_shadow_draws
    );
}
