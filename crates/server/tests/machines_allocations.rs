//! Isolate the active owner wrapper from sockets/AI so its actual heap traffic
//! can be attributed without a wall-time acceptance threshold.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use rules::{
    blocks::MekKind,
    ids::ENERGIZED_SMELTER,
    items::I_DUST_IRON,
    machines::{Machine, Slot},
    stack::ItemStack,
};
use server::machines::{Budget, Config, MachineRuntime, machine_component};
use telemetry::AllocationScope;
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator =
    telemetry::CountingAllocator::new(std::alloc::System);
#[test]
fn active_machine_overlay_storage_is_bounded_and_preserves_all_2048_results() {
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for z in 0..2 {
        for x in 0..4 {
            tx.create_page(
                PageId {
                    space: SpaceId(0),
                    x,
                    y: 0,
                    z,
                },
                vec![Cell::AIR; 4096],
            );
        }
    }
    for i in 0..2048 {
        let p = BlockPos {
            space: SpaceId(0),
            x: (i % 64) as i32,
            y: 1,
            z: (i / 64) as i32,
        };
        let mut m = Machine::new(MekKind::Smelter);
        m.energy = 10_000;
        m.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
        tx.set_cell(p, Cell::from_parts(ENERGIZED_SMELTER, 2));
        tx.set_component(
            ComponentId(i + 1),
            machine_component(p, &m),
            ChannelMask::CONTENTS,
        );
    }
    world.commit(tx).unwrap();
    let mut owner = MachineRuntime::new(Config::default()).unwrap();
    let d = owner
        .extend_registration(
            owner.noop_delta().unwrap(),
            &world,
            (1..=2048).map(ComponentId),
            Tick(0),
        )
        .unwrap();
    owner.acknowledge(d).unwrap();
    let scope = AllocationScope::begin().unwrap();
    let plan = owner.plan_tick(&world, Tick(1), Budget::default()).unwrap();
    let allocations = scope.finish();
    assert_eq!(plan.work.machines, 2048);
    assert_eq!(plan.work.recipe_ticks, 2048);
    assert_eq!(plan.transactions.len(), 1);
    // Includes decoding, event-local overlays, merging and transaction encoding.
    // The former inline BTreeMap values allocated 181,425,936 bytes here.
    assert!(
        allocations.allocated_bytes < 32 * 1024 * 1024,
        "{allocations:?}"
    );
    for id in 1..=2048 {
        let original = server::machines::read_machine(&world, ComponentId(id)).unwrap();
        assert_eq!((original.energy, original.progress), (10_000, 0));
    }
    owner.validate(&world, &plan).unwrap();
    for transaction in plan.transactions {
        world.commit(transaction).unwrap();
    }
    owner.acknowledge(plan.delta).unwrap();
    for id in 1..=2048 {
        let changed = server::machines::read_machine(&world, ComponentId(id)).unwrap();
        assert_eq!((changed.energy, changed.progress), (9_950, 1));
        assert_eq!(changed.slots[0].stack.count(), 1);
        assert!(changed.slots[1].stack.is_empty());
    }
}
