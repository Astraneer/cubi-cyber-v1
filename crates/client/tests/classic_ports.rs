#![cfg(not(target_arch = "wasm32"))]
use client::props_source::{PropsLimits, PropsSource};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use render::{
    block_models::{BlockConfig, BlockModelsGpu, HIDDEN, PORT_MARKER_ONLY, kind},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use world::{Transaction, World};
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 4,
        z: 8,
    }
}
fn records(gpu: &render::Gpu, models: &BlockModelsGpu) -> Vec<[u32; 16]> {
    let bytes = models.instance_buffer().size();
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = gpu.device.create_command_encoder(&Default::default());
    e.copy_buffer_to_buffer(models.instance_buffer(), 0, &read, 0, bytes);
    gpu.queue.submit([e.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = read.slice(..).get_mapped_range();
    let result = mapped
        .chunks_exact(64)
        .map(|b| {
            std::array::from_fn(|i| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap()))
        })
        .filter(|r: &[u32; 16]| r[5] & HIDDEN == 0)
        .collect();
    drop(mapped);
    read.unmap();
    result
}
#[test]
fn source_opt_in_keeps_classic_bodies_in_terrain_and_splits_public_connector_color() {
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
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        models
            .reserve_slots(&gpu.queue, 16, &mut UploadBudget::new(1024))
            .unwrap();
        let mut world = World::new(rules::properties, 1024);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(p(0).page(), vec![Cell::AIR; 4096]);
        let ids = [
            rules::ids::ENRICHMENT_CHAMBER,
            rules::ids::ENERGIZED_SMELTER,
            rules::ids::CRUSHER,
            rules::ids::PURIFICATION_CHAMBER,
            rules::ids::ELECTRIC_PUMP,
        ];
        for (x, id) in ids.into_iter().enumerate() {
            tx.set_cell(p(x as i32), Cell::from_parts(id, 3));
        }
        tx.set_cell(
            p(6),
            Cell::from_parts(rules::ids::LOGISTICAL_TRANSPORTER, 0),
        );
        world.commit(tx).unwrap();
        let mut source = PropsSource::attach(
            &mut world,
            PropsLimits {
                max_pages: 8,
                max_instances: 16,
                max_running: 2,
                max_ready: 2,
                max_age_ticks: 40,
            },
        )
        .unwrap();
        while let Some(job) = source.take_job(&world, |_| Box::new([u32::MAX; 256])) {
            source
                .complete(&world, job.execute(|| false).unwrap())
                .map_err(|_| ())
                .unwrap();
            source.pump_ready(&world, 128).unwrap();
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
        assert_eq!(source.stats(&world).models, 6);
        source.take_work();
        let mut hints = Vec::new();
        for x in 0..5 {
            hints.push(
                net::props::Hint::from_visual(
                    p(x).local_index(),
                    net::props::Kind::Machine,
                    Some(0o543210),
                    false,
                    None,
                    None,
                )
                .unwrap(),
            );
        }
        hints.push(
            net::props::Hint::from_visual(
                p(6).local_index(),
                net::props::Kind::Connector,
                Some(0x555 | (18 << 12)),
                false,
                None,
                None,
            )
            .unwrap(),
        );
        let public = net::props::Frame {
            page: p(0).page(),
            cell_view: 7,
            view: 1,
            source: Version(1),
            tick: Tick(1),
            hints,
        };
        assert!(
            source
                .publish_public_frame(std::sync::Arc::new(public), 7)
                .unwrap()
        );
        assert_eq!(source.pump_public(Tick(1), 0., 16), 7); // six members plus completed-page cursor
        source
            .flush(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(1024),
                16,
            )
            .unwrap();
        let visible = records(&gpu, &models);
        assert_eq!(visible.len(), 6);
        for row in &visible {
            if row[4] == kind::CUBE_MACHINE {
                assert_ne!(row[5] & PORT_MARKER_ONLY, 0);
                assert_eq!(row[15], 0x80000000 | 0o543210);
            } else {
                assert_eq!(row[4], kind::TRANSPORTER);
                assert_eq!(row[6], 0x555);
                assert_eq!(row[15], 0x40000012);
            }
        }
        for id in ids {
            assert_eq!(
                rules::block(id).unwrap().render,
                rules::blocks::Render::Cube
            );
        }
        source.revoke_public_frame(p(0).page(), Tick(2)).unwrap();
        source.pump_public(Tick(2), 0., 16);
        source
            .flush(
                &gpu.queue,
                &mut models,
                [0; 3],
                &mut UploadBudget::new(1024),
                16,
            )
            .unwrap();
        assert!(records(&gpu, &models).iter().all(|r| r[15] == 0));
        assert_eq!(source.take_work().worker_cells, 0);
        source.detach(&mut world);
        println!(
            "M7 publicSource5classicMarkerOnly+coloredTransporter GPUrecordsPASS, terrainBodyRegistryUnchanged, color18/modes0x555 split, revoke0/publicChangesWorkerReads0"
        );
    });
}
