#![cfg(not(target_arch = "wasm32"))]
use client::lighting::{ClientLighting, ClientLightingConfig, SkyPolicy};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use render::{
    OutputTransfer,
    light_volume::{LIGHT_PAGE_BYTES, light_index},
    shadows::{ShadowConfig, WorldBounds},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget, translation},
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
use world::{Transaction, World};
fn setup(gpu: &render::Gpu) -> TerrainGpu {
    TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 8,
            max_quads: 512,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap()
}
fn config() -> ClientLightingConfig {
    ClientLightingConfig {
        shadows: ShadowConfig {
            resolution: 128,
            half_widths: [16., 32., 64.],
            depth_half_extent: 32.,
            ..ShadowConfig::default()
        },
        sky: SkyPolicy::GeneratedProfile {
            space: SpaceId(0),
            top_y: 15,
        },
        max_journal_entries: 1024,
        cpu_budget: world::lighting::LightBudget {
            max_cells: 512,
            max_neighbor_reads: 3072,
            max_seed_cells: 512,
            max_updates: 512,
        },
        ..ClientLightingConfig::default()
    }
}
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    }
}
fn settle(gpu: &render::Gpu, lighting: &mut ClientLighting, world: &mut World) -> (u64, u64) {
    let mut bytes = 0;
    let mut cells = 0;
    for i in 0..2048 {
        let tick = Tick(world.tick().0 + 1);
        let work = lighting
            .pump(&gpu.queue, world, tick, &mut UploadBudget::new(1000000))
            .unwrap();
        bytes += work.upload_bytes;
        cells += work.cpu_cells;
        if work.ready {
            return (bytes, cells);
        }
        if i == 2047 {
            panic!("lighting did not settle: {work:?}");
        }
    }
    unreachable!()
}
fn read_sample(gpu: &render::Gpu, lighting: &ClientLighting, index: usize) -> u32 {
    read_word(
        gpu,
        lighting,
        index / render::light_volume::LIGHT_SAMPLES * render::light_volume::LIGHT_PAGE_WORDS
            + 1
            + index % render::light_volume::LIGHT_SAMPLES,
    )
}
fn read_word(gpu: &render::Gpu, lighting: &ClientLighting, index: usize) -> u32 {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("CPU to GPU light owner oracle"),
        size: 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(
        lighting.light_volumes().buffer(),
        index as u64 * 4,
        &buffer,
        0,
        4,
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range();
    let result = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    drop(bytes);
    buffer.unmap();
    result
}
#[test]
fn owner_runs_cpu_solver_dirty_uploads_and_shadow_cache_without_geometry_rebuild() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = setup(&gpu);
        let mut world = World::new(rules::properties, 256);
        let mut cells = vec![Cell::AIR; 4096];
        for z in 0..16 {
            for x in 0..16 {
                cells[x + 16 * z] = Cell::from_parts(rules::ids::STONE, 0);
            }
        }
        let torch = BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 8,
            z: 8,
        };
        cells[usize::from(torch.local_index().get())] = Cell::from_parts(rules::ids::TORCH, 0);
        let raw = std::array::from_fn(|i| cells[i].raw());
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        tx.create_page(page(), cells.into_boxed_slice());
        world.commit(tx).unwrap();
        let mut mesh = PageMesh::default();
        mesh_page(
            &PageHalo::from_page(&raw, 0),
            |v| {
                if v == 0 {
                    CellMaterial::EMPTY
                } else {
                    CellMaterial::opaque(3)
                }
            },
            &mut MeshScratch::new(),
            &mut mesh,
        );
        let handle = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &mesh,
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert_eq!(handle.slot, 0);
        let mut lighting = ClientLighting::prepare(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut world,
            config(),
            [8., 0., 8.],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        lighting
            .register_page(page(), handle, WorldBounds::new([0.; 3], [16.; 3]).unwrap())
            .unwrap();
        terrain.take_work();
        let (uploads, cells) = settle(&gpu, &mut lighting, &mut world);
        assert!(uploads >= LIGHT_PAGE_BYTES && cells > 4096);
        let light = world.page(page()).unwrap().light(torch.local_index());
        assert!(light.block >= 14 && light.rgb.iter().all(|v| *v >= 14));
        let sample = read_sample(&gpu, &lighting, light_index([8, 8, 8]).unwrap());
        assert_eq!((sample >> 4) & 15, u32::from(light.block));
        assert_eq!((sample >> 8) & 255, u32::from(light.rgb[0]));
        assert_eq!(
            terrain.take_work().upload_bytes,
            0,
            "lighting changed terrain descriptors"
        );
        let mut role_budget = UploadBudget::new(1000000);
        let overlay = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &mesh,
                &mut role_budget,
            )
            .unwrap();
        terrain
            .set_overlay(&gpu.queue, overlay, true, &mut role_budget)
            .unwrap();
        lighting
            .register_overlay(&gpu.queue, page(), overlay, &mut role_budget)
            .unwrap();
        let model = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([1., 0., 0.]),
                &mesh,
                &mut role_budget,
            )
            .unwrap();
        lighting
            .register_instance(model, torch, WorldBounds::new([8.; 3], [9.; 3]).unwrap())
            .unwrap();
        settle(&gpu, &mut lighting, &mut world);
        assert_eq!(
            read_word(
                &gpu,
                &lighting,
                overlay.slot as usize * render::light_volume::LIGHT_PAGE_WORDS
            ),
            0x80000000 | handle.slot,
            "overlay must follow its base, never replace it"
        );
        assert_ne!(
            read_word(
                &gpu,
                &lighting,
                model.slot as usize * render::light_volume::LIGHT_PAGE_WORDS
            ) & 0x40000000,
            0,
            "model should use explicit uniform header lighting"
        );

        let mut tx = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
        tx.set_cell(torch, Cell::AIR);
        world.commit(tx).unwrap();
        let (_, removal_cells) = settle(&gpu, &mut lighting, &mut world);
        assert!(removal_cells > 0);
        let light = world.page(page()).unwrap().light(torch.local_index());
        assert_eq!(light.block, 0);
        assert_eq!(light.rgb, [0; 3]);
        assert_eq!(
            read_sample(&gpu, &lighting, light_index([8, 8, 8]).unwrap()) >> 8,
            0
        );
        assert_eq!(
            read_sample(
                &gpu,
                &lighting,
                model.slot as usize * render::light_volume::LIGHT_SAMPLES
            ) >> 8,
            0,
            "instance light must follow lamp removal"
        );
        assert_eq!(
            read_word(
                &gpu,
                &lighting,
                overlay.slot as usize * render::light_volume::LIGHT_PAGE_WORDS
            ),
            0x80000000 | handle.slot,
            "RGB updates must preserve live overlay alias"
        );
        lighting.unregister_handle(overlay);
        lighting.unregister_handle(model);

        for _ in 0..3 {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            lighting
                .encode_lighting(
                    &gpu.queue,
                    &mut encoder,
                    &mut terrain,
                    [8., 0., 8.],
                    [0.3, 0.8, 0.2],
                )
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
        }
        let tick = Tick(world.tick().0);
        let idle = lighting
            .pump(
                &gpu.queue,
                &mut world,
                tick,
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert_eq!(idle.cpu_cells, 0);
        assert_eq!(idle.upload_bytes, 0);
        assert!(!lighting.needs_work(&world));
        assert!(lighting.presentation_ready(&world));
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        lighting
            .encode_lighting(
                &gpu.queue,
                &mut encoder,
                &mut terrain,
                [8., 0., 8.],
                [0.4, 0.8, 0.2],
            )
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        assert!(lighting.needs_work(&world));
        assert!(
            lighting.presentation_ready(&world),
            "coherent Sun refresh must not revoke source/data readiness"
        );
        lighting.invalidate_bounds(render::shadows::WorldBounds {
            min: [8., 0., 8.],
            max: [9., 1., 9.],
        });
        assert!(
            !lighting.presentation_ready(&world),
            "structural invalidation must retain coverage guard"
        );
        println!(
            "M4 CPU→GPUowner initialCells={cells},uploadedBytes={uploads},removalCells={removal_cells},idleCells=0,idleUploads=0"
        );
    });
}
#[test]
fn sky_certification_accounts_for_floating_edits_across_unknown_gap() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = setup(&gpu);
        let mut world = World::new(rules::properties, 512);
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        tx.create_page(page(), vec![Cell::AIR; 4096].into_boxed_slice());
        world.commit(tx).unwrap();
        let mut lighting = ClientLighting::prepare(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut world,
            config(),
            [8., 0., 8.],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        settle(&gpu, &mut lighting, &mut world);
        let top = BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 15,
            z: 8,
        };
        assert_eq!(world.page(page()).unwrap().light(top.local_index()).sky, 15);
        let floating = BlockPos { y: 60, ..top };
        let mut tx = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
        tx.create_page(floating.page(), vec![Cell::AIR; 4096].into_boxed_slice());
        tx.set_cell(floating, Cell::from_parts(rules::ids::STONE, 0));
        world.commit(tx).unwrap();
        settle(&gpu, &mut lighting, &mut world);
        assert_eq!(
            world.page(page()).unwrap().light(top.local_index()).sky,
            14,
            "unknown gap must not preserve stale direct sky below a new floating blocker"
        );
        let mut tx = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
        tx.set_cell(floating, Cell::AIR);
        world.commit(tx).unwrap();
        settle(&gpu, &mut lighting, &mut world);
        assert_eq!(
            world.page(page()).unwrap().light(top.local_index()).sky,
            15,
            "removal must recertify direct sky"
        );
    });
}
#[test]
fn bootstrap_larger_than_seed_queue_makes_bounded_progress() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = setup(&gpu);
        let mut world = World::new(rules::properties, 128);
        for x in 0..3 {
            let mut tx = Transaction::new(Tick(0), SpaceId(0));
            tx.create_page(
                PageId { x, ..page() },
                vec![Cell::from_parts(rules::ids::STONE, 0); 4096].into_boxed_slice(),
            );
            world.commit(tx).unwrap();
        }
        let mut options = config();
        options.sky = SkyPolicy::UnknownIsDark;
        options.cpu_limits.max_seed_pages = 1;
        let mut lighting = ClientLighting::prepare(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut world,
            options,
            [8., 0., 8.],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        let (_, cells) = settle(&gpu, &mut lighting, &mut world);
        assert!(cells >= 3 * 4096);
    });
}

#[test]
fn authoritative_replica_keeps_server_light_and_never_runs_local_sky_or_solver() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = setup(&gpu);
        let mut world = World::new(rules::properties, 256);
        let p = BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 8,
            z: 8,
        };
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(page(), vec![Cell::AIR; 4096]);
        tx.set_light(
            p,
            world::LightSample {
                sky: 15,
                block: 3,
                rgb: [15, 0, 0],
            },
        );
        world.commit(tx).unwrap();
        let handle = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &PageMesh::default(),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        // GeneratedProfile is deliberately present before selecting replica mode;
        // with_authoritative_source must replace it with UnknownIsDark.
        let mut lighting = ClientLighting::prepare(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut world,
            config().with_authoritative_source(),
            [8., 0., 8.],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        lighting
            .register_page(page(), handle, WorldBounds::new([0.; 3], [16.; 3]).unwrap())
            .unwrap();
        let boundary = world.journal_next();
        let (bytes, cells) = settle(&gpu, &mut lighting, &mut world);
        assert_eq!(cells, 0);
        assert_eq!(bytes, LIGHT_PAGE_BYTES);
        assert_eq!(world.journal_next(), boundary);
        assert_eq!(world.page(page()).unwrap().light(p.local_index()).sky, 15);
        let index = light_index([8, 8, 8]).unwrap();
        let sample = read_sample(&gpu, &lighting, index);
        assert_eq!((sample >> 8) & 255, 15);
        assert_eq!(sample & 15, 15);
        assert_eq!((sample >> 4) & 15, 3);
        assert_eq!(
            read_sample(&gpu, &lighting, light_index([-1, 8, 8]).unwrap()),
            0,
            "Unknown halo certified as sky"
        );
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_light(
            p,
            world::LightSample {
                sky: 0,
                block: 9,
                rgb: [0, 0, 15],
            },
        );
        world.commit(tx).unwrap();
        let boundary = world.journal_next();
        let (bytes, cells) = settle(&gpu, &mut lighting, &mut world);
        assert_eq!(bytes, 4);
        assert_eq!(cells, 0);
        assert_eq!(world.journal_next(), boundary);
        let sample = read_sample(&gpu, &lighting, index);
        assert_eq!((sample >> 24) & 255, 15);
        assert_eq!(sample & 15, 0);
        assert_eq!((sample >> 4) & 15, 9);
        for _ in 0..100 {
            let work = lighting
                .pump(&gpu.queue, &mut world, Tick(100), &mut UploadBudget::new(0))
                .unwrap();
            assert_eq!(work.cpu_cells, 0);
            assert_eq!(work.cpu_neighbors, 0);
            assert_eq!(work.light_updates, 0);
            assert_eq!(work.pending_seed_pages, 0);
            assert_eq!(work.upload_bytes, 0);
            assert!(work.ready);
        }
        lighting.detach(&mut world);
        println!(
            "M4 authoritative LIGHT initial23332B delta4B,0CPUsolver/sky/derivedTx,UnknownhaloDark,idle1000writes"
        );
    });
}

#[test]
fn scene_source_certificate_gates_zero_snapshot_and_revokes_high_rgb_on_cell_view_change() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = setup(&gpu);
        let point = BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 8,
            z: 8,
        };
        let mut scene = client::ClientScene::new(client::SceneConfig {
            center: point,
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 27,
            authoritative_remote: true,
            ..client::SceneConfig::forest()
        })
        .unwrap();
        let image = |view| {
            net::PageUpdate::Snapshot(net::PageSnapshot {
                page: page(),
                version: view,
                source_version: core_types::Version(500 + view),
                applied_action: Default::default(),
                cells: Box::new([Cell::AIR; 4096]),
            })
        };
        scene
            .import_authoritative_page(&image(10), Tick(1))
            .unwrap();
        let handle = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &PageMesh::default(),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        let mut lighting = ClientLighting::prepare_scene(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut scene,
            config().with_authoritative_source(),
            [8., 0., 8.],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        lighting
            .register_page(page(), handle, WorldBounds::new([0.; 3], [16.; 3]).unwrap())
            .unwrap();
        let unknown = lighting
            .pump_scene(
                &gpu.queue,
                &mut scene,
                Tick(2),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert!(!unknown.ready);
        assert_eq!(unknown.unknown_source_pages, 1);
        assert_eq!(unknown.cpu_cells, 0);
        let packet = |version, light| {
            net::light::Update::Snapshot(net::light::Snapshot {
                header: net::light::Header {
                    page: page(),
                    cell_view: 10,
                    version,
                    source: core_types::Version(1000 + version),
                },
                values: Box::new([light; 4096]),
            })
        };
        let imported = scene
            .import_authoritative_light(&packet(1, 0), Tick(3))
            .unwrap();
        assert!(imported.applied);
        assert!(imported.change.is_none());
        let known = lighting
            .pump_scene(
                &gpu.queue,
                &mut scene,
                Tick(3),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert!(known.ready);
        assert_eq!(known.confirmed_source_pages, 1);
        assert_eq!(known.upload_bytes, LIGHT_PAGE_BYTES);
        let hdr = net::light::pack(15, 7, [231, 17, 255]).unwrap();
        scene
            .import_authoritative_light(&packet(2, hdr), Tick(4))
            .unwrap();
        let before = lighting.ready_diagnostics(&scene);
        assert!(before.authoritative && before.journal_lag_records > 0);
        lighting.take_upload_bytes();
        let cpu = lighting.ingest_scene_cpu(&mut scene).unwrap();
        assert_eq!(cpu.coalesced_light_changes, 4096);
        assert_eq!(cpu.journal_entries, 1);
        assert_eq!(
            (cpu.upload_bytes, cpu.cpu_cells, cpu.sample_reads),
            (0, 0, 0)
        );
        assert_eq!(lighting.take_upload_bytes(), 0);
        assert_eq!(
            read_sample(&gpu, &lighting, light_index([8, 8, 8]).unwrap()),
            0,
            "CPU phase wrote into an in-flight GPU resource"
        );
        let after = lighting.ready_diagnostics(&scene);
        assert_eq!(after.journal_lag_records, 0);
        assert!(!after.partial_record);
        assert_eq!((after.dirty_residents, after.unknown_source_pages), (1, 0));
        let known = loop {
            let work = lighting
                .pump_scene(
                    &gpu.queue,
                    &mut scene,
                    Tick(4),
                    &mut UploadBudget::new(1000000),
                )
                .unwrap();
            if work.ready {
                break work;
            }
        };
        assert!(known.ready);
        assert_eq!(
            read_sample(&gpu, &lighting, light_index([8, 8, 8]).unwrap()),
            hdr,
            "legal RGB8 was truncated or tagged as alias"
        );
        scene
            .import_authoritative_page(&image(11), Tick(5))
            .unwrap();
        assert!(scene.authoritative_light_version(page()).is_none());
        let revoked = lighting
            .pump_scene(
                &gpu.queue,
                &mut scene,
                Tick(5),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert!(!revoked.ready);
        assert_eq!(revoked.unknown_source_pages, 1);
        assert_eq!(
            read_sample(&gpu, &lighting, light_index([8, 8, 8]).unwrap()),
            0
        );
        assert_eq!(revoked.cpu_cells, 0);
        println!(
            "M4 Scene LIGHT defaultzeroUnknown,zeroSnapshotcertifieswithoutWorldTx,RGB[231,17,255]GPUexact,cellView11revokesGPUtoDark"
        );
    });
}

#[test]
fn cpu_ingest_retires_nonresident_metadata_without_losing_future_volume_snapshot() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = setup(&gpu);
        let point = BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 8,
            z: 8,
        };
        let mut scene = client::ClientScene::new(client::SceneConfig {
            center: point,
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 27,
            authoritative_remote: true,
            ..client::SceneConfig::forest()
        })
        .unwrap();
        let mut config = config().with_authoritative_source();
        config.max_gpu_pages_per_pump = 1;
        let mut owner = ClientLighting::prepare_scene(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut scene,
            config,
            [8.; 3],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        scene
            .import_authoritative_page(
                &net::PageUpdate::Snapshot(net::PageSnapshot {
                    page: page(),
                    version: 1,
                    source_version: core_types::Version(1),
                    applied_action: Default::default(),
                    cells: Box::new([Cell::AIR; 4096]),
                }),
                Tick(1),
            )
            .unwrap();
        let mut values = Box::new([0u32; 4096]);
        let packed = net::light::pack(15, 0, [231, 17, 255]).unwrap();
        values[usize::from(point.local_index().get())] = packed;
        scene
            .import_authoritative_light(
                &net::light::Update::Snapshot(net::light::Snapshot {
                    header: net::light::Header {
                        page: page(),
                        cell_view: 1,
                        version: 1,
                        source: core_types::Version(1),
                    },
                    values,
                }),
                Tick(1),
            )
            .unwrap();
        let work = owner.ingest_scene_cpu(&mut scene).unwrap();
        assert_eq!(work.retired_dirty_pages, 27);
        assert_eq!(work.pending_gpu_pages, 0);
        assert_eq!(work.upload_bytes, 0);
        assert_eq!(work.sample_reads, 0);
        let handle = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &PageMesh::default(),
                &mut UploadBudget::new(100000),
            )
            .unwrap();
        owner
            .register_page(
                page(),
                handle,
                WorldBounds {
                    min: [0.; 3],
                    max: [16.; 3],
                },
            )
            .unwrap();
        let work = owner
            .pump_scene(
                &gpu.queue,
                &mut scene,
                Tick(1),
                &mut UploadBudget::new(LIGHT_PAGE_BYTES),
            )
            .unwrap();
        assert_eq!(work.gpu_pages, 1);
        let index = light_index([8, 8, 8]).unwrap() as usize;
        assert_eq!(
            read_word(
                &gpu,
                &owner,
                handle.slot as usize * render::light_volume::LIGHT_PAGE_WORDS + 1 + index
            ),
            packed
        );
        let next = net::light::pack(14, 0, [17, 231, 255]).unwrap();
        scene
            .import_authoritative_light(
                &net::light::Update::Delta(net::light::Delta {
                    header: net::light::Header {
                        page: page(),
                        cell_view: 1,
                        version: 2,
                        source: core_types::Version(2),
                    },
                    base: 1,
                    changes: vec![(point.local_index().get(), next)],
                }),
                Tick(2),
            )
            .unwrap();
        let cpu = owner.ingest_scene_cpu(&mut scene).unwrap();
        assert_eq!(
            cpu.retired_dirty_pages, 0,
            "resident volume cannot be retired"
        );
        assert_eq!(cpu.pending_gpu_pages, 1);
        assert_eq!(cpu.upload_bytes, 0);
        assert_eq!(cpu.sample_reads, 0);
        assert_eq!(
            read_word(
                &gpu,
                &owner,
                handle.slot as usize * render::light_volume::LIGHT_PAGE_WORDS + 1 + index
            ),
            packed,
            "CPU-only phase cannot change GPU data"
        );
        let upload = owner
            .pump_scene(&gpu.queue, &mut scene, Tick(2), &mut UploadBudget::new(4))
            .unwrap();
        assert_eq!(upload.upload_bytes, 4);
        assert_eq!(
            read_word(
                &gpu,
                &owner,
                handle.slot as usize * render::light_volume::LIGHT_PAGE_WORDS + 1 + index
            ),
            next
        );
        println!(
            "M4 CPU_nonresident27retired/0samples/0GPU; laterregistrationfullSnapshot23332B HDR_exact; residentdirtyKEPT then4Bdelta/sourceguardsRetained"
        );
    });
}
