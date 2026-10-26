use core_types::{BlockPos, Cell, ComponentId, SpaceId, Tick, Version};
use rules::{
    blocks::MekKind,
    ids::*,
    items::I_IRON_INGOT,
    machines::{Machine, SideMode},
    stack::ItemStack,
    survival::furnace::Chest,
};
use server::{
    machines::{self, Config, MachineRuntime, TransmitterConfig},
    props::{self, Budget, Error, Stamp},
};
use telemetry::{AllocationScope, CountingAllocator};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 1,
        z: 1,
    }
}
fn stamp() -> Stamp {
    Stamp {
        page: p(1).page(),
        cell_view: 10,
        view: 11,
        source: Version(12),
        tick: Tick(17),
    }
}
fn fixture() -> (World, MachineRuntime) {
    let mut world = World::new(rules::properties, 1000);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p(1).page(), vec![Cell::from_parts(STONE, 0); 4096]);
    tx.set_cell(p(1), Cell::from_parts(CHEST, 3));
    tx.set_component(
        ComponentId(1),
        server::survival::chest_component(p(1), &Chest::default()),
        core_types::ChannelMask::CONTENTS,
    );
    let mut machine = Machine::new(MekKind::Smelter);
    machine.sides = [
        SideMode::None,
        SideMode::Input,
        SideMode::Output,
        SideMode::Both,
        SideMode::Energy,
        SideMode::Extra,
    ];
    machine.active.shown = true;
    machine.energy = 100;
    tx.set_cell(p(2), Cell::from_parts(ENERGIZED_SMELTER, 3));
    tx.set_component(
        ComponentId(2),
        machines::machine_component(p(2), &machine),
        core_types::ChannelMask::CONTENTS,
    );
    let mut wind = Machine::new(MekKind::Wind);
    wind.active.shown = true;
    wind.ticker = 17;
    tx.set_cell(p(3), Cell::from_parts(WIND_GENERATOR, 3));
    tx.set_component(
        ComponentId(3),
        machines::machine_component(p(3), &wind),
        core_types::ChannelMask::CONTENTS,
    );
    tx.set_cell(p(4), Cell::from_parts(UNIVERSAL_CABLE, 0));
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    for id in [ComponentId(2), ComponentId(3)] {
        runtime
            .acknowledge(runtime.plan_register(&world, id, Tick(1)).unwrap())
            .unwrap();
    }
    let mut snapshot = runtime.snapshot();
    snapshot.configs.push(TransmitterConfig {
        position: p(4),
        block: UNIVERSAL_CABLE,
        modes: 2 << (2 * 5),
        color: 0,
    });
    runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    (world, runtime)
}
#[test]
fn indexed_props_match_absolute_ports_lid_and_wind_golden_without_page_scan() {
    let (world, runtime) = fixture();
    let (frame, work) = props::build(
        &world,
        &runtime,
        stamp(),
        &|_, _| true,
        &|_| 0.5,
        Budget::default(),
    )
    .unwrap();
    assert_eq!(work.component_anchors, 3);
    assert_eq!(work.transmitter_anchors, 1);
    assert_eq!(work.machine_decodes, 2);
    assert_eq!(work.cell_reads, 6);
    assert_eq!(work.hints, 4);
    let expected = [
        net::props::Hint {
            index: p(1).local_index(),
            kind: net::props::Kind::Chest,
            flags: net::props::LID,
            ports: 0,
            lid: 32768,
            rotor: 0,
            rotor_velocity: 0,
        },
        net::props::Hint {
            index: p(2).local_index(),
            kind: net::props::Kind::Machine,
            flags: net::props::ACTIVE | net::props::PORTS,
            ports: 5 | (4 << 3) | (3 << 6) | (1 << 12) | (2 << 15),
            lid: 0,
            rotor: 0,
            rotor_velocity: 0,
        },
        net::props::Hint {
            index: p(3).local_index(),
            kind: net::props::Kind::Wind,
            flags: net::props::ACTIVE | net::props::PORTS | net::props::ROTOR,
            ports: 0,
            lid: 0,
            rotor: 13926,
            rotor_velocity: 819,
        },
        net::props::Hint {
            index: p(4).local_index(),
            kind: net::props::Kind::Connector,
            flags: net::props::PORTS,
            ports: 2 << (2 * 5),
            lid: 0,
            rotor: 0,
            rotor_velocity: 0,
        },
    ];
    assert_eq!(frame.hints, expected);
    assert_eq!(
        net::props::Frame::decode(&frame.encode().unwrap()).unwrap(),
        frame
    );
}
#[test]
fn private_contents_energy_and_payload_bytes_do_not_change_public_visual_projection() {
    let (mut world, runtime) = fixture();
    let before = props::build(
        &world,
        &runtime,
        stamp(),
        &|_, _| true,
        &|_| 0.,
        Budget::default(),
    )
    .unwrap()
    .0;
    let mut machine = machines::read_machine(&world, ComponentId(2)).unwrap();
    machine.energy = 999;
    machine.progress = 50;
    machine.slots[1] = rules::machines::Slot::new(ItemStack::new(I_IRON_INGOT, 12).unwrap());
    let mut chest = Chest::default();
    chest
        .set_trusted(0, ItemStack::new(I_IRON_INGOT, 64).unwrap())
        .unwrap();
    let mut tx = Transaction::new(Tick(18), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        server::survival::chest_component(p(1), &chest),
        core_types::ChannelMask::CONTENTS,
    );
    tx.set_component(
        ComponentId(2),
        machines::machine_component(p(2), &machine),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    assert_eq!(
        props::build(
            &world,
            &runtime,
            stamp(),
            &|_, _| true,
            &|_| 0.,
            Budget::default()
        )
        .unwrap()
        .0,
        before
    );
}
#[test]
fn stale_or_masked_public_cells_withhold_anchors_and_bounds_never_publish_partial_frame() {
    let (mut world, runtime) = fixture();
    let (frame, work) = props::build(
        &world,
        &runtime,
        stamp(),
        &|index, _| index != p(2).local_index(),
        &|_| 0.,
        Budget::default(),
    )
    .unwrap();
    assert_eq!(frame.hints.len(), 3);
    assert_eq!(work.withheld, 1);
    assert!(matches!(
        props::build(
            &world,
            &runtime,
            stamp(),
            &|_, _| true,
            &|_| f32::NAN,
            Budget::default()
        ),
        Err(Error::Hint)
    ));
    assert!(matches!(
        props::build(
            &world,
            &runtime,
            stamp(),
            &|_, _| true,
            &|_| 0.,
            Budget {
                anchors: 4,
                hints: 3
            }
        ),
        Err(Error::Bounds)
    ));
    let mut tx = Transaction::new(Tick(18), SpaceId(0));
    tx.set_cell(p(4), Cell::from_parts(STONE, 0));
    world.commit(tx).unwrap();
    let (frame, work) = props::build(
        &world,
        &runtime,
        stamp(),
        &|_, _| true,
        &|_| 0.,
        Budget::default(),
    )
    .unwrap();
    assert_eq!(work.stale, 1);
    assert_eq!(frame.hints.len(), 3);
    let mut missing = stamp();
    missing.page.x += 1;
    assert!(matches!(
        props::build(
            &world,
            &runtime,
            missing,
            &|_, _| true,
            &|_| 0.,
            Budget::default()
        ),
        Err(Error::Unknown)
    ));
}
#[test]
fn page_index_visits_one_config_among_ten_thousand_and_survives_restore() {
    let mut world = World::new(rules::properties, 1000);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p(1).page(), vec![Cell::from_parts(STONE, 0); 4096]);
    tx.set_cell(p(1), Cell::from_parts(UNIVERSAL_CABLE, 0));
    world.commit(tx).unwrap();
    let mut snapshot = MachineRuntime::new(Config::default()).unwrap().snapshot();
    snapshot.configs = (0..10_000)
        .map(|i| TransmitterConfig {
            position: p(i * 16 + 1),
            block: UNIVERSAL_CABLE,
            modes: 0,
            color: 0,
        })
        .collect();
    let runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let scope = AllocationScope::begin().unwrap();
    let (frame, work) = props::build(
        &world,
        &runtime,
        stamp(),
        &|_, _| true,
        &|_| 0.,
        Budget::default(),
    )
    .unwrap();
    let count = scope.finish();
    assert_eq!(work.transmitter_anchors, 1);
    assert_eq!(work.cell_reads, 1);
    assert_eq!(frame.hints.len(), 1);
    assert!(count.allocated_bytes < 64 * 1024, "{count:?}");
    let restored = MachineRuntime::restore(Config::default(), &runtime.snapshot()).unwrap();
    assert_eq!(restored.transmitter_configs_in_page(p(1).page()).count(), 1);
}

#[test]
fn transporter_color_uses_public_five_bits_and_preserves_all_side_modes() {
    let (mut world, runtime) = fixture();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(p(4), Cell::from_parts(LOGISTICAL_TRANSPORTER, 0));
    world.commit(tx).unwrap();
    for color in 0..=18 {
        let mut snapshot = runtime.snapshot();
        snapshot.configs[0].block = LOGISTICAL_TRANSPORTER;
        snapshot.configs[0].color = color;
        let colored = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
        let (frame, work) = props::build(
            &world,
            &colored,
            stamp(),
            &|_, _| true,
            &|_| 0.,
            Budget::default(),
        )
        .unwrap();
        assert_eq!(work.transmitter_anchors, 1);
        assert_eq!(work.hints, 4);
        let hint = frame.hints.last().unwrap();
        assert_eq!(hint.kind, net::props::Kind::Connector);
        assert_eq!(hint.ports & 0xfff, 2048, "East push mode survives color");
        assert_eq!(hint.ports >> 12, u32::from(color));
        assert_eq!(hint.ports >> 17, 0, "no private state in reserved bits");
        let bytes = frame.encode().unwrap();
        assert_eq!(net::props::Frame::decode(&bytes).unwrap(), frame);
        let at = net::props::HEADER_BYTES + 3 * net::props::HINT_BYTES + 4;
        // Independent little-endian oracle for modes0x800 + color*0x1000.
        let expected = [0, 8 | (color << 4), color >> 4, 0];
        assert_eq!(&bytes[at..at + 4], &expected);
    }
}
