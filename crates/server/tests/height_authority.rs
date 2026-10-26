use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, TxId};
use server::{
    height_authority::{Config, Error, HeightAuthority},
    height_index::{CellEdit, HeightEditIndex, IndexError},
};
use world::{Transaction, World, lighting::SkyAccess};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn pos(y: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 1,
        y,
        z: 1,
    }
}
fn edit(y: i32, id: u16) -> CellEdit {
    CellEdit {
        position: pos(y),
        cell: Cell::from_parts(id, 0),
    }
}
fn world() -> World {
    let mut world = World::new(rules::properties, 128);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for y in [8, 9] {
        tx.create_page(
            PageId {
                space: SpaceId(0),
                x: 0,
                y,
                z: 0,
            },
            vec![Cell::AIR; 4096],
        );
    }
    world.commit(tx).unwrap();
    world
}
fn change(world: &mut World, y: i32, id: u16) -> world::WorldChange {
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(pos(y), Cell::from_parts(id, 0));
    world.commit(tx).unwrap()
}
#[test]
fn actual_batch_coalesces_cells_and_pending_roof_proof_matches_durable_recovery() {
    let mut world = world();
    let mut owner = HeightAuthority::new(Config::default()).unwrap();
    assert!(rules::properties(Cell::from_parts(rules::ids::WATER, 0)).light_opacity > 0);
    let a = change(&mut world, 130, rules::ids::STONE);
    let b = change(&mut world, 130, rules::ids::WATER);
    let c = change(&mut world, 145, rules::ids::STONE);
    let batch = [a, b, c];
    let plan = owner.plan_batch(&batch).unwrap().unwrap();
    assert_eq!(plan.tx(), batch.last().unwrap().tx);
    assert_eq!(plan.edits().len(), 2);
    assert_eq!(plan.edits()[0].cell.id(), rules::ids::WATER);
    assert_eq!(
        owner.blocker_columns(),
        0,
        "planning cannot mutate persisted ownership"
    );
    assert_eq!(
        owner
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(pos(127)),
        Some(15)
    );
    let proof = owner.sky_proof(SpaceId(0), Some(&plan)).unwrap();
    assert_eq!(proof.changed_columns(), 1);
    assert_eq!(
        proof.incoming_from_unknown_above(pos(140)),
        None,
        "highest of two pending blockers wins"
    );
    assert_eq!(proof.incoming_from_unknown_above(pos(127)), None);
    assert_eq!(proof.incoming_from_unknown_above(pos(145)), Some(15));
    let bytes = plan.encode().unwrap();
    owner.apply_after_durable(plan).unwrap();
    let mut replay = HeightAuthority::new(Config::default()).unwrap();
    replay.apply_encoded_delta(&bytes).unwrap();
    assert_eq!(owner.snapshot().unwrap(), replay.snapshot().unwrap());
    assert!(
        replay.apply_encoded_delta(&bytes).is_err(),
        "duplicate recovery is rejected"
    );
    let mut owner =
        HeightAuthority::restore(&owner.snapshot().unwrap(), Config::default()).unwrap();
    // Unloading both actual pages cannot erase the persisted roof certificate.
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    for y in [8, 9] {
        tx.remove_page(PageId {
            space: SpaceId(0),
            x: 0,
            y,
            z: 0,
        });
    }
    world.commit(tx).unwrap();
    assert_eq!(
        owner
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(pos(127)),
        None
    );
    let removal =
        HeightEditIndex::encode_delta(TxId(owner.last_tx().0 + 1), &[edit(145, rules::ids::AIR)])
            .unwrap();
    owner.apply_encoded_delta(&removal).unwrap();
    assert_eq!(
        owner
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(pos(129)),
        None,
        "water is a blocker although not opaque"
    );
    let removal =
        HeightEditIndex::encode_delta(TxId(owner.last_tx().0 + 1), &[edit(130, rules::ids::AIR)])
            .unwrap();
    owner.apply_encoded_delta(&removal).unwrap();
    assert_eq!(owner.blocker_columns(), 0);
    assert_eq!(
        owner
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(pos(127)),
        Some(15)
    );
}
#[test]
fn quotas_stale_plans_and_wrong_order_fail_before_index_publication() {
    let mut world = world();
    let mut owner = HeightAuthority::new(Config::default()).unwrap();
    let a = change(&mut world, 130, rules::ids::STONE);
    let plan = owner.plan_batch(std::slice::from_ref(&a)).unwrap().unwrap();
    let stale = plan.clone();
    assert_eq!(
        owner.plan_batch(&[a.clone(), a.clone()]).unwrap_err(),
        Error::InvalidBatch
    );
    owner.apply_after_durable(plan).unwrap();
    let before = owner.snapshot().unwrap();
    assert_eq!(
        owner.apply_after_durable(stale).unwrap_err(),
        Error::StalePlan
    );
    assert_eq!(owner.snapshot().unwrap(), before);
    let mut config = Config::default();
    config.index.max_snapshot_bytes = 36;
    let limited = HeightAuthority::new(config).unwrap();
    assert_eq!(
        limited.plan_batch(&[a]).unwrap_err(),
        Error::Index(IndexError::SnapshotTooLarge)
    );
    assert_eq!(limited.snapshot().unwrap().len(), 36);
    let mut limited = HeightAuthority::new(config).unwrap();
    assert_eq!(
        limited
            .begin_legacy(TxId(10), [pos(130).page()])
            .unwrap_err(),
        Error::Index(IndexError::SnapshotTooLarge)
    );
    assert!(limited.is_ready());
}
#[test]
fn unfinished_legacy_world_stays_dark_and_new_live_air_wins_late_worker_data() {
    let mut owner = HeightAuthority::new(Config::default()).unwrap();
    owner.begin_legacy(TxId(10), [pos(130).page()]).unwrap();
    let snapshot = owner.snapshot().unwrap();
    let restored = HeightAuthority::restore(&snapshot, Config::default()).unwrap();
    assert!(!restored.is_ready());
    assert_eq!(
        restored
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(pos(150)),
        None
    );
    assert!(owner.finish_legacy().is_err());
    owner
        .apply_encoded_delta(
            &HeightEditIndex::encode_delta(TxId(11), &[edit(130, rules::ids::AIR)]).unwrap(),
        )
        .unwrap();
    owner
        .ingest_legacy_page(pos(130).page(), &[edit(130, rules::ids::STONE)])
        .unwrap();
    owner.finish_legacy().unwrap();
    assert_eq!(owner.blocker_columns(), 0);
    assert_eq!(owner.index().get(pos(130)).unwrap().cell, Cell::AIR);
    assert_eq!(
        owner
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(pos(127)),
        Some(15)
    );
    let wrong = BlockPos {
        space: SpaceId(1),
        ..pos(127)
    };
    assert_eq!(
        owner
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(wrong),
        None
    );
}
#[test]
fn stationary_sky_queries_do_not_clone_or_scan_ten_thousand_unrelated_columns() {
    let mut owner = HeightAuthority::new(Config::default()).unwrap();
    let edits: Vec<_> = (0..10000)
        .map(|x| CellEdit {
            position: BlockPos {
                x: 10000 + x,
                ..pos(200)
            },
            cell: Cell::from_parts(rules::ids::STONE, 0),
        })
        .collect();
    owner
        .apply_encoded_delta(&HeightEditIndex::encode_delta(TxId(1), &edits).unwrap())
        .unwrap();
    assert_eq!(owner.blocker_columns(), 10000);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        let sky = owner.sky_proof(SpaceId(0), None).unwrap();
        assert_eq!(sky.changed_columns(), 0);
        assert_eq!(sky.incoming_from_unknown_above(pos(127)), Some(15));
        assert_eq!(
            sky.incoming_from_unknown_above(BlockPos {
                x: 10001,
                ..pos(127)
            }),
            None
        );
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
}
