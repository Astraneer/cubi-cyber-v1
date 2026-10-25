use client::{ClientScene, SceneConfig, SceneError, SceneOutput, SceneTask};
use core_types::{BlockPos, Cell, LocalIdx, PAGE_VOLUME, PageId, SpaceId, Tick, Version};
use jobs::{Completion, JobOutcome};
use net::{PageDelta, PageSnapshot, PageUpdate};
use world::{CellRead, Transaction};

fn config() -> SceneConfig {
    SceneConfig {
        center: point(8, 8, 8),
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        authoritative_remote: true,
        ..SceneConfig::forest()
    }
}
fn point(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn center() -> PageId {
    point(8, 8, 8).page()
}
fn image(page: PageId, view: u64, source: u64, cell: Cell) -> PageUpdate {
    PageUpdate::Snapshot(PageSnapshot {
        page,
        version: view,
        source_version: Version(source),
        cells: Box::new([cell; PAGE_VOLUME]),
    })
}
fn loaded() -> ClientScene {
    let mut scene = ClientScene::new(config()).unwrap();
    let pages: Vec<_> = scene.requested_pages().collect();
    for page in pages {
        scene
            .import_authoritative_page(&image(page, 10, 500, Cell::AIR), Tick(0))
            .unwrap();
    }
    scene
}
#[test]
fn remote_unknown_never_generates_or_accepts_standalone_world_edits() {
    let mut scene = ClientScene::new(config()).unwrap();
    let hash = scene.world().canonical_hash();
    for _ in 0..100 {
        scene.phase(Tick(0), 64).unwrap();
        assert!(
            scene
                .take_task(Tick(0), &mut ClientScene::default_budget())
                .is_none()
        );
    }
    assert_eq!(scene.world().canonical_hash(), hash);
    assert_eq!(scene.world().read(point(8, 8, 8)), CellRead::Unknown);
    assert_eq!(scene.stats().generation_published, 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(center(), vec![Cell::AIR; PAGE_VOLUME]);
    assert!(matches!(
        scene.apply_transaction(tx),
        Err(SceneError::AuthorityMode)
    ));
    assert_eq!(scene.world().loaded_pages(), 0);
}
#[test]
fn welcome_configures_public_seed_and_spawn_only_before_first_import() {
    let mut scene = ClientScene::new(config()).unwrap();
    let spawn = point(200, 70, -300);
    scene.configure_authority(1, spawn).unwrap();
    assert_eq!(scene.config().seed.0, 1);
    assert_eq!(scene.config().center, spawn);
    let page = spawn.page();
    scene
        .import_authoritative_page(&image(page, 1, 1, Cell::AIR), Tick(0))
        .unwrap();
    assert!(matches!(
        scene.configure_authority(2, point(0, 0, 0)),
        Err(SceneError::AuthorityMode)
    ));
    assert_eq!(scene.config().seed.0, 1);
    assert_eq!(scene.config().center, spawn);
}
#[test]
fn confirmed_versions_are_separate_and_one_cell_delta_does_one_cell_work() {
    let mut scene = loaded();
    let local = scene.world().page_version(center());
    assert_ne!(local, Version(500));
    assert_eq!(scene.authoritative_version(center()).unwrap().view, 10);
    let pos = point(8, 8, 8);
    let update = PageUpdate::Delta(PageDelta {
        page: center(),
        base: 10,
        version: 11,
        source_version: Version(501),
        changes: vec![(
            pos.local_index().get(),
            Cell::from_parts(rules::ids::STONE, 0),
        )],
    });
    let imported = scene.import_authoritative_page(&update, Tick(1)).unwrap();
    assert_eq!(imported.cells_scanned, 1);
    let change = imported.change.unwrap();
    assert_eq!(change.work.pages_staged, 1);
    assert_eq!(change.cells.len(), 1);
    assert_eq!(change.work.mask_cells_classified, 1);
    assert_eq!(scene.stats().override_cells, 0);
    assert_eq!(
        scene.authoritative_version(center()).unwrap().source,
        Version(501)
    );
    assert_eq!(
        scene.world().read(pos),
        CellRead::Known(Cell::from_parts(rules::ids::STONE, 0))
    );
    let hash = scene.world().canonical_hash();
    let stale = scene
        .import_authoritative_page(&image(center(), 10, 500, Cell::AIR), Tick(1))
        .unwrap();
    assert!(!stale.applied);
    assert_eq!(stale.cells_scanned, 0);
    assert_eq!(hash, scene.world().canonical_hash());
}
#[test]
fn malformed_or_missing_bases_are_atomic_and_confirmed_replica_can_repair() {
    let mut scene = loaded();
    let hash = scene.world().canonical_hash();
    let prior = scene.authoritative_version(center());
    let update = |base, changes| {
        PageUpdate::Delta(PageDelta {
            page: center(),
            base,
            version: 11,
            source_version: Version(501),
            changes,
        })
    };
    assert!(matches!(
        scene.import_authoritative_page(&update(9, vec![]), Tick(1)),
        Err(SceneError::MissingAuthoritativeBase { .. })
    ));
    for changes in [
        vec![(4096, Cell::AIR)],
        vec![(0, Cell::AIR), (0, Cell::from_parts(rules::ids::STONE, 0))],
    ] {
        assert!(matches!(
            scene.import_authoritative_page(&update(10, changes), Tick(1)),
            Err(SceneError::InvalidAuthoritativeUpdate)
        ));
    }
    assert_eq!(hash, scene.world().canonical_hash());
    assert_eq!(prior, scene.authoritative_version(center()));
    let PageUpdate::Snapshot(snapshot) =
        image(center(), 11, 501, Cell::from_parts(rules::ids::DIRT, 0))
    else {
        unreachable!()
    };
    assert!(
        scene
            .import_replica_page(&snapshot, Tick(1))
            .unwrap()
            .applied
    );
    assert_eq!(
        scene.world().read(point(8, 8, 8)),
        CellRead::Known(Cell::from_parts(rules::ids::DIRT, 0))
    );
}
#[test]
fn ore_reveal_can_change_view_without_changing_authority_source_version() {
    let mut scene = loaded();
    let pos = point(8, 8, 8);
    let ore = Cell::from_parts(rules::ids::IRON_ORE, 0);
    let update = PageUpdate::Delta(PageDelta {
        page: center(),
        base: 10,
        version: 11,
        source_version: Version(500),
        changes: vec![(pos.local_index().get(), ore)],
    });
    scene.import_authoritative_page(&update, Tick(1)).unwrap();
    assert_eq!(
        scene.authoritative_version(center()).unwrap().source,
        Version(500)
    );
    assert_eq!(scene.world().read(pos), CellRead::Known(ore));
    assert_eq!(scene.stats().generation_published, 0);
}
#[test]
fn optimistic_edits_are_guarded_unsaved_and_ack_does_not_apply_twice() {
    let mut scene = loaded();
    let pos = point(8, 8, 8);
    let stone = Cell::from_parts(rules::ids::STONE, 0);
    scene
        .apply_remote_prediction(pos, Cell::AIR, stone, Tick(1))
        .unwrap();
    let version = scene.world().version();
    assert_eq!(scene.stats().override_cells, 0);
    assert_eq!(scene.authoritative_version(center()).unwrap().view, 10);
    assert!(matches!(
        scene.apply_remote_prediction(pos, Cell::AIR, Cell::AIR, Tick(1)),
        Err(SceneError::PredictionConflict)
    ));
    let update = PageUpdate::Delta(PageDelta {
        page: center(),
        base: 10,
        version: 11,
        source_version: Version(501),
        changes: vec![(pos.local_index().get(), stone)],
    });
    let accepted = scene.import_authoritative_page(&update, Tick(2)).unwrap();
    assert!(accepted.change.is_none());
    assert_eq!(scene.world().version(), version);
    scene
        .apply_remote_prediction(pos, stone, Cell::AIR, Tick(2))
        .unwrap();
    assert_eq!(scene.world().read(pos), CellRead::Known(Cell::AIR));
}
#[test]
fn authoritative_unavailable_revokes_gpu_resident_and_late_mesh_then_reloads() {
    let mut scene = loaded();
    scene.phase(Tick(0), 64).unwrap();
    let job = scene
        .take_task(Tick(0), &mut ClientScene::default_budget())
        .unwrap();
    assert!(matches!(job.payload, SceneTask::Mesh(_)));
    scene.remove_authoritative_page(center(), Tick(1)).unwrap();
    assert_eq!(scene.world().read(point(8, 8, 8)), CellRead::Unknown);
    assert_eq!(scene.take_hidden_page(), Some(center()));
    assert!(scene.authoritative_version(center()).is_none());
    scene
        .complete(Completion {
            id: job.id,
            outcome: JobOutcome::Cancelled,
        })
        .unwrap();
    scene.phase(Tick(1), 64).unwrap();
    assert!(scene.take_mesh().is_none());
    scene
        .import_authoritative_page(
            &image(center(), 1, 1000, Cell::from_parts(rules::ids::GRASS, 0)),
            Tick(1),
        )
        .unwrap();
    assert_ne!(scene.biome_tint(center(), 8, 8), Some(u16::MAX));
    assert!(
        scene
            .world()
            .page(center())
            .unwrap()
            .cell(LocalIdx::new(0).unwrap())
            .id()
            == rules::ids::GRASS
    );
    scene.phase(Tick(1), 64).unwrap();
    let job = scene
        .take_task(Tick(1), &mut ClientScene::default_budget())
        .unwrap();
    let result = job.payload.execute(&job.cancellation).unwrap();
    assert!(matches!(result, SceneOutput::Mesh(_)));
    scene
        .complete(Completion {
            id: job.id,
            outcome: JobOutcome::Success(result),
        })
        .unwrap();
    scene.phase(Tick(1), 64).unwrap();
    assert!(scene.take_mesh().is_some());
}
#[test]
fn removal_after_aoi_move_can_drop_an_unrequested_retained_page() {
    let mut scene = loaded();
    scene.set_center(point(300, 8, 300)).unwrap();
    assert!(!scene.requested_pages().any(|page| page == center()));
    assert!(
        scene
            .remove_authoritative_page(center(), Tick(1))
            .unwrap()
            .is_some()
    );
    assert_eq!(scene.world().read(point(8, 8, 8)), CellRead::Unknown);
    assert!(scene.authoritative_version(center()).is_none());
}
