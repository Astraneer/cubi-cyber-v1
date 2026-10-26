use core_types::{BlockPos, ComponentId, SpaceId, Tick};
use rules::{items::I_IRON_INGOT, stack::ItemStack};
use server::machines::{transport::*, *};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 2,
        z: 2,
    }
}
#[test]
fn ten_thousand_packets_pause_with_one_small_wal_row_and_zero_idle_work() {
    let mut snapshot = MachineRuntime::new(Config::default()).unwrap().snapshot();
    snapshot.transport.next_packet = 10_001;
    snapshot.transport.clocks = vec![ClockRecord {
        page: p(2).page(),
        clock: PageClock::default(),
    }];
    for id in 1..=10_000 {
        snapshot.transport.packets.push(Packet {
            id,
            stack: ItemStack::new(I_IRON_INGOT, 1).unwrap(),
            color: 0,
            from: p(1),
            path: vec![p(2), p(3)],
            step: 0,
            progress: 5,
            goal: Goal::Destination,
            home: Some(p(1)),
            idle_direction: None,
            destination: Some(ComponentId(2)),
            last_tick: Tick(1),
            due: Tick(2),
            first_due: Tick(2),
        });
    }
    let mut runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let scope = AllocationScope::begin().unwrap();
    let delta = runtime
        .extend_page_activity(
            runtime.noop_delta().unwrap(),
            &[(p(2).page(), false)],
            Tick(2),
        )
        .unwrap();
    let bytes = delta.encode().unwrap();
    let counts = scope.finish();
    assert!(counts.allocated_bytes < 64 * 1024, "{counts:?}");
    assert!(bytes.len() < 512);
    let change = delta.transport.as_ref().unwrap();
    assert!(change.packets.is_empty());
    assert!(change.pulls.is_empty());
    assert_eq!(change.clocks.len(), 1);
    runtime.acknowledge(delta).unwrap();
    assert_eq!(runtime.packet_count(), 10_000);
    assert!(!runtime.transport_due(Tick(1_000_000)));
    let world = world::World::new(rules::properties, 1000);
    let inactive = |_: BlockPos| false;
    let scope = AllocationScope::begin().unwrap();
    for tick in 3..=10_003 {
        let plan = runtime
            .plan_tick_with_context(
                &world,
                PhaseContext {
                    payloads: None,
                    active: Some(&inactive),
                    activity: &[],
                },
                Tick(tick),
                Budget::default(),
            )
            .unwrap();
        assert_eq!(plan.work, Work::default());
        assert!(plan.delta.is_empty());
    }
    assert_eq!(scope.finish(), AllocationCounts::default());
}
#[test]
fn page_clock_rejects_invalid_offsets_and_preserves_active_time_across_two_pauses() {
    let clock = PageClock::default().change(false, Tick(6)).unwrap();
    assert_eq!(clock.local_now(Tick(90)).unwrap(), Tick(5));
    let clock = clock.change(true, Tick(100)).unwrap();
    assert_eq!(clock.offset, 94);
    assert_eq!(clock.local_now(Tick(100)).unwrap(), Tick(6));
    assert_eq!(clock.global_tick(Tick(7)).unwrap(), Tick(101));
    let clock = clock
        .change(false, Tick(102))
        .unwrap()
        .change(true, Tick(200))
        .unwrap();
    assert_eq!(clock.local_now(Tick(200)).unwrap(), Tick(8));
    assert!(
        PageClock {
            active: false,
            offset: 10,
            paused_since: Tick(5)
        }
        .validate()
        .is_err()
    );
    assert!(
        PageClock {
            active: true,
            offset: 0,
            paused_since: Tick(5)
        }
        .validate()
        .is_err()
    );
}

#[test]
fn machine_page_activity_looks_up_one_page_among_ten_thousand_cold_pages() {
    let mut snapshot = MachineRuntime::new(Config::default()).unwrap().snapshot();
    snapshot.next_order = 10_000;
    for id in 0..10_000u64 {
        snapshot.machines.push(MachineMeta {
            id: ComponentId(id),
            anchor: p((id as i32) * 16),
            order: id,
            last_tick: Tick(0),
            active: false,
        });
    }
    let mut runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let scope = AllocationScope::begin().unwrap();
    let delta = runtime
        .extend_machine_page_activity(
            runtime.noop_delta().unwrap(),
            &[(p(16).page(), true)],
            Tick(100),
        )
        .unwrap();
    let bytes = delta.encode().unwrap();
    let counts = scope.finish();
    assert!(counts.allocated_bytes < 64 * 1024, "{counts:?}");
    assert!(bytes.len() < 512);
    assert_eq!(delta.machines.len(), 1);
    assert_eq!(delta.queue.len(), 1);
    runtime.acknowledge(delta).unwrap();
    assert_eq!(
        runtime.machine_meta(ComponentId(1)).unwrap().last_tick,
        Tick(99)
    );
    assert_eq!(
        runtime.machine_meta(ComponentId(2)).unwrap().last_tick,
        Tick(0)
    );
    let pages = runtime.activity_pages().count();
    assert_eq!(pages, 10_000);
    let restored = MachineRuntime::restore(Config::default(), &runtime.snapshot()).unwrap();
    assert_eq!(restored.activity_pages().count(), pages);
    assert!(
        runtime
            .extend_machine_page_activity(
                runtime.noop_delta().unwrap(),
                &[(p(16).page(), false), (p(16).page(), true)],
                Tick(101)
            )
            .is_err()
    );
}

#[test]
fn recipe_freezes_before_sleep_tick_and_resumes_exact_progress_after_recovery() {
    use rules::{
        blocks::MekKind,
        ids::ENERGIZED_SMELTER,
        items::I_DUST_IRON,
        machines::{Machine, Slot},
    };
    let mut world = world::World::new(rules::properties, 1000);
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        p(2).page(),
        vec![core_types::Cell::AIR; core_types::PAGE_VOLUME],
    );
    let mut machine = Machine::new(MekKind::Smelter);
    machine.energy = 10_000;
    machine.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
    tx.set_cell(p(2), core_types::Cell::from_parts(ENERGIZED_SMELTER, 3));
    tx.set_component(
        ComponentId(1),
        machine_component(p(2), &machine),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    runtime
        .acknowledge(
            runtime
                .plan_register(&world, ComponentId(1), Tick(1))
                .unwrap(),
        )
        .unwrap();
    fn commit(world: &mut world::World, runtime: &mut MachineRuntime, plan: PhasePlan) {
        runtime.validate(world, &plan).unwrap();
        for tx in plan.transactions {
            world.commit(tx).unwrap();
        }
        runtime.acknowledge(plan.delta).unwrap();
    }
    for tick in 1..=5 {
        let plan = runtime
            .plan_tick(&world, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, plan);
    }
    let frozen = read_machine(&world, ComponentId(1)).unwrap();
    assert_eq!(frozen.progress, 5);
    assert_eq!(frozen.energy, 9750);
    let inactive = |_: BlockPos| false;
    let plan = runtime
        .plan_tick_with_context(
            &world,
            PhaseContext {
                payloads: None,
                active: Some(&inactive),
                activity: &[(p(2).page(), false)],
            },
            Tick(6),
            Budget::default(),
        )
        .unwrap();
    assert_eq!(plan.work, Work::default());
    assert!(plan.transactions.is_empty());
    commit(&mut world, &mut runtime, plan);
    assert_eq!(runtime.event_count(), 0);
    assert!(!runtime.machine_meta(ComponentId(1)).unwrap().active);
    let snapshot =
        RuntimeSnapshot::decode(&runtime.snapshot().encode().unwrap(), Config::default()).unwrap();
    runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let scope = AllocationScope::begin().unwrap();
    for tick in 7..100 {
        let plan = runtime
            .plan_tick_with_context(
                &world,
                PhaseContext {
                    payloads: None,
                    active: Some(&inactive),
                    activity: &[],
                },
                Tick(tick),
                Budget::default(),
            )
            .unwrap();
        assert_eq!(plan.work, Work::default());
        assert!(plan.delta.is_empty());
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert_eq!(read_machine(&world, ComponentId(1)).unwrap(), frozen);
    let active = |_: BlockPos| true;
    let wake = [(p(2).page(), true)];
    for tick in 100..=294 {
        let plan = runtime
            .plan_tick_with_context(
                &world,
                PhaseContext {
                    payloads: None,
                    active: Some(&active),
                    activity: if tick == 100 { &wake } else { &[] },
                },
                Tick(tick),
                Budget::default(),
            )
            .unwrap();
        assert_eq!(plan.work.recipe_ticks, 1);
        commit(&mut world, &mut runtime, plan);
        let machine = read_machine(&world, ComponentId(1)).unwrap();
        assert_eq!(machine.slots[1].stack.count(), u16::from(tick == 294));
        assert_eq!(machine.energy, 9750 - 50 * (tick - 99));
    }
}

#[test]
fn predicate_parks_unannounced_inactive_machine_without_reading_world() {
    let mut snapshot = MachineRuntime::new(Config::default()).unwrap().snapshot();
    snapshot.next_order = 1;
    snapshot.machines.push(MachineMeta {
        id: ComponentId(1),
        anchor: p(2),
        order: 0,
        last_tick: Tick(0),
        active: true,
    });
    snapshot.events.push(Pending {
        event: Event::Machine(ComponentId(1)),
        due: Tick(1),
        first_due: Tick(1),
        order: 0,
    });
    let mut runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let world = world::World::new(rules::properties, 1000);
    let inactive = |_: BlockPos| false;
    let plan = runtime
        .plan_tick_with_context(
            &world,
            PhaseContext {
                payloads: None,
                active: Some(&inactive),
                activity: &[],
            },
            Tick(1),
            Budget::default(),
        )
        .unwrap();
    assert_eq!(plan.work.events, 1);
    assert_eq!(plan.work.reads, 0);
    assert_eq!(plan.work.recipe_ticks, 0);
    runtime.acknowledge(plan.delta).unwrap();
    assert!(!runtime.machine_meta(ComponentId(1)).unwrap().active);
    assert_eq!(runtime.event_count(), 0);
}

#[test]
fn public_render_hint_rotates_exact_side_classes_and_omits_private_changes() {
    use rules::{
        blocks::MekKind,
        ids::WIND_GENERATOR,
        machines::{Machine, SideMode},
    };
    let mut world = world::World::new(rules::properties, 1000);
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        p(2).page(),
        vec![core_types::Cell::AIR; core_types::PAGE_VOLUME],
    );
    let mut machine = Machine::new(MekKind::Wind);
    machine.sides = [
        SideMode::None,
        SideMode::Input,
        SideMode::Output,
        SideMode::Both,
        SideMode::Energy,
        SideMode::Extra,
    ];
    machine.ticker = 17;
    machine.active.shown = true;
    tx.set_cell(p(2), core_types::Cell::from_parts(WIND_GENERATOR, 3));
    tx.set_component(
        ComponentId(1),
        machine_component(p(2), &machine),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    runtime
        .acknowledge(
            runtime
                .plan_register(&world, ComponentId(1), Tick(1))
                .unwrap(),
        )
        .unwrap();
    let hint = runtime
        .render_hint(&world, p(2), Tick(17))
        .unwrap()
        .unwrap();
    // South-facing relative [front,left,right,back,top,bottom] maps to
    // world [bottom,top,back,front,left,right].
    assert_eq!(hint.modes, 5 | (4 << 3) | (3 << 6) | (1 << 12) | (2 << 15));
    assert!(hint.active);
    assert!(hint.rotor_phase.is_finite());
    assert!((hint.rotor_velocity - std::f32::consts::TAU / 80.).abs() < 1e-6);
    machine.energy = 1234;
    machine.slots[0] = rules::machines::Slot::new(ItemStack::new(I_IRON_INGOT, 1).unwrap());
    machine.progress = 9;
    let mut tx = world::Transaction::new(Tick(17), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        machine_component(p(2), &machine),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    assert_eq!(
        runtime.render_hint(&world, p(2), Tick(17)).unwrap(),
        Some(hint)
    );
    let delta = runtime
        .extend_machine_page_activity(
            runtime.noop_delta().unwrap(),
            &[(p(2).page(), false)],
            Tick(18),
        )
        .unwrap();
    runtime.acknowledge(delta).unwrap();
    let sleeping = runtime
        .render_hint(&world, p(2), Tick(100))
        .unwrap()
        .unwrap();
    assert_eq!(sleeping.rotor_phase, hint.rotor_phase);
    assert_eq!(sleeping.rotor_velocity, 0.);
    assert!(!sleeping.active);
}
