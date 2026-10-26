#![cfg(not(target_arch = "wasm32"))]
pub use client::{height_cache, lodcache};
#[path = "../src/view/horizon.rs"]
#[allow(dead_code)]
mod horizon;
use core_types::{PageId, SpaceId, Version};
use horizon::*;
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
use render::terrain_gpu::{Camera, UploadBudget};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn tile(key: HeightKey, source: u64) -> Box<HeightTile> {
    let mut t = HeightTile {
        key,
        source,
        heights: [32; 289],
        known: [255; 37],
        atlas: [3; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: Replacement::None,
    };
    t.known[36] = 1;
    Box::new(t)
}
fn page(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1,
        y,
        z: 0,
    }
}
fn geometry(y: i32, view: u64) -> net::PublicGeometry {
    let mut occupied = [0u8; 512];
    if y < 2 {
        occupied.fill(255);
    }
    if y == 2 {
        for ly in 0..=8 {
            for x in 4..=11 {
                if x == 4 || x == 11 || ly == 8 {
                    let index = x + 16 * (8 + 16 * ly);
                    occupied[index / 8] |= 1 << (index % 8);
                }
            }
        }
    }
    net::PublicGeometry {
        page: page(y),
        source_version: Version(42),
        view_version: view,
        known: [255; 512],
        occupied,
        palette: if y <= 2 {
            vec![net::PublicSurface {
                atlas_layer: 3,
                tint: u32::MAX,
                kind: net::PublicSurfaceKind::Opaque,
            }]
        } else {
            Vec::new()
        },
        index_bits: 0,
        indices: Vec::new(),
    }
}
fn frame(
    gpu: &render::Gpu,
    terrain: &mut render::terrain_gpu::TerrainGpu,
    horizon: &mut ViewHorizon,
    camera: Camera,
) -> Vec<u8> {
    frame_at(gpu, terrain, horizon, camera, [24.5, 36.5, 56.])
}
fn frame_at(
    gpu: &render::Gpu,
    terrain: &mut render::terrain_gpu::TerrainGpu,
    horizon: &mut ViewHorizon,
    camera: Camera,
    eye: [f32; 3],
) -> Vec<u8> {
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("live horizon source/view oracle"),
        size: wgpu::Extent3d {
            width: 96,
            height: 96,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("horizon image readback"),
        size: 512 * 96,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let view = output.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    horizon
        .prepare_camera(&gpu.queue, &mut encoder, camera, eye, 96)
        .unwrap();
    terrain.invalidate();
    terrain
        .encode_frame_with_extra(
            &gpu.queue,
            &mut encoder,
            &view,
            &camera,
            render::terrain_gpu::CullingMode::Frustum,
            |pass| horizon.draw(pass),
        )
        .unwrap();
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
                bytes_per_row: Some(512),
                rows_per_image: Some(96),
            },
        },
        wgpu::Extent3d {
            width: 96,
            height: 96,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let image = read
        .slice(..)
        .get_mapped_range()
        .chunks_exact(512)
        .flat_map(|row| row[..384].iter().copied())
        .collect();
    read.unmap();
    image
}
#[test]
fn live_horizon_requires_gpu_manifest_and_preserves_old_cover_and_arch_air() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = render::terrain_gpu::TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            96,
            96,
            render::OutputTransfer::Attachment,
            render::terrain_gpu::GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: false,
            },
            &render::terrain_gpu::AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mut horizon = ViewHorizon::prepare(
            &gpu.device,
            &gpu.queue,
            &terrain,
            None,
            Config {
                max_tiles: 64,
                max_sources: 128,
                max_proxy_pages: 16,
                horizon_level: 0,
                ..Config::default()
            },
        )
        .unwrap();
        assert!(!horizon.stats().enabled);
        horizon.reserve_near_cutout(0).unwrap();
        assert_eq!(
            horizon
                .guard_near(&gpu.queue, 0, [0, 0], &mut UploadBudget::new(0))
                .unwrap(),
            Some(0)
        );
        let plan = horizon
            .plan_task(0, [0, 0])
            .unwrap()
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(plan.plan.tiles.len(), 15);
        horizon.complete_plan(plan).unwrap();
        let keys = horizon.wanted_height().collect::<Vec<_>>();
        for key in keys {
            horizon.ingest_height(tile(key, 1)).unwrap();
        }
        let initial = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap();
        assert!(initial.published);
        let key = HeightKey {
            space: 0,
            level: 0,
            x: 1,
            z: 0,
        };
        assert_eq!(horizon.front_source(key), Some(1));
        let camera = Camera::perspective(
            [24.5, 36.5, 56.],
            [24.5, 36.5, 8.5],
            [0., 1., 0.],
            1.,
            1.,
            0.1,
            200.,
        );
        let before = frame(&gpu, &mut terrain, &mut horizon, camera);
        let mut replacement = tile(key, 2);
        replacement.protected = [255; 32];
        replacement.replacement = Replacement::Column {
            min_y: 0,
            max_y: 7,
            geometry: (0..8)
                .map(|y| GeometryStamp {
                    page: page(y),
                    source: Version(42),
                    view: 2,
                })
                .collect(),
        };
        horizon.ingest_height(replacement).unwrap();
        for y in 0..7 {
            horizon.ingest_geometry(&geometry(y, 2)).unwrap();
            while let Some(task) = horizon.take_proxy_task() {
                horizon
                    .complete_proxy(task.execute().unwrap())
                    .map_err(|_| ())
                    .unwrap();
            }
        }
        let waiting = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap();
        assert!(waiting.held_previous);
        assert_eq!(horizon.front_source(key), Some(1));
        assert_eq!(horizon.stats().proxy_front, 0);
        assert_eq!(before, frame(&gpu, &mut terrain, &mut horizon, camera));
        horizon.ingest_geometry(&geometry(7, 1)).unwrap();
        let stale = horizon.take_proxy_task().unwrap().execute().unwrap();
        horizon.ingest_geometry(&geometry(7, 2)).unwrap();
        assert!(!horizon.complete_proxy(stale).map_err(|_| ()).unwrap());
        while let Some(task) = horizon.take_proxy_task() {
            horizon
                .complete_proxy(task.execute().unwrap())
                .map_err(|_| ())
                .unwrap();
        }
        let published = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap();
        assert!(published.published);
        assert_eq!(horizon.front_source(key), Some(2));
        assert_eq!(horizon.stats().proxy_front, 8);
        let after = frame(&gpu, &mut terrain, &mut horizon, camera);
        assert_ne!(before, after);
        let center = (48 + 48 * 96) * 4;
        assert_eq!(
            &before[center..center + 4],
            &after[center..center + 4],
            "protected arch air was filled"
        );
        assert_eq!(
            horizon
                .rebase(&gpu.queue, [16, 0, 0], &mut UploadBudget::new(0))
                .unwrap(),
            None
        );
        let rebase = horizon
            .rebase(&gpu.queue, [16, 0, 0], &mut UploadBudget::new(1_000_000))
            .unwrap()
            .unwrap();
        assert!(rebase > 0);
        let rebased = Camera::perspective(
            [8.5, 36.5, 56.],
            [8.5, 36.5, 8.5],
            [0., 1., 0.],
            1.,
            1.,
            0.1,
            200.,
        );
        assert_eq!(
            after,
            frame_at(&gpu, &mut terrain, &mut horizon, rebased, [8.5, 36.5, 56.])
        );
        horizon
            .rebase(&gpu.queue, [0; 3], &mut UploadBudget::new(1_000_000))
            .unwrap()
            .unwrap();
        horizon
            .pump(&gpu.queue, &mut UploadBudget::new(10000), 0, 16)
            .unwrap();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            let work = horizon
                .pump(&gpu.queue, &mut UploadBudget::new(0), 4, 4)
                .unwrap();
            assert_eq!(work.upload_bytes, 0);
            assert!(!work.published);
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
        assert_eq!(
            horizon
                .guard_near(&gpu.queue, 0, [1, 0], &mut UploadBudget::new(0))
                .unwrap(),
            None
        );
        assert!(
            horizon
                .guard_near(&gpu.queue, 0, [1, 0], &mut UploadBudget::new(1024))
                .unwrap()
                .is_some()
        );
        let guarded = frame(&gpu, &mut terrain, &mut horizon, camera);
        assert_eq!(&before[center..center + 4], &guarded[center..center + 4]);
        horizon
            .guard_near(&gpu.queue, 0, [0, 0], &mut UploadBudget::new(1024))
            .unwrap()
            .unwrap();
        horizon.remove_geometry(page(2)).unwrap();
        let waiting = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(0), 4, 4)
            .unwrap();
        assert!(waiting.held_previous);
        assert!(!horizon.stats().enabled);
        horizon
            .guard_near(&gpu.queue, 0, [0, 0], &mut UploadBudget::new(1024))
            .unwrap()
            .unwrap();
        let removed = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(10000), 4, 4)
            .unwrap();
        assert!(removed.held_previous);
        assert_eq!(horizon.stats().proxy_front, 7);
        horizon.ingest_geometry(&geometry(2, 2)).unwrap();
        while let Some(task) = horizon.take_proxy_task() {
            horizon
                .complete_proxy(task.execute().unwrap())
                .map_err(|_| ())
                .unwrap();
        }
        horizon
            .pump(&gpu.queue, &mut UploadBudget::new(10000), 4, 4)
            .unwrap();
        assert_eq!(horizon.stats().proxy_front, 8);
        assert_eq!(after, frame(&gpu, &mut terrain, &mut horizon, camera));
        println!(
            "M5 live GPU manifest8pages source+view exact,staleproxydiscarded,oldcoverBITWISEheld,archairpreserved,idle100cycles0alloc0upload"
        );
    });
}

#[test]
fn browser_horizon_packets_keep_plan_and_public_proxy_results_exact_and_bounded() {
    pollster::block_on(async {
        assert_eq!(
            execute_background_request(&[], || true),
            Err(WireError::Cancelled)
        );
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let terrain = render::terrain_gpu::TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            96,
            96,
            render::OutputTransfer::Attachment,
            render::terrain_gpu::GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: false,
            },
            &render::terrain_gpu::AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mut owner =
            ViewHorizon::prepare(&gpu.device, &gpu.queue, &terrain, None, Config::default())
                .unwrap();
        owner.reserve_near_cutout(4).unwrap();
        let task = BackgroundTask::Plan(owner.plan_task(0, [0, 0]).unwrap().unwrap());
        let context = task.context();
        let request = encode_background_task(&task).unwrap();
        for n in 0..request.len() {
            assert!(execute_background_request(&request[..n], || false).is_err());
        }
        let response = execute_background_request(&request, || false).unwrap();
        assert!(
            owner
                .complete_background(context, decode_background_result(&response).unwrap())
                .unwrap()
        );
        assert!(owner.wanted_height().count() < 512);
        owner.ingest_geometry(&geometry(2, 2)).unwrap();
        let task = BackgroundTask::Proxy(owner.take_proxy_task().unwrap());
        let context = task.context();
        context.fail(&mut owner);
        let retry = BackgroundTask::Proxy(
            owner
                .take_proxy_task()
                .expect("cancelled input must remain retained"),
        );
        assert_eq!(task.job_key(), retry.job_key());
        assert_eq!(task.retained_bytes(), retry.retained_bytes());
        let task = retry;

        let request = encode_background_task(&task).unwrap();
        assert!(
            request.len() < 1200,
            "compressed publicDTO was reconstructed to4096vertices/voxels on owner"
        );
        let context = task.context();
        let native = match task {
            BackgroundTask::Proxy(task) => task.execute().unwrap(),
            _ => unreachable!(),
        };
        let response = execute_background_request(&request, || false).unwrap();
        for n in 0..response.len() {
            assert!(decode_background_result(&response[..n]).is_err());
        }
        let decoded = decode_background_result(&response).unwrap();
        let BackgroundResult::Proxy(Ok(wire)) = &decoded else {
            panic!("proxy browser worker failed")
        };
        assert_eq!(
            wire.built.payload.encode_public(),
            native.built.payload.encode_public()
        );
        assert!(owner.complete_background(context, decoded).unwrap());
        let encoded = native.built.payload.encode_public();
        let mut bad = encoded.clone();
        bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(render::lod_gpu::ProxyPayload::decode_public(&bad, 186).is_err());
        for n in 0..encoded.len() {
            assert!(render::lod_gpu::ProxyPayload::decode_public(&encoded[..n], 186).is_err());
        }
        println!(
            "M5 browser plan+packedpublicDTO request{}B result{}B nativepackedpayloadBITWISEsame,malformedlengthsranksRejected",
            request.len(),
            response.len()
        );
    });
}
