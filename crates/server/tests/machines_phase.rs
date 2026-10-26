use core_types::{BlockPos, Cell, ComponentId, PAGE_VOLUME, PageId, SpaceId, Tick};
use rules::{
    blocks::MekKind,
    ids::*,
    items::*,
    machines::{Machine, Slot},
    stack::ItemStack,
};
use server::machines::*;
use server::survival;
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);

fn position(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn scene() -> World {
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
    world
}
fn add(world: &mut World, id: u64, anchor: BlockPos, block: u16, machine: Machine) {
    let mut tx = Transaction::new(Tick(0), anchor.space);
    tx.set_cell(anchor, Cell::from_parts(block, 3));
    tx.set_component(
        ComponentId(id),
        machine_component(anchor, &machine),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
}
fn commit(world: &mut World, runtime: &mut MachineRuntime, plan: PhasePlan) {
    runtime.validate(world, &plan).unwrap();
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    runtime.acknowledge(plan.delta).unwrap();
}
#[test]
fn real_world_smelter_finishes_exactly_on_tick200() {
    let mut world = scene();
    let mut machine = Machine::new(MekKind::Smelter);
    machine.energy = 10_000;
    machine.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
    add(&mut world, 1, position(1, 1, 1), ENERGIZED_SMELTER, machine);
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let delta = runtime
        .plan_register(&world, ComponentId(1), Tick(1))
        .unwrap();
    runtime.acknowledge(delta).unwrap();
    for tick in 1..=200 {
        let plan = runtime
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        assert_eq!(plan.work.machines, 1);
        commit(&mut world, &mut runtime, plan);
        let machine = read_machine(&world, ComponentId(1)).unwrap();
        assert_eq!(machine.slots[1].stack.count(), u16::from(tick == 200));
    }
    assert_eq!(read_machine(&world, ComponentId(1)).unwrap().energy, 0);
}

fn ore_line(raw: u16, prefill: bool) -> (World, MachineRuntime, ComponentId) {
    let mut world = World::new(rules::properties, 10_000);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 6,
            z: 0,
        },
        vec![Cell::AIR; PAGE_VOLUME],
    );
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let mut cube = Machine::new(MekKind::Cube);
    cube.energy = cube.capacity();
    let cube_pos = position(5, 99, 5);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(cube_pos, Cell::from_parts(ENERGY_CUBE, 5));
    tx.set_component(
        ComponentId(1),
        machine_component(cube_pos, &cube),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    runtime
        .acknowledge(
            runtime
                .plan_register(&world, ComponentId(1), Tick(1))
                .unwrap(),
        )
        .unwrap();
    for (index, (kind, block)) in [
        (MekKind::Separator, ELECTROLYTIC_SEPARATOR),
        (MekKind::Purifier, PURIFICATION_CHAMBER),
        (MekKind::Crusher, CRUSHER),
        (MekKind::Enrich, ENRICHMENT_CHAMBER),
        (MekKind::Smelter, ENERGIZED_SMELTER),
    ]
    .into_iter()
    .enumerate()
    {
        let mut machine = Machine::new(kind);
        machine.eject = kind != MekKind::Separator;
        if prefill {
            machine.energy = machine.capacity();
        }
        if kind == MekKind::Separator {
            machine.tank = rules::machines::Tank {
                substance: rules::machine_recipes::SUB_WATER,
                amount: 12000,
            };
            machine.gas_mode[0] = rules::machines::GasMode::Excess;
        }
        if kind == MekKind::Purifier {
            machine.slots[0] = Slot::new(ItemStack::new(I_RAW_IRON, raw).unwrap());
            if prefill {
                machine.tank = rules::machines::Tank {
                    substance: rules::machine_recipes::SUB_OXYGEN,
                    amount: 210,
                };
            }
        }
        let id = ComponentId(index as u64 + 2);
        add(
            &mut world,
            id.0,
            position(index as i32 + 6, 100, 5),
            block,
            machine,
        );
        runtime
            .acknowledge(runtime.plan_register(&world, id, Tick(1)).unwrap())
            .unwrap();
    }
    let chest_pos = position(11, 100, 5);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(chest_pos, Cell::from_parts(CHEST, 3));
    tx.set_component(
        ComponentId(7),
        survival::chest_component(chest_pos, &rules::survival::furnace::Chest::default()),
        core_types::ChannelMask::CONTENTS,
    );
    for x in 6..=10 {
        tx.set_cell(
            position(x, 99, 5),
            Cell::from_parts(
                UNIVERSAL_CABLE,
                rules::arms::with_arms(0, rules::arms::arms_from_mask(63)),
            ),
        );
    }
    world.commit(tx).unwrap();
    for x in 6..=10 {
        runtime
            .register_transmitter_fixture(&world, position(x, 99, 5), Tick(1))
            .unwrap();
    }
    (world, runtime, ComponentId(7))
}
#[test]
fn ore_one_prefilled_matches_approved_world_golden837_and1035() {
    let (mut world, mut runtime, chest) = ore_line(1, true);
    let mut seen = 0;
    let mut got = Vec::new();
    for tick in 1..=1100 {
        let plan = runtime
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, plan);
        let chest =
            rules::survival::furnace::Chest::decode(&world.component(chest).unwrap().data.payload)
                .unwrap();
        let count = chest
            .slots()
            .iter()
            .filter(|s| s.item() == I_IRON_INGOT)
            .map(|s| s.count())
            .sum::<u16>();
        while seen < count {
            got.push(tick);
            seen += 1;
        }
    }
    assert_eq!(got, [837, 1035]);
}

#[test]
fn ore_ten_world_chain_prefilled_last4632_and_cold_before4700() {
    for prefill in [true, false] {
        let (mut world, mut runtime, chest_id) = ore_line(10, prefill);
        let mut seen = 0;
        let mut last = 0;
        for tick in 1..=4700 {
            let plan = runtime
                .plan_tick(&world, Tick(tick), Budget::default())
                .unwrap();
            commit(&mut world, &mut runtime, plan);
            let chest = rules::survival::furnace::Chest::decode(
                &world.component(chest_id).unwrap().data.payload,
            )
            .unwrap();
            let count = chest
                .slots()
                .iter()
                .filter(|stack| stack.item() == I_IRON_INGOT)
                .map(|s| s.count())
                .sum::<u16>();
            if count > seen {
                seen = count;
                last = tick;
            }
            if seen == 20 {
                break;
            }
        }
        assert_eq!(seen, 20, "prefill{prefill}");
        if prefill {
            assert_eq!(last, 4632);
        } else {
            assert!(last < 4700, "cold{last}");
        }
        let purifier = read_machine(&world, ComponentId(3)).unwrap();
        assert!(purifier.slots[0].stack.is_empty());
    }
}

#[test]
fn ten_thousand_actual_world_machine_components_add_zero_idle_work_or_allocations() {
    let mut world = World::new(rules::properties, 10_000);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 0..10 {
        for z in 0..4 {
            tx.create_page(
                PageId {
                    space: SpaceId(0),
                    x,
                    y: 6,
                    z,
                },
                vec![Cell::AIR; PAGE_VOLUME],
            );
        }
    }
    let mut metadata = Vec::with_capacity(10_000);
    for id in 0..10_000u64 {
        let anchor = position((id % 160) as i32, 100, (id / 160) as i32);
        tx.set_cell(anchor, Cell::from_parts(ENERGIZED_SMELTER, 3));
        tx.set_component(
            ComponentId(id),
            machine_component(anchor, &Machine::new(MekKind::Smelter)),
            core_types::ChannelMask::CONTENTS,
        );
        metadata.push(MachineMeta {
            id: ComponentId(id),
            anchor,
            order: id,
            last_tick: Tick(0),
            active: false,
        });
    }
    world.commit(tx).unwrap();
    assert_eq!(world.components().count(), 10_000);
    let mut sleeping = MachineRuntime::restore(
        Config::default(),
        &RuntimeSnapshot {
            owner: OwnerSnapshot::default(),
            revision: 0,
            next_order: 10_000,
            machines: metadata,
            events: Vec::new(),
            grids: Vec::new(),
            network_orders: Vec::new(),
            producers: Vec::new(),
            configs: Vec::new(),
            transport: transport::TransportSnapshot::default(),
            topology_queue: server::topology_queue::Snapshot::default(),
        },
    )
    .unwrap();
    let mut empty = MachineRuntime::new(Config::default()).unwrap();
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(11));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for tick in 1..=10_000 {
        let a = empty
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        let b = sleeping
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        assert_eq!(a.work, b.work);
        assert_eq!(b.work, Work::default());
        let _ = black_box(b);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}

#[test]
fn machine_runtime_snapshot_and_delta_keep_exact_phase_order_and_reject_replay() {
    let (mut world, mut runtime, _) = ore_line(10, true);
    for tick in 1..=311 {
        let plan = runtime
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, plan);
    }
    let before = runtime.snapshot();
    let snapshot = RuntimeSnapshot::decode(&before.encode().unwrap(), Config::default()).unwrap();
    let mut restored = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    assert_eq!(before, restored.snapshot());
    let plan = runtime
        .plan_tick(&world, Tick(312), Budget::default())
        .unwrap();
    let bytes = plan.delta.encode().unwrap();
    let decoded = RuntimeDelta::decode(&bytes, Config::default()).unwrap();
    restored.validate_delta(&decoded).unwrap();
    restored.acknowledge(decoded).unwrap();
    commit(&mut world, &mut runtime, plan);
    assert_eq!(runtime.snapshot(), restored.snapshot());
    assert_eq!(
        restored.acknowledge(RuntimeDelta::decode(&bytes, Config::default()).unwrap()),
        Err(MachineError::Stale)
    );
    let mut corrupt = before.encode().unwrap();
    corrupt.extend_from_slice(&[0]);
    assert!(RuntimeSnapshot::decode(&corrupt, Config::default()).is_err());
    let mut corrupt = bytes.clone();
    corrupt[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(RuntimeDelta::decode(&corrupt, Config::default()).is_err());
    let mut bad = before.clone();
    bad.events[0].order = u64::MAX;
    assert!(MachineRuntime::restore(Config::default(), &bad).is_err());
}

#[test]
fn invalid_late_reservoir_guard_rejects_before_metadata_changes() {
    let (world, mut runtime, _) = ore_line(1, true);
    let plan = runtime
        .plan_tick(&world, Tick(1), Budget::default())
        .unwrap();
    let before = runtime.snapshot();
    let mut delta = plan.delta;
    assert!(!delta.machines.is_empty());
    assert!(!delta.reservoirs.is_empty());
    delta.reservoirs.last_mut().unwrap().after.stored = u64::MAX;
    assert!(runtime.acknowledge(delta).is_err());
    assert_eq!(runtime.snapshot(), before);
}

#[test]
fn unknown_pump_source_defers_whole_event_without_drain_or_energy_loss() {
    let mut world = scene();
    let mut pump = Machine::new(MekKind::Pump);
    pump.energy = pump.capacity();
    pump.progress = 18;
    add(
        &mut world,
        1,
        position(15, 0, 1),
        ELECTRIC_PUMP,
        pump.clone(),
    );
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    runtime
        .acknowledge(
            runtime
                .plan_register(&world, ComponentId(1), Tick(1))
                .unwrap(),
        )
        .unwrap();
    let plan = runtime
        .plan_tick(&world, Tick(1), Budget::default())
        .unwrap();
    assert_eq!(plan.work.unknown, 1);
    assert!(plan.transactions.is_empty());
    runtime.acknowledge(plan.delta).unwrap();
    assert_eq!(read_machine(&world, ComponentId(1)).unwrap(), pump);
    assert_eq!(
        runtime.machine_meta(ComponentId(1)).unwrap().last_tick,
        Tick(0)
    );
    assert_eq!(
        runtime
            .pending(Event::Machine(ComponentId(1)))
            .unwrap()
            .first_due,
        Tick(1)
    );
}

#[test]
fn actual_store_kind4_checkpoint_and_wal_recovery_finish_same_ore4632_chain() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/test-machine-save");
    std::fs::create_dir_all(&root).unwrap();
    let directory = Directory(root.join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let (mut world, mut runtime, chest) = ore_line(10, true);
    for tick in 1..=311 {
        let plan = runtime
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, plan);
    }
    let (mut store, _) = save::Store::open(&directory.0).unwrap();
    store
        .checkpoint(&save::Snapshot {
            record_sequence: 0,
            server_tick: Tick(311),
            public_seed: 1,
            generator_version: 4,
            rules_version: 1,
            world: world.checkpoint().to_restore_state(),
            identities: Vec::new(),
            players: Vec::new(),
            dedup: Vec::new(),
            runtimes: vec![save::RuntimeRecord {
                kind: save::RUNTIME_MACHINES,
                payload: runtime.snapshot().encode().unwrap(),
            }],
        })
        .unwrap();
    for tick in 312..=400 {
        let before = runtime.snapshot();
        let plan = runtime
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        runtime.validate(&world, &plan).unwrap();
        assert_eq!(runtime.snapshot(), before);
        let mut batch = save::Batch::empty(Tick(tick));
        for tx in plan.transactions {
            batch.changes.push(world.commit(tx).unwrap());
        }
        batch.runtimes.push(save::RuntimeRecord {
            kind: save::RUNTIME_MACHINES,
            payload: plan.delta.encode().unwrap(),
        });
        store.append(&batch).unwrap();
        runtime.acknowledge(plan.delta).unwrap();
    }
    drop(store);
    let (_, recovery) = save::Store::open(&directory.0).unwrap();
    let snapshot = recovery.snapshot.unwrap();
    let mut recovered_world = World::restore(
        rules::properties,
        world::JournalLimits::default(),
        snapshot.world,
    )
    .unwrap();
    let machine_record = snapshot
        .runtimes
        .iter()
        .find(|record| record.kind == save::RUNTIME_MACHINES)
        .unwrap();
    let mut recovered_runtime = MachineRuntime::restore(
        Config::default(),
        &RuntimeSnapshot::decode(&machine_record.payload, Config::default()).unwrap(),
    )
    .unwrap();
    for (_, batch) in recovery.batches {
        for change in batch.changes {
            recovered_world.apply_recovered_change(&change).unwrap();
        }
        for record in batch.runtimes {
            assert_eq!(record.kind, save::RUNTIME_MACHINES);
            recovered_runtime
                .acknowledge(RuntimeDelta::decode(&record.payload, Config::default()).unwrap())
                .unwrap();
        }
    }
    assert_eq!(world.canonical_hash(), recovered_world.canonical_hash());
    assert_eq!(runtime.snapshot(), recovered_runtime.snapshot());
    let mut last = 0;
    for tick in 401..=4700 {
        let plan = recovered_runtime
            .plan_tick(&recovered_world, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut recovered_world, &mut recovered_runtime, plan);
        let chest = rules::survival::furnace::Chest::decode(
            &recovered_world.component(chest).unwrap().data.payload,
        )
        .unwrap();
        if chest
            .slots()
            .iter()
            .filter(|s| s.item() == I_IRON_INGOT)
            .map(|s| s.count())
            .sum::<u16>()
            == 20
        {
            last = tick;
            break;
        }
    }
    assert_eq!(last, 4632);
}

#[test]
fn live_connections_drive_real_energy_and_survive_exact_delta_codec() {
    let mut world = scene();
    let cube_pos = position(4, 4, 4);
    let cable = position(5, 4, 4);
    let target = position(5, 5, 4);
    let mut cube = Machine::new(MekKind::Cube);
    cube.energy = 100_000;
    add(&mut world, 1, cube_pos, ENERGY_CUBE, cube);
    add(
        &mut world,
        2,
        target,
        ENERGIZED_SMELTER,
        Machine::new(MekKind::Smelter),
    );
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(cube_pos, Cell::from_parts(ENERGY_CUBE, 5));
    tx.set_cell(cable, Cell::from_parts(UNIVERSAL_CABLE, 0));
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let plan = runtime
        .plan_topology(&world, &[cable], Tick(0), Budget::default())
        .unwrap();
    assert!(runtime.network_at(cable).is_none());
    let bytes = plan.delta.encode().unwrap();
    let decoded = RuntimeDelta::decode(&bytes, Config::default()).unwrap();
    runtime.validate_delta(&decoded).unwrap();
    commit(&mut world, &mut runtime, plan);
    let state = match world.read(cable) {
        world::CellRead::Known(c) => c.state(),
        _ => panic!(),
    };
    assert_eq!(rules::arms::arm(rules::arms::arms_of(state), 1), 1);
    assert_eq!(rules::arms::arm(rules::arms::arms_of(state), 4), 1);
    assert_eq!(runtime.transmitter_config(cable).unwrap().modes, 0);
    let plan = runtime
        .plan_tick(&world, Tick(1), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, plan);
    assert!(read_machine(&world, ComponentId(2)).unwrap().energy > 0);
    let snap = runtime.snapshot();
    let data = snap.encode().unwrap();
    let restored = RuntimeSnapshot::decode(&data, Config::default()).unwrap();
    assert_eq!(restored, snap);
    let mut replay = MachineRuntime::new(Config::default()).unwrap();
    replay.acknowledge(decoded).unwrap();
    assert!(replay.network_at(cable).is_some());
}

#[test]
fn live_center_joins_six_networks_and_none_mode_splits_without_losing_energy() {
    let mut world = scene();
    let center = position(8, 8, 8);
    let neighbors: Vec<_> = rules::machines::Face::ALL
        .into_iter()
        .map(|f| {
            let q = rules::machines::sides::offset([8, 8, 8], f).unwrap();
            position(q[0], q[1], q[2])
        })
        .collect();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for &p in &neighbors {
        tx.set_cell(p, Cell::from_parts(UNIVERSAL_CABLE, 0));
    }
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let plan = runtime
        .plan_topology(&world, &neighbors, Tick(0), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, plan);
    assert_eq!(runtime.snapshot().network_orders.len(), 6);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(center, Cell::from_parts(UNIVERSAL_CABLE, 0));
    world.commit(tx).unwrap();
    let plan = runtime
        .plan_topology(&world, &[center], Tick(0), Budget::default())
        .unwrap();
    assert_eq!(runtime.snapshot().network_orders.len(), 6);
    commit(&mut world, &mut runtime, plan);
    let key = runtime.network_at(center).unwrap();
    assert!(
        neighbors
            .iter()
            .all(|p| runtime.network_at(*p) == Some(key))
    );
    assert_eq!(runtime.snapshot().network_orders.len(), 1);
    let mut snapshot = runtime.snapshot();
    let network = snapshot.grids[0].1.slots[key.network.slot as usize]
        .network
        .as_mut()
        .unwrap();
    network.reservoir.stored = 6000;
    let mut runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let plan = runtime
        .plan_connections(
            &world,
            TransmitterConfig {
                position: center,
                block: UNIVERSAL_CABLE,
                modes: 0xfff,
                color: 0,
            },
            Tick(1),
            Budget::default(),
        )
        .unwrap();
    assert_eq!(plan.delta.topology.len(), 1);
    assert_eq!(plan.delta.topology[0].records.overflow, 0);
    commit(&mut world, &mut runtime, plan);
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.network_orders.len(), 7);
    assert_eq!(
        snapshot.grids[0]
            .1
            .slots
            .iter()
            .filter_map(|s| s.network.as_ref())
            .map(|n| n.reservoir.stored)
            .sum::<u64>(),
        6000
    );
    assert!(
        neighbors
            .iter()
            .all(|p| runtime.network_at(*p) != runtime.network_at(center))
    );
}

#[test]
fn removal_before_phase_retires_event_and_cached_endpoint_atomically() {
    let (mut world, mut runtime, _) = ore_line(10, true);
    let id = ComponentId(3);
    let anchor = runtime.machine_meta(id).unwrap().anchor;
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.remove_component(id, core_types::ChannelMask::CONTENTS)
        .set_cell(anchor, Cell::AIR);
    world.commit(tx).unwrap();
    let plan = runtime
        .plan_tick(&world, Tick(1), Budget::default())
        .unwrap();
    assert!(
        plan.delta
            .machines
            .iter()
            .any(|p| p.before.is_some_and(|m| m.id == id) && p.after.is_none())
    );
    commit(&mut world, &mut runtime, plan);
    assert!(runtime.machine_meta(id).is_none());
    assert!(runtime.pending(Event::Machine(id)).is_none());
    let snapshot = runtime.snapshot();
    assert!(snapshot.producers.iter().all(|(_, ids)| !ids.contains(&id)));
    assert!(snapshot.grids.iter().all(|(_, g)| g.slots.iter().all(|s| {
        s.network
            .as_ref()
            .is_none_or(|n| n.endpoints.iter().all(|e| e.owner != id.0))
    })));
    MachineRuntime::restore(Config::default(), &snapshot).unwrap();
}

#[test]
fn topology_guard_rejects_missing_metadata_duplicate_creation_order_and_late_tamper() {
    let mut world = scene();
    let p = position(5, 5, 5);
    let machine = position(5, 6, 5);
    add(
        &mut world,
        1,
        machine,
        ENERGIZED_SMELTER,
        Machine::new(MekKind::Smelter),
    );
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(p, Cell::from_parts(UNIVERSAL_CABLE, 0));
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let plan = runtime
        .plan_topology(&world, &[p], Tick(0), Budget::default())
        .unwrap();
    let before = runtime.snapshot();
    let mut invalid = plan.delta.clone();
    invalid.networks.clear();
    assert!(runtime.acknowledge(invalid).is_err());
    assert_eq!(runtime.snapshot(), before);
    let mut invalid = plan.delta.clone();
    invalid.networks[0].after.as_mut().unwrap().order = invalid.machines[0].after.unwrap().order;
    assert!(runtime.acknowledge(invalid).is_err());
    assert_eq!(runtime.snapshot(), before);
    let mut invalid = plan.delta.clone();
    invalid.topology[0].records.after_nodes[0].links ^= 1;
    assert!(runtime.acknowledge(invalid).is_err());
    assert_eq!(runtime.snapshot(), before);
    commit(&mut world, &mut runtime, plan);
}
#[test]
fn same_batch_immutable_charge_is_resolved_and_normalized_before_machine_energy_tick() {
    let mut world = scene();
    let payload = rules::item_payload::Payload::charge(180_000).unwrap();
    let typed = ItemStack::new(I_ENERGY_TABLET, 1)
        .unwrap()
        .with_payload(payload.id())
        .unwrap();
    let mut source = Machine::new(MekKind::Smelter);
    source.eject = true;
    source.sides = [rules::machines::sides::SideMode::Output; 6];
    source.slots[1] = Slot::new(typed);
    add(&mut world, 1, position(4, 4, 4), ENERGIZED_SMELTER, source);
    let mut cube = Machine::new(MekKind::Cube);
    cube.sides = [rules::machines::sides::SideMode::Both; 6];
    add(&mut world, 2, position(5, 4, 4), ENERGY_CUBE, cube);
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    for id in [ComponentId(1), ComponentId(2)] {
        runtime
            .acknowledge(runtime.plan_register(&world, id, Tick(1)).unwrap())
            .unwrap();
    }
    let ledger = rules::item_payload::Ledger::new(rules::item_payload::Config::default()).unwrap();
    let delta = ledger
        .extend(ledger.plan().unwrap(), [payload], [])
        .unwrap();
    let view = ledger.view(&delta).unwrap();
    let plan = runtime
        .plan_tick_with_payloads(&world, &view, Tick(1), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, plan);
    assert!(
        read_machine(&world, ComponentId(1)).unwrap().slots[1]
            .stack
            .is_empty()
    );
    let cube = read_machine(&world, ComponentId(2)).unwrap();
    assert_eq!(cube.energy + u64::from(cube.slots[0].charge), 180_000);
    assert!(cube.energy > 0);
    assert!(cube.slots[0].stack.payload().is_none());
    assert_eq!(cube.slots[0].stack.damage(), 0);
}
