use client::lodcache::{
    GeometryInput, LodCacheConfig, LodCacheError, PublicLodCache, PublishDisposition,
    decode_geometry,
};
use core_types::{PageId, SpaceId, Version};
use render::{
    lod::{GeometrySource, LodKey, LodObject, ObjectSelection, PublicVoxel},
    lod_gpu::ProxyError,
};
fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn key(x: i32) -> LodKey {
    LodKey::page(0, i64::from(x), 0, 0)
}
fn summary(x: i32, version: u64, occupied: u64) -> net::FarSummary {
    net::FarSummary {
        page: page(x),
        source_version: Version(version),
        occupied,
        materials: [rules::ids::STONE; 64],
    }
}
fn selected(cache: &PublicLodCache, xs: &[i32]) -> ObjectSelection {
    ObjectSelection {
        objects: xs
            .iter()
            .map(|x| LodObject {
                key: key(*x),
                revision: cache.forest().get(key(*x)).unwrap().revision,
                origin_relative: [*x as f32 * 16., 0., 0.],
                cell_scale: 1,
                geometry: GeometrySource::FinePage,
            })
            .collect(),
        coverage_complete: true,
        ..ObjectSelection::default()
    }
}
#[test]
fn actual_coarse_edit_updates_ancestor_summaries_without_generator() {
    let mut cache = PublicLodCache::new(LodCacheConfig::default()).unwrap();
    cache.ingest_summary(&summary(0, 1, 1)).unwrap();
    assert_eq!(cache.forest().get(key(0)).unwrap().occupied_cells, 64);
    let parent = key(0).parent().unwrap();
    assert_eq!(cache.forest().get(parent).unwrap().occupied_cells, 64);
    cache.ingest_summary(&summary(0, 2, 0)).unwrap();
    assert_eq!(cache.forest().get(key(0)).unwrap().occupied_cells, 0);
    assert_eq!(cache.forest().get(parent).unwrap().occupied_cells, 0);
    assert!(!cache.ingest_summary(&summary(0, 1, 1)).unwrap().accepted);
}
#[test]
fn coalesced_proxy_jobs_never_publish_stale_data_or_exceed_inflight_bound() {
    let mut cache = PublicLodCache::new(LodCacheConfig {
        max_inflight: 1,
        ..LodCacheConfig::default()
    })
    .unwrap();
    cache.ingest_summary(&summary(0, 1, 1)).unwrap();
    let first = selected(&cache, &[0]);
    assert_eq!(cache.queue_selection(&first).unwrap(), 1);
    let task = cache.take_task().unwrap();
    cache.ingest_summary(&summary(0, 2, 2)).unwrap();
    let next = selected(&cache, &[0]);
    assert_eq!(cache.queue_selection(&next).unwrap(), 1);
    assert!(
        cache.take_task().is_none(),
        "same-key replacement must wait for its earlier job"
    );
    let old = task.execute().unwrap();
    assert_eq!(cache.complete(old).unwrap(), PublishDisposition::Stale);
    assert_eq!(cache.stale_results(), 1);
    let task = cache.take_task().unwrap();
    let revision = task.revision;
    assert_eq!(
        cache.complete(task.execute().unwrap()).unwrap(),
        PublishDisposition::Published
    );
    assert!(cache.ready(key(0), revision).is_some());
    assert_eq!(cache.queue_selection(&next).unwrap(), 0);
}
#[test]
fn coarse_announcement_keeps_previous_protected_shape_until_confirmed_replacement() {
    let mut cache = PublicLodCache::new(LodCacheConfig::default()).unwrap();
    let known = [255; 512];
    let mut occupied = [0; 512];
    let voxel_index = 7 + 16 * (7 + 16 * 7);
    occupied[voxel_index / 8] |= 1 << (voxel_index % 8);
    let palette = [PublicVoxel::solid(3, u32::MAX)];
    cache
        .ingest_geometry(GeometryInput {
            page: page(0),
            source: Version(1),
            view: 1,
            known: &known,
            occupied: &occupied,
            palette: &palette,
            index_bits: 0,
            indices: &[],
        })
        .unwrap();
    let before = cache.forest().get(key(0)).unwrap().public_hash;
    assert_eq!(cache.forest().get(key(0)).unwrap().occupied_cells, 1);
    let update = cache.ingest_summary(&summary(0, 2, 0)).unwrap();
    assert!(update.accepted && update.held_previous);
    assert_eq!(cache.forest().get(key(0)).unwrap().public_hash, before);
    assert_eq!(
        cache.geometry_requests(&selected(&cache, &[0]), 16),
        [page(0)]
    );
    let empty = [0; 512];
    cache
        .ingest_geometry(GeometryInput {
            page: page(0),
            source: Version(2),
            view: 2,
            known: &known,
            occupied: &empty,
            palette: &[],
            index_bits: 0,
            indices: &[],
        })
        .unwrap();
    assert_eq!(cache.forest().get(key(0)).unwrap().occupied_cells, 0);
    assert!(
        cache
            .geometry_requests(&selected(&cache, &[0]), 16)
            .is_empty()
    );
}
#[test]
fn geometry_without_initial_summary_evicts_to_a_valid_occupancy_summary() {
    let mut cache = PublicLodCache::new(LodCacheConfig::default()).unwrap();
    let known = [255; 512];
    let mut occupied = [0; 512];
    let voxel_index = 7 + 16 * (7 + 16 * 7);
    occupied[voxel_index / 8] |= 1 << (voxel_index % 8);
    let palette = [PublicVoxel::solid(3, u32::MAX)];
    cache
        .ingest_geometry(GeometryInput {
            page: page(0),
            source: Version(1),
            view: 1,
            known: &known,
            occupied: &occupied,
            palette: &palette,
            index_bits: 0,
            indices: &[],
        })
        .unwrap();
    cache.evict_geometry(page(0)).unwrap();
    assert!(
        cache.forest().get(key(0)).unwrap().occupied_cells > 0,
        "one far block must not vanish on protected cache eviction"
    );
    assert_eq!(cache.protected_pages(), 0);
}
#[test]
fn queue_capacity_is_checked_before_retaining_partial_selection() {
    let mut cache = PublicLodCache::new(LodCacheConfig {
        max_pending: 1,
        ..LodCacheConfig::default()
    })
    .unwrap();
    cache.ingest_summary(&summary(0, 1, 1)).unwrap();
    cache.ingest_summary(&summary(1, 1, 1)).unwrap();
    assert_eq!(
        cache.queue_selection(&selected(&cache, &[0, 1])),
        Err(LodCacheError::Capacity)
    );
    assert_eq!(cache.pending_jobs(), 0);
}
#[test]
fn malformed_geometry_and_conflicting_versions_keep_previous_view() {
    let mut cache = PublicLodCache::new(LodCacheConfig::default()).unwrap();
    cache.ingest_summary(&summary(0, 1, 1)).unwrap();
    let hash = cache.forest().get(key(0)).unwrap().public_hash;
    assert_eq!(
        cache.ingest_summary(&summary(0, 1, 2)).unwrap_err(),
        LodCacheError::ConflictingVersion
    );
    let known = [0; 512];
    let occupied = [255; 512];
    assert_eq!(
        cache
            .ingest_geometry(GeometryInput {
                page: page(0),
                source: Version(2),
                view: 2,
                known: &known,
                occupied: &occupied,
                palette: &[PublicVoxel::solid(3, u32::MAX)],
                index_bits: 0,
                indices: &[]
            })
            .unwrap_err(),
        LodCacheError::MalformedGeometry
    );
    assert_eq!(cache.forest().get(key(0)).unwrap().public_hash, hash);
}
#[test]
fn unknown_geometry_stays_unknown_and_cannot_build_a_sky_hole() {
    let known = [0; 512];
    let occupied = [0; 512];
    let input = GeometryInput {
        page: page(0),
        source: Version(1),
        view: 1,
        known: &known,
        occupied: &occupied,
        palette: &[],
        index_bits: 0,
        indices: &[],
    };
    let page = decode_geometry(&input, 186).unwrap();
    assert_eq!(page.get([7; 3]), PublicVoxel::Unknown);
    let proxy = render::lod::MaskedRayProxy::from_public_page(
        &page,
        render::lod::protection_groups(&page, 4),
    );
    assert_eq!(
        render::lod_gpu::ProxyPayload::from_public(&proxy, 186).unwrap_err(),
        ProxyError::UnknownData
    );
}
