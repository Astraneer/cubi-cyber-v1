//! Actual View: full immutable GI source jobs and shared per-frame publication.
use client::{
    ClientScene, SceneConfig, native_pool,
    view::{ClientView, EnvironmentProfile},
};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, Version};
use net::{
    PageSnapshot, PageUpdate,
    light::{Header, Snapshot, Update, pack},
};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    gi::GiConfig,
    post::{Exposure, PostConfig},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

#[test]
fn actual_view_publishes_worker_sources_and_reports_worker_reads_once() {
    actual_view_gi(None);
    actual_view_gi(Some(1));
}
fn actual_view_gi(contact_quality: Option<u8>) {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 32,
            max_quads: 262144,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let mut view = ClientView::new(
        renderer,
        SceneConfig {
            center: BlockPos {
                space: SpaceId(0),
                x: 8,
                y: 4,
                z: 8,
            },
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 32,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    let mut lighting =
        client::lighting::ClientLightingConfig::default().with_authoritative_source();
    lighting.shadows.resolution = 32;
    lighting.shadows.max_updates_per_frame = 3;
    lighting.shadows.dynamic_casters = true;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        96,
        64,
        OutputTransfer::Attachment,
        EnvironmentProfile {
            graphics: EnvironmentConfig {
                contact_ao: contact_quality.map(|quality| render::contact_ao::ContactAoConfig {
                    quality,
                    ..Default::default()
                }),
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                gi: Some(GiConfig {
                    world_pages: [3, 3, 3],
                    probe_counts: [2, 1, 2],
                    max_jobs: 4,
                    ..Default::default()
                }),
                post: PostConfig {
                    exposure: Exposure::Manual(1.),
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
    let pages: Vec<PageId> = view.scene.requested_pages().collect();
    for page in pages {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if page.y == 0 {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * z] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
        }
        let mut light_values = Box::new([pack(0, 0, [0; 3]).unwrap(); 4096]);
        if page.y == 0 {
            for z in 0..16 {
                for x in 0..16 {
                    light_values[x + 16 * z + 256] = pack(0, 15, [0, 0, 15]).unwrap();
                }
            }
        }
        view.scene
            .import_authoritative_page(
                &PageUpdate::Snapshot(PageSnapshot {
                    page,
                    version: 1,
                    source_version: Version(1),
                    applied_action: Default::default(),
                    cells,
                }),
                Tick(1),
            )
            .unwrap();
        view.scene
            .import_authoritative_light(
                &Update::Snapshot(Snapshot {
                    header: Header {
                        page,
                        cell_view: 1,
                        version: 1,
                        source: Version(2),
                    },
                    values: light_values,
                }),
                Tick(2),
            )
            .unwrap();
    }
    assert!(
        view.set_remote_entities(
            &[rules::entities::mob::Instance {
                position: [28., 13., 28.],
                kind: 0,
                id_low: 1001,
                scale: 1.,
                ..Default::default()
            }],
            SpaceId(0),
            [0; 3],
        )
        .unwrap()
    );
    let known = view.scene.world().loaded_pages();
    let pool = native_pool(2, 8).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut worker_reads = 0;
    loop {
        assert!(
            Instant::now() < deadline,
            "actual View GI failed to settle: {:?}",
            view.environment_stats()
        );
        view.scene
            .pump_native(&pool, Tick(2), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let work = view.take_frame_work();
        let detail = view.last_frame_work_details();
        worker_reads += detail.workers.cells_scanned;
        assert_eq!(
            work.cells_scanned,
            detail.foreground.cells_scanned + detail.workers.cells_scanned
        );
        if view.scene.is_ready()
            && view.stats().resident_pages == 1
            && view.environment_stats().unwrap().source_ready
        {
            break;
        }
        std::thread::yield_now();
    }
    let stats = view.environment_stats().unwrap();
    assert!(stats.source_jobs.completed > 0);
    let source_config = view.environment_mut().unwrap().gpu.gi().unwrap().config();
    let mut known_sources = 0;
    for y in 0..source_config.world_pages[1] {
        for z in 0..source_config.world_pages[2] {
            for x in 0..source_config.world_pages[0] {
                known_sources += usize::from(
                    view.scene
                        .world()
                        .page(PageId {
                            space: SpaceId(0),
                            x: source_config.world_origin_page[0] + x as i32,
                            y: source_config.world_origin_page[1] + y as i32,
                            z: source_config.world_origin_page[2] + z as i32,
                        })
                        .is_some(),
                );
            }
        }
    }
    assert!(known_sources > 0);
    assert!(stats.source_jobs.worker_reads >= known_sources as u64 * 4096);
    assert!(
        worker_reads >= stats.source_jobs.worker_reads,
        "GI workers must be reported under completed worker work"
    );
    assert_eq!(stats.source_jobs.wire_reads, 0);
    assert_eq!(
        view.scene.world().loaded_pages(),
        known,
        "GI must not generate Unknown"
    );
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual View directional GI"),
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
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(
        &gpu.queue,
        &mut encoder,
        &target.create_view(&Default::default()),
        96,
        64,
    )
    .unwrap();
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual View GI probe witness"),
        size: 4 * 128,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_buffer_to_buffer(
        view.environment_mut()
            .unwrap()
            .gpu
            .gi()
            .unwrap()
            .probe_buffer(),
        0,
        &read,
        0,
        4 * 128,
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
    assert!(
        (0..4).any(|probe| f32::from_le_bytes(
            bytes[probe * 128 + 48..probe * 128 + 52]
                .try_into()
                .unwrap()
        ) > 0.),
        "authoritative RGB must reach the actual View directional GI probes"
    );
    drop(bytes);
    read.unmap();
    assert!(view.environment_stats().unwrap().gpu.gi.rays > 0);
    assert_eq!(view.environment_stats().unwrap().gpu.gi.updated_probes, 4);
    assert!(view.environment_stats().unwrap().gi_started);
    assert_eq!(
        view.environment_stats().unwrap().gpu.contact_ao.passes,
        u32::from(contact_quality.is_some())
    );
    assert_eq!(view.entity_stats().unwrap().remote_records, 1);
    let model_pixels = actual_view_entity_pixels(&gpu, &mut view);
    assert!(
        model_pixels >= 5,
        "actual View must attach GI to its entity renderer"
    );
    view.advance_input(0.).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let _ = view.take_frame_work();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
    }
    assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
    eprintln!(
        "M10_ACTUAL_VIEW_GI_WORKER_OK full_worker_reads={} foreground_full_reads=0 source_jobs={} idle1000_allocations=0 known_pages={} updated_probes=4 model_blue_pixels={model_pixels} shadow_attachment=true contact_ao_quality={contact_quality:?}",
        stats.source_jobs.worker_reads, stats.source_jobs.completed, known
    );
}

// Draw the actual View-owned renderer, using the camera/visibility lists the
// complete View frame prepared. A transparent target isolates the receiver
// from blue sky/terrain; the actor's certified CPU RGB and sky are both zero.
fn actual_view_entity_pixels(gpu: &render::Gpu, view: &mut ClientView) -> usize {
    let color = |samples| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("actual View GI-only model witness"),
            size: wgpu::Extent3d {
                width: 96,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | if samples == 1 {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
    };
    let ambient_mrt = view.environment_mut().unwrap().gpu.contact_ao().is_some();
    let msaa = color(4);
    let ambient = ambient_mrt.then(|| color(4));
    let output = color(1);
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual View model reverse-Z"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 4,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual View model HDR pixels"),
        size: 96 * 64 * 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mv = msaa.create_view(&Default::default());
        let ov = output.create_view(&Default::default());
        let dv = depth.create_view(&Default::default());
        let av = ambient
            .as_ref()
            .map(|texture| texture.create_view(&Default::default()));
        let mut attachments = vec![Some(wgpu::RenderPassColorAttachment {
            view: &mv,
            depth_slice: None,
            resolve_target: Some(&ov),
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })];
        if let Some(av) = &av {
            attachments.push(Some(wgpu::RenderPassColorAttachment {
                view: av,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Discard,
                },
            }));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("actual View prepared GI receiver"),
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &dv,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        view.entities_mut().unwrap().draw(&mut pass);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &read,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(96 * 8),
                rows_per_image: Some(64),
            },
        },
        wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receiver.recv().unwrap().unwrap();
    let bytes = read.slice(..).get_mapped_range();
    let mut lit = 0;
    for pixel in bytes.chunks_exact(8) {
        let values: [u16; 4] = std::array::from_fn(|channel| {
            u16::from_le_bytes([pixel[channel * 2], pixel[channel * 2 + 1]])
        });
        if values[3] != 0 {
            assert_eq!(values[0], 0, "GI-only actor must have no red direct light");
            assert_eq!(
                values[1], 0,
                "GI-only actor must have no green direct light"
            );
            lit += usize::from(values[2] > 0);
        }
    }
    drop(bytes);
    read.unmap();
    lit
}
