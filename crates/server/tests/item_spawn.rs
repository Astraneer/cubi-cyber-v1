use core_types::{Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use rules::{
    entities::{EntityError, EntityId, item::ItemEntity},
    item_payload::Payload,
    items::I_ENERGY_TABLET,
    stack::ItemStack,
};
use server::entities::items::{Budget, ItemRuntime, Snapshot};
use world::{Transaction, World};
fn row(id: u64) -> ItemEntity {
    let payload = Payload::charge(180_000).unwrap();
    let stack = ItemStack::new(I_ENERGY_TABLET, 1)
        .unwrap()
        .with_payload(payload.id())
        .unwrap();
    let mut item =
        ItemEntity::new(EntityId(id), SpaceId(0), stack, [2.0, 2.0, 2.0], 8, 10).unwrap();
    item.last_tick = Tick(7);
    item
}
#[test]
fn trusted_packet_drop_composes_one_item_phase_and_survives_recovery_with_reference() {
    let mut world = World::new(rules::properties, 1000);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        vec![Cell::AIR; PAGE_VOLUME],
    );
    world.commit(tx).unwrap();
    let mut runtime = ItemRuntime::new(8, 9).unwrap();
    let item = row(41);
    let plan = runtime
        .plan_tick(&world, Tick(7), &[], Budget::default())
        .unwrap();
    let delta = runtime.extend_spawn(plan.delta, [item], true).unwrap();
    assert!(runtime.entities().is_empty());
    runtime.acknowledge(delta.clone()).unwrap();
    assert_eq!(runtime.acknowledge(delta), Err(EntityError::StalePlan));
    assert_eq!(runtime.entities().get(item.id), Some(&item));
    let snapshot = Snapshot::decode(&runtime.snapshot().encode().unwrap(), 8).unwrap();
    let mut runtime = ItemRuntime::restore(8, &snapshot).unwrap();
    let plan = runtime
        .plan_tick(&world, Tick(8), &[], Budget::default())
        .unwrap();
    runtime.acknowledge(plan.delta).unwrap();
    let after = runtime.entities().get(item.id).unwrap();
    assert_eq!(after.last_tick, Tick(8));
    assert_eq!(after.stack, item.stack);
    assert_ne!(after.position, item.position);
}
#[test]
fn capacity_and_duplicate_rows_are_rejected_before_any_item_is_published() {
    let runtime = ItemRuntime::new(1, 9).unwrap();
    let empty = runtime.snapshot();
    assert!(matches!(
        runtime.extend_spawn(runtime.plan_activity(&[]).unwrap(), [row(1), row(2)], true),
        Err(EntityError::Capacity)
    ));
    assert_eq!(runtime.snapshot(), empty);
    assert!(matches!(
        runtime.extend_spawn(runtime.plan_activity(&[]).unwrap(), [row(1), row(1)], true),
        Err(EntityError::Capacity) | Err(EntityError::DuplicateId)
    ));
    assert_eq!(runtime.snapshot(), empty);
}
#[test]
fn spawn_capacity_uses_effective_existing_delta_and_stale_guard() {
    let mut runtime = ItemRuntime::new(2, 9).unwrap();
    let empty = runtime.plan_activity(&[]).unwrap();
    assert_eq!(runtime.spawn_capacity(&empty), Ok(2));
    let staged = runtime.extend_spawn(empty, [row(1)], true).unwrap();
    assert_eq!(runtime.spawn_capacity(&staged), Ok(1));
    runtime.acknowledge(staged.clone()).unwrap();
    assert_eq!(runtime.spawn_capacity(&staged), Err(EntityError::StalePlan));
}
