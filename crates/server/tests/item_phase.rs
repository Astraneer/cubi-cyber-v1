use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use rules::{
    entities::{EntityError, EntityId, item::ItemEntity},
    ids::*,
    items::*,
    stack::ItemStack,
    survival::Inventory,
};
use server::{
    entities::items::{Budget, ItemDelta, ItemRuntime, PickupPlayer, Snapshot},
    survival,
};
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn stack(id: u16, count: u16) -> ItemStack {
    ItemStack::new(id, count).unwrap()
}
fn scene() -> World {
    let mut world = World::new(rules::properties, 100);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        (0..PAGE_VOLUME)
            .map(|index| Cell::from_parts(if index / 256 == 0 { STONE } else { AIR }, 0))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    world.commit(tx).unwrap();
    world
}
fn player() -> PickupPlayer {
    PickupPlayer {
        id: EntityId(99),
        space: SpaceId(0),
        position: [2.5, 1.0, 2.5],
        crouching: false,
        alive: true,
        inventory: Inventory::empty(),
    }
}
fn apply_tick(
    world: &World,
    runtime: &mut ItemRuntime,
    players: &mut [PickupPlayer],
    tick: u64,
    budget: Budget,
) -> server::entities::items::Work {
    let plan = runtime
        .plan_tick(world, Tick(tick), players, budget)
        .unwrap();
    runtime.validate(world, players, &plan).unwrap();
    for patch in &plan.inventories {
        let player = players
            .iter_mut()
            .find(|player| player.id == patch.player)
            .unwrap();
        assert_eq!(player.inventory, patch.before);
        player.inventory = patch.after;
    }
    let work = plan.work;
    let encoded = plan.delta.encode().unwrap();
    runtime
        .acknowledge(ItemDelta::decode(&encoded, 5000).unwrap())
        .unwrap();
    work
}

#[test]
fn confirmed_block_drop_import_and_tick10_pickup_preserve_all_items() {
    let mut world = scene();
    let anchor = BlockPos {
        space: SpaceId(0),
        x: 2,
        y: 1,
        z: 2,
    };
    let drop = survival::plan_drops(
        &world,
        anchor,
        [4; 16],
        7,
        Tick(0),
        &[stack(COBBLE, 3), stack(I_RAW_IRON, 2)],
        10,
        None,
    )
    .unwrap();
    let id = drop.id;
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    drop.append_to(&mut tx);
    world.commit(tx).unwrap();
    let mut runtime = ItemRuntime::new(100, 1337).unwrap();
    let plan = runtime.plan_import(&world, &[id], Tick(1), true).unwrap();
    assert!(runtime.entities().is_empty());
    assert!(world.component(id).is_some());
    runtime.validate_import(&world, &plan).unwrap();
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    runtime
        .acknowledge(ItemDelta::decode(&plan.delta.encode().unwrap(), 100).unwrap())
        .unwrap();
    assert!(world.component(id).is_none());
    assert_eq!(runtime.entities().len(), 2);
    let mut players = [player()];
    let mut picked = 0;
    for tick in 2..=10 {
        let work = apply_tick(&world, &mut runtime, &mut players, tick, Budget::default());
        picked += work.picked_items;
        if tick < 10 {
            assert_eq!(picked, 0);
        }
    }
    assert_eq!(picked, 5);
    assert!(runtime.entities().is_empty());
    assert_eq!(
        players[0]
            .inventory
            .main()
            .iter()
            .filter(|stack| stack.item() == COBBLE)
            .map(|stack| stack.count())
            .sum::<u16>(),
        3
    );
    assert_eq!(
        players[0]
            .inventory
            .main()
            .iter()
            .filter(|stack| stack.item() == I_RAW_IRON)
            .map(|stack| stack.count())
            .sum::<u16>(),
        2
    );
}
#[test]
fn import_capacity_failure_leaves_durable_source_and_ledger_intact() {
    let mut world = scene();
    let drop = survival::plan_drops(
        &world,
        BlockPos {
            space: SpaceId(0),
            x: 2,
            y: 1,
            z: 2,
        },
        [1; 16],
        1,
        Tick(0),
        &[stack(DIRT, 1), stack(COBBLE, 1)],
        10,
        None,
    )
    .unwrap();
    let id = drop.id;
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    drop.append_to(&mut tx);
    world.commit(tx).unwrap();
    let runtime = ItemRuntime::new(1, 1).unwrap();
    assert!(matches!(
        runtime.plan_import(&world, &[id], Tick(1), true),
        Err(EntityError::Capacity)
    ));
    assert!(runtime.entities().is_empty());
    assert!(world.component(id).is_some());
}
#[test]
fn actual_owner_thrown_delay40_and_partial_pickup_remainder() {
    let world = scene();
    let mut runtime = ItemRuntime::new(10, 1).unwrap();
    let mut item = ItemEntity::new(
        EntityId(1),
        SpaceId(0),
        stack(DIRT, 10),
        [2.5, 1.0, 2.5],
        1,
        40,
    )
    .unwrap();
    item.velocity = [0.0; 3];
    item.on_ground = true;
    runtime.insert_initial(item, true).unwrap();
    let mut players = [player()];
    for slot in 0..36 {
        players[0]
            .inventory
            .set_trusted(slot, stack(COBBLE, 64))
            .unwrap();
    }
    players[0]
        .inventory
        .set_trusted(3, stack(DIRT, 60))
        .unwrap();
    for tick in 1..40 {
        assert_eq!(
            apply_tick(&world, &mut runtime, &mut players, tick, Budget::default()).picked_items,
            0
        );
    }
    assert_eq!(
        apply_tick(&world, &mut runtime, &mut players, 40, Budget::default()).picked_items,
        4
    );
    assert_eq!(
        runtime.entities().get(item.id).unwrap().stack,
        stack(DIRT, 6)
    );
    assert_eq!(players[0].inventory.get(3), Some(stack(DIRT, 64)));
}
#[test]
fn merge_uses_spatial_candidates_conserves_five_stacks_and_keeps_budget_debt() {
    let world = scene();
    let mut runtime = ItemRuntime::new(10, 1).unwrap();
    for id in 0..5 {
        let mut item = ItemEntity::new(
            EntityId(id),
            SpaceId(0),
            stack(DIRT, 1),
            [2.5 + id as f64 * 0.05, 1.3, 2.5],
            1,
            0,
        )
        .unwrap();
        item.velocity = [0.0, 0.1, 0.0];
        runtime.insert_initial(item, true).unwrap();
    }
    for tick in 1..=40 {
        let work = apply_tick(
            &world,
            &mut runtime,
            &mut [],
            tick,
            Budget {
                merge_checks: 0,
                ..Budget::default()
            },
        );
        assert_eq!(work.merge_candidates, 0);
    }
    assert_eq!(runtime.entities().len(), 5);
    assert!(runtime.entities().rows().iter().any(|item| item.merge_debt));
    for tick in 41..=45 {
        let work = apply_tick(&world, &mut runtime, &mut [], tick, Budget::default());
        assert!(work.merge_candidates <= Budget::default().merge_checks);
    }
    assert_eq!(runtime.entities().len(), 1);
    assert_eq!(runtime.entities().rows()[0].stack.count(), 5);
}
#[test]
fn checkpoint_and_delta_recovery_preserve_item_motion_and_pickup_inventory_guard() {
    let world = scene();
    let mut runtime = ItemRuntime::new(10, 7).unwrap();
    let item = ItemEntity::new(
        EntityId(1),
        SpaceId(0),
        stack(DIRT, 3),
        [2.5, 1.0, 2.5],
        7,
        0,
    )
    .unwrap();
    runtime.insert_initial(item, true).unwrap();
    let mut players = [player()];
    let plan = runtime
        .plan_tick(&world, Tick(1), &players, Budget::default())
        .unwrap();
    assert_eq!(runtime.entities().get(item.id), Some(&item));
    players[0]
        .inventory
        .set_trusted(5, stack(STONE, 1))
        .unwrap();
    assert_eq!(
        runtime.validate(&world, &players, &plan),
        Err(EntityError::StalePlan)
    );
    assert_eq!(runtime.entities().get(item.id), Some(&item));
    let bytes = runtime.snapshot().encode().unwrap();
    let snapshot = Snapshot::decode(&bytes, 10).unwrap();
    let restored = ItemRuntime::restore(10, &snapshot).unwrap();
    assert_eq!(restored.snapshot(), runtime.snapshot());
    let mut corrupt = bytes.clone();
    corrupt.push(0);
    assert!(matches!(
        Snapshot::decode(&corrupt, 10),
        Err(EntityError::InvalidEncoding)
    ));
}
#[test]
fn thousand_sleeping_items_add_zero_idle_work_and_main_thread_allocations() {
    let world = World::new(rules::properties, 4);
    let mut empty = ItemRuntime::new(0, 1).unwrap();
    let mut sleeping = ItemRuntime::new(1000, 1).unwrap();
    for id in 0..1000 {
        sleeping
            .insert_initial(
                ItemEntity::new(
                    EntityId(id),
                    SpaceId(0),
                    stack(DIRT, 1),
                    [id as f64, 1.0, 1.0],
                    1,
                    10,
                )
                .unwrap(),
                false,
            )
            .unwrap();
    }
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(11));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for tick in 1..=100 {
        let a = empty
            .plan_tick(&world, Tick(tick), &[], Budget::default())
            .unwrap();
        let b = sleeping
            .plan_tick(&world, Tick(tick), &[], Budget::default())
            .unwrap();
        assert_eq!(a.work, b.work);
        assert_eq!(b.work.entities, 0);
        black_box(a);
        black_box(b);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert!(
        sleeping
            .entities()
            .rows()
            .iter()
            .all(|item| item.age == 0 && item.pickup_delay == 10)
    );
}
