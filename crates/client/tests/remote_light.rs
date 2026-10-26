use client::{ClientScene, SceneConfig, SceneError};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, Version};
use net::{
    PageSnapshot, PageUpdate,
    light::{Delta, Error, Header, Snapshot, Update, pack},
};
fn point() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 8,
        z: 8,
    }
}
fn config() -> SceneConfig {
    SceneConfig {
        center: point(),
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        authoritative_remote: true,
        ..SceneConfig::forest()
    }
}
fn cells(page: PageId, view: u64) -> PageUpdate {
    PageUpdate::Snapshot(PageSnapshot {
        page,
        version: view,
        source_version: Version(500),
        applied_action: Default::default(),
        cells: Box::new([Cell::AIR; 4096]),
    })
}
fn loaded() -> ClientScene {
    let mut scene = ClientScene::new(config()).unwrap();
    let pages: Vec<_> = scene.requested_pages().collect();
    for page in pages {
        scene
            .import_authoritative_page(&cells(page, 10), Tick(0))
            .unwrap();
    }
    scene
}
fn header(cell_view: u64, version: u64, source: u64) -> Header {
    Header {
        page: point().page(),
        cell_view,
        version,
        source: Version(source),
    }
}
fn full(cell_view: u64, version: u64) -> Update {
    Update::Snapshot(Snapshot {
        header: header(cell_view, version, 1000),
        values: Box::new([pack(15, 7, [12, 5, 1]).unwrap(); 4096]),
    })
}
#[test]
fn confirmed_light_changes_only_light_channels_and_one_sample_does_one_sample_work() {
    let mut scene = loaded();
    let input = scene
        .world()
        .input_version(point().page(), client::MESH_CHANNELS);
    assert!(scene.read_authoritative_light(point()).is_none());
    let imported = scene
        .import_authoritative_light(&full(10, 1), Tick(1))
        .unwrap();
    assert!(imported.applied);
    assert_eq!(imported.samples_scanned, 4096);
    let change = imported.change.unwrap();
    assert!(change.cells.is_empty());
    assert_eq!(change.lights.len(), 4096);
    assert_eq!(change.work.mask_cells_classified, 0);
    assert!(!change.channels.intersects(client::MESH_CHANNELS));
    assert_eq!(
        scene
            .world()
            .input_version(point().page(), client::MESH_CHANNELS),
        input
    );
    assert_eq!(
        scene.read_authoritative_light(point()).unwrap(),
        world::LightSample {
            sky: 15,
            block: 7,
            rgb: [12, 5, 1]
        }
    );
    let delta = Update::Delta(Delta {
        header: header(10, 2, 1001),
        base: 1,
        changes: vec![(point().local_index().get(), pack(0, 3, [1, 2, 3]).unwrap())],
    });
    let imported = scene.import_authoritative_light(&delta, Tick(2)).unwrap();
    assert_eq!(imported.samples_scanned, 1);
    let change = imported.change.unwrap();
    assert_eq!(change.lights.len(), 1);
    assert!(change.cells.is_empty());
    assert_eq!(change.work.mask_cells_classified, 0);
    assert_eq!(
        scene
            .world()
            .input_version(point().page(), client::MESH_CHANNELS),
        input
    );
    assert_eq!(
        scene.authoritative_version(point().page()).unwrap().view,
        10
    );
    assert_eq!(
        scene
            .authoritative_light_version(point().page())
            .unwrap()
            .view,
        2
    );
    assert_eq!(scene.stats().generation_published, 0);
    assert_eq!(scene.stats().override_cells, 0);
    let before = scene.world().canonical_hash();
    let stale = scene.import_authoritative_light(&delta, Tick(3)).unwrap();
    assert!(!stale.applied);
    assert_eq!(stale.samples_scanned, 0);
    assert_eq!(scene.world().canonical_hash(), before);
}
#[test]
fn stale_cell_views_missing_light_bases_and_malformed_data_are_atomic() {
    let mut scene = loaded();
    scene
        .import_authoritative_light(&full(10, 1), Tick(1))
        .unwrap();
    let before = scene.world().canonical_hash();
    let bad = Update::Delta(Delta {
        header: header(10, 3, 1002),
        base: 2,
        changes: vec![(0, 0)],
    });
    assert!(matches!(
        scene.import_authoritative_light(&bad, Tick(2)),
        Err(SceneError::Light(Error::MissingBase { .. }))
    ));
    assert_eq!(scene.world().canonical_hash(), before);
    let duplicate = Update::Delta(Delta {
        header: header(10, 2, 1001),
        base: 1,
        changes: vec![(0, 0), (0, 1)],
    });
    assert!(matches!(
        scene.import_authoritative_light(&duplicate, Tick(2)),
        Err(SceneError::Light(Error::InvalidDelta))
    ));
    assert_eq!(scene.world().canonical_hash(), before);
    scene
        .import_authoritative_page(&cells(point().page(), 11), Tick(2))
        .unwrap();
    assert!(scene.authoritative_light_version(point().page()).is_none());
    assert!(scene.read_authoritative_light(point()).is_none());
    assert!(
        !scene
            .import_authoritative_light(&full(10, 2), Tick(2))
            .unwrap()
            .applied
    );
    let rebind = Update::Delta(Delta {
        header: header(11, 2, 1000),
        base: 1,
        changes: vec![],
    });
    let imported = scene.import_authoritative_light(&rebind, Tick(2)).unwrap();
    assert!(imported.applied);
    assert_eq!(imported.samples_scanned, 0);
    assert!(imported.change.is_none());
    assert!(scene.authoritative_light_version(point().page()).is_some());
    scene
        .remove_authoritative_page(point().page(), Tick(3))
        .unwrap();
    assert!(scene.authoritative_light_version(point().page()).is_none());
    scene
        .import_authoritative_page(&cells(point().page(), 12), Tick(4))
        .unwrap();
    assert!(
        !scene
            .import_authoritative_light(&full(11, 3), Tick(4))
            .unwrap()
            .applied
    );
    let aba = Update::Delta(Delta {
        header: header(12, 3, 1001),
        base: 2,
        changes: vec![],
    });
    assert!(matches!(
        scene.import_authoritative_light(&aba, Tick(4)),
        Err(SceneError::Light(Error::MissingBase { .. }))
    ));
}
#[test]
fn a_zero_snapshot_is_confirmed_without_a_world_transaction_and_remote_unknown_stays_unknown() {
    let mut scene = ClientScene::new(config()).unwrap();
    let packet = Update::Snapshot(Snapshot {
        header: header(10, 1, 0),
        values: Box::new([0; 4096]),
    });
    let hash = scene.world().canonical_hash();
    assert!(matches!(
        scene.import_authoritative_light(&packet, Tick(0)),
        Err(SceneError::Light(Error::MissingCells { .. }))
    ));
    assert_eq!(scene.world().canonical_hash(), hash);
    assert_eq!(scene.world().loaded_pages(), 0);
    scene
        .import_authoritative_page(&cells(point().page(), 10), Tick(0))
        .unwrap();
    let hash = scene.world().canonical_hash();
    let applied = scene.import_authoritative_light(&packet, Tick(0)).unwrap();
    assert!(applied.applied);
    assert!(applied.change.is_none());
    assert_eq!(scene.world().canonical_hash(), hash);
    assert_eq!(
        scene.read_authoritative_light(point()),
        Some(world::LightSample::default())
    );
}
