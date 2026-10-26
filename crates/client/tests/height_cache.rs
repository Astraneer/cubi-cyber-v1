#![cfg(not(target_arch = "wasm32"))]
use client::height_cache::{Config, Error, HeightCache};
use render::{
    height_gpu::{HeightGpu, HeightGpuConfig, HeightGpuError, TILE_BYTES},
    height_tile::{HeightPlan, HeightTile, HeightTileKey, PlannedHeightTile},
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
use std::sync::Arc;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn key(x: i32) -> HeightTileKey {
    HeightTileKey {
        space: 0,
        level: 0,
        x,
        z: 0,
    }
}
fn tile(key: HeightTileKey, source: u64, height: i32) -> Arc<HeightTile> {
    let mut tile = HeightTile {
        key,
        source,
        heights: [height; 289],
        known: [255; 37],
        atlas: [3; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
    };
    tile.known[36] = 1;
    Arc::new(tile)
}
fn exact(old: &HeightTile) -> Arc<HeightTile> {
    let mut tile = old.clone();
    tile.protected = [255; 32];
    Arc::new(tile)
}

#[test]
fn exact_leaf_promotion_cannot_change_the_world_source_or_surface_payload() {
    let mut cache = HeightCache::new(Config {
        max_tiles: 1,
        max_sources: 2,
        atlas_layers: 186,
    })
    .unwrap();
    cache.set_plan(&plan(&[key(0)])).unwrap();
    let original = tile(key(0), 1, 32);
    cache.ingest(original.clone()).unwrap();
    let promoted = exact(&original);
    assert_eq!(
        cache.ingest(promoted.clone()),
        Err(Error::ConflictingSource)
    );
    for kind in 0..5 {
        let mut forged = (*promoted).clone();
        match kind {
            0 => forged.heights[0] += 1,
            1 => {
                forged.known[0] &= !1;
                forged.heights[0] = 0;
            }
            2 => forged.atlas[0] = 4,
            3 => forged.tint[0] = 0,
            _ => forged.protected[0] = 1,
        }
        assert_eq!(
            cache.ingest_exact_promotion(Arc::new(forged)),
            Err(Error::ConflictingSource)
        );
    }
    assert!(cache.ingest_exact_promotion(promoted.clone()).unwrap());
    assert!(!cache.ingest_exact_promotion(promoted).unwrap());
    assert_eq!(cache.ingest(original), Err(Error::ConflictingSource));
}

#[test]
fn same_source_exact_promotion_borrows_the_front_and_switches_only_at_atomic_publication() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        for abandoned in [false, true] {
            let mut terrain = renderer(&gpu);
            let mut heights = heights(&gpu, &terrain, 2);
            let mut cache = HeightCache::new(Config {
                max_tiles: 1,
                max_sources: 2,
                atlas_layers: 186,
            })
            .unwrap();
            cache.set_plan(&plan(&[key(0)])).unwrap();
            let original = tile(key(0), 1, 32);
            cache.ingest(original.clone()).unwrap();
            assert!(
                cache
                    .pump(
                        &mut heights,
                        &gpu.queue,
                        &mut UploadBudget::new(100000),
                        1,
                        |_| true
                    )
                    .unwrap()
                    .published
            );
            let before = image(&gpu, &mut terrain, &mut heights);
            assert_eq!(before.1, 1);
            let epoch = cache.epoch();
            cache.ingest_exact_promotion(exact(&original)).unwrap();
            let staged = cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(0),
                    1,
                    |_| false,
                )
                .unwrap();
            assert_eq!(staged.staged, 1);
            assert_eq!(staged.upload_bytes, 0);
            assert!(staged.held_previous && !staged.published);
            assert_eq!(cache.epoch(), epoch);
            assert_eq!(image(&gpu, &mut terrain, &mut heights), before);
            if abandoned {
                // Superseding the pending promotion must not free its borrowed
                // front handle, even when no upload/release budget is available.
                cache.ingest(tile(key(0), 2, 33)).unwrap();
                let held = cache
                    .pump(
                        &mut heights,
                        &gpu.queue,
                        &mut UploadBudget::new(0),
                        1,
                        |_| true,
                    )
                    .unwrap();
                assert_eq!(held.upload_bytes, 0);
                assert_eq!(cache.staged_len(), 0);
                assert_eq!(image(&gpu, &mut terrain, &mut heights), before);
                assert!(
                    cache
                        .pump(
                            &mut heights,
                            &gpu.queue,
                            &mut UploadBudget::new(100000),
                            1,
                            |_| true
                        )
                        .unwrap()
                        .published
                );
                assert_eq!(cache.front_source(key(0)), Some(2));
                assert_eq!(image(&gpu, &mut terrain, &mut heights).1, 1);
                continue;
            }
            let short = cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(3),
                    1,
                    |_| true,
                )
                .unwrap();
            assert_eq!(short.upload_bytes, 0);
            assert!(!short.published);
            assert_eq!(image(&gpu, &mut terrain, &mut heights), before);
            let published = cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(4),
                    1,
                    |_| true,
                )
                .unwrap();
            assert!(published.published);
            assert_eq!(published.upload_bytes, 4);
            assert_eq!(cache.front_source(key(0)), Some(1));
            assert_eq!(cache.pending_retirements(), 0);
            assert!(cache.epoch() > epoch);
            assert_eq!(image(&gpu, &mut terrain, &mut heights).1, 0);
            // Moving the near cutout can never re-enable the borrowed old
            // unprotected GPU payload after the exact cover replaced it.
            for bounds in [[0, 0, 16, 16], [32, 0, 48, 16]] {
                assert_eq!(
                    cache
                        .suppress_region(
                            &mut heights,
                            &gpu.queue,
                            0,
                            bounds,
                            &mut UploadBudget::new(0)
                        )
                        .unwrap(),
                    0
                );
                assert_eq!(image(&gpu, &mut terrain, &mut heights).1, 0);
            }
            let mut stitched = plan(&[key(0)]);
            stitched.tiles[0].seams = 1;
            cache.set_plan(&stitched).unwrap();
            assert!(
                cache
                    .pump(
                        &mut heights,
                        &gpu.queue,
                        &mut UploadBudget::new(4),
                        1,
                        |_| true
                    )
                    .unwrap()
                    .published
            );
            assert_eq!(cache.pending_retirements(), 0);
            assert_eq!(image(&gpu, &mut terrain, &mut heights).1, 0);
            let scope = telemetry::AllocationScope::begin().unwrap();
            for _ in 0..1000 {
                let work = cache
                    .pump(
                        &mut heights,
                        &gpu.queue,
                        &mut UploadBudget::new(0),
                        1,
                        |_| true,
                    )
                    .unwrap();
                assert_eq!(work.upload_bytes, 0);
                assert!(!work.published);
            }
            assert_eq!(scope.finish().allocating_calls(), 0);
        }
    });
}
fn plan(keys: &[HeightTileKey]) -> HeightPlan {
    HeightPlan {
        tiles: keys
            .iter()
            .map(|key| PlannedHeightTile {
                key: *key,
                seams: 0,
            })
            .collect(),
        visited_nodes: keys.len() as u32,
        outer_bounds_pages: [0, 0, 2, 1],
    }
}
fn renderer(gpu: &render::Gpu) -> TerrainGpu {
    TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        48,
        render::OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 4,
            max_quads: 4096,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap()
}
fn heights(gpu: &render::Gpu, terrain: &TerrainGpu, count: u32) -> HeightGpu {
    HeightGpu::new(
        &gpu.device,
        terrain.geometry(),
        HeightGpuConfig {
            max_tiles: count,
            atlas_layers: 186,
        },
        None,
    )
    .unwrap()
}
fn image(gpu: &render::Gpu, terrain: &mut TerrainGpu, height: &mut HeightGpu) -> (Vec<u8>, u32) {
    let camera = Camera::orthographic(
        [16., 40., 8.],
        [16., 0., 8.],
        [0., 0., -1.],
        16.,
        2.,
        0.1,
        100.,
    );
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atomic height cover oracle"),
        size: wgpu::Extent3d {
            width: 96,
            height: 48,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let buffer = |size| {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("height cover readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    let pixels = buffer(512 * 48);
    let indirect = buffer(16);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    height
        .prepare_frame(&gpu.queue, &mut encoder, camera)
        .unwrap();
    terrain
        .encode_frame_with_extra(
            &gpu.queue,
            &mut encoder,
            &texture.create_view(&Default::default()),
            &camera,
            CullingMode::Frustum,
            |pass| height.draw(pass),
        )
        .unwrap();
    encoder.copy_buffer_to_buffer(height.indirect_buffer(), 0, &indirect, 0, 16);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(512),
                rows_per_image: Some(48),
            },
        },
        wgpu::Extent3d {
            width: 96,
            height: 48,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let read = |buffer: &wgpu::Buffer| {
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        rx.recv().unwrap().unwrap();
        let out = buffer.slice(..).get_mapped_range().to_vec();
        buffer.unmap();
        out
    };
    let raw = read(&pixels);
    let count = u32::from_le_bytes(read(&indirect)[4..8].try_into().unwrap());
    (
        raw.chunks_exact(512)
            .flat_map(|row| row[..96 * 4].iter().copied())
            .collect(),
        count,
    )
}
#[test]
fn same_key_replacements_wait_for_real_neighbour_sources_then_publish_together() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = renderer(&gpu);
        let mut heights = heights(&gpu, &terrain, 8);
        let mut cache = HeightCache::new(Config {
            max_tiles: 4,
            max_sources: 8,
            atlas_layers: 186,
        })
        .unwrap();
        cache.set_plan(&plan(&[key(0), key(1)])).unwrap();
        cache.ingest(tile(key(0), 1, 0)).unwrap();
        cache.ingest(tile(key(1), 1, 0)).unwrap();
        let held = cache
            .pump(
                &mut heights,
                &gpu.queue,
                &mut UploadBudget::new(TILE_BYTES + 64),
                1,
                |_| true,
            )
            .unwrap();
        assert!(!held.published);
        assert_eq!(cache.front_len(), 0);
        assert!(
            cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(TILE_BYTES + 72),
                    1,
                    |_| true
                )
                .unwrap()
                .published
        );
        let first = image(&gpu, &mut terrain, &mut heights);
        assert_eq!(first.1, 2);
        let old_epoch = cache.epoch();
        let mut a = tile(key(0), 2, 1);
        Arc::make_mut(&mut a).tint.fill(0x000000ff);
        cache.ingest(a.clone()).unwrap();
        let waiting = cache
            .pump(
                &mut heights,
                &gpu.queue,
                &mut UploadBudget::new(10000),
                1,
                |_| true,
            )
            .unwrap();
        assert!(waiting.held_previous);
        assert!(!waiting.published);
        assert!(waiting.seams_checked > 0);
        assert_eq!(cache.front_source(key(0)), Some(1));
        assert_eq!(cache.epoch(), old_epoch);
        assert_eq!(
            image(&gpu, &mut terrain, &mut heights),
            first,
            "disabled staging is bitwise invisible"
        );
        // A third copy cannot consume unbounded arena space while two epochs exist.
        assert_eq!(
            heights
                .upload_staged(&gpu.queue, &a, 0, [0; 3], &mut UploadBudget::new(10000))
                .unwrap_err(),
            HeightGpuError::Capacity
        );
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            let w = cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(0),
                    1,
                    |_| true,
                )
                .unwrap();
            assert_eq!(w.upload_bytes, 0);
        }
        assert_eq!(
            scope.finish().allocating_calls(),
            0,
            "waiting for an edge is also allocation-free"
        );
        let mut b = tile(key(1), 2, 1);
        Arc::make_mut(&mut b).protected[0] = 1;
        cache.ingest(b).unwrap();
        assert!(
            !cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(10000),
                    1,
                    |_| false
                )
                .unwrap()
                .published
        );
        assert_eq!(
            image(&gpu, &mut terrain, &mut heights),
            first,
            "protected geometry readiness is mandatory"
        );
        assert!(
            !cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(15),
                    0,
                    |_| true
                )
                .unwrap()
                .published
        );
        let publication = cache
            .pump(
                &mut heights,
                &gpu.queue,
                &mut UploadBudget::new(16),
                0,
                |_| true,
            )
            .unwrap();
        assert!(publication.published);
        assert_eq!(publication.upload_bytes, 16);
        assert_eq!(cache.front_source(key(0)), Some(2));
        assert!(cache.epoch() > old_epoch);
        let next = image(&gpu, &mut terrain, &mut heights);
        assert_eq!(next.1, 2);
        assert_ne!(next.0, first.0);
        assert_eq!(
            cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(8),
                    0,
                    |_| true
                )
                .unwrap()
                .released,
            2
        );
        heights.take_work();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..10000 {
            assert_eq!(
                cache
                    .pump(
                        &mut heights,
                        &gpu.queue,
                        &mut UploadBudget::new(0),
                        0,
                        |_| true
                    )
                    .unwrap()
                    .upload_bytes,
                0
            );
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
        assert_eq!(heights.take_work().upload_bytes, 0);
        assert_eq!(
            cache.set_plan(&plan(&[key(0), key(0)])).unwrap_err(),
            Error::InvalidPlan
        );
        cache.set_plan(&plan(&[])).unwrap();
        assert!(
            !cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(7),
                    0,
                    |_| true
                )
                .unwrap()
                .published
        );
        assert_eq!(cache.front_len(), 2);
        assert!(
            cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(8),
                    0,
                    |_| true
                )
                .unwrap()
                .published
        );
        assert_eq!(cache.front_len(), 0);
        assert_eq!(
            cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(8),
                    0,
                    |_| true
                )
                .unwrap()
                .released,
            2
        );
        assert_eq!(cache.staged_len(), 0);
        assert_eq!(cache.source_len(), 0);
        assert_eq!(image(&gpu, &mut terrain, &mut heights).1, 0);
    });
}
#[test]
fn horizon1024_uses259_public_tiles_and_one_interior_edit_uploads_one_tile() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let terrain = renderer(&gpu);
        let mut heights = heights(&gpu, &terrain, 1024);
        let mut cache = HeightCache::new(Config::default()).unwrap();
        let plan = HeightPlan::new(0, [0, 0], 9, 512).unwrap();
        assert_eq!(plan.tiles.len(), 259);
        assert!(plan.visited_nodes <= 340);
        cache.set_plan(&plan).unwrap();
        for planned in &plan.tiles {
            cache.ingest(tile(planned.key, 1, 0)).unwrap();
        }
        let ready = cache
            .pump(
                &mut heights,
                &gpu.queue,
                &mut UploadBudget::new(2 * 1024 * 1024),
                512,
                |_| true,
            )
            .unwrap();
        assert!(ready.published);
        assert_eq!(cache.front_len(), 259);
        assert_eq!(ready.staged, 259);
        let changed = plan
            .tiles
            .iter()
            .find(|tile| tile.key.level == 0)
            .unwrap()
            .key;
        let mut update = tile(changed, 2, 0);
        Arc::make_mut(&mut update).heights[8 + 17 * 8] = 1;
        cache.ingest(update.clone()).unwrap();
        let edit = cache
            .pump(
                &mut heights,
                &gpu.queue,
                &mut UploadBudget::new(10000),
                1,
                |_| true,
            )
            .unwrap();
        assert!(edit.published);
        assert_eq!(edit.staged, 1);
        assert_eq!(edit.upload_bytes, TILE_BYTES + 64 + 8);
        assert_eq!(cache.front_len(), 259);
        assert_eq!(cache.front_source(changed), Some(2));
        assert!(!cache.ingest(tile(changed, 1, 0)).unwrap());
        let mut forged = update;
        Arc::make_mut(&mut forged).heights[8 + 17 * 8] = 2;
        assert_eq!(cache.ingest(forged).unwrap_err(), Error::ConflictingSource);
        for x in 10000..11000 {
            assert!(!cache.ingest(tile(key(x), 1, 0)).unwrap());
        }
        assert_eq!(cache.source_len(), 259);
        println!(
            "HEIGHT_CACHE_HORIZON_OK tiles259 visits{} oneEdit{}B sourceReads0 privateCells0",
            plan.visited_nodes, edit.upload_bytes
        );
    });
}

#[test]
fn moving_near_unknown_cutout_is_atomic_and_survives_an_old_pending_cover() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = renderer(&gpu);
        let mut heights = heights(&gpu, &terrain, 8);
        let mut cache = HeightCache::new(Config {
            max_tiles: 4,
            max_sources: 8,
            atlas_layers: 186,
        })
        .unwrap();
        cache.set_plan(&plan(&[key(0), key(1)])).unwrap();
        cache.ingest(tile(key(0), 1, 0)).unwrap();
        cache.ingest(tile(key(1), 1, 0)).unwrap();
        assert!(
            cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(10000),
                    4,
                    |_| true
                )
                .unwrap()
                .published
        );
        let full = image(&gpu, &mut terrain, &mut heights);
        assert_eq!(full.1, 2);
        assert_eq!(
            cache
                .suppress_region(
                    &mut heights,
                    &gpu.queue,
                    0,
                    [0, 0, 16, 16],
                    &mut UploadBudget::new(3)
                )
                .unwrap_err(),
            Error::Gpu(HeightGpuError::UploadBudget)
        );
        assert_eq!(image(&gpu, &mut terrain, &mut heights), full);
        assert_eq!(
            cache
                .suppress_region(
                    &mut heights,
                    &gpu.queue,
                    0,
                    [0, 0, 16, 16],
                    &mut UploadBudget::new(4)
                )
                .unwrap(),
            4
        );
        let left_cutout = image(&gpu, &mut terrain, &mut heights);
        assert_eq!(left_cutout.1, 1);
        assert_ne!(left_cutout.0, full.0);
        // A stale plan still asks for both tiles; its later source publication
        // must obey the new camera cutout and keep natural openings Unknown.
        let mut update = tile(key(0), 2, 0);
        Arc::make_mut(&mut update).tint.fill(0x000000ff);
        cache.ingest(update).unwrap();
        assert!(
            cache
                .pump(
                    &mut heights,
                    &gpu.queue,
                    &mut UploadBudget::new(10000),
                    1,
                    |_| true
                )
                .unwrap()
                .published
        );
        assert_eq!(image(&gpu, &mut terrain, &mut heights), left_cutout);
        assert_eq!(
            cache
                .suppress_region(
                    &mut heights,
                    &gpu.queue,
                    0,
                    [16, 0, 32, 16],
                    &mut UploadBudget::new(7)
                )
                .unwrap_err(),
            Error::Gpu(HeightGpuError::UploadBudget)
        );
        assert_eq!(image(&gpu, &mut terrain, &mut heights), left_cutout);
        assert_eq!(
            cache
                .suppress_region(
                    &mut heights,
                    &gpu.queue,
                    0,
                    [16, 0, 32, 16],
                    &mut UploadBudget::new(8)
                )
                .unwrap(),
            8
        );
        let right_cutout = image(&gpu, &mut terrain, &mut heights);
        assert_eq!(right_cutout.1, 1);
        assert_ne!(right_cutout.0, left_cutout.0);
    });
}

#[test]
fn staging_overlaps_authority_delivery_but_cannot_publish_until_the_explicit_front_gate() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut terrain = renderer(&gpu);
    let mut height = heights(&gpu, &terrain, 8);
    let mut cache = HeightCache::new(Config {
        max_tiles: 4,
        max_sources: 8,
        atlas_layers: 186,
    })
    .unwrap();
    cache.set_plan(&plan(&[key(0)])).unwrap();
    cache.ingest(tile(key(0), 1, 0)).unwrap();
    assert!(
        cache
            .pump(
                &mut height,
                &gpu.queue,
                &mut UploadBudget::new(100000),
                1,
                |_| true
            )
            .unwrap()
            .published
    );
    let before = image(&gpu, &mut terrain, &mut height);
    let epoch = cache.epoch();
    cache.set_plan(&plan(&[key(0), key(1)])).unwrap();
    cache.ingest(tile(key(0), 2, 2)).unwrap();
    let first = cache
        .pump_staging(&mut height, &gpu.queue, &mut UploadBudget::new(100000), 1)
        .unwrap();
    assert_eq!(first.staged, 1);
    assert!(!first.published);
    assert_eq!(cache.epoch(), epoch);
    assert_eq!(image(&gpu, &mut terrain, &mut height), before);
    cache.ingest(tile(key(1), 1, 2)).unwrap();
    let second = cache
        .pump_staging(&mut height, &gpu.queue, &mut UploadBudget::new(100000), 1)
        .unwrap();
    assert_eq!(second.staged, 1);
    assert!(!second.published);
    assert_eq!(cache.epoch(), epoch);
    assert_eq!(image(&gpu, &mut terrain, &mut height), before);
    let held = cache
        .pump_staging(&mut height, &gpu.queue, &mut UploadBudget::new(0), 4)
        .unwrap();
    assert!(!held.published);
    assert!(held.held_previous);
    assert_eq!(held.seams_checked, 0);
    let held = cache
        .pump(
            &mut height,
            &gpu.queue,
            &mut UploadBudget::new(11),
            4,
            |_| true,
        )
        .unwrap();
    assert!(!held.published);
    assert_eq!(held.seams_checked, 1);
    let repeated = cache
        .pump(
            &mut height,
            &gpu.queue,
            &mut UploadBudget::new(11),
            4,
            |_| true,
        )
        .unwrap();
    assert!(!repeated.published);
    assert_eq!(
        repeated.seams_checked, 0,
        "valid unchanged seam epoch is cached"
    );
    assert_eq!(image(&gpu, &mut terrain, &mut height), before);
    let done = cache
        .pump(
            &mut height,
            &gpu.queue,
            &mut UploadBudget::new(12),
            4,
            |_| true,
        )
        .unwrap();
    assert!(done.published);
    assert_eq!(done.upload_bytes, 12);
    assert_eq!(done.seams_checked, 0);
    assert!(cache.epoch() > epoch);
    let current = image(&gpu, &mut terrain, &mut height);
    assert_ne!(current, before);
    assert_eq!(current.1, 2);
    cache.ingest(tile(key(0), 3, 3)).unwrap();
    let incompatible = cache
        .pump(
            &mut height,
            &gpu.queue,
            &mut UploadBudget::new(100000),
            4,
            |_| true,
        )
        .unwrap();
    assert!(incompatible.held_previous);
    assert!(!incompatible.published);
    assert_eq!(incompatible.seams_checked, 1);
    assert_eq!(
        image(&gpu, &mut terrain, &mut height),
        current,
        "changed source invalidates cached seam proof"
    );
}
