use core_types::{
    BlockPos, Cell, CellProperties, ChannelMask, ComponentId, LocalIdx, PAGE_VOLUME, PageId,
    SpaceId, SpaceTransform, Tick, Version,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use world::persistence::{ReplayError, RestoreBounds, RestoreError};
use world::{
    ComponentData, GameplayEvent, JournalLimits, LightSample, Transaction, World, WorldChange,
};

#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn properties(cell: Cell) -> CellProperties {
    if cell == Cell::AIR {
        CellProperties::AIR
    } else {
        CellProperties {
            shape: cell.raw(),
            ..CellProperties::SOLID
        }
    }
}
fn initial() -> World {
    let mut world = World::new(properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page(0), vec![Cell::AIR; PAGE_VOLUME]);
    tx.create_page(page(1), vec![Cell::from_parts(1, 0); PAGE_VOLUME]);
    tx.create_page(page(42), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(tx).unwrap();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.remove_page(page(42));
    tx.set_component(
        ComponentId(1),
        ComponentData {
            anchor: pos(1, 1, 1),
            kind: 7,
            payload: vec![1, 2, 3],
        },
        ChannelMask::CONTENTS,
    );
    tx.set_light(
        pos(2, 2, 2),
        LightSample {
            sky: 14,
            block: 9,
            rgb: [9, 3, 1],
        },
    );
    world.commit(tx).unwrap();
    world
}
fn commit(world: &mut World, tx: Transaction, tail: &mut Vec<(WorldChange, u64)>) {
    let change = world.commit(tx).unwrap();
    tail.push((change, world.canonical_hash()));
}

#[test]
fn snapshot_and_tail_restore_identical_hash_after_every_transaction() {
    let mut world = initial();
    let checkpoint = world.checkpoint();
    let prefix_hash = world.canonical_hash();
    let mut tail = Vec::new();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(pos(0, 0, 0), Cell::from_parts(9, 0x8001));
    tx.set_light(
        pos(3, 3, 3),
        LightSample {
            sky: 15,
            block: 15,
            rgb: [255, 19, 1],
        },
    );
    commit(&mut world, tx, &mut tail);
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        ComponentData {
            anchor: pos(17, 1, 1),
            kind: 7,
            payload: vec![5, 6],
        },
        ChannelMask::CONTENTS,
    );
    commit(&mut world, tx, &mut tail);
    let mut tx = Transaction::new(Tick(4), SpaceId(0));
    tx.create_page(page(42), vec![Cell::from_parts(2, 0); PAGE_VOLUME]);
    commit(&mut world, tx, &mut tail);
    let mut tx = Transaction::new(Tick(5), SpaceId(1));
    tx.create_space(SpaceTransform::IDENTITY);
    tx.create_page(
        PageId {
            space: SpaceId(1),
            ..page(0)
        },
        vec![Cell::AIR; PAGE_VOLUME],
    );
    commit(&mut world, tx, &mut tail);
    let mut tx = Transaction::new(Tick(6), SpaceId(1));
    tx.set_transform(SpaceTransform {
        translation: [10., 2., -4.],
        ..SpaceTransform::IDENTITY
    });
    tx.set_component(
        ComponentId(8),
        ComponentData {
            anchor: BlockPos {
                space: SpaceId(1),
                ..pos(2, 2, 2)
            },
            kind: 3,
            payload: vec![8, 9],
        },
        ChannelMask::CONNECTIONS,
    );
    commit(&mut world, tx, &mut tail);
    let mut tx = Transaction::new(Tick(7), SpaceId(0));
    tx.remove_component(ComponentId(1), ChannelMask::CONTENTS);
    tx.remove_page(page(1));
    tx.event(GameplayEvent {
        kind: 3,
        subject: Some(ComponentId(1)),
        position: Some(pos(17, 1, 1)),
        payload: vec![7],
    });
    commit(&mut world, tx, &mut tail);
    // Even an empty logical tick advances the global version and transaction order.
    commit(&mut world, Transaction::new(Tick(8), SpaceId(0)), &mut tail);
    let mut restored = World::restore(
        properties,
        JournalLimits::default(),
        checkpoint.to_restore_state(),
    )
    .unwrap();
    assert_eq!(restored.canonical_hash(), prefix_hash);
    assert_eq!(restored.journal_next(), 0);
    for (change, expected) in &tail {
        restored.apply_recovered_change(change).unwrap();
        assert_eq!(restored.canonical_hash(), *expected);
    }
    assert_eq!(
        restored.checkpoint().next_tx(),
        world.checkpoint().next_tx()
    );
    assert_eq!(
        restored
            .region(core_types::RegionId::from_page(page(42)))
            .unwrap()
            .loaded_pages,
        world
            .region(core_types::RegionId::from_page(page(42)))
            .unwrap()
            .loaded_pages
    );
    let cursor = restored.register_cursor();
    assert!(
        restored
            .read_cursor(cursor, 100)
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn snapshot_pages_remain_immutable_and_tombstones_prevent_aba() {
    let mut world = initial();
    let checkpoint = world.checkpoint();
    let original = checkpoint.page_snapshot(page(0)).unwrap().clone();
    assert!(Arc::ptr_eq(&original, &world.snapshot(page(0)).unwrap()));
    let old_version = world.page_version(page(42));
    let old_input = world.capture_input(page(42), ChannelMask::ALL);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(pos(0, 0, 0), Cell::from_parts(8, 0));
    world.commit(tx).unwrap();
    assert_eq!(original.cell(LocalIdx::new(0).unwrap()), Cell::AIR);
    assert!(!Arc::ptr_eq(&original, &world.snapshot(page(0)).unwrap()));
    let mut restored = World::restore(
        properties,
        JournalLimits::default(),
        checkpoint.to_restore_state(),
    )
    .unwrap();
    assert_eq!(restored.page_version(page(42)), old_version);
    assert!(restored.matches_inputs(&[old_input]));
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.create_page(page(42), vec![Cell::AIR; PAGE_VOLUME]);
    restored.commit(tx).unwrap();
    assert!(restored.page_version(page(42)) > old_version);
    assert!(!restored.matches_inputs(&[old_input]));
}

#[test]
fn restore_reclassifies_pages_using_the_supplied_registry_without_changing_logical_state() {
    let world = initial();
    let hash = world.canonical_hash();
    let state = world.checkpoint().to_restore_state();
    let count = Arc::new(AtomicU64::new(0));
    let calls = count.clone();
    let restored = World::restore(
        move |_: Cell| {
            calls.fetch_add(1, Ordering::Relaxed);
            CellProperties::AIR
        },
        JournalLimits::default(),
        state,
    )
    .unwrap();
    assert_eq!(count.load(Ordering::Relaxed), (2 * PAGE_VOLUME) as u64);
    assert_eq!(restored.canonical_hash(), hash);
    assert!(
        !restored
            .page(page(1))
            .unwrap()
            .masks()
            .collidable
            .get(LocalIdx::new(0).unwrap())
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn foreground_checkpoint_shares_pages_instead_of_copying_cell_or_light_arrays() {
    use telemetry::AllocationScope;
    let count = Arc::new(AtomicU64::new(0));
    let calls = count.clone();
    let mut world = World::new(
        move |cell: Cell| {
            calls.fetch_add(1, Ordering::Relaxed);
            properties(cell)
        },
        4096,
    );
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 0..8 {
        tx.create_page(page(x), vec![Cell::AIR; PAGE_VOLUME]);
    }
    world.commit(tx).unwrap();
    count.store(0, Ordering::Relaxed);
    let scope = AllocationScope::begin().unwrap();
    let checkpoint = world.checkpoint();
    let allocation = scope.finish();
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert!(
        allocation.allocated_bytes < (8 * PAGE_VOLUME * 4 / 4) as u64,
        "checkpoint copied page payload: {allocation:?}"
    );
    for x in 0..8 {
        assert!(Arc::ptr_eq(
            checkpoint.page_snapshot(page(x)).unwrap(),
            &world.snapshot(page(x)).unwrap()
        ));
    }
    assert_eq!(checkpoint.loaded_pages(), 8);
}

#[test]
fn malformed_snapshots_are_rejected_before_registry_rebuild() {
    let original = initial().checkpoint().to_restore_state();
    let mut variants = Vec::new();
    let mut state = original.clone();
    state.schema = 99;
    variants.push(state);
    let mut state = original.clone();
    state.next_tx += 1;
    variants.push(state);
    let mut state = original.clone();
    state.pages.push(state.pages[0].clone());
    variants.push(state);
    let mut state = original.clone();
    state.tombstones.push(state.tombstones[0]);
    variants.push(state);
    let mut state = original.clone();
    state
        .tombstones
        .push((state.pages[0].id, state.pages[0].version));
    variants.push(state);
    let mut state = original.clone();
    state.pages[0].channel_versions[0] = Version(state.version.0 + 1);
    variants.push(state);
    let mut state = original.clone();
    state.pages[0].lights.as_mut().unwrap()[0].sky = 16;
    variants.push(state);
    let mut state = original.clone();
    state.pages[0].components.clear();
    variants.push(state);
    let mut state = original.clone();
    state.components[0].1.data.anchor = pos(999, 1, 1);
    variants.push(state);
    let mut state = original.clone();
    state.spaces.clear();
    variants.push(state);
    let mut state = original.clone();
    state.spaces[0].1.transform.rotation[0] = f64::NAN;
    variants.push(state);
    let mut state = original.clone();
    state.regions[0].1.loaded_pages += 1;
    variants.push(state);
    let mut state = original.clone();
    state.regions.clear();
    variants.push(state);
    for state in variants {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = calls.clone();
        assert!(
            World::restore(
                move |cell| {
                    counter.fetch_add(1, Ordering::Relaxed);
                    properties(cell)
                },
                JournalLimits::default(),
                state
            )
            .is_err()
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "invalid snapshot reached registry construction"
        );
    }
    assert!(matches!(
        World::restore_with_bounds(
            properties,
            JournalLimits::default(),
            RestoreBounds {
                max_pages: 0,
                ..RestoreBounds::default()
            },
            original
        ),
        Err(RestoreError::Bounds("pages"))
    ));
}

#[test]
fn corrupted_wal_metadata_is_rejected_without_any_partial_mutation() {
    let mut source = initial();
    let checkpoint = source.checkpoint();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(pos(3, 3, 3), Cell::from_parts(9, 0));
    tx.set_light(
        pos(4, 4, 4),
        LightSample {
            sky: 3,
            block: 5,
            rgb: [5, 1, 0],
        },
    );
    tx.set_component(
        ComponentId(1),
        ComponentData {
            anchor: pos(2, 1, 1),
            kind: 7,
            payload: vec![9],
        },
        ChannelMask::CONTENTS,
    );
    let valid = source.commit(tx).unwrap();
    let mut variants = Vec::new();
    let mut change = valid.clone();
    change.tx.0 += 1;
    variants.push(change);
    let mut change = valid.clone();
    change.pages[0].before.0 += 1;
    variants.push(change);
    let mut change = valid.clone();
    change.pages[0].after.0 += 1;
    variants.push(change);
    let mut change = valid.clone();
    change.pages[0].channels = ChannelMask::NONE;
    variants.push(change);
    let mut change = valid.clone();
    change.cells[0].old = Cell::from_parts(123, 0);
    variants.push(change);
    let mut change = valid.clone();
    change.cells.push(change.cells[0].clone());
    variants.push(change);
    let mut change = valid.clone();
    change.lights[0].old.block = 12;
    variants.push(change);
    let mut change = valid.clone();
    change.components[0].before.as_mut().unwrap().version.0 += 1;
    variants.push(change);
    let mut change = valid.clone();
    change.components[0].after.as_mut().unwrap().version.0 += 1;
    variants.push(change);
    let mut change = valid.clone();
    change.pages.clear();
    variants.push(change);
    for change in variants {
        let mut restored = World::restore(
            properties,
            JournalLimits::default(),
            checkpoint.to_restore_state(),
        )
        .unwrap();
        let before = restored.canonical_hash();
        let boundary = restored.journal_next();
        assert!(restored.apply_recovered_change(&change).is_err());
        assert_eq!(restored.canonical_hash(), before);
        assert_eq!(restored.journal_next(), boundary);
        restored.apply_recovered_change(&valid).unwrap();
        assert_eq!(restored.canonical_hash(), source.canonical_hash());
    }
}

#[test]
fn replay_rejects_out_of_order_and_repeated_transactions() {
    let mut source = initial();
    let checkpoint = source.checkpoint();
    let first = source
        .commit(Transaction::new(Tick(2), SpaceId(0)))
        .unwrap();
    let second = source
        .commit(Transaction::new(Tick(3), SpaceId(0)))
        .unwrap();
    let mut target = World::restore(
        properties,
        JournalLimits::default(),
        checkpoint.to_restore_state(),
    )
    .unwrap();
    assert!(matches!(
        target.apply_recovered_change(&second),
        Err(ReplayError::TransactionOrder { .. })
    ));
    target.apply_recovered_change(&first).unwrap();
    let hash = target.canonical_hash();
    assert!(matches!(
        target.apply_recovered_change(&first),
        Err(ReplayError::TransactionOrder { .. })
    ));
    assert_eq!(target.canonical_hash(), hash);
    target.apply_recovered_change(&second).unwrap();
    assert_eq!(target.canonical_hash(), source.canonical_hash());
}
