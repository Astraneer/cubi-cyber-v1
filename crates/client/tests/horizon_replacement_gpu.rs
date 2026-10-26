//! Source replacement may wait across arbitrarily many bounded upload phases.
//! An old committed row must not conceal its already-uploaded successor.
use client::view::horizon::{Config, ViewHorizon};
use core_types::{PageId, SpaceId, Version};
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
use render::terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget};

const KEY: HeightKey = HeightKey {
    space: 0,
    level: 0,
    x: 1,
    z: 0,
};
fn page(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1,
        y,
        z: 0,
    }
}
fn tile(key: HeightKey, source: u64, column: bool) -> Box<HeightTile> {
    let mut known = [255; 37];
    known[36] = 1;
    Box::new(HeightTile {
        key,
        source,
        error: None,
        heights: [32; 289],
        known,
        atlas: [rules::ids::L_STONE as u16; 256],
        tint: [u32::MAX; 256],
        protected: if column { [255; 32] } else { [0; 32] },
        replacement: if column {
            Replacement::Column {
                min_y: 0,
                max_y: 7,
                geometry: (0..8)
                    .map(|y| GeometryStamp {
                        page: page(y),
                        source: Version(42),
                        view: source,
                    })
                    .collect(),
            }
        } else {
            Replacement::None
        },
    })
}
fn geometry(y: i32, source: u64) -> net::PublicGeometry {
    net::PublicGeometry {
        page: page(y),
        source_version: Version(42),
        view_version: source,
        known: [255; 512],
        occupied: if y < 2 { [255; 512] } else { [0; 512] },
        palette: if y < 2 {
            vec![net::PublicSurface {
                atlas_layer: rules::ids::L_STONE as u16,
                tint: u32::MAX,
                kind: net::PublicSurfaceKind::Opaque,
            }]
        } else {
            vec![]
        },
        index_bits: 0,
        indices: vec![],
    }
}
fn ingest(horizon: &mut ViewHorizon, y: i32, source: u64) {
    horizon.ingest_geometry(&geometry(y, source)).unwrap();
    while let Some(task) = horizon.take_proxy_task() {
        horizon
            .complete_proxy(task.execute().unwrap())
            .map_err(|_| ())
            .unwrap();
    }
}

#[test]
fn held_proxy_replacement_uploads_each_source_once_and_reuses_retired_slots() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        32,
        32,
        render::OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 8,
            max_quads: 8192,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
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
            ..Default::default()
        },
    )
    .unwrap();
    horizon.reserve_near_cutout(0).unwrap();
    horizon
        .guard_near(&gpu.queue, 0, [0, 0], &mut UploadBudget::new(0))
        .unwrap();
    let plan = horizon
        .plan_task(0, [0, 0])
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();
    horizon.complete_plan(plan).unwrap();
    for key in horizon.wanted_height().collect::<Vec<_>>() {
        horizon.ingest_height(tile(key, 1, false)).unwrap();
    }
    assert!(
        horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap()
            .published
    );
    horizon.ingest_height(tile(KEY, 2, true)).unwrap();
    for y in 0..8 {
        ingest(&mut horizon, y, 2);
    }
    assert!(
        horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap()
            .published
    );
    assert_eq!(horizon.front_source(KEY), Some(2));
    assert_eq!(horizon.stats().proxy_front, 8);
    // Repeating this more often than the 32-object GPU capacity proves that
    // retained front/staging allocations, rather than larger limits, suffice.
    for source in 3..=5 {
        horizon.ingest_height(tile(KEY, source, true)).unwrap();
        for y in 0..7 {
            ingest(&mut horizon, y, source);
        }
        let staged = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap();
        assert_eq!(staged.proxy_pages, 7);
        assert!(staged.held_previous);
        assert_eq!(horizon.front_source(KEY), Some(source - 1));
        for _ in 0..64 {
            let held = horizon
                .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
                .unwrap();
            assert_eq!(
                held.proxy_pages, 0,
                "a matching staged source is already uploaded"
            );
            assert_eq!(held.upload_bytes, 0);
            assert!(held.held_previous);
            assert_eq!(horizon.stats().proxy_staged, 7);
            assert_eq!(horizon.stats().proxy_front, 8);
            assert_eq!(horizon.front_source(KEY), Some(source - 1));
        }
        ingest(&mut horizon, 7, source);
        let published = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap();
        assert_eq!(published.proxy_pages, 1);
        assert!(published.published);
        assert_eq!(horizon.front_source(KEY), Some(source));
        assert_eq!(horizon.stats().proxy_front, 8);
        assert_eq!(horizon.stats().proxy_staged, 0);
    }
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
}
