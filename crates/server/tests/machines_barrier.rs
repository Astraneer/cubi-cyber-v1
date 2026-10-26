//! Budget partition and recovery must not change a logical machine tick.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use rules::{
    blocks::MekKind,
    ids::*,
    items::I_DUST_IRON,
    machines::{Machine, Slot},
    stack::ItemStack,
};
use server::machines::*;
use world::{Transaction, World};

fn smelters(count: u64) -> (World, MachineRuntime) {
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for z in 0..((count.div_ceil(64) as i32).div_euclid(16) + 1) {
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
    for i in 0..count {
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
    let mut r = MachineRuntime::new(Config::default()).unwrap();
    let delta = r
        .extend_registration(
            r.noop_delta().unwrap(),
            &world,
            (1..=count).map(ComponentId),
            Tick(0),
        )
        .unwrap();
    r.acknowledge(delta).unwrap();
    (world, r)
}
fn commit(w: &mut World, r: &mut MachineRuntime, p: PhasePlan) {
    r.validate(w, &p).unwrap();
    let encoded = p.delta.encode().unwrap();
    let replay = RuntimeDelta::decode(&encoded, Config::default()).unwrap();
    for tx in p.transactions {
        w.commit(tx).unwrap();
    }
    r.acknowledge(replay).unwrap();
}
fn recover(r: &MachineRuntime) -> MachineRuntime {
    let bytes = r.snapshot().encode().unwrap();
    let image = RuntimeSnapshot::decode(&bytes, Config::default()).unwrap();
    MachineRuntime::restore(Config::default(), &image).unwrap()
}
fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn resource_and_transit() -> (World, MachineRuntime) {
    use rules::{arms, items::I_IRON_INGOT, survival::furnace::Chest};
    let mut w = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p(0, 0, 0).page(), vec![Cell::AIR; 4096]);
    let mut cube = Machine::new(MekKind::Cube);
    cube.energy = 50_000;
    tx.set_cell(p(1, 2, 8), Cell::from_parts(ENERGY_CUBE, 5));
    tx.set_component(
        ComponentId(1),
        machine_component(p(1, 2, 8), &cube),
        ChannelMask::CONTENTS,
    );
    for id in 2..=6 {
        let anchor = if id == 2 {
            p(3, 2, 8)
        } else {
            p(id as i32, 5, 8)
        };
        let mut m = Machine::new(MekKind::Smelter);
        m.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
        if id > 2 {
            m.energy = 10_000;
        }
        tx.set_cell(anchor, Cell::from_parts(ENERGIZED_SMELTER, 2));
        tx.set_component(
            ComponentId(id),
            machine_component(anchor, &m),
            ChannelMask::CONTENTS,
        );
    }
    tx.set_cell(
        p(2, 2, 8),
        Cell::from_parts(
            UNIVERSAL_CABLE,
            arms::with_arms(0, arms::arms_from_mask(63)),
        ),
    );
    let mut source = Chest::default();
    source
        .set_trusted(0, ItemStack::new(I_IRON_INGOT, 3).unwrap())
        .unwrap();
    for (id, anchor, chest) in [(7, p(8, 2, 2), source), (8, p(11, 2, 2), Chest::default())] {
        tx.set_cell(anchor, Cell::from_parts(CHEST, 0));
        tx.set_component(
            ComponentId(id),
            server::survival::chest_component(anchor, &chest),
            ChannelMask::CONTENTS,
        );
    }
    for x in 9..=10 {
        tx.set_cell(p(x, 2, 2), Cell::from_parts(LOGISTICAL_TRANSPORTER, 0));
    }
    w.commit(tx).unwrap();
    let mut r = MachineRuntime::new(Config::default()).unwrap();
    let d = r
        .extend_registration(
            r.noop_delta().unwrap(),
            &w,
            (1..=6).map(ComponentId),
            Tick(0),
        )
        .unwrap();
    r.acknowledge(d).unwrap();
    r.register_transmitter_fixture(&w, p(2, 2, 8), Tick(1))
        .unwrap();
    let plan = r
        .plan_topology(&w, &[p(9, 2, 2), p(10, 2, 2)], Tick(0), Budget::default())
        .unwrap();
    commit(&mut w, &mut r, plan);
    let plan = r
        .plan_connections(
            &w,
            TransmitterConfig {
                position: p(9, 2, 2),
                block: LOGISTICAL_TRANSPORTER,
                modes: 2 << (2 * 4),
                color: 0,
            },
            Tick(0),
            Budget::default(),
        )
        .unwrap();
    commit(&mut w, &mut r, plan);
    (w, r)
}
#[test]
fn one_event_batches_and_every_phase_restart_match_whole_tick_energy_pull_and_packet_results() {
    let (mut whole_w, mut whole) = resource_and_transit();
    let (mut split_w, mut split) = resource_and_transit();
    for tick in 1..=42 {
        let full = whole
            .plan_tick(&whole_w, Tick(tick), Budget::default())
            .unwrap();
        assert!(full.owner_complete());
        commit(&mut whole_w, &mut whole, full);
        for batch in 0..24 {
            let plan = split
                .plan_tick(
                    &split_w,
                    Tick(tick),
                    Budget {
                        events: 1,
                        ..Budget::default()
                    },
                )
                .unwrap();
            assert!(plan.work.events <= 1 && plan.work.prepared <= 1);
            if plan.work.networks > 0 {
                for id in 1..=6 {
                    assert_eq!(
                        split.machine_meta(ComponentId(id)).unwrap().last_tick,
                        Tick(tick)
                    );
                }
            }
            if plan.work.pulls > 0 || plan.work.packets > 0 {
                assert!(
                    split
                        .pending(Event::Network(split.network_at(p(2, 2, 8)).unwrap()))
                        .is_none_or(|p| p.due > Tick(tick))
                );
            }
            let complete = plan.owner_complete();
            commit(&mut split_w, &mut split, plan);
            split = recover(&split);
            if complete {
                break;
            }
            assert!(batch < 23, "stalled cohort at tick {tick}");
        }
        assert!(!split.owner_pending());
        for id in 1..=6 {
            assert_eq!(
                read_machine(&split_w, ComponentId(id)).unwrap(),
                read_machine(&whole_w, ComponentId(id)).unwrap(),
                "tick{tick},id{id}"
            );
        }
        assert_eq!(
            split.reservoir_at(p(2, 2, 8)),
            whole.reservoir_at(p(2, 2, 8))
        );
        assert_eq!(
            split.snapshot().transport,
            whole.snapshot().transport,
            "tick{tick}"
        );
        assert_eq!(
            split.snapshot().events,
            whole.snapshot().events,
            "tick{tick}"
        );
        for id in [7, 8] {
            assert_eq!(
                split_w.component(ComponentId(id)).unwrap().data.payload,
                whole_w.component(ComponentId(id)).unwrap().data.payload
            );
        }
    }
}
#[test]
fn ten_thousand_adjacent_active_machines_all_finish_each_tick_without_wake_starvation() {
    let (mut w, mut r) = smelters(10_000);
    for tick in 1..=3 {
        let mut batches = 0;
        let mut machines = 0;
        let mut prepared = 0;
        loop {
            batches += 1;
            assert!(batches <= 12, "finite cohort failed to finish tick {tick}");
            let p = r.plan_tick(&w, Tick(tick), Budget::default()).unwrap();
            assert!(p.work.events <= 2048 && p.work.prepared <= 2048);
            assert_eq!(p.work.unknown, 0);
            machines += p.work.machines;
            prepared += p.work.prepared;
            let complete = p.owner_complete();
            commit(&mut w, &mut r, p);
            // Restart during preparation and again after partial execution.
            if batches == 2 || batches == 6 {
                r = recover(&r);
            }
            if complete {
                break;
            }
            assert!(r.owner_pending());
            assert_eq!(r.owner_tick(), Some(Tick(tick)));
            assert!(matches!(
                r.plan_tick(&w, Tick(tick + 1), Budget::default()),
                Err(MachineError::Stale)
            ));
        }
        assert_eq!(machines, 10_000);
        assert_eq!(prepared, 10_000);
        for id in 1..=10_000 {
            let m = read_machine(&w, ComponentId(id)).unwrap();
            assert_eq!(
                (m.energy, m.progress),
                (10_000 - 50 * tick, tick as u32),
                "machine {id}"
            );
            assert_eq!(
                r.machine_meta(ComponentId(id)).unwrap().last_tick,
                Tick(tick)
            );
        }
        assert!(!r.owner_pending());
    }
}
#[test]
fn finite_cohort_replay_guards_and_legacy_idle_encoding_are_exact() {
    let (mut w, mut r) = smelters(7);
    let idle = r.snapshot().encode().unwrap();
    assert!(!idle.windows(4).any(|b| b == b"XPH1"));
    let p = r
        .plan_tick(
            &w,
            Tick(1),
            Budget {
                events: 2,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!((p.work.prepared, p.work.events), (2, 0));
    assert!(!p.owner_complete());
    let stale = p.delta.clone();
    commit(&mut w, &mut r, p);
    assert!(r.validate_delta(&stale).is_err());
    let snapshot = r.snapshot();
    assert_eq!(snapshot.owner.state.unwrap().stage, OwnerStage::Prepare);
    assert_eq!(snapshot.owner.ready.len(), 2);
    let bytes = snapshot.encode().unwrap();
    for n in bytes.len() - 5..bytes.len() {
        assert!(RuntimeSnapshot::decode(&bytes[..n], Config::default()).is_err());
    }
    let mut bad = snapshot.clone();
    bad.owner.ready[0].order += 1;
    assert!(MachineRuntime::restore(Config::default(), &bad).is_err());
    let mut missing = snapshot.clone();
    missing.owner.ready.remove(0);
    assert!(MachineRuntime::restore(Config::default(), &missing).is_err());
    let mut bad = snapshot;
    bad.owner.state.as_mut().unwrap().stage = OwnerStage::Transit;
    assert!(MachineRuntime::restore(Config::default(), &bad).is_err());
    r = recover(&r);
    for _ in 0..8 {
        let p = r
            .plan_tick(
                &w,
                Tick(1),
                Budget {
                    events: 2,
                    ..Budget::default()
                },
            )
            .unwrap();
        let complete = p.owner_complete();
        commit(&mut w, &mut r, p);
        if complete {
            break;
        }
    }
    assert!(!r.owner_pending());
    assert!(
        !r.snapshot()
            .encode()
            .unwrap()
            .windows(4)
            .any(|b| b == b"XPH1")
    );
    for id in 1..=7 {
        assert_eq!(read_machine(&w, ComponentId(id)).unwrap().progress, 1);
    }
}

#[test]
fn zero_budget_cannot_declare_unexecuted_due_work_complete() {
    let (mut world, mut r) = smelters(1);
    let plan = r
        .plan_tick(
            &world,
            Tick(1),
            Budget {
                events: 0,
                reads: 0,
                bytes: 0,
                endpoints: 0,
                route_nodes: 0,
                item_spawns: 0,
            },
        )
        .unwrap();
    assert_eq!(plan.work, Work::default());
    assert!(plan.transactions.is_empty());
    assert!(!plan.owner_complete());
    commit(&mut world, &mut r, plan);
    assert_eq!(r.owner_stage(), Some(OwnerStage::Prepare));
    assert_eq!(read_machine(&world, ComponentId(1)).unwrap().progress, 0);
    r = recover(&r);
    let plan = r.plan_tick(&world, Tick(1), Budget::default()).unwrap();
    assert!(plan.owner_complete());
    commit(&mut world, &mut r, plan);
    assert_eq!(read_machine(&world, ComponentId(1)).unwrap().progress, 1);
    let mut empty = MachineRuntime::new(Config::default()).unwrap();
    let plan = empty
        .plan_tick(
            &world,
            Tick(1),
            Budget {
                events: 0,
                ..Budget::default()
            },
        )
        .unwrap();
    assert!(plan.owner_complete() && plan.delta.is_empty());
}

#[test]
fn indivisible_route_over_production_bound_reports_capacity_without_extraction_or_infinite_retry() {
    use rules::{arms, items::I_IRON_INGOT, survival::furnace::Chest};
    use server::machines::transport::{ClockRecord, PageClock, Pull};
    let mut w = World::new(rules::properties, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for z in 0..5 {
        for x in 0..5 {
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
    let normal = arms::arms_from_mask(63);
    let start = p(33, 2, 33);
    for z in 1..=65 {
        for x in 1..=65 {
            let at = p(x, 2, z);
            let packed = if at == start {
                normal | (3 << 2)
            } else {
                normal
            };
            tx.set_cell(
                at,
                Cell::from_parts(LOGISTICAL_TRANSPORTER, arms::with_arms(0, packed)),
            );
        }
    }
    let source = p(33, 3, 33);
    let mut chest = Chest::default();
    chest
        .set_trusted(0, ItemStack::new(I_IRON_INGOT, 3).unwrap())
        .unwrap();
    tx.set_cell(source, Cell::from_parts(CHEST, 0));
    tx.set_component(
        ComponentId(1),
        server::survival::chest_component(source, &chest),
        ChannelMask::CONTENTS,
    );
    w.commit(tx).unwrap();
    let mut image = MachineRuntime::new(Config::default()).unwrap().snapshot();
    image.configs.push(TransmitterConfig {
        position: start,
        block: LOGISTICAL_TRANSPORTER,
        modes: 2 << 2,
        color: 0,
    });
    image.transport.next_pull_order = 1;
    image.transport.pulls.push(Pull {
        position: start,
        order: 0,
        failures: 0,
        due: Tick(1),
        first_due: Tick(1),
    });
    image.transport.clocks.push(ClockRecord {
        page: start.page(),
        clock: PageClock::default(),
    });
    let mut r = MachineRuntime::restore(Config::default(), &image).unwrap();
    let before_world = w.canonical_hash();
    let before = r.snapshot();
    assert!(matches!(
        r.plan_tick(&w, Tick(1), Budget::default()),
        Err(MachineError::RouteCapacity)
    ));
    assert_eq!(r.snapshot(), before);
    assert_eq!(w.canonical_hash(), before_world);
    assert_eq!(
        Chest::decode(&w.component(ComponentId(1)).unwrap().data.payload).unwrap(),
        chest
    );
    let partial = r
        .plan_tick(
            &w,
            Tick(1),
            Budget {
                route_nodes: 1024,
                ..Budget::default()
            },
        )
        .unwrap();
    assert!(!partial.owner_complete());
    assert_eq!(partial.work.route_nodes, 1024);
    assert!(partial.transactions.is_empty());
    commit(&mut w, &mut r, partial);
    r = recover(&r);
    assert!(matches!(
        r.plan_tick(&w, Tick(1), Budget::default()),
        Err(MachineError::RouteCapacity)
    ));
    assert_eq!(r.packet_count(), 0);
    assert_eq!(w.canonical_hash(), before_world);
}

#[test]
fn real_store_reopens_each_prepare_machine_resource_and_transit_microbatch() {
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let d = Directory(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("machine-phase-wal-{}", std::process::id())),
    );
    std::fs::create_dir_all(&d.0).unwrap();
    let (mut w, mut r) = resource_and_transit();
    let (mut expected_w, mut expected) = resource_and_transit();
    let full = expected
        .plan_tick(&expected_w, Tick(1), Budget::default())
        .unwrap();
    commit(&mut expected_w, &mut expected, full);
    let (mut store, _) = save::Store::open(&d.0).unwrap();
    store
        .checkpoint(&save::Snapshot {
            record_sequence: 0,
            server_tick: Tick(0),
            public_seed: 1,
            generator_version: 4,
            rules_version: 1,
            world: w.checkpoint().to_restore_state(),
            identities: Vec::new(),
            players: Vec::new(),
            dedup: Vec::new(),
            runtimes: vec![save::RuntimeRecord {
                kind: save::RUNTIME_MACHINES,
                payload: r.snapshot().encode().unwrap(),
            }],
        })
        .unwrap();
    let mut stages = std::collections::BTreeSet::new();
    for i in 0..24 {
        let plan = r
            .plan_tick(
                &w,
                Tick(1),
                Budget {
                    events: 1,
                    ..Budget::default()
                },
            )
            .unwrap();
        let done = plan.owner_complete();
        r.validate(&w, &plan).unwrap();
        let mut batch = save::Batch::empty(Tick(1));
        for tx in plan.transactions {
            batch.changes.push(w.commit(tx).unwrap());
        }
        batch.runtimes.push(save::RuntimeRecord {
            kind: save::RUNTIME_MACHINES,
            payload: plan.delta.encode().unwrap(),
        });
        store.append(&batch).unwrap();
        r.acknowledge(plan.delta).unwrap();
        let before = r.snapshot();
        if let Some(stage) = r.owner_stage() {
            stages.insert(stage);
        }
        drop(store);
        let (reopened, recovery) = save::Store::open(&d.0).unwrap();
        store = reopened;
        let image = recovery.snapshot.unwrap();
        w = World::restore(
            rules::properties,
            world::JournalLimits::default(),
            image.world,
        )
        .unwrap();
        r = MachineRuntime::restore(
            Config::default(),
            &RuntimeSnapshot::decode(&image.runtimes[0].payload, Config::default()).unwrap(),
        )
        .unwrap();
        for (_, batch) in recovery.batches {
            for change in batch.changes {
                w.apply_recovered_change(&change).unwrap();
            }
            for runtime in batch.runtimes {
                r.acknowledge(RuntimeDelta::decode(&runtime.payload, Config::default()).unwrap())
                    .unwrap();
            }
        }
        assert_eq!(r.snapshot(), before);
        if done {
            break;
        }
        assert!(i < 23);
    }
    assert_eq!(
        stages,
        std::collections::BTreeSet::from([
            OwnerStage::Prepare,
            OwnerStage::Machines,
            OwnerStage::Resources,
            OwnerStage::Transit
        ])
    );
    assert!(!r.owner_pending());
    for id in 1..=6 {
        assert_eq!(
            read_machine(&w, ComponentId(id)).unwrap(),
            read_machine(&expected_w, ComponentId(id)).unwrap()
        );
    }
    assert_eq!(
        r.reservoir_at(p(2, 2, 8)),
        expected.reservoir_at(p(2, 2, 8))
    );
    assert_eq!(r.snapshot().transport, expected.snapshot().transport);
}
