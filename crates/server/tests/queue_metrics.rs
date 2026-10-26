//! Independent agenda byte/age accounting includes retries and real restoration.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use rules::{blocks::MekKind, ids::*, machines::Machine};
use server::machines::{Budget, Config, MachineRuntime, machine_component};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
#[test]
fn indexed_oldest_original_deadline_and_exact_event_bytes_survive_retry_and_restore() {
    let mut world = World::new(rules::properties, 1024);
    let page = PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let p = BlockPos {
        space: SpaceId(0),
        x: 15,
        y: 0,
        z: 1,
    };
    let wire = BlockPos { x: 14, ..p };
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page, vec![Cell::AIR; 4096]);
    tx.set_cell(p, Cell::from_parts(ELECTRIC_PUMP, 2));
    tx.set_cell(wire, Cell::from_parts(UNIVERSAL_CABLE, 0));
    tx.set_component(
        ComponentId(1),
        machine_component(p, &{
            let mut pump = Machine::new(MekKind::Pump);
            pump.energy = pump.capacity();
            pump.progress = 18;
            pump
        }),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut owner = MachineRuntime::new(Config::default()).unwrap();
    owner
        .register_transmitter_fixture(&world, wire, Tick(5))
        .unwrap();
    let delta = owner
        .extend_registration(
            owner.noop_delta().unwrap(),
            &world,
            [ComponentId(1)],
            Tick(0),
        )
        .unwrap();
    owner.acknowledge(delta).unwrap();
    assert_eq!(owner.machine_count(), 1);
    assert_eq!(owner.event_count(), 2);
    assert_eq!(owner.event_payload_bytes(), 70);
    assert_eq!(owner.oldest_overdue(Tick(100)), 99);
    let snapshot = owner.snapshot();
    let encoded = snapshot.encode().unwrap();
    let snapshot = server::machines::RuntimeSnapshot::decode(&encoded, Config::default()).unwrap();
    let mut owner = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    assert_eq!(owner.event_payload_bytes(), 70);
    assert_eq!(owner.oldest_overdue(Tick(100)), 99);
    let plan = owner
        .plan_tick(&world, Tick(100), Budget::default())
        .unwrap();
    assert!(plan.work.unknown > 0);
    owner.acknowledge(plan.delta).unwrap();
    assert_eq!(owner.event_count(), 1);
    assert_eq!(owner.event_payload_bytes(), 33);
    assert_eq!(
        owner.oldest_overdue(Tick(100)),
        99,
        "retry due must not reset original age"
    );
    let positive = AllocationScope::begin().unwrap();
    let v = Vec::<u8>::with_capacity(std::hint::black_box(9));
    std::hint::black_box(&v);
    assert!(positive.finish().allocated_bytes > 0);
    drop(v);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        std::hint::black_box((
            owner.machine_count(),
            owner.event_count(),
            owner.event_payload_bytes(),
            owner.oldest_overdue(Tick(100)),
        ));
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
