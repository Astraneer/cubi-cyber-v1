use rules::{
    item_payload::*,
    items::{I_CONFIGURATOR, I_IRON_INGOT},
    machines::Slot,
    stack::ItemStack,
    survival::Inventory,
};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);

#[test]
fn all_five_donor_modes_cycle_both_ways_without_repurposing_durability() {
    use ConfiguratorMode::*;
    assert_eq!(rules::items::idef(I_CONFIGURATOR).max_damage, 0);
    assert!(ItemStack::from_parts(I_CONFIGURATOR, 1, 1).is_err());
    let mut ledger = Ledger::new(Config::default()).unwrap();
    let mut stack = ItemStack::new(I_CONFIGURATOR, 1).unwrap();
    assert_eq!(resolve_configurator(stack, &ledger), Ok(Items));
    for expected in [Empty, Rotate, Wrench, Gas, Items, Empty] {
        let before = stack;
        let (after, payload) = cycle_configurator(stack, false, &ledger).unwrap();
        assert_eq!(payload.configurator_mode(), Ok(expected));
        assert_eq!(after.damage(), 0);
        assert_eq!(Payload::decode(payload.bytes()).unwrap(), payload);
        let delta = ledger
            .extend(ledger.plan().unwrap(), [payload], [])
            .unwrap();
        // A staged reference is not authority until the owner's batch commits.
        assert_eq!(stack, before);
        ledger.acknowledge(delta).unwrap();
        stack = after;
    }
    assert_eq!(ledger.len(), 5, "only five immutable mode records exist");
    for expected in [Items, Gas, Wrench, Rotate, Empty, Items] {
        let (after, payload) = cycle_configurator(stack, true, &ledger).unwrap();
        assert_eq!(payload.configurator_mode(), Ok(expected));
        ledger
            .acknowledge(
                ledger
                    .extend(ledger.plan().unwrap(), [payload], [])
                    .unwrap(),
            )
            .unwrap();
        stack = after;
    }
    let snapshot = ledger.snapshot().encode(Config::default()).unwrap();
    let ledger = Ledger::restore(
        Config::default(),
        &Snapshot::decode(&snapshot, Config::default()).unwrap(),
    )
    .unwrap();
    assert_eq!(ledger.len(), 5);
    assert_eq!(resolve_configurator(stack, &ledger), Ok(Items));
    assert_eq!(Payload::configurator(Wrench).bytes(), [3, 71, 1, 3]);
}

#[test]
fn mode_survives_real_inventory_and_transport_slot_codecs_and_resolving_is_allocation_free() {
    let payload = Payload::configurator(ConfiguratorMode::Gas);
    let stack = ItemStack::new(I_CONFIGURATOR, 1)
        .unwrap()
        .with_payload(payload.id())
        .unwrap();
    let mut ledger = Ledger::new(Config::default()).unwrap();
    ledger
        .acknowledge(
            ledger
                .extend(ledger.plan().unwrap(), [payload], [])
                .unwrap(),
        )
        .unwrap();
    let mut inventory = Inventory::default();
    inventory
        .plan_pickup(stack)
        .unwrap()
        .apply(&mut inventory)
        .unwrap();
    let inventory = Inventory::decode(&inventory.encode()).unwrap();
    let retained = inventory.hand();
    assert_eq!(retained, stack);
    let slot = Slot::from_item(retained, &ledger).unwrap();
    assert_eq!(slot.to_item().unwrap(), (stack, None));
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert_eq!(
            resolve_configurator(retained, &ledger),
            Ok(ConfiguratorMode::Gas)
        );
        assert_eq!(Slot::from_item(retained, &ledger).unwrap().stack, stack);
    }
    assert_eq!(scope.finish(), AllocationCounts::default());
}

#[test]
fn malformed_modes_and_unowned_or_wrong_item_references_fail_closed() {
    for mode in 5..=255u8 {
        assert!(Payload::decode(&[3, 71, 1, mode]).is_err());
    }
    let valid = Payload::configurator(ConfiguratorMode::Rotate);
    for n in 0..valid.bytes().len() {
        assert!(Payload::decode(&valid.bytes()[..n]).is_err());
    }
    let mut trailing = valid.bytes().to_vec();
    trailing.push(0);
    assert!(Payload::decode(&trailing).is_err());
    assert!(Payload::decode(&[3, 72, 1, 2]).is_err());
    let mut ledger = Ledger::new(Config::default()).unwrap();
    let bare = ItemStack::new(I_CONFIGURATOR, 1).unwrap();
    let missing = bare.with_payload(valid.id()).unwrap();
    assert_eq!(resolve_configurator(missing, &ledger), Err(Error::Missing));
    let forged = ItemStack::new(I_IRON_INGOT, 1)
        .unwrap()
        .with_payload(valid.id())
        .unwrap();
    ledger
        .acknowledge(ledger.extend(ledger.plan().unwrap(), [valid], []).unwrap())
        .unwrap();
    assert_eq!(resolve_configurator(forged, &ledger), Err(Error::Item));
    assert_eq!(Slot::from_item(forged, &ledger), Err(Error::Item));
}
