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
        assert_eq!((sample >> 16) & 15, u32::from(light.block));
        assert_eq!(sample & 15, u32::from(light.rgb[0]));
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
            read_sample(
                &gpu,
                &lighting,
                overlay.slot as usize * render::light_volume::LIGHT_SAMPLES
            ),
            0x80000000 | handle.slot,
            "overlay must follow its base, never replace it"
        );
        assert_ne!(
            read_sample(
                &gpu,
                &lighting,
                model.slot as usize * render::light_volume::LIGHT_SAMPLES
            ) & 0x40000000,
            0,
            "model should use four-byte uniform lighting"
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
            read_sample(&gpu, &lighting, light_index([8, 8, 8]).unwrap()) & 0xfff,
            0
        );
        assert_eq!(
            read_sample(
                &gpu,
                &lighting,
                model.slot as usize * render::light_volume::LIGHT_SAMPLES
            ) & 0xfff,
            0,
            "instance light must follow lamp removal"
        );
        assert_eq!(
            read_sample(
                &gpu,
                &lighting,
                overlay.slot as usize * render::light_volume::LIGHT_SAMPLES
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
