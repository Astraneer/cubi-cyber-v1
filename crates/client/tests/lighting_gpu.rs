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
fn source_proof_raw(page: PageId, view: u64, state: u16) -> net::PageUpdate {
    let mut cells = Box::new([Cell::AIR; 4096]);
    cells[8 + 16 * (8 + 16 * 8)] = Cell::from_parts(rules::ids::AIR, state);
    net::PageUpdate::Snapshot(net::PageSnapshot {
        page,
        version: view,
        source_version: core_types::Version(500 + view),
        applied_action: net::ActionId(view),
        cells,
    })
}
fn source_proof_light(page: PageId, cells: u64, version: u64, value: u32) -> net::light::Update {
    net::light::Update::Snapshot(net::light::Snapshot {
        header: net::light::Header {
            page,
            cell_view: cells,
            version,
            source: core_types::Version(1),
        },
        values: Box::new([value; 4096]),
    })
}
fn source_proof_settle(
    gpu: &render::Gpu,
    lighting: &mut ClientLighting,
    scene: &mut client::ClientScene,
) {
    for _ in 0..512 {
        let work = lighting
            .pump_scene(
                &gpu.queue,
                scene,
                Tick(100),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        if work.ready {
            return;
        }
    }
    panic!("source proof fixture did not settle");
}
fn source_proof_volume(
    gpu: &render::Gpu,
    lighting: &ClientLighting,
    handle: render::terrain_gpu::PageHandle,
) -> Vec<u32> {
    source_buffer_words(gpu, lighting.light_volumes().buffer(), handle)
}
fn source_buffer_words(
    gpu: &render::Gpu,
    source: &wgpu::Buffer,
    handle: render::terrain_gpu::PageHandle,
) -> Vec<u32> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("complete real GPU light image equality oracle"),
        size: LIGHT_PAGE_BYTES,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(
        source,
        u64::from(handle.slot) * LIGHT_PAGE_BYTES,
        &buffer,
        0,
        LIGHT_PAGE_BYTES,
    );
    gpu.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receiver.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range();
    let words = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect();
    drop(bytes);
    buffer.unmap();
    words
}
#[test]
fn actual_full_volume_retags_only_after_current_certificates_and_exact_support_proof() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = setup(&gpu);
        let mut scene = client::ClientScene::new(client::SceneConfig {
            center: BlockPos {
                space: SpaceId(0),
                x: 8,
                y: 8,
                z: 8,
            },
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 27,
            authoritative_remote: true,
            ..client::SceneConfig::forest()
        })
        .unwrap();
        let hdr = net::light::pack(15, 7, [231, 17, 255]).unwrap();
        let supports: Vec<_> = scene.requested_pages().collect();
        assert_eq!(supports.len(), 27);
        for source in &supports {
            scene
                .import_authoritative_page(&source_proof_raw(*source, 10, 0), Tick(1))
                .unwrap();
            scene
                .import_authoritative_light(&source_proof_light(*source, 10, 1, hdr), Tick(1))
                .unwrap();
        }
        let mut settings = config().with_authoritative_source();
        // Exercise partial journal ownership rather than relying on one large drain.
        settings.max_journal_entries = 1;
        let mut lighting = ClientLighting::prepare_scene(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut scene,
            settings,
            [8.; 3],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        let mut handle = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &PageMesh::default(),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        let bounds = WorldBounds::new([0.; 3], [16.; 3]).unwrap();
        lighting.register_page(page(), handle, bounds).unwrap();
        source_proof_settle(&gpu, &mut lighting, &mut scene);
        let initial = source_proof_volume(&gpu, &lighting, handle);
        assert_eq!(initial[0], 0);
        assert!(initial[1..].iter().all(|value| *value == hdr));

        // Positive: two actual new certificates precede the next GPU pump. No
        // old Scene certificate is synthesized, and all5832 GPU words stay equal.
        let changed = [PageId { z: -1, ..page() }, PageId { y: -1, ..page() }];
        let versions: Vec<_> = supports
            .iter()
            .map(|p| scene.world().page_version(*p))
            .collect();
        for source in changed {
            let raw = scene
                .import_authoritative_page(&source_proof_raw(source, 11, 0), Tick(2))
                .unwrap();
            assert!(raw.change.is_none());
            assert!(scene.authoritative_light_version(source).is_none());
            let light = scene
                .import_authoritative_light(&source_proof_light(source, 11, 2, hdr), Tick(2))
                .unwrap();
            assert!(light.change.is_none());
        }
        assert_eq!(
            versions,
            supports
                .iter()
                .map(|p| scene.world().page_version(*p))
                .collect::<Vec<_>>()
        );
        let retag = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(2), &mut UploadBudget::new(0))
            .unwrap();
        assert_eq!(
            (retag.source_retags, retag.upload_bytes, retag.gpu_pages),
            (1, 0, 0)
        );
        assert!(retag.ready);
        assert_eq!(source_proof_volume(&gpu, &lighting, handle), initial);

        // Negative1: identical Raw alone still revokes. Eight bytes suffice for
        // the real uniform-zero shader header; no full upload can hide the test.
        scene
            .import_authoritative_page(&source_proof_raw(page(), 12, 0), Tick(3))
            .unwrap();
        assert!(scene.authoritative_light_version(page()).is_none());
        let revoked = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(3), &mut UploadBudget::new(8))
            .unwrap();
        assert_eq!(revoked.source_retags, 0);
        assert_eq!(revoked.unknown_source_pages, 1);
        let dark = source_proof_volume(&gpu, &lighting, handle);
        assert_ne!(dark[0] & (1 << 13), 0);
        assert_eq!(source_region_raster(&gpu, &lighting, handle), [0.; 4]);
        assert!(source_region_raster_at(&gpu, &lighting, handle, [-1.; 3])[0] > 15.);

        // Negative2: an already-cleared slot cannot be recovered by retagging,
        // even when a real same-valued certificate now makes all sources current.
        scene
            .import_authoritative_light(&source_proof_light(page(), 12, 3, hdr), Tick(4))
            .unwrap();
        let waiting = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(4), &mut UploadBudget::new(0))
            .unwrap();
        assert_eq!(waiting.source_retags, 0);
        assert!(!waiting.ready);
        assert_eq!(source_region_raster(&gpu, &lighting, handle), [0.; 4]);
        source_proof_settle(&gpu, &mut lighting, &mut scene);
        assert_eq!(source_proof_volume(&gpu, &lighting, handle), initial);

        // Negative3a: a full image can contain current World samples while an
        // older journal record is partial. A later wire-only change must still
        // refuse a zero-copy retag. Its face repair cannot fit the8B budget.
        let corner = PageId {
            x: -1,
            y: -1,
            z: -1,
            ..page()
        };
        let intermediate = net::light::pack(12, 2, [5, 77, 199]).unwrap();
        scene
            .import_authoritative_light(
                &net::light::Update::Delta(net::light::Delta {
                    header: net::light::Header {
                        page: corner,
                        cell_view: 10,
                        version: 2,
                        source: core_types::Version(2),
                    },
                    base: 1,
                    //4094 lies beyond the primary halo;4095 is its corner.
                    changes: vec![(4094, intermediate), (4095, intermediate)],
                }),
                Tick(5),
            )
            .unwrap();
        scene
            .import_authoritative_page(&source_proof_raw(changed[0], 13, 0), Tick(5))
            .unwrap();
        scene
            .import_authoritative_light(&source_proof_light(changed[0], 13, 3, hdr), Tick(5))
            .unwrap();
        let full_with_partial = lighting
            .pump_scene(
                &gpu.queue,
                &mut scene,
                Tick(5),
                &mut UploadBudget::new(LIGHT_PAGE_BYTES + 16 * 16 * 4),
            )
            .unwrap();
        assert_eq!(full_with_partial.source_retags, 0);
        assert_eq!(
            full_with_partial.upload_bytes,
            LIGHT_PAGE_BYTES + 16 * 16 * 4
        );
        assert!(lighting.ready_diagnostics(&scene).partial_record);
        assert_eq!(
            source_proof_volume(&gpu, &lighting, handle)[1],
            intermediate
        );
        scene
            .import_authoritative_page(&source_proof_raw(changed[1], 14, 0), Tick(5))
            .unwrap();
        scene
            .import_authoritative_light(&source_proof_light(changed[1], 14, 3, hdr), Tick(5))
            .unwrap();
        let partial = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(5), &mut UploadBudget::new(8))
            .unwrap();
        assert!(lighting.ready_diagnostics(&scene).partial_record);
        assert_eq!((partial.source_retags, partial.upload_bytes), (0, 4));
        assert_eq!(
            source_region_raster_at(&gpu, &lighting, handle, [8., -1., 8.]),
            [0.; 4]
        );
        assert!(source_region_raster(&gpu, &lighting, handle)[0] > 15.);
        source_proof_settle(&gpu, &mut lighting, &mut scene);

        // Negative3b: a successful one-corner patch invalidates the full witness.
        let next = net::light::pack(14, 3, [17, 231, 255]).unwrap();
        scene
            .import_authoritative_light(
                &net::light::Update::Delta(net::light::Delta {
                    header: net::light::Header {
                        page: corner,
                        cell_view: 10,
                        version: 3,
                        source: core_types::Version(3),
                    },
                    base: 2,
                    changes: vec![(4095, next)],
                }),
                Tick(5),
            )
            .unwrap();
        lighting.ingest_scene_cpu(&mut scene).unwrap();
        assert!(lighting.ready_diagnostics(&scene).partial_record);
        for _ in 0..32 {
            let state = lighting.ready_diagnostics(&scene);
            if !state.partial_record && state.journal_lag_records == 0 {
                break;
            }
            lighting.ingest_scene_cpu(&mut scene).unwrap();
        }
        let patch = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(5), &mut UploadBudget::new(4))
            .unwrap();
        assert_eq!((patch.source_retags, patch.upload_bytes), (0, 4));
        assert_eq!(source_proof_volume(&gpu, &lighting, handle)[1], next);
        let source = changed[0];
        scene
            .import_authoritative_page(&source_proof_raw(source, 15, 0), Tick(6))
            .unwrap();
        scene
            .import_authoritative_light(&source_proof_light(source, 15, 4, hdr), Tick(6))
            .unwrap();
        let after_patch = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(6), &mut UploadBudget::new(8))
            .unwrap();
        assert_eq!(after_patch.source_retags, 0);
        assert_eq!(
            source_region_raster_at(&gpu, &lighting, handle, [8., 8., -1.]),
            [0.; 4]
        );
        assert!(source_region_raster(&gpu, &lighting, handle)[0] > 15.);
        source_proof_settle(&gpu, &mut lighting, &mut scene);

        // Negative4: a raw mutation with no derived channel still changes the
        // actual whole-page support and cannot reuse the old image proof.
        let mesh = scene.world().input_version(page(), client::MESH_CHANNELS);
        let light = scene
            .world()
            .input_version(page(), core_types::ChannelMask::LIGHTING);
        let before = scene.world().page_version(page());
        let raw = scene
            .import_authoritative_page(&source_proof_raw(page(), 14, 9), Tick(7))
            .unwrap();
        assert!(raw.change.unwrap().channels.is_empty());
        assert_ne!(scene.world().page_version(page()), before);
        assert_eq!(
            scene.world().input_version(page(), client::MESH_CHANNELS),
            mesh
        );
        assert_eq!(
            scene
                .world()
                .input_version(page(), core_types::ChannelMask::LIGHTING),
            light
        );
        scene
            .import_authoritative_light(&source_proof_light(page(), 14, 4, hdr), Tick(7))
            .unwrap();
        let raw_changed = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(7), &mut UploadBudget::new(8))
            .unwrap();
        assert_eq!(raw_changed.source_retags, 0);
        source_proof_settle(&gpu, &mut lighting, &mut scene);

        // Negative5: removal/recreation cannot ABA back to a stored support.
        scene.remove_authoritative_page(corner, Tick(8)).unwrap();
        scene
            .import_authoritative_page(&source_proof_raw(corner, 20, 0), Tick(9))
            .unwrap();
        scene
            .import_authoritative_light(&source_proof_light(corner, 20, 1, hdr), Tick(9))
            .unwrap();
        let recreated = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(9), &mut UploadBudget::new(8))
            .unwrap();
        assert_eq!(recreated.source_retags, 0);
        source_proof_settle(&gpu, &mut lighting, &mut scene);

        // Negative6: terrain slot reuse creates a new generation. Registration
        // has no inherited proof even when every source certificate is current.
        lighting.unregister_handle(handle);
        terrain
            .release_page(&gpu.queue, handle, &mut UploadBudget::new(1000000))
            .unwrap();
        let old_handle = handle;
        handle = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &PageMesh::default(),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert_eq!(handle.slot, old_handle.slot);
        assert_ne!(handle.generation, old_handle.generation);
        assert!(!lighting.light_volumes().retains_full_image(handle, 1));
        lighting.register_page(page(), handle, bounds).unwrap();
        let new_generation = lighting
            .pump_scene(&gpu.queue, &mut scene, Tick(10), &mut UploadBudget::new(0))
            .unwrap();
        assert_eq!(new_generation.source_retags, 0);
        assert!(!new_generation.ready);
        source_proof_settle(&gpu, &mut lighting, &mut scene);

        // Negative7: reconnect prepares a new lighting owner; no successful GPU
        // image/proof can be inherited from another owner or its local clocks.
        drop(lighting);
        let mut fresh = ClientLighting::prepare_scene(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            &mut scene,
            settings,
            [8.; 3],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        fresh.register_page(page(), handle, bounds).unwrap();
        let new_owner = fresh
            .pump_scene(&gpu.queue, &mut scene, Tick(11), &mut UploadBudget::new(0))
            .unwrap();
        assert_eq!(new_owner.source_retags, 0);
        assert!(!new_owner.ready);
        source_proof_settle(&gpu, &mut fresh, &mut scene);
        assert!(
            source_proof_volume(&gpu, &fresh, handle)[1..]
                .iter()
                .all(|value| *value == hdr)
        );
    });
}

struct SourceRegionFixture {
    terrain: TerrainGpu,
    scene: client::ClientScene,
    lighting: ClientLighting,
    handle: render::terrain_gpu::PageHandle,
    hdr: u32,
}
fn source_region_fixture(gpu: &render::Gpu) -> SourceRegionFixture {
    let mut terrain = setup(gpu);
    let mut scene = client::ClientScene::new(client::SceneConfig {
        center: BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 8,
            z: 8,
        },
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        authoritative_remote: true,
        ..client::SceneConfig::forest()
    })
    .unwrap();
    let hdr = net::light::pack(15, 7, [231, 17, 255]).unwrap();
    for source in scene.requested_pages().collect::<Vec<_>>() {
        scene
            .import_authoritative_page(&source_proof_raw(source, 10, 0), Tick(1))
            .unwrap();
        scene
            .import_authoritative_light(&source_proof_light(source, 10, 1, hdr), Tick(1))
            .unwrap();
    }
    let mut settings = config().with_authoritative_source();
    settings.max_gpu_pages_per_pump = 1;
    let mut lighting = ClientLighting::prepare_scene(
        &gpu.device,
        &gpu.queue,
        &mut terrain,
        &mut scene,
        settings,
        [8.; 3],
        [0.3, 0.8, 0.2],
    )
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
    lighting
        .register_page(page(), handle, WorldBounds::new([0.; 3], [16.; 3]).unwrap())
        .unwrap();
    source_proof_settle(gpu, &mut lighting, &mut scene);
    SourceRegionFixture {
        terrain,
        scene,
        lighting,
        handle,
        hdr,
    }
}
// Real float raster using the unchanged product's header/alias/sample shader.
// This proves that a preserved unrelated sample is physically visible and HDR,
// rather than merely leaving stale words behind a uniform-black header.
fn source_region_raster(
    gpu: &render::Gpu,
    lighting: &ClientLighting,
    handle: render::terrain_gpu::PageHandle,
) -> [f32; 4] {
    source_region_raster_at(gpu, lighting, handle, [8.; 3])
}
fn source_region_raster_at(
    gpu: &render::Gpu,
    lighting: &ClientLighting,
    handle: render::terrain_gpu::PageHandle,
    position: [f32; 3],
) -> [f32; 4] {
    source_buffer_raster(gpu, lighting.light_volumes().buffer(), handle, position, 1)
}
fn source_buffer_raster(
    gpu: &render::Gpu,
    buffer: &wgpu::Buffer,
    handle: render::terrain_gpu::PageHandle,
    position: [f32; 3],
    samples: u32,
) -> [f32; 4] {
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("source repair actual product LIGHT sampling raster"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    r#"{}
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{{
 let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}}
@fragment fn fragment()->@location(0)vec4<f32>{{
 return sample_page_light({}u,vec3({:?},{:?},{:?}),vec3(0.,1.,0.));}}
"#,
                    include_str!("../../render/src/light_volume.wgsl"),
                    handle.slot,
                    position[0],
                    position[1],
                    position[2]
                )
                .into(),
            ),
        });
    let format = if samples == 1 {
        wgpu::TextureFormat::Rgba32Float
    } else {
        wgpu::TextureFormat::Rgba16Float
    };
    let row_bytes = if samples == 1 { 512 } else { 256 };
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("actual HDR source-region repair oracle"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("actual LIGHT volume"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 12,
            resource: buffer.as_entire_binding(),
        }],
    });
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("16 actual HDR fragments"),
        size: wgpu::Extent3d {
            width: 32,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("completed HDR fragment readback"),
        size: row_bytes as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let msaa = (samples > 1).then(|| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("actual four-sample LIGHT mask raster"),
            size: wgpu::Extent3d {
                width: 32,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
    });
    let view = msaa
        .as_ref()
        .unwrap_or(&texture)
        .create_view(&Default::default());
    let resolved = texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("source repair retained sample actual raster"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: (samples > 1).then_some(&resolved),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &read,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 32,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receiver.recv().unwrap().unwrap();
    let bytes = read.slice(..).get_mapped_range();
    let result = std::array::from_fn(|i| {
        if samples == 1 {
            f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        } else {
            let h = u16::from_le_bytes(bytes[i * 2..i * 2 + 2].try_into().unwrap());
            let sign = if h & 0x8000 == 0 { 1.0 } else { -1.0 };
            let e = (h >> 10) & 31;
            let m = h & 1023;
            sign * if e == 0 {
                f32::from(m) * 2f32.powi(-24)
            } else if e == 31 {
                if m == 0 { f32::INFINITY } else { f32::NAN }
            } else {
                (1.0 + f32::from(m) / 1024.0) * 2f32.powi(i32::from(e) - 15)
            }
        }
    });
    drop(bytes);
    read.unmap();
    result
}

#[test]
fn changed_source_regions_repair_real_patched_gpu_image_without_blacking_untouched_hdr() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut f = source_region_fixture(&gpu);
        let corner = PageId {
            x: -1,
            y: -1,
            z: -1,
            ..page()
        };
        let patched = net::light::pack(12, 3, [5, 77, 199]).unwrap();
        f.scene
            .import_authoritative_light(
                &net::light::Update::Delta(net::light::Delta {
                    header: net::light::Header {
                        page: corner,
                        cell_view: 10,
                        version: 2,
                        source: core_types::Version(2),
                    },
                    base: 1,
                    changes: vec![(4095, patched)],
                }),
                Tick(2),
            )
            .unwrap();
        let work = f
            .lighting
            .pump_scene(&gpu.queue, &mut f.scene, Tick(2), &mut UploadBudget::new(4))
            .unwrap();
        assert_eq!((work.upload_bytes, work.sample_reads), (4, 1));
        let before = source_proof_volume(&gpu, &f.lighting, f.handle);
        assert_eq!(before[0], 0);
        assert_eq!(before[1], patched);

        // A real prior PATCH erased the successful-FULL witness. Two new
        // current sources cover a256-word bottom face and16-word bottom edge.
        let bottom = PageId { y: -1, ..page() };
        let edge = PageId {
            y: -1,
            z: -1,
            ..page()
        };
        for source in [bottom, edge] {
            assert!(
                f.scene
                    .import_authoritative_page(&source_proof_raw(source, 11, 0), Tick(3))
                    .unwrap()
                    .change
                    .is_none()
            );
            assert!(f.scene.authoritative_light_version(source).is_none());
            assert!(
                f.scene
                    .import_authoritative_light(&source_proof_light(source, 11, 2, f.hdr), Tick(3))
                    .unwrap()
                    .change
                    .is_none()
            );
        }
        let work = f
            .lighting
            .pump_scene(
                &gpu.queue,
                &mut f.scene,
                Tick(3),
                &mut UploadBudget::new(272 * 4),
            )
            .unwrap();
        // Old code with the same fixture queues the8B uniform-black header;
        // comparing every real GPU word here fails before telemetry assertions.
        assert_eq!(source_proof_volume(&gpu, &f.lighting, f.handle), before);
        let hdr = source_region_raster(&gpu, &f.lighting, f.handle);
        assert!((hdr[0] - 231. / 15.).abs() < 0.0001 && hdr[2] > 16.);
        assert_eq!(
            (work.source_retags, work.upload_bytes, work.sample_reads),
            (0, 1088, 272)
        );
        assert!(!work.ready && work.pending_gpu_pages > 0);

        // A current changed corner repairs exactly one actual sample, even
        // with ample byte credit: repair+normal uploads share5832 sample credit.
        let value = net::light::pack(14, 2, [19, 221, 255]).unwrap();
        f.scene
            .import_authoritative_page(&source_proof_raw(corner, 12, 0), Tick(4))
            .unwrap();
        // The previous corner Delta carried source2; the replacement snapshot
        // must preserve monotonic source causality, independently of cell_view.
        let corner_light = net::light::Update::Snapshot(net::light::Snapshot {
            header: net::light::Header {
                page: corner,
                cell_view: 12,
                version: 3,
                source: core_types::Version(3),
            },
            values: Box::new([value; 4096]),
        });
        f.scene
            .import_authoritative_light(&corner_light, Tick(4))
            .unwrap();
        let work = f
            .lighting
            .pump_scene(
                &gpu.queue,
                &mut f.scene,
                Tick(4),
                &mut UploadBudget::new(2 * LIGHT_PAGE_BYTES),
            )
            .unwrap();
        let mut expected = before.clone();
        expected[1] = value;
        assert_eq!(source_proof_volume(&gpu, &f.lighting, f.handle), expected);
        assert_eq!((work.upload_bytes, work.sample_reads), (4, 1));
        assert!(work.backpressure && !work.ready);

        // Actual observed None zeros its entire16-word source edge; unrelated
        // corner/center/HDR words stay exactly equal, without any fabricated cert.
        f.scene
            .import_authoritative_page(&source_proof_raw(edge, 12, 0), Tick(5))
            .unwrap();
        assert!(f.scene.authoritative_light_version(edge).is_none());
        let work = f
            .lighting
            .pump_scene(
                &gpu.queue,
                &mut f.scene,
                Tick(5),
                &mut UploadBudget::new(64),
            )
            .unwrap();
        for x in 0..16 {
            expected[1 + light_index([x, -1, -1]).unwrap()] = 0;
        }
        assert_eq!(source_proof_volume(&gpu, &f.lighting, f.handle), expected);
        assert_eq!((work.upload_bytes, work.sample_reads), (64, 16));
        assert!(source_region_raster(&gpu, &f.lighting, f.handle)[0] > 15.);

        // One byte short cannot partially repair a face. The original full
        // revoke remains the fallback, physically black in the product shader.
        f.scene
            .import_authoritative_page(&source_proof_raw(bottom, 12, 0), Tick(6))
            .unwrap();
        f.scene
            .import_authoritative_light(&source_proof_light(bottom, 12, 3, f.hdr), Tick(6))
            .unwrap();
        let work = f
            .lighting
            .pump_scene(
                &gpu.queue,
                &mut f.scene,
                Tick(6),
                &mut UploadBudget::new(1023),
            )
            .unwrap();
        assert_ne!(
            source_proof_volume(&gpu, &f.lighting, f.handle)[0] & (1 << 4),
            0
        );
        assert_eq!(
            source_region_raster_at(&gpu, &f.lighting, f.handle, [8., -1., 8.]),
            [0.; 4]
        );
        assert!(source_region_raster(&gpu, &f.lighting, f.handle)[0] > 15.);
        assert_eq!(work.upload_bytes, 4);
        assert_eq!(work.sample_reads, 0);
        // Repairing another source cannot clear the still-revoked bottom box.
        f.scene
            .import_authoritative_light(&source_proof_light(edge, 12, 3, f.hdr), Tick(7))
            .unwrap();
        let work = f
            .lighting
            .pump_scene(
                &gpu.queue,
                &mut f.scene,
                Tick(7),
                &mut UploadBudget::new(64),
            )
            .unwrap();
        assert_eq!(work.upload_bytes, 64);
        assert_eq!(work.sample_reads, 16);
        assert_eq!(
            source_region_raster_at(&gpu, &f.lighting, f.handle, [8., -1., 8.]),
            [0.; 4]
        );
        assert!(source_region_raster(&gpu, &f.lighting, f.handle)[0] > 15.);
        source_proof_settle(&gpu, &mut f.lighting, &mut f.scene);
        assert!(source_region_raster(&gpu, &f.lighting, f.handle)[0] > 15.);
    });
}

#[test]
fn source_region_repair_reserves_later_residents_mandatory_revoke_bytes() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut f = source_region_fixture(&gpu);
        let right = PageId { x: 1, ..page() };
        let handle = f
            .terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([16., 0., 0.]),
                &PageMesh::default(),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        f.lighting
            .register_page(
                right,
                handle,
                WorldBounds::new([16., 0., 0.], [32., 16., 16.]).unwrap(),
            )
            .unwrap();
        source_proof_settle(&gpu, &mut f.lighting, &mut f.scene);
        let left = PageId { x: -1, ..page() };
        f.scene
            .import_authoritative_page(&source_proof_raw(left, 11, 0), Tick(2))
            .unwrap();
        f.scene
            .import_authoritative_light(&source_proof_light(left, 11, 2, f.hdr), Tick(2))
            .unwrap();
        f.scene
            .import_authoritative_page(&source_proof_raw(right, 11, 0), Tick(2))
            .unwrap();
        assert!(f.scene.authoritative_light_version(right).is_none());
        // First resident's two256-word faces individually fit2048B, but
        // spending them would starve the later resident's mandatory8B revoke.
        let work = f
            .lighting
            .pump_scene(
                &gpu.queue,
                &mut f.scene,
                Tick(2),
                &mut UploadBudget::new(2048),
            )
            .unwrap();
        let image = source_proof_volume(&gpu, &f.lighting, handle);
        assert_ne!(image[0] & (1 << 13), 0);
        assert_eq!(source_region_raster(&gpu, &f.lighting, handle), [0.; 4]);
        assert!(source_region_raster(&gpu, &f.lighting, f.handle)[0] > 15.);
        assert_eq!(work.upload_bytes, 8);
        assert_eq!(work.sample_reads, 0);
    });
}

#[test]
fn source_region_slot_guard_rejects_uniform_alias_stale_generation_and_revision() {
    pollster::block_on(async {
        use render::{
            light_volume::{LightUpdate, LightVolumeGpu, PackedLight},
            terrain_gpu::PageHandle,
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut volumes = LightVolumeGpu::new(&gpu.device, 3).unwrap();
        let base = PageHandle {
            slot: 0,
            generation: 1,
        };
        let alias = PageHandle {
            slot: 1,
            generation: 1,
        };
        let value = PackedLight::new(15, 7, [231, 17, 255]).unwrap();
        assert!(!volumes.retains_full_image(base, 0));
        volumes
            .upload_page(
                &gpu.queue,
                base,
                1,
                &[value; render::light_volume::LIGHT_SAMPLES],
                &mut UploadBudget::new(LIGHT_PAGE_BYTES),
            )
            .unwrap();
        volumes
            .upload_patch(
                &gpu.queue,
                base,
                2,
                &[LightUpdate { index: 0, value }],
                &mut UploadBudget::new(4),
            )
            .unwrap();
        assert!(volumes.retains_full_image(base, 2));
        assert!(!volumes.retains_full_image(base, 1));
        assert!(!volumes.retains_full_image(
            PageHandle {
                generation: 2,
                ..base
            },
            2
        ));
        volumes
            .alias_page(&gpu.queue, alias, base, &mut UploadBudget::new(4))
            .unwrap();
        assert!(!volumes.retains_full_image(alias, 2));
        volumes
            .upload_uniform(
                &gpu.queue,
                base,
                3,
                PackedLight::default(),
                &mut UploadBudget::new(8),
            )
            .unwrap();
        assert!(!volumes.retains_full_image(base, 3));
    });
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
fn static_world_metadata_batch_exhausts_sample_credit_without_blacking_unchanged_source_box() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut f = source_region_fixture(&gpu);
        let right = PageId { x: 1, ..page() };
        let handle = f
            .terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([16., 0., 0.]),
                &PageMesh::default(),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        f.lighting
            .register_page(
                right,
                handle,
                WorldBounds::new([16., 0., 0.], [32., 16., 16.]).unwrap(),
            )
            .unwrap();
        source_proof_settle(&gpu, &mut f.lighting, &mut f.scene);
        // One boundary light changes both real volumes by successful sparse
        // PATCHes, invalidating their conservative FULL witnesses.
        let value = net::light::pack(14, 3, [17, 231, 255]).unwrap();
        f.scene
            .import_authoritative_light(
                &net::light::Update::Delta(net::light::Delta {
                    header: net::light::Header {
                        page: page(),
                        cell_view: 10,
                        version: 2,
                        source: core_types::Version(2),
                    },
                    base: 1,
                    changes: vec![(15 + 16 * 8 + 256 * 8, value)],
                }),
                Tick(2),
            )
            .unwrap();
        assert_eq!(
            f.lighting
                .pump_scene(&gpu.queue, &mut f.scene, Tick(2), &mut UploadBudget::new(4))
                .unwrap()
                .upload_bytes,
            4
        );
        assert_eq!(
            f.lighting
                .pump_scene(&gpu.queue, &mut f.scene, Tick(2), &mut UploadBudget::new(4))
                .unwrap()
                .upload_bytes,
            4
        );
        let before = source_proof_volume(&gpu, &f.lighting, handle);
        let world = f.scene.world().version();
        for source in [page(), right] {
            let values = Box::new(std::array::from_fn(|i| {
                let light = f
                    .scene
                    .world()
                    .page(source)
                    .unwrap()
                    .light(core_types::LocalIdx::new(i as u16).unwrap());
                net::light::pack(light.sky, light.block, light.rgb).unwrap()
            }));
            assert!(
                f.scene
                    .import_authoritative_page(&source_proof_raw(source, 11, 0), Tick(3))
                    .unwrap()
                    .change
                    .is_none()
            );
            assert!(
                f.scene
                    .import_authoritative_light(
                        &net::light::Update::Snapshot(net::light::Snapshot {
                            header: net::light::Header {
                                page: source,
                                cell_view: 11,
                                version: 3,
                                source: core_types::Version(3)
                            },
                            values,
                        }),
                        Tick(3)
                    )
                    .unwrap()
                    .change
                    .is_none()
            );
        }
        assert_eq!(f.scene.world().version(), world);
        let work = f
            .lighting
            .pump_scene(
                &gpu.queue,
                &mut f.scene,
                Tick(3),
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        // First4352-sample repair leaves only1480 of the5832 shared credit.
        // Second4352-sample repair must refuse; its two changed source boxes
        // become masked with4B. The independent north source must stay HDR.
        let after = source_proof_volume(&gpu, &f.lighting, handle);
        assert_eq!(&after[1..], &before[1..]);
        assert_eq!(after[0], (1 << 12) | (1 << 13));
        assert_eq!(source_region_raster(&gpu, &f.lighting, handle), [0.; 4]);
        assert!(source_region_raster_at(&gpu, &f.lighting, handle, [8., 8., -1.])[0] > 15.);
        assert_eq!((work.sample_reads, work.upload_bytes), (4352, 4352 * 4 + 4));
        assert!(!work.ready && work.pending_gpu_pages > 0);
        source_proof_settle(&gpu, &mut f.lighting, &mut f.scene);
        assert_eq!(source_proof_volume(&gpu, &f.lighting, handle)[0], 0);
        assert!(source_region_raster(&gpu, &f.lighting, handle)[0] > 15.);
    });
}

#[test]
fn source_mask_and_complete_repair_are_atomic_and_generic_patch_cannot_clear_revocation() {
    pollster::block_on(async {
        use render::{
            light_volume::{LightUpdate, LightUploadError, LightVolumeGpu, PackedLight},
            terrain_gpu::PageHandle,
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut light = LightVolumeGpu::new(&gpu.device, 2).unwrap();
        let handle = PageHandle {
            slot: 0,
            generation: 1,
        };
        let hdr = PackedLight::new(15, 7, [240, 0, 240]).unwrap();
        light
            .upload_page(
                &gpu.queue,
                handle,
                1,
                &[hdr; render::light_volume::LIGHT_SAMPLES],
                &mut UploadBudget::new(LIGHT_PAGE_BYTES),
            )
            .unwrap();
        let before = source_buffer_words(&gpu, light.buffer(), handle);
        assert_eq!(
            light.revoke_sources(&gpu.queue, handle, 2, 1, &mut UploadBudget::new(3)),
            Err(LightUploadError::UploadBudget)
        );
        assert_eq!(source_buffer_words(&gpu, light.buffer(), handle), before);
        light
            .revoke_sources(&gpu.queue, handle, 2, 1, &mut UploadBudget::new(4))
            .unwrap();
        assert!(!light.retains_full_image(handle, 2));
        assert!(light.retains_sample_image(handle, 2));
        assert_eq!(
            source_buffer_raster(&gpu, light.buffer(), handle, [-1.; 3], 1),
            [0.; 4]
        );
        assert_eq!(
            source_buffer_raster(&gpu, light.buffer(), handle, [8.; 3], 1),
            [16., 0., 16., 1.]
        );
        let changed = PackedLight::new(12, 5, [120, 0, 120]).unwrap();
        let update = [LightUpdate {
            index: 0,
            value: changed,
        }];
        light
            .upload_patch(&gpu.queue, handle, 3, &update, &mut UploadBudget::new(4))
            .unwrap();
        assert_eq!(
            source_buffer_raster(&gpu, light.buffer(), handle, [-1.; 3], 1),
            [0.; 4]
        );
        let before = source_buffer_words(&gpu, light.buffer(), handle);
        assert_eq!(
            light.upload_sources(&gpu.queue, handle, 4, 1, &update, &mut UploadBudget::new(4)),
            Err(LightUploadError::UploadBudget)
        );
        assert_eq!(source_buffer_words(&gpu, light.buffer(), handle), before);
        assert_eq!(
            light.upload_sources(
                &gpu.queue,
                handle,
                4,
                1 << 13,
                &update,
                &mut UploadBudget::new(1000000)
            ),
            Err(LightUploadError::InvalidIndex)
        );
        assert_eq!(source_buffer_words(&gpu, light.buffer(), handle), before);
        light
            .upload_sources(&gpu.queue, handle, 4, 1, &update, &mut UploadBudget::new(8))
            .unwrap();
        assert_eq!(source_buffer_words(&gpu, light.buffer(), handle)[0], 0);
        assert_eq!(
            source_buffer_raster(&gpu, light.buffer(), handle, [-1.; 3], 1)[0],
            8.
        );
        let before = source_buffer_words(&gpu, light.buffer(), handle);
        assert_eq!(
            light.revoke_sources(&gpu.queue, handle, 5, 1 << 13, &mut UploadBudget::new(0)),
            Err(LightUploadError::UploadBudget)
        );
        assert_eq!(source_buffer_words(&gpu, light.buffer(), handle), before);
    });
}

#[test]
fn alias_and_uniform_region_masks_are_visible_in_aa1_aa4_and_full_upload_resets_them() {
    pollster::block_on(async {
        use render::{
            light_volume::{LightUpdate, LightUploadError, LightVolumeGpu, PackedLight},
            terrain_gpu::PageHandle,
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut light = LightVolumeGpu::new(&gpu.device, 2).unwrap();
        let base = PageHandle {
            slot: 0,
            generation: 1,
        };
        let alias = PageHandle {
            slot: 1,
            generation: 1,
        };
        let hdr = PackedLight::new(15, 7, [240, 0, 240]).unwrap();
        light
            .upload_uniform(&gpu.queue, base, 1, hdr, &mut UploadBudget::new(8))
            .unwrap();
        light
            .alias_page(&gpu.queue, alias, base, &mut UploadBudget::new(4))
            .unwrap();
        assert!(light.retains_region_image(base, 1));
        assert!(!light.retains_sample_image(base, 1));
        assert!(!light.retains_region_image(alias, 1));
        assert!(!light.retains_region_image(
            PageHandle {
                generation: 2,
                ..base
            },
            1
        ));
        light
            .revoke_sources(&gpu.queue, base, 2, 1, &mut UploadBudget::new(4))
            .unwrap();
        for aa in [1, 4] {
            assert_eq!(
                source_buffer_raster(&gpu, light.buffer(), alias, [-1.; 3], aa),
                [0.; 4]
            );
            assert_eq!(
                source_buffer_raster(&gpu, light.buffer(), alias, [8.; 3], aa),
                [16., 0., 16., 1.]
            );
        }
        assert_eq!(
            light.upload_sources(
                &gpu.queue,
                base,
                3,
                1,
                &[LightUpdate {
                    index: 0,
                    value: hdr
                }],
                &mut UploadBudget::new(8)
            ),
            Err(LightUploadError::StaleHandle)
        );
        assert_eq!(
            light.revoke_sources(&gpu.queue, alias, 3, 1, &mut UploadBudget::new(4)),
            Err(LightUploadError::StaleHandle)
        );
        light
            .upload_page(
                &gpu.queue,
                base,
                3,
                &[hdr; render::light_volume::LIGHT_SAMPLES],
                &mut UploadBudget::new(LIGHT_PAGE_BYTES),
            )
            .unwrap();
        assert_eq!(source_buffer_words(&gpu, light.buffer(), base)[0], 0);
        assert!(light.retains_full_image(base, 3));
        for aa in [1, 4] {
            assert_eq!(
                source_buffer_raster(&gpu, light.buffer(), alias, [-1.; 3], aa),
                [16., 0., 16., 1.]
            );
        }
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
        // Filling a previously unknown source may first repair its4096-word
        // region, then finish the unchanged full-publication queue in this pump.
        assert!((LIGHT_PAGE_BYTES..=LIGHT_PAGE_BYTES + 4096 * 4).contains(&known.upload_bytes));
        assert!(known.sample_reads <= 4 * render::light_volume::LIGHT_SAMPLES as u64);
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
