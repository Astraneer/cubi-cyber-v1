use core_types::{BlockPos, Cell, ComponentId, PAGE_VOLUME, PageId, SpaceId, Tick};
use rules::{blocks::MekKind, ids::*, machines::Machine};
use server::machines::*;
use world::{Transaction, World};
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 2,
        z: 2,
    }
}
fn scene() -> World {
    let mut world = World::new(rules::properties, 1000);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p(0).page(), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(tx).unwrap();
    world
}
fn commit(world: &mut World, runtime: &mut MachineRuntime, plan: PhasePlan) {
    runtime.validate(world, &plan).unwrap();
    assert!(plan.payloads.is_empty() && plan.item_spawns.is_empty());
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    runtime.acknowledge(plan.delta).unwrap();
}
#[test]
fn accepted_dirty_position_retry_age_and_completion_survive_kind4_snapshot() {
    let mut world = scene();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let position = p(15);
    let plan = runtime
        .plan_tick_with_topology(
            &world,
            PhaseContext {
                payloads: None,
                active: None,
                activity: &[],
            },
            &[position],
            Tick(1),
            Budget {
                events: 0,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!(plan.work, Work::default());
    assert_eq!(runtime.pending_topology().len(), 0);
    commit(&mut world, &mut runtime, plan);
    let first = runtime.pending_topology().get(position).unwrap();
    assert_eq!(first.first_due, Tick(1));
    assert!(matches!(
        runtime.plan_topology(&world, &[position], Tick(2), Budget::default()),
        Err(MachineError::Unknown)
    ));
    let retry = runtime
        .extend_topology_retry(runtime.noop_delta().unwrap(), [position], Tick(3))
        .unwrap();
    let bytes = retry.encode().unwrap();
    runtime
        .acknowledge(RuntimeDelta::decode(&bytes, Config::default()).unwrap())
        .unwrap();
    let snapshot =
        RuntimeSnapshot::decode(&runtime.snapshot().encode().unwrap(), Config::default()).unwrap();
    runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let pending = runtime.pending_topology().get(position).unwrap();
    assert_eq!(pending.first_due, first.first_due);
    assert_eq!(pending.order, first.order);
    assert_eq!(pending.due, Tick(3));
    assert_eq!(runtime.pending_topology().due(Tick(2), 1).count(), 0);
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 1,
            y: 0,
            z: 0,
        },
        vec![Cell::AIR; PAGE_VOLUME],
    );
    world.commit(tx).unwrap();
    let mut plan = runtime
        .plan_topology(&world, &[position], Tick(3), Budget::default())
        .unwrap();
    plan.delta = runtime
        .extend_topology_completion(plan.delta, [position])
        .unwrap();
    let replay = plan.delta.clone();
    commit(&mut world, &mut runtime, plan);
    assert_eq!(runtime.pending_topology().len(), 0);
    assert!(runtime.acknowledge(replay).is_err());
    assert!(
        RuntimeSnapshot::decode(&runtime.snapshot().encode().unwrap(), Config::default())
            .unwrap()
            .topology_queue
            .entries
            .is_empty()
    );
}
fn add(world: &mut World, id: u64, x: i32, block: u16, kind: MekKind, energy: u64) {
    let mut m = Machine::new(kind);
    m.energy = energy;
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(
        p(x),
        Cell::from_parts(block, if kind == MekKind::Cube { 5 } else { 3 }),
    )
    .set_component(
        ComponentId(id),
        machine_component(p(x), &m),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
}
#[test]
fn same_wal_dirty_cut_blocks_stale_flow_but_unrelated_network_keeps_running() {
    let mut world = scene();
    add(&mut world, 1, 1, ENERGY_CUBE, MekKind::Cube, 10_000);
    add(&mut world, 2, 4, ENERGIZED_SMELTER, MekKind::Smelter, 0);
    add(&mut world, 3, 7, ENERGY_CUBE, MekKind::Cube, 3000);
    add(&mut world, 4, 9, ENERGIZED_SMELTER, MekKind::Smelter, 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in [2, 3, 8] {
        tx.set_cell(p(x), Cell::from_parts(UNIVERSAL_CABLE, 0));
    }
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let plan = runtime
        .plan_topology(&world, &[p(2), p(3), p(8)], Tick(0), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, plan);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(p(3), Cell::AIR);
    world.commit(tx).unwrap();
    let plan = runtime
        .plan_tick_with_topology(
            &world,
            PhaseContext {
                payloads: None,
                active: None,
                activity: &[],
            },
            &[p(3)],
            Tick(1),
            Budget::default(),
        )
        .unwrap();
    assert!(plan.work.deferred >= 1);
    commit(&mut world, &mut runtime, plan);
    assert_eq!(read_machine(&world, ComponentId(1)).unwrap().energy, 10_000);
    assert_eq!(read_machine(&world, ComponentId(2)).unwrap().energy, 0);
    assert_eq!(read_machine(&world, ComponentId(4)).unwrap().energy, 3000);
    runtime = MachineRuntime::restore(
        Config::default(),
        &RuntimeSnapshot::decode(&runtime.snapshot().encode().unwrap(), Config::default()).unwrap(),
    )
    .unwrap();
    let plan = runtime
        .plan_tick(&world, Tick(2), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, plan);
    assert_eq!(read_machine(&world, ComponentId(2)).unwrap().energy, 0);
    let mut plan = runtime
        .plan_topology(&world, &[p(3)], Tick(3), Budget::default())
        .unwrap();
    plan.delta = runtime
        .extend_topology_completion(plan.delta, [p(3)])
        .unwrap();
    commit(&mut world, &mut runtime, plan);
    let plan = runtime
        .plan_tick(&world, Tick(4), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, plan);
    assert_eq!(read_machine(&world, ComponentId(2)).unwrap().energy, 0);
    assert_eq!(
        read_machine(&world, ComponentId(1)).unwrap().energy
            + runtime.reservoir_at(p(2)).unwrap().stored,
        10_000
    );
}
