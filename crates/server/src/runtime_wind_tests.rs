//! Sleeping wind generators depend on their actual upper LIGHT sample.
use super::*;
use core_types::{ChannelMask, ComponentId, PAGE_VOLUME};
use rules::{blocks::MekKind, ids::*, machines::Machine};
use world::LightSample;
fn position(x: i32, y: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z: 4,
    }
}
fn change_light(world: &mut World, p: BlockPos, sky: u8, rgb: u8) -> WorldChange {
    let mut tx = Transaction::new(Tick(21), SpaceId(0));
    tx.set_light(
        p,
        LightSample {
            sky,
            rgb: [rgb; 3],
            ..LightSample::default()
        },
    );
    world.commit(tx).unwrap()
}
#[test]
fn committed_sky_wakes_only_its_sleeping_wind_and_recovery_derives_a_lost_wake() {
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(position(4, 2).page(), vec![Cell::AIR; PAGE_VOLUME]);
    for (id, x) in [(1, 4), (2, 8)] {
        let base = position(x, 2);
        tx.set_cell(base, Cell::from_parts(WIND_GENERATOR, 5));
        for dy in 1..=4 {
            tx.set_cell(position(x, 2 + dy), Cell::from_parts(BOUNDING, dy as u16));
        }
        tx.set_component(
            ComponentId(id),
            machines::machine_component(base, &Machine::new(MekKind::Wind)),
            ChannelMask::CONTENTS,
        );
    }
    world.commit(tx).unwrap();
    let mut runtimes = Runtimes::new(&[0; 32], 1).unwrap();
    for id in [ComponentId(1), ComponentId(2)] {
        let delta = runtimes
            .machines
            .plan_register(&world, id, Tick(1))
            .unwrap();
        runtimes.machines.acknowledge(delta).unwrap();
    }
    for tick in 1..=20 {
        let plan = runtimes
            .machines
            .plan_tick(&world, Tick(tick), machines::Budget::default())
            .unwrap();
        if tick > 1 {
            assert_eq!(
                (plan.work.events, plan.work.machines, plan.work.reads),
                (0, 0, 0),
                "dark sleeping wind performs no idle work"
            );
        }
        runtimes.machines.validate(&world, &plan).unwrap();
        for tx in plan.transactions {
            world.commit(tx).unwrap();
        }
        runtimes.machines.acknowledge(plan.delta).unwrap();
    }
    assert_eq!(
        runtimes.machines.event_count(),
        0,
        "both dark machines genuinely sleep after their own sky poll"
    );
    let rgb = change_light(&mut world, position(4, 6), 0, 1);
    runtimes.committed_changes(&world, &[rgb]).unwrap();
    assert!(runtimes.machine_bootstrap.is_empty());
    let lower_mast = change_light(&mut world, position(8, 4), 15, 0);
    runtimes.committed_changes(&world, &[lower_mast]).unwrap();
    assert!(runtimes.machine_bootstrap.is_empty());
    let sky = change_light(&mut world, position(4, 6), 15, 1);
    runtimes
        .committed_changes(&world, std::slice::from_ref(&sky))
        .unwrap();
    runtimes.committed_changes(&world, &[sky]).unwrap();
    assert_eq!(
        runtimes.machine_bootstrap,
        BTreeSet::from([ComponentId(1)]),
        "repeated durable observation coalesces one exact head, never the other wind"
    );
    // Reconstruct the derived pending dependency after fsync-before-observe
    // interruption, without changing persisted machine bytes or polling them.
    runtimes.machine_bootstrap.clear();
    runtimes.bootstrap_drops(&world).unwrap();
    assert_eq!(runtimes.machine_bootstrap, BTreeSet::from([ComponentId(1)]));
    assert_eq!(
        Machine::decode(&world.component(ComponentId(1)).unwrap().data.payload)
            .unwrap()
            .produced,
        0
    );
    let registration = runtimes
        .machines
        .extend_registration(
            runtimes.machines.noop_delta().unwrap(),
            &world,
            [ComponentId(1)],
            Tick(21),
        )
        .unwrap();
    runtimes.machines.acknowledge(registration).unwrap();
    for tick in 22..=45 {
        let plan = runtimes
            .machines
            .plan_tick(&world, Tick(tick), machines::Budget::default())
            .unwrap();
        for tx in plan.transactions {
            world.commit(tx).unwrap();
        }
        runtimes.machines.acknowledge(plan.delta).unwrap();
    }
    assert!(
        Machine::decode(&world.component(ComponentId(1)).unwrap().data.payload)
            .unwrap()
            .produced
            > 0
    );
}
#[test]
fn new_wind_performs_its_initial_sky_poll_on_first_owner_execution() {
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(20), SpaceId(0));
    let base = position(4, 2);
    let head = position(4, 6);
    tx.create_page(base.page(), vec![Cell::AIR; PAGE_VOLUME]);
    tx.set_cell(base, Cell::from_parts(WIND_GENERATOR, 5));
    for dy in 1..=4 {
        tx.set_cell(position(4, 2 + dy), Cell::from_parts(BOUNDING, dy as u16));
    }
    tx.set_light(
        head,
        LightSample {
            sky: 15,
            ..LightSample::default()
        },
    );
    tx.set_component(
        ComponentId(1),
        machines::machine_component(base, &Machine::new(MekKind::Wind)),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut runtimes = Runtimes::new(&[0; 32], 1).unwrap();
    let delta = runtimes
        .machines
        .extend_registration(
            runtimes.machines.noop_delta().unwrap(),
            &world,
            [ComponentId(1)],
            Tick(20),
        )
        .unwrap();
    runtimes.machines.acknowledge(delta).unwrap();
    let plan = runtimes
        .machines
        .plan_tick(&world, Tick(21), machines::Budget::default())
        .unwrap();
    assert_eq!(plan.work.machines, 1);
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    runtimes.machines.acknowledge(plan.delta).unwrap();
    let wind = Machine::decode(&world.component(ComponentId(1)).unwrap().data.payload).unwrap();
    assert_eq!(wind.ticker, 1, "prebirth time cannot skip initial poll0");
    assert_eq!(wind.produced, rules::machines::wind_power(base.y));
    assert!(wind.produced > 0);
}
