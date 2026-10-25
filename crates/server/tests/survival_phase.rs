use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PAGE_VOLUME, PageId, SpaceId, Tick};
use rules::{
    ids::*,
    items::*,
    stack::ItemStack,
    survival::{
        SurvivalError,
        fluid::{Fluid, Liquid},
        furnace::{Chest, Furnace},
    },
};
use server::survival::{
    self, Budget, DropBundle, Event, RuntimeDelta, RuntimeSnapshot, SurvivalRuntime,
};
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
use world::{CellRead, Transaction, World};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn stack(id: u16, count: u16) -> ItemStack {
    ItemStack::new(id, count).unwrap()
}
fn scene() -> World {
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in -1..=1 {
        for z in -1..=1 {
            for y in -1..=0 {
                let page = PageId {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                };
                let values: Vec<_> = (0..PAGE_VOLUME)
                    .map(|index| {
                        if y * 16 + (index / 256) as i32 <= 0 {
                            Cell::from_parts(STONE, 0)
                        } else {
                            Cell::AIR
                        }
                    })
                    .collect();
                tx.create_page(page, values.into_boxed_slice());
            }
        }
    }
    world.commit(tx).unwrap();
    world
}
fn commit(world: &mut World, runtime: &mut SurvivalRuntime, plan: survival::PhasePlan) {
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    let encoded = plan.delta.encode();
    runtime
        .acknowledge(RuntimeDelta::decode(&encoded, 10_000).unwrap())
        .unwrap();
}
#[test]
fn detached_furnace_phase_exact_donor_ticks_and_snapshot_wal_resume() {
    let mut world = scene();
    let anchor = pos(2, 1, 2);
    let id = ComponentId(91);
    let mut furnace = Furnace::default();
    furnace.set_trusted(0, stack(I_RAW_IRON, 8)).unwrap();
    furnace.set_trusted(1, stack(I_COAL, 1)).unwrap();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(anchor, Cell::from_parts(FURNACE, 0))
        .set_component(
            id,
            survival::furnace_component(anchor, &furnace),
            ChannelMask::CONTENTS,
        );
    world.commit(tx).unwrap();
    let mut runtime = SurvivalRuntime::new(100);
    runtime.register(Event::Furnace(id), Tick(1)).unwrap();
    let mut ingots = Vec::new();
    for tick in 1..=1601 {
        let before = world.canonical_hash();
        let plan = runtime
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        assert_eq!(world.canonical_hash(), before);
        assert!(plan.work.reads <= 1);
        assert!(plan.transactions.len() <= 1);
        commit(&mut world, &mut runtime, plan);
        let furnace = Furnace::decode(&world.component(id).unwrap().data.payload).unwrap();
        if tick.is_multiple_of(200) {
            ingots.push((tick, furnace.slots()[2].count()));
        }
        if tick == 437 {
            let snapshot = runtime.snapshot().encode();
            runtime =
                SurvivalRuntime::restore(100, &RuntimeSnapshot::decode(&snapshot, 100).unwrap())
                    .unwrap();
        }
    }
    assert_eq!(
        ingots,
        [
            (200, 1),
            (400, 2),
            (600, 3),
            (800, 4),
            (1000, 5),
            (1200, 6),
            (1400, 7),
            (1600, 8)
        ]
    );
    assert!(runtime.is_empty());
    assert_eq!(
        world.read(anchor),
        CellRead::Known(Cell::from_parts(FURNACE, 0))
    );
}
#[test]
fn real_world_fluid_phase_reproduces_113_diamond_under_budget() {
    let mut world = scene();
    let anchor = pos(0, 1, 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(anchor, Fluid::source(Liquid::Water).cell());
    world.commit(tx).unwrap();
    let mut runtime = SurvivalRuntime::new(10_000);
    runtime
        .register(Event::Fluid(anchor, Liquid::Water), Tick(5))
        .unwrap();
    let budget = Budget {
        events: 1000,
        cells: 2_048_000,
        writes: 100_000,
    };
    for tick in 1..=42 {
        let plan = runtime.plan_tick(&world, Tick(tick), budget).unwrap();
        assert!(plan.work.reads <= budget.cells);
        assert!(plan.work.writes <= budget.writes as u64);
        commit(&mut world, &mut runtime, plan);
    }
    let mut count = 0;
    for x in -14i32..=14 {
        for z in -14i32..=14 {
            let CellRead::Known(cell) = world.read(pos(x, 1, z)) else {
                panic!("known")
            };
            if cell.id() == WATER {
                count += 1;
                assert_eq!(cell.state(), (x.abs() + z.abs()) as u16);
            }
        }
    }
    assert_eq!(count, 113);
    assert!(runtime.is_empty());
}
#[test]
fn unknown_frontier_defers_without_partial_world_writes_or_lost_age() {
    let mut world = World::new(rules::properties, 20);
    let page = PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let mut cells = vec![Cell::AIR; PAGE_VOLUME];
    cells[0] = Fluid::source(Liquid::Water).cell();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page, cells.into_boxed_slice());
    world.commit(tx).unwrap();
    let mut runtime = SurvivalRuntime::new(10);
    runtime
        .register(Event::Fluid(pos(0, 0, 0), Liquid::Water), Tick(5))
        .unwrap();
    let plan = runtime
        .plan_tick(&world, Tick(8), Budget::default())
        .unwrap();
    assert_eq!(plan.work.unknown_deferred, 1);
    assert_eq!(plan.work.writes, 0);
    assert!(plan.transactions.is_empty());
    commit(&mut world, &mut runtime, plan);
    assert_eq!(runtime.oldest_overdue(Tick(12)), 7);
    assert_eq!(runtime.len(), 1);
}
#[test]
fn stale_runtime_delta_rejected_atomically_and_unknown_is_fair() {
    let world = scene();
    let mut runtime = SurvivalRuntime::new(10);
    runtime
        .register(Event::Fluid(pos(5000, 1, 0), Liquid::Water), Tick(1))
        .unwrap();
    runtime
        .register(Event::Fluid(pos(0, 1, 0), Liquid::Water), Tick(1))
        .unwrap();
    let first = runtime
        .plan_tick(
            &world,
            Tick(1),
            Budget {
                events: 1,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!(first.work.unknown_deferred, 1);
    let duplicate = RuntimeDelta::decode(&first.delta.encode(), 100).unwrap();
    runtime.acknowledge(first.delta).unwrap();
    let before = runtime.snapshot();
    assert_eq!(
        runtime.acknowledge(duplicate),
        Err(SurvivalError::StalePlan)
    );
    assert_eq!(runtime.snapshot(), before);
    let next = runtime
        .plan_tick(
            &world,
            Tick(2),
            Budget {
                events: 1,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!(next.work.unknown_deferred, 0);
    assert_eq!(next.work.events, 1);
}
#[test]
fn durable_drop_bundle_keeps_stable_ids_merges_replays_and_caps_before_mutation() {
    let mut world = scene();
    let anchor = pos(1, 1, 1);
    let before = world.canonical_hash();
    let stacks = [stack(COBBLE, 1), stack(I_RAW_IRON, 2)];
    let drop =
        survival::plan_drops(&world, anchor, [7; 16], 42, Tick(1), &stacks, 10, None).unwrap();
    let id = drop.id;
    assert_eq!(world.canonical_hash(), before);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    drop.append_to(&mut tx);
    world.commit(tx).unwrap();
    let bundle = DropBundle::decode(&world.component(id).unwrap().data.payload).unwrap();
    assert_eq!(bundle.entries.len(), 2);
    assert_ne!(bundle.entries[0].entity_id, bundle.entries[1].entity_id);
    assert_eq!(bundle.entries[0].pickup_at, Tick(11));
    let replay =
        survival::plan_drops(&world, anchor, [7; 16], 42, Tick(99), &stacks, 10, None).unwrap();
    assert_eq!(
        replay.data.payload,
        world.component(id).unwrap().data.payload
    );
    assert!(matches!(
        survival::plan_drops(
            &world,
            anchor,
            [7; 16],
            42,
            Tick(1),
            &[stack(DIRT, 1)],
            10,
            None
        ),
        Err(SurvivalError::NoEntitlement)
    ));
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    survival::plan_drops(
        &world,
        anchor,
        [7; 16],
        43,
        Tick(2),
        &[stack(TORCH, 1); 62],
        10,
        None,
    )
    .unwrap()
    .append_to(&mut tx);
    world.commit(tx).unwrap();
    assert_eq!(
        DropBundle::decode(&world.component(id).unwrap().data.payload)
            .unwrap()
            .entries
            .len(),
        64
    );
    let before = world.canonical_hash();
    assert!(matches!(
        survival::plan_drops(
            &world,
            anchor,
            [7; 16],
            44,
            Tick(3),
            &[stack(DIRT, 1)],
            10,
            None
        ),
        Err(SurvivalError::Capacity)
    ));
    assert_eq!(world.canonical_hash(), before);
}
#[test]
fn chest_contents_are_durable_before_break_bundle_replaces_anchor() {
    let mut world = scene();
    let anchor = pos(1, 1, 1);
    let id = ComponentId(7);
    let mut chest = Chest::default();
    chest.set_trusted(0, stack(COBBLE, 12)).unwrap();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(anchor, Cell::from_parts(CHEST, 0))
        .set_component(
            id,
            survival::chest_component(anchor, &chest),
            ChannelMask::CONTENTS,
        );
    world.commit(tx).unwrap();
    assert_eq!(
        Chest::decode(&world.component(id).unwrap().data.payload),
        Ok(chest)
    );
    assert!(matches!(
        survival::plan_drops(&world, anchor, [1; 16], 1, Tick(1), chest.slots(), 10, None),
        Err(SurvivalError::NoEntitlement)
    ));
    let drop = survival::plan_drops(
        &world,
        anchor,
        [1; 16],
        1,
        Tick(1),
        chest.slots(),
        10,
        Some(id),
    )
    .unwrap();
    let new_id = drop.id;
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(anchor, Cell::AIR);
    drop.append_to(&mut tx);
    world.commit(tx).unwrap();
    assert!(world.component(id).is_none());
    assert_eq!(
        DropBundle::decode(&world.component(new_id).unwrap().data.payload)
            .unwrap()
            .entries[0]
            .stack,
        stack(COBBLE, 12)
    );
}
#[test]
fn no_events_and_100k_future_events_both_idle_zero_work_and_allocation() {
    let world = World::new(rules::properties, 4);
    let empty = SurvivalRuntime::new(0);
    let mut full = SurvivalRuntime::new(100_000);
    for x in 0..100_000 {
        full.register(Event::Fluid(pos(x, 1, 0), Liquid::Water), Tick(1000))
            .unwrap();
    }
    let control = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(29));
    black_box(&allocation);
    assert!(control.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..100 {
        let a = empty.plan_tick(&world, Tick(1), Budget::default()).unwrap();
        let b = full.plan_tick(&world, Tick(1), Budget::default()).unwrap();
        assert_eq!(a.work, b.work);
        black_box(a);
        black_box(b);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert_eq!(full.len(), 100_000);
}

#[test]
fn accepted_edit_wake_is_detached_durable_and_codec_preserves_private_rng_seed() {
    let mut world = scene();
    let anchor = pos(0, 1, 0);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(anchor, Fluid::source(Liquid::Water).cell());
    world.commit(tx).unwrap();
    let mut runtime = SurvivalRuntime::with_seed(10, 0x12345678);
    let before = runtime.snapshot();
    let delta = runtime.plan_wake(&world, anchor, Tick(1)).unwrap();
    assert_eq!(runtime.snapshot(), before);
    assert!(runtime.is_empty());
    runtime
        .acknowledge(RuntimeDelta::decode(&delta.encode(), 10).unwrap())
        .unwrap();
    assert_eq!(runtime.len(), 1);
    let snapshot = runtime.snapshot().encode();
    let decoded = RuntimeSnapshot::decode(&snapshot, 10).unwrap();
    assert_eq!(decoded.seed, 0x12345678);
    assert_eq!(decoded.events[0].due, Tick(6));
    assert_eq!(
        SurvivalRuntime::restore(10, &decoded)
            .unwrap()
            .snapshot()
            .encode(),
        snapshot
    );
    let mut corrupt = snapshot.clone();
    *corrupt.last_mut().unwrap() = 1;
    assert!(matches!(
        RuntimeSnapshot::decode(&corrupt, 10),
        Err(SurvivalError::InvalidEncoding)
    ));
    let before = runtime.snapshot();
    assert!(matches!(
        runtime
            .plan_register((0..20).map(|x| (Event::Fluid(pos(x, 1, 0), Liquid::Water), Tick(6)))),
        Err(SurvivalError::Capacity)
    ));
    assert_eq!(runtime.snapshot(), before);
}

#[test]
fn composed_tick_wakes_keep_retry_age_order_and_one_revision_boundary() {
    let world = World::new(rules::properties, 4);
    let mut runtime = SurvivalRuntime::new(4);
    let old = Event::Fluid(pos(0, 1, 0), Liquid::Water);
    runtime.register(old, Tick(1)).unwrap();
    let before = runtime.snapshot();
    let plan = runtime
        .plan_tick(&world, Tick(9), Budget::default())
        .unwrap();
    assert_eq!(plan.work.unknown_deferred, 1);
    let delta = runtime
        .extend_registration(
            plan.delta,
            [
                (old, Tick(2)),
                (Event::Furnace(ComponentId(7)), Tick(20)),
                (Event::Furnace(ComponentId(7)), Tick(10)),
            ],
        )
        .unwrap();
    assert_eq!(runtime.snapshot(), before);
    assert!(!delta.is_empty());
    runtime
        .acknowledge(RuntimeDelta::decode(&delta.encode(), 4).unwrap())
        .unwrap();
    let after = runtime.snapshot();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.events.len(), 2);
    let retry = after.events.iter().find(|p| p.event == old).unwrap();
    assert_eq!(
        (retry.first_due, retry.due, retry.order),
        (Tick(1), Tick(10), 0)
    );
    let new = after
        .events
        .iter()
        .find(|p| p.event == Event::Furnace(ComponentId(7)))
        .unwrap();
    assert_eq!((new.first_due, new.due, new.order), (Tick(10), Tick(10), 1));
}
#[test]
fn composed_registration_excludes_consumed_keys_and_capacity_failure_is_atomic() {
    let mut world = scene();
    let anchor = pos(0, 1, 0);
    let id = ComponentId(41);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(anchor, Cell::from_parts(FURNACE, 2));
    tx.set_component(
        id,
        survival::furnace_component(anchor, &Furnace::default()),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut runtime = SurvivalRuntime::new(1);
    let event = Event::Furnace(id);
    runtime.register(event, Tick(1)).unwrap();
    let before = runtime.snapshot();
    let plan = runtime
        .plan_tick(&world, Tick(1), Budget::default())
        .unwrap();
    let original = plan.delta.clone();
    let composed = runtime
        .extend_registration(plan.delta, [(event, Tick(15))])
        .unwrap();
    assert_eq!(runtime.snapshot(), before);
    runtime.acknowledge(composed.clone()).unwrap();
    assert_eq!(runtime.snapshot().events[0].order, 1);
    assert_eq!(runtime.snapshot().events[0].first_due, Tick(15));
    assert!(matches!(
        runtime.extend_registration(original, []),
        Err(SurvivalError::StalePlan)
    ));
    let plan = runtime
        .plan_tick(&world, Tick(2), Budget::default())
        .unwrap();
    let snapshot = runtime.snapshot();
    assert!(matches!(
        runtime.extend_registration(plan.delta, [(Event::Farm(pos(2, 1, 0)), Tick(3))]),
        Err(SurvivalError::Capacity)
    ));
    assert_eq!(runtime.snapshot(), snapshot);
}
#[test]
fn composing_two_wakes_does_not_clone_or_scan100000_cold_queue_entries() {
    let world = World::new(rules::properties, 4);
    let mut runtime = SurvivalRuntime::new(100_002);
    for x in 0..100_000 {
        runtime
            .register(Event::Fluid(pos(x, 1, 0), Liquid::Water), Tick(1000))
            .unwrap();
    }
    let plan = runtime
        .plan_tick(&world, Tick(1), Budget::default())
        .unwrap();
    assert!(plan.delta.is_empty());
    let scope = AllocationScope::begin().unwrap();
    let delta = runtime
        .extend_registration(
            plan.delta,
            [
                (Event::Furnace(ComponentId(1)), Tick(2)),
                (Event::Farm(pos(200_000, 1, 0)), Tick(3)),
                (Event::Fluid(pos(0, 1, 0), Liquid::Water), Tick(2)),
            ],
        )
        .unwrap();
    let counts = scope.finish();
    assert!(counts.allocated_bytes < 65_536, "{counts:?}");
    assert_eq!(runtime.len(), 100_000);
    runtime.acknowledge(delta).unwrap();
    assert_eq!(runtime.len(), 100_002);
}

#[test]
fn two_with_replacement_farm_samples_tick_same_crop_twice_and_survive_codec() {
    let mut world = scene();
    let crop = pos(0, 2, 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(pos(0, 1, 0), Cell::from_parts(FARMLAND, 7));
    tx.set_cell(pos(4, 1, 0), Cell::from_parts(WATER, 0));
    tx.set_cell(crop, Cell::from_parts(WHEAT, 0));
    tx.set_light(
        crop,
        world::LightSample {
            sky: 15,
            ..Default::default()
        },
    );
    world.commit(tx).unwrap();
    let seed = (0..10_000u64)
        .find(|&seed| {
            (0..2).all(|order| {
                core_types::DeterministicRng::from_key(seed, 0x4641524d, order, 1).below(7)
                    == Some(0)
            })
        })
        .unwrap();
    let mut runtime = SurvivalRuntime::with_seed(4, seed);
    let a = Event::FarmSample(crop, survival::farm_sample_token(Tick(1), 0).unwrap());
    let b = Event::FarmSample(crop, survival::farm_sample_token(Tick(1), 1).unwrap());
    let delta = runtime
        .plan_register([(a, Tick(1)), (b, Tick(1)), (a, Tick(1))])
        .unwrap();
    runtime
        .acknowledge(RuntimeDelta::decode(&delta.encode(), 4).unwrap())
        .unwrap();
    assert_eq!(runtime.len(), 2);
    let snapshot = RuntimeSnapshot::decode(&runtime.snapshot().encode(), 4).unwrap();
    assert_eq!(
        snapshot.events.iter().map(|p| p.event).collect::<Vec<_>>(),
        [a, b]
    );
    let plan = runtime
        .plan_tick(&world, Tick(1), Budget::default())
        .unwrap();
    assert_eq!(plan.work.events, 2);
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    runtime.acknowledge(plan.delta).unwrap();
    assert_eq!(
        world.read(crop),
        CellRead::Known(Cell::from_parts(WHEAT, 2))
    );
    assert!(runtime.is_empty());
    assert!(
        runtime
            .plan_register([(Event::FarmSample(crop, 1u64 << 56), Tick(2))])
            .is_err()
    );
    assert!(survival::farm_sample_token(Tick(1u64 << 44), 0).is_err());
    assert!(survival::farm_sample_token(Tick(1), 4096).is_err());
}
