use client::{ClientScene, OverrideLimits, SceneConfig, SceneError};
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, SpaceId, Tick};
use jobs::{Completion, JobOutcome};
use world::{CellRead, ComponentData, Transaction};

fn origin() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 64,
        z: 8,
    }
}
fn point(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 72,
        z: 8,
    }
}
fn config() -> SceneConfig {
    SceneConfig {
        center: origin(),
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        ..SceneConfig::forest()
    }
}
fn drive(scene: &mut ClientScene) {
    for _ in 0..200 {
        let tick = scene.world().tick();
        scene.phase(tick, 64).unwrap();
        while let Some(job) = scene.take_task(tick, &mut ClientScene::default_budget()) {
            let result = job.payload.execute(&job.cancellation).unwrap();
            scene
                .complete(Completion {
                    id: job.id,
                    outcome: JobOutcome::Success(result),
                })
                .unwrap();
        }
        scene.phase(tick, 64).unwrap();
        while scene.take_mesh().is_some() {}
        while scene.take_hidden_page().is_some() {}
        while scene.take_cancellation().is_some() {}
        assert!(scene.world().loaded_pages() <= scene.config().max_loaded_pages);
        if scene.is_ready() && scene.stats().pending_evictions == 0 {
            return;
        }
    }
    panic!(
        "bounded public generation/mesh route did not finish: {:?}",
        scene.stats()
    );
}
fn known(scene: &ClientScene, position: BlockPos) -> Cell {
    match scene.world().read(position) {
        CellRead::Known(value) => value,
        CellRead::Unknown => panic!("expected loaded page"),
    }
}
fn different(cell: Cell) -> Cell {
    Cell::from_parts(
        if cell.id() == rules::ids::STONE {
            rules::ids::DIRT
        } else {
            rules::ids::STONE
        },
        0,
    )
}

#[test]
fn pristine_streaming_evicts_with_a_fixed_budget_without_filling_override_cache() {
    let mut scene = ClientScene::new(config()).unwrap();
    drive(&mut scene);
    assert_eq!(scene.stats().override_pages, 0);
    scene.set_center(BlockPos { x: 104, ..origin() }).unwrap();
    assert_eq!(scene.stats().pending_evictions, 27);
    scene.phase(Tick(0), 0).unwrap();
    assert_eq!(scene.stats().pending_evictions, 25);
    assert_eq!(scene.world().loaded_pages(), 25);
    drive(&mut scene);
    assert_eq!(scene.world().loaded_pages(), 27);
    assert_eq!(scene.stats().override_pages, 0);
    assert_eq!(scene.world().read(point(8)), CellRead::Unknown);
}

#[test]
fn edited_cells_and_inventory_survive_eviction_and_async_public_regeneration() {
    let mut scene = ClientScene::new(config()).unwrap();
    drive(&mut scene);
    let replacement = different(known(&scene, point(8)));
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(point(8), replacement);
    tx.set_component(
        ComponentId(900),
        ComponentData {
            anchor: point(8),
            kind: 9,
            payload: vec![1, 7, 3],
        },
        ChannelMask::CONTENTS,
    );
    scene.apply_transaction(tx).unwrap();
    assert_eq!(scene.stats().override_pages, 1);
    assert_eq!(scene.stats().override_cells, 1);
    scene.set_center(BlockPos { x: 104, ..origin() }).unwrap();
    drive(&mut scene);
    assert_eq!(scene.world().read(point(8)), CellRead::Unknown);
    assert!(scene.world().component(ComponentId(900)).is_none());
    assert_eq!(scene.stats().override_cells, 1);
    scene.set_center(origin()).unwrap();
    drive(&mut scene);
    assert_eq!(known(&scene, point(8)), replacement);
    assert_eq!(
        scene
            .world()
            .component(ComponentId(900))
            .unwrap()
            .data
            .payload,
        [1, 7, 3]
    );
    assert_eq!(scene.stats().override_pages, 1);
    assert_eq!(scene.stats().override_cells, 1);
}

#[test]
fn override_overflow_rejects_before_world_mutation_and_reverting_releases_quota() {
    let mut cfg = config();
    cfg.overrides = OverrideLimits {
        max_pages: 2,
        max_cells: 1,
        max_bytes: 1024 * 1024,
    };
    let mut scene = ClientScene::new(cfg).unwrap();
    drive(&mut scene);
    let base = known(&scene, point(8));
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(point(8), different(base));
    scene.apply_transaction(tx).unwrap();
    let before = scene.world().canonical_hash();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(point(9), different(known(&scene, point(9))));
    assert!(matches!(
        scene.apply_transaction(tx),
        Err(SceneError::OverrideLimit)
    ));
    assert_eq!(scene.world().canonical_hash(), before);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(point(8), base);
    scene.apply_transaction(tx).unwrap();
    assert_eq!(scene.stats().override_pages, 0);
    assert_eq!(scene.stats().override_cells, 0);
}

#[test]
fn repeated_transaction_writes_do_not_replace_the_original_base_marker() {
    let mut scene = ClientScene::new(config()).unwrap();
    drive(&mut scene);
    let base = known(&scene, point(8));
    let first = different(base);
    let second = Cell::from_parts(
        if first.id() == rules::ids::SAND {
            rules::ids::GRAVEL
        } else {
            rules::ids::SAND
        },
        0,
    );
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(point(8), first);
    scene.apply_transaction(tx).unwrap();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(point(8), base).set_cell(point(8), second);
    scene.apply_transaction(tx).unwrap();
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.set_cell(point(8), first);
    scene.apply_transaction(tx).unwrap();
    assert_eq!(scene.stats().override_cells, 1);
    scene.set_center(BlockPos { x: 104, ..origin() }).unwrap();
    drive(&mut scene);
    scene.set_center(origin()).unwrap();
    drive(&mut scene);
    assert_eq!(known(&scene, point(8)), first);
}

#[test]
fn hidden_and_cancellation_queues_are_explicit_and_bounded() {
    let mut scene = ClientScene::new(config()).unwrap();
    scene.phase(Tick(0), 0).unwrap();
    let old = scene
        .take_task(Tick(0), &mut ClientScene::default_budget())
        .unwrap();
    scene.set_center(BlockPos { x: 104, ..origin() }).unwrap();
    assert_eq!(scene.take_hidden_page(), Some(origin().page()));
    assert!(scene.take_hidden_page().is_none());
    assert_eq!(scene.take_cancellation(), Some(old.id));
    assert!(scene.take_cancellation().is_none());
    // A cancellation still completes its original reservation before retrying.
    scene.cancelled(old.id).unwrap();
    scene.phase(Tick(0), 8).unwrap();
    assert!(scene.stats().running_jobs <= scene.config().scheduler.max_jobs);
}
