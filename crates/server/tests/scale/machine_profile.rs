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
pub fn run() -> u64 {
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
    let started = std::time::Instant::now();
    let plan = owner.plan_tick(&world, Tick(1), Budget::default()).unwrap();
    let micros = started.elapsed().as_micros();
    let allocations = scope.finish();
    assert_eq!(plan.work.machines, 2048);
    assert_eq!(plan.work.recipe_ticks, 2048);
    assert_eq!(plan.transactions.len(), 1);
    println!(
        "machine_profile machines={} actual_allocated_bytes={} allocations={} charged_work_bytes={} micros_information_only={}",
        plan.work.machines,
        allocations.allocated_bytes,
        allocations.allocating_calls(),
        plan.work.bytes,
        micros
    );
    allocations.allocated_bytes
}
