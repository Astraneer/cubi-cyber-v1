use core_types::{SpaceId, Tick};
use rules::{
    entities::{
        EntityError, EntityId,
        item::{self, ItemEntity, ItemStore, PickupPlan},
    },
    ids::*,
    stack::ItemStack,
    survival::Inventory,
};
fn stack(id: u16, count: u16) -> ItemStack {
    ItemStack::new(id, count).unwrap()
}
fn item(id: u64, count: u16, position: [f64; 3]) -> ItemEntity {
    let mut item = ItemEntity::new(
        EntityId(id),
        SpaceId(0),
        stack(DIRT, count),
        position,
        1337,
        0,
    )
    .unwrap();
    item.velocity = [0.0; 3];
    item
}
#[test]
fn donor_merge_larger_survives_delay_max_age_min_and_exact_distance() {
    let mut a = item(1, 30, [0.5, 1.0, 0.5]);
    let mut b = item(2, 34, [0.9, 1.0, 0.5]);
    a.pickup_delay = 40;
    a.item_age = 100;
    b.item_age = 3000;
    let plans = item::plan_merge(a, b, true, true).unwrap();
    assert_eq!(plans[0].before, Some(a));
    assert!(plans[0].after.is_none());
    let target = plans[1].after.unwrap();
    assert_eq!(target.stack.count(), 64);
    assert_eq!(
        (target.pickup_delay, target.item_age, target.position[0]),
        (40, 100, 0.9)
    );
    for (a, b, distance, merge) in [
        (40, 40, 0.3, false),
        (32, 32, 0.3, true),
        (64, 1, 0.3, false),
        (2, 2, 0.75, false),
        (2, 2, 0.7499, true),
    ] {
        assert_eq!(
            item::plan_merge(
                item(1, a, [0.5, 1.0, 0.5]),
                item(2, b, [0.5 + distance, 1.0, 0.5]),
                true,
                true
            )
            .is_ok(),
            merge
        );
    }
}
#[test]
fn pickup_strict_bounds_and_partial_visible_remainder_match_donor() {
    for (position, ok) in [
        ([0.5 + 1.4249, 1.0, 0.5], true),
        ([0.5 + 1.425, 1.0, 0.5], false),
        ([0.5, 0.25, 0.5], false),
        ([0.5, 0.2501, 0.5], true),
        ([0.5, 3.2999, 0.5], true),
        ([0.5, 3.3, 0.5], false),
    ] {
        let item = item(1, 1, position);
        assert_eq!(
            PickupPlan::prepare(
                item,
                true,
                &Inventory::empty(),
                SpaceId(0),
                [0.5, 1.0, 0.5],
                false,
                true
            )
            .is_ok(),
            ok,
            "{position:?}"
        );
    }
    let mut inv = Inventory::empty();
    for slot in 0..36 {
        inv.set_trusted(slot, stack(COBBLE, 64)).unwrap();
    }
    inv.set_trusted(3, stack(DIRT, 60)).unwrap();
    let original = item(1, 10, [0.5, 1.0, 0.5]);
    let plan = PickupPlan::prepare(
        original,
        true,
        &inv,
        SpaceId(0),
        [0.5, 1.0, 0.5],
        false,
        true,
    )
    .unwrap();
    assert_eq!(plan.picked, 4);
    assert_eq!(plan.item.after.unwrap().stack, stack(DIRT, 6));
    let mut items = ItemStore::new(10).unwrap();
    items.insert(original, true).unwrap();
    plan.apply(&mut items, &mut inv).unwrap();
    assert_eq!(inv.get(3), Some(stack(DIRT, 64)));
    assert_eq!(items.get(original.id).unwrap().stack, stack(DIRT, 6));
    assert!(matches!(
        PickupPlan::prepare(
            *items.get(original.id).unwrap(),
            true,
            &inv,
            SpaceId(0),
            [0.5, 1.0, 0.5],
            false,
            true
        ),
        Err(EntityError::NoEntitlement)
    ));
}
#[test]
fn pickup_rejects_dead_different_space_delay_and_stale_inventory_atomically() {
    let mut inv = Inventory::empty();
    let mut items = ItemStore::new(2).unwrap();
    let original = item(1, 3, [0.5, 1.0, 0.5]);
    items.insert(original, true).unwrap();
    for (space, alive) in [(SpaceId(0), false), (SpaceId(1), true)] {
        assert!(matches!(
            PickupPlan::prepare(original, true, &inv, space, [0.5, 1.0, 0.5], false, alive),
            Err(EntityError::NoEntitlement)
        ));
    }
    let mut delayed = original;
    delayed.pickup_delay = 1;
    assert!(
        PickupPlan::prepare(
            delayed,
            true,
            &inv,
            SpaceId(0),
            [0.5, 1.0, 0.5],
            false,
            true
        )
        .is_err()
    );
    let plan = PickupPlan::prepare(
        original,
        true,
        &inv,
        SpaceId(0),
        [0.5, 1.0, 0.5],
        false,
        true,
    )
    .unwrap();
    inv.set_trusted(5, stack(STONE, 1)).unwrap();
    assert_eq!(
        plan.apply(&mut items, &mut inv),
        Err(EntityError::StalePlan)
    );
    assert_eq!(items.get(original.id), Some(&original));
    assert_eq!(inv.get(5), Some(stack(STONE, 1)));
}
#[test]
fn item_codec_keeps_exact_motion_ticks_and_rejects_corruption() {
    let mut item = item(9, 3, [-1.0, 2.5, 1e9 + 0.125]);
    item.last_tick = Tick(77);
    item.item_age = 5999;
    item.age = 1234;
    item.spin_age = 987;
    item.pickup_delay = 40;
    item.thrown = true;
    let bytes = item.encode().unwrap();
    assert_eq!(ItemEntity::decode(&bytes), Ok(item));
    let mut corrupt = bytes;
    corrupt[159] = 1;
    assert_eq!(
        ItemEntity::decode(&corrupt),
        Err(EntityError::InvalidEncoding)
    );
    let mut corrupt = bytes;
    corrupt[24..32].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
    assert_eq!(
        ItemEntity::decode(&corrupt),
        Err(EntityError::InvalidPosition)
    );
}
