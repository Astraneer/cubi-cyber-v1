//! Exact physical frontier/seed/input continuation, independently replaying the
//! World WAL before sparse solver deltas. No extra settling is allowed.
use core_types::{BlockPos, Cell, CellProperties, PageId, SpaceId, Tick};
use world::{
    JournalLimits, Transaction, World,
    lighting::{
        LightBudget, LightIngestBudget, LightPriority, LightingEngine, LightingLimits,
        durable::{Delta, Snapshot},
    },
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator =
    telemetry::CountingAllocator::new(std::alloc::System);
fn properties(cell: Cell) -> CellProperties {
    let mut p = CellProperties::AIR;
    match cell.id() {
        1 => p.opaque = true,
        2 => p.emission = [5, 3, 1],
        3 => p.emission = [1, 5, 3],
        _ => {}
    }
    p
}
fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn position(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 8,
        z: 8,
    }
}
fn limits() -> LightingLimits {
    LightingLimits {
        max_frontier_cells: 128,
        reserved_interactive_cells: 16,
        max_seed_pages: 2,
        max_queue_age_ticks: 40,
    }
}
fn world() -> World {
    let mut world = World::new(properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 0..2 {
        tx.create_page(page(x), vec![Cell::AIR; 4096]);
    }
    world.commit(tx).unwrap();
    world
}
#[test]
fn every_partial_input_seed_and_fifo_delta_replays_exactly_without_reseeding() {
    let mut live = world();
    let mut engine = LightingEngine::attach(&mut live, limits()).unwrap();
    engine.enable_durable_inputs().unwrap();
    for x in 0..2 {
        engine
            .queue_page(page(x), Tick(0), LightPriority::Background)
            .unwrap();
    }
    engine.take_durable_delta().unwrap();
    let budget = LightBudget {
        max_cells: 17,
        max_neighbor_reads: 102,
        max_seed_cells: 13,
        max_updates: 17,
    };
    let ingest = LightIngestBudget {
        max_records: 1,
        max_entries: 1,
        max_seeds: 3,
    };
    let mut operation_deltas = 0;
    let mut reached_idle = false;
    for phase in 0..2500 {
        let snapshot = engine.durable_snapshot().unwrap().encode().unwrap();
        let mut recovered = World::restore(
            properties,
            JournalLimits::default(),
            live.checkpoint().to_restore_state(),
        )
        .unwrap();
        let mut replayed = LightingEngine::restore_durable(
            &mut recovered,
            limits(),
            Snapshot::decode(&snapshot).unwrap(),
        )
        .unwrap();
        let cursor = live.register_cursor();
        let now = Tick(phase + 1);
        if [2, 17, 53].contains(&phase) {
            let mut tx = Transaction::new(now, SpaceId(0));
            tx.set_cell(
                position(15),
                Cell::from_parts(
                    if phase == 2 {
                        2
                    } else if phase == 17 {
                        3
                    } else {
                        0
                    },
                    0,
                ),
            );
            let change = live.commit(tx).unwrap();
            engine.observe_durable_change(&live, &change).unwrap();
            // Duplicate delivery (before step and final submit) is harmless.
            engine.observe_durable_change(&live, &change).unwrap();
        }
        engine.ingest(&mut live, now, ingest).unwrap();
        let progress = engine.step(&mut live, now, budget, &|_| None).unwrap();
        let changes = live.read_cursor(cursor, 16).unwrap();
        for change in &changes.changes {
            recovered.apply_recovered_change(change).unwrap();
        }
        live.remove_cursor(cursor);
        if let Some(delta) = engine.take_durable_delta().unwrap() {
            let bytes = delta.encode().unwrap();
            assert!(
                bytes.len() < 32 * 1024,
                "a 17-cell portion must not serialize the full frontier"
            );
            replayed
                .apply_durable_delta(Delta::decode(&bytes).unwrap())
                .unwrap();
            operation_deltas += 1;
        }
        assert_eq!(
            live.canonical_hash(),
            recovered.canonical_hash(),
            "phase{phase}"
        );
        assert!(
            engine.durable_snapshot().unwrap().encode().unwrap()
                == replayed.durable_snapshot().unwrap().encode().unwrap(),
            "exact free slots/FIFO/seed cursors/unconsumed semantic input at phase{phase}"
        );
        assert!(progress.work.cells_examined <= 17 && progress.work.neighbor_reads <= 102);
        if phase > 53 && engine.is_idle() {
            reached_idle = true;
            break;
        }
    }
    assert!(reached_idle);
    assert!(operation_deltas > 100);
}
#[test]
fn restored_static_idle_has_zero_allocations_and_zero_deltas() {
    let mut world = world();
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    engine.enable_durable_inputs().unwrap();
    let snapshot = engine.durable_snapshot().unwrap().encode().unwrap();
    engine.detach(&mut world);
    let mut engine =
        LightingEngine::restore_durable(&mut world, limits(), Snapshot::decode(&snapshot).unwrap())
            .unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for i in 1..=10_000 {
        assert_eq!(
            engine
                .ingest(&mut world, Tick(i), LightIngestBudget::default())
                .unwrap()
                .work
                .journal_records,
            0
        );
        assert_eq!(
            engine
                .step(&mut world, Tick(i), LightBudget::default(), &|_| None)
                .unwrap()
                .work
                .cells_examined,
            0
        );
        assert!(engine.take_durable_delta().unwrap().is_none());
    }
    let counts = scope.finish();
    assert_eq!(counts, telemetry::AllocationCounts::ZERO);
}
#[test]
fn truncated_snapshot_and_stale_delta_are_rejected_without_mutation() {
    let mut world = world();
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    engine.enable_durable_inputs().unwrap();
    engine
        .queue_cell(position(8), Tick(0), LightPriority::Interactive)
        .unwrap();
    let delta = engine
        .take_durable_delta()
        .unwrap()
        .unwrap()
        .encode()
        .unwrap();
    let snapshot = engine.durable_snapshot().unwrap().encode().unwrap();
    for n in 0..snapshot.len() {
        assert!(Snapshot::decode(&snapshot[..n]).is_err());
    }
    for n in 0..delta.len() {
        assert!(Delta::decode(&delta[..n]).is_err());
    }
    assert!(
        engine
            .apply_durable_delta(Delta::decode(&delta).unwrap())
            .is_err()
    );
    assert!(engine.durable_snapshot().unwrap().encode().unwrap() == snapshot);
}

#[test]
fn invalid_later_queue_operation_rolls_back_earlier_operations_exactly() {
    let mut world = world();
    let mut source = LightingEngine::attach(&mut world, limits()).unwrap();
    source.enable_durable_inputs().unwrap();
    let initial = source.durable_snapshot().unwrap().encode().unwrap();
    source
        .queue_cell(position(8), Tick(0), LightPriority::Interactive)
        .unwrap();
    let mut bytes = source
        .take_durable_delta()
        .unwrap()
        .unwrap()
        .encode()
        .unwrap();
    // WLD1 header33B, followed by the first Insert26B. Keep a valid first
    // mutation, then append a structurally valid Remove of an absent slot.
    bytes[29..33].copy_from_slice(&2u32.to_le_bytes());
    bytes.splice(59..59, [1, 255, 255, 255, 255]);
    let delta = Delta::decode(&bytes).unwrap();
    let mut restored =
        LightingEngine::restore_durable(&mut world, limits(), Snapshot::decode(&initial).unwrap())
            .unwrap();
    assert!(restored.apply_durable_delta(delta).is_err());
    assert!(restored.durable_snapshot().unwrap().encode().unwrap() == initial);
}

#[test]
fn unread_semantic_input_is_not_idle_before_ingest_or_after_snapshot_restore() {
    let mut live = world();
    let mut engine = LightingEngine::attach(&mut live, limits()).unwrap();
    engine.enable_durable_inputs().unwrap();
    assert!(engine.is_idle());
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(position(8), Cell::from_parts(2, 0));
    let change = live.commit(tx).unwrap();
    engine.observe_durable_change(&live, &change).unwrap();
    assert_eq!(engine.pending_cells(), 0);
    assert_eq!(engine.pending_seed_pages(), 0);
    assert!(
        !engine.is_idle(),
        "the solver has not ingested the emitter yet"
    );
    assert!(engine.take_durable_delta().unwrap().is_some());
    let bytes = engine.durable_snapshot().unwrap().encode().unwrap();
    let mut world = World::restore(
        properties,
        JournalLimits::default(),
        live.checkpoint().to_restore_state(),
    )
    .unwrap();
    let mut restored =
        LightingEngine::restore_durable(&mut world, limits(), Snapshot::decode(&bytes).unwrap())
            .unwrap();
    assert_eq!(restored.pending_cells(), 0);
    assert_eq!(restored.pending_seed_pages(), 0);
    assert!(
        !restored.is_idle(),
        "an unread durable feed survives restart"
    );
    let progress = restored
        .ingest(&mut world, Tick(1), LightIngestBudget::default())
        .unwrap();
    assert_eq!(progress.work.journal_records, 1);
    assert!(restored.pending_cells() > 0);
    assert!(!restored.is_idle());
}
