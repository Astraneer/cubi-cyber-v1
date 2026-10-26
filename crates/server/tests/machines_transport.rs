use core_types::{BlockPos, Cell, ComponentId, PageId, SpaceId, Tick};
use rules::{arms, ids::*, machines::Face, stack::ItemStack};
use server::machines::{transport::*, *};
use std::collections::BTreeMap;
fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn packet() -> Packet {
    Packet {
        id: 1,
        stack: ItemStack::new(rules::items::I_IRON_INGOT, 1).unwrap(),
        color: 0,
        from: p(1, 2, 2),
        path: vec![p(2, 2, 2), p(3, 2, 2)],
        step: 0,
        progress: 5,
        goal: Goal::Destination,
        home: Some(p(1, 2, 2)),
        idle_direction: None,
        destination: Some(ComponentId(0)),
        last_tick: Tick(1),
        due: Tick(2),
        first_due: Tick(2),
    }
}
#[test]
fn exact_packet_path_payload_and_due_index_survive_kind4_extension() {
    let config = Config::default();
    let mut runtime = MachineRuntime::new(config).unwrap();
    let plain = runtime.snapshot().encode().unwrap();
    let payload = rules::item_payload::Payload::charge(20).unwrap();
    let mut item = packet();
    item.stack = ItemStack::new(rules::items::I_ENERGY_TABLET, 1)
        .unwrap()
        .with_payload(payload.id())
        .unwrap();
    let snapshot = TransportSnapshot {
        next_packet: 2,
        next_pull_order: 1,
        packets: vec![item.clone()],
        pulls: vec![Pull {
            position: p(4, 2, 2),
            order: 0,
            failures: 3,
            due: Tick(9),
            first_due: Tick(3),
        }],
        clocks: vec![ClockRecord {
            page: p(2, 2, 2).page(),
            clock: PageClock::default(),
        }],
    };
    assert_eq!(
        TransportSnapshot::decode(&snapshot.encode(config).unwrap(), config).unwrap(),
        snapshot
    );
    let mut all = runtime.snapshot();
    all.transport = snapshot;
    runtime = MachineRuntime::restore(config, &all).unwrap();
    assert!(!runtime.transport_due(Tick(1)));
    assert!(runtime.transport_due(Tick(2)));
    let bytes = runtime.snapshot().encode().unwrap();
    assert_eq!(&bytes[..plain.len()], &plain);
    assert_eq!(&bytes[plain.len()..plain.len() + 4], b"XPKT");
    let restored = RuntimeSnapshot::decode(&bytes, config).unwrap();
    assert_eq!(restored, all);
    let mut out = Vec::new();
    runtime
        .packets_in_pages(&[item.at().unwrap().page()], 8, &mut out)
        .unwrap();
    assert_eq!(out, [item]);
    runtime
        .packets_in_pages(
            &[PageId {
                space: SpaceId(0),
                x: 20,
                y: 0,
                z: 0,
            }],
            8,
            &mut out,
        )
        .unwrap();
    assert!(out.is_empty());
}
#[test]
fn packet_before_state_and_late_invalid_update_reject_without_publication() {
    let config = Config::default();
    let mut runtime = MachineRuntime::new(config).unwrap();
    let mut snapshot = runtime.snapshot();
    snapshot.transport.next_packet = 2;
    snapshot.transport.packets.push(packet());
    snapshot.transport.clocks.push(ClockRecord {
        page: p(2, 2, 2).page(),
        clock: PageClock::default(),
    });
    runtime = MachineRuntime::restore(config, &snapshot).unwrap();
    let mut td = runtime.transport_delta();
    let before = packet();
    let mut after = before.clone();
    after.progress = 100;
    td.packets.push(PacketPatch {
        before: Some(before.clone()),
        after: Some(after.clone()),
    });
    let world = world::World::new(rules::properties, 1000);
    let mut plan = runtime
        .plan_tick(&world, Tick(0), Budget::default())
        .unwrap();
    plan.delta.transport = Some(td.clone());
    assert!(runtime.acknowledge(plan.delta).is_err());
    assert_eq!(runtime.snapshot(), snapshot);
    after.progress = 10;
    after.last_tick = Tick(2);
    after.due = Tick(3);
    after.first_due = Tick(3);
    td.packets[0].after = Some(after.clone());
    let mut plan = runtime
        .plan_tick(&world, Tick(0), Budget::default())
        .unwrap();
    plan.delta.transport = Some(td);
    let encoded = plan.delta.encode().unwrap();
    let delta = RuntimeDelta::decode(&encoded, config).unwrap();
    runtime.acknowledge(delta.clone()).unwrap();
    assert_eq!(runtime.packet(1), Some(&after));
    assert!(runtime.acknowledge(delta).is_err());
}
struct Field {
    cells: BTreeMap<BlockPos, Cell>,
    colors: BTreeMap<BlockPos, u8>,
    capacity: BTreeMap<BlockPos, (ComponentId, u16)>,
}
impl RouteWorld for Field {
    fn cell(&mut self, p: BlockPos) -> Result<Cell, MachineError> {
        if [p.x, p.y, p.z].iter().any(|&n| !(0..16).contains(&n)) {
            return Err(MachineError::Unknown);
        }
        Ok(self.cells.get(&p).copied().unwrap_or(Cell::AIR))
    }
    fn color(&self, p: BlockPos) -> u8 {
        self.colors.get(&p).copied().unwrap_or(0)
    }
    fn capacity(
        &mut self,
        p: BlockPos,
        _: Face,
        _: ItemStack,
        _: Option<u64>,
    ) -> Result<Option<(ComponentId, u16)>, MachineError> {
        Ok(self.capacity.get(&p).copied())
    }
}
fn field() -> Field {
    let mut cells = BTreeMap::new();
    for x in 2..=3 {
        cells.insert(
            p(x, 2, 2),
            Cell::from_parts(
                LOGISTICAL_TRANSPORTER,
                arms::with_arms(0, arms::arms_from_mask(63)),
            ),
        );
    }
    Field {
        cells,
        colors: BTreeMap::new(),
        capacity: BTreeMap::from([
            (p(4, 2, 2), (ComponentId(1), 64)),
            (p(3, 3, 2), (ComponentId(2), 1)),
            (p(1, 2, 2), (ComponentId(3), 64)),
        ]),
    }
}
#[test]
fn route_chooses_distance_then_position_and_honors_home_color_and_budget() {
    let stack = ItemStack::new(rules::items::I_IRON_INGOT, 2).unwrap();
    let budget = RouteBudget {
        nodes: 16,
        candidates: 32,
    };
    let mut f = field();
    let route = find_route(
        &mut f,
        p(2, 2, 2),
        0,
        stack,
        Some(p(1, 2, 2)),
        None,
        None,
        budget,
        &mut RouteWork::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(route.path, [p(2, 2, 2), p(3, 2, 2), p(3, 3, 2)]);
    assert_eq!(route.amount, 1);
    assert_eq!(route.destination, ComponentId(2));
    f.colors.insert(p(3, 2, 2), 7);
    assert!(
        find_route(
            &mut f,
            p(2, 2, 2),
            0,
            stack,
            Some(p(1, 2, 2)),
            None,
            None,
            budget,
            &mut RouteWork::default()
        )
        .unwrap()
        .is_none()
    );
    assert!(
        find_route(
            &mut f,
            p(2, 2, 2),
            7,
            stack,
            Some(p(1, 2, 2)),
            None,
            None,
            budget,
            &mut RouteWork::default()
        )
        .unwrap()
        .is_some()
    );
    f.colors.clear();
    assert!(
        find_route(
            &mut f,
            p(2, 2, 2),
            0,
            stack,
            Some(p(1, 2, 2)),
            None,
            None,
            RouteBudget {
                nodes: 1,
                candidates: 32
            },
            &mut RouteWork::default()
        )
        .is_err()
    );
    let home = find_route(
        &mut f,
        p(2, 2, 2),
        0,
        stack,
        None,
        Some(p(1, 2, 2)),
        None,
        budget,
        &mut RouteWork::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(home.path, [p(2, 2, 2), p(1, 2, 2)]);
}
fn scene() -> world::World {
    let mut world = world::World::new(rules::properties, 1000);
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        vec![Cell::AIR; core_types::PAGE_VOLUME],
    );
    world.commit(tx).unwrap();
    world
}
fn commit(
    world: &mut world::World,
    runtime: &mut MachineRuntime,
    ledger: &mut rules::item_payload::Ledger,
    items: &mut server::entities::items::ItemRuntime,
    plan: PhasePlan,
) {
    runtime.validate(world, &plan).unwrap();
    let payload_delta = ledger
        .extend(ledger.plan().unwrap(), plan.payloads, [])
        .unwrap();
    let item_delta = items
        .extend_spawn(items.plan_activity(&[]).unwrap(), plan.item_spawns, true)
        .unwrap();
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    runtime.acknowledge(plan.delta).unwrap();
    if !payload_delta.is_empty() {
        ledger.acknowledge(payload_delta).unwrap();
    }
    items.acknowledge(item_delta).unwrap();
}
fn pipeline(
    source_machine: bool,
    count: u16,
    typed: bool,
) -> (
    world::World,
    MachineRuntime,
    rules::item_payload::Ledger,
    server::entities::items::ItemRuntime,
) {
    use rules::{
        blocks::MekKind,
        machines::{Machine, Slot, sides::SideMode},
        survival::furnace::Chest,
    };
    let mut world = scene();
    let mut ledger =
        rules::item_payload::Ledger::new(rules::item_payload::Config::default()).unwrap();
    let mut items = server::entities::items::ItemRuntime::new(100, 9).unwrap();
    let mut stack = ItemStack::new(rules::items::I_IRON_INGOT, count).unwrap();
    if typed {
        let payload = rules::item_payload::Payload::charge(180_000).unwrap();
        stack = ItemStack::new(rules::items::I_ENERGY_TABLET, 1)
            .unwrap()
            .with_payload(payload.id())
            .unwrap();
        let delta = ledger
            .extend(ledger.plan().unwrap(), [payload], [])
            .unwrap();
        ledger.acknowledge(delta).unwrap();
    }
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    if source_machine {
        let mut m = Machine::new(MekKind::Smelter);
        m.eject = true;
        m.sides = [SideMode::Output; 6];
        m.slots[1] = Slot::new(stack);
        tx.set_cell(p(1, 2, 2), Cell::from_parts(ENERGIZED_SMELTER, 3))
            .set_component(
                ComponentId(1),
                machine_component(p(1, 2, 2), &m),
                core_types::ChannelMask::CONTENTS,
            );
    } else {
        let mut chest = Chest::default();
        chest.set_trusted(0, stack).unwrap();
        tx.set_cell(p(1, 2, 2), Cell::from_parts(CHEST, 0))
            .set_component(
                ComponentId(1),
                server::survival::chest_component(p(1, 2, 2), &chest),
                core_types::ChannelMask::CONTENTS,
            );
    }
    tx.set_cell(p(4, 2, 2), Cell::from_parts(CHEST, 0))
        .set_component(
            ComponentId(2),
            server::survival::chest_component(p(4, 2, 2), &Chest::default()),
            core_types::ChannelMask::CONTENTS,
        );
    for x in 2..=3 {
        tx.set_cell(p(x, 2, 2), Cell::from_parts(LOGISTICAL_TRANSPORTER, 0));
    }
    world.commit(tx).unwrap();
    let mut runtime = MachineRuntime::new(Config::default()).unwrap();
    let plan = runtime
        .plan_topology(
            &world,
            &[p(2, 2, 2), p(3, 2, 2)],
            Tick(0),
            Budget::default(),
        )
        .unwrap();
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    if !source_machine {
        let plan = runtime
            .plan_connections(
                &world,
                TransmitterConfig {
                    position: p(2, 2, 2),
                    block: LOGISTICAL_TRANSPORTER,
                    modes: 2 << (2 * 4),
                    color: 0,
                },
                Tick(0),
                Budget::default(),
            )
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    (world, runtime, ledger, items)
}
fn count(world: &world::World, id: u64) -> u16 {
    rules::survival::furnace::Chest::decode(&world.component(ComponentId(id)).unwrap().data.payload)
        .unwrap()
        .slots()
        .iter()
        .map(|s| s.count())
        .sum()
}
#[test]
fn actual_world_machine_eject_packet_advances_same_tick_and_delivers_at40() {
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(true, 1, false);
    for tick in 1..=40 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        if tick == 1 {
            assert_eq!(runtime.packet(1).unwrap().progress, 5);
            assert!(
                read_machine(&world, ComponentId(1)).unwrap().slots[1]
                    .stack
                    .is_empty()
            );
        }
        assert_eq!(count(&world, 2), u16::from(tick == 40));
    }
    assert_eq!(runtime.packet_count(), 0);
    assert!(items.entities().is_empty());
}
#[test]
fn actual_world_pull_success_cadence11_delivers_on40_51_62() {
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 3, false);
    let mut arrivals = Vec::new();
    let mut previous = 0;
    for tick in 1..=62 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        let now = count(&world, 2);
        if now > previous {
            arrivals.push(tick);
            previous = now;
        }
    }
    assert_eq!(arrivals, [40, 51, 62]);
    assert_eq!(count(&world, 1), 0);
    assert_eq!(runtime.packet_count(), 0);
}
#[test]
fn removing_transporter_for_a_machine_drops_exact_owned_typed_item_as_entity() {
    use rules::{blocks::MekKind, machines::Machine};
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 1, true);
    for tick in 1..=5 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    let stack = runtime.packet(1).unwrap().stack;
    let mut tx = world::Transaction::new(Tick(6), SpaceId(0));
    tx.set_cell(p(2, 2, 2), Cell::from_parts(ENERGY_CUBE, 3))
        .set_component(
            ComponentId(3),
            machine_component(p(2, 2, 2), &Machine::new(MekKind::Cube)),
            core_types::ChannelMask::CONTENTS,
        );
    world.commit(tx).unwrap();
    let plan = runtime
        .plan_topology(&world, &[p(2, 2, 2)], Tick(5), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    let plan = runtime
        .plan_tick_with_payloads(&world, &ledger, Tick(6), Budget::default())
        .unwrap();
    assert_eq!(plan.item_spawns.len(), 1);
    assert_eq!(plan.item_spawns[0].stack, stack);
    assert_eq!(plan.item_spawns[0].position, [2.25, 2.5, 2.5]);
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    assert_eq!(runtime.packet_count(), 0);
    assert_eq!(items.entities().len(), 1);
    assert_eq!(count(&world, 1), 0);
    assert!(world.component(ComponentId(3)).is_some());
}
#[test]
fn furnace_pull_takes_output_only_and_top_insert_preserves_input() {
    use rules::survival::furnace::Furnace;
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 1, false);
    let mut source = Furnace::default();
    source
        .set_trusted(2, ItemStack::new(rules::items::I_IRON_INGOT, 1).unwrap())
        .unwrap();
    source
        .set_trusted(1, ItemStack::new(rules::items::I_COAL, 3).unwrap())
        .unwrap();
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(p(1, 2, 2), Cell::from_parts(FURNACE, 0))
        .set_component(
            ComponentId(1),
            server::survival::furnace_component(p(1, 2, 2), &source),
            core_types::ChannelMask::CONTENTS,
        );
    world.commit(tx).unwrap();
    let plan = runtime
        .plan_topology(&world, &[p(1, 2, 2)], Tick(0), Budget::default())
        .unwrap();
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    for tick in 1..=40 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    let source = Furnace::decode(&world.component(ComponentId(1)).unwrap().data.payload).unwrap();
    assert!(source.slots()[2].is_empty());
    assert_eq!(source.slots()[1].count(), 3);
    assert_eq!(count(&world, 2), 1);
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 1, false);
    let mut chest = rules::survival::furnace::Chest::default();
    chest
        .set_trusted(0, ItemStack::new(rules::items::I_RAW_IRON, 1).unwrap())
        .unwrap();
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        server::survival::chest_component(p(1, 2, 2), &chest),
        core_types::ChannelMask::CONTENTS,
    )
    .remove_component(ComponentId(2), core_types::ChannelMask::CONTENTS)
    .set_cell(p(4, 2, 2), Cell::AIR)
    .set_cell(p(3, 1, 2), Cell::from_parts(FURNACE, 0))
    .set_component(
        ComponentId(4),
        server::survival::furnace_component(p(3, 1, 2), &Furnace::default()),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let plan = runtime
        .plan_topology(
            &world,
            &[p(3, 2, 2), p(4, 2, 2)],
            Tick(0),
            Budget::default(),
        )
        .unwrap();
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    for tick in 1..=40 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    let target = Furnace::decode(&world.component(ComponentId(4)).unwrap().data.payload).unwrap();
    assert_eq!(target.slots()[0].item(), rules::items::I_RAW_IRON);
    assert_eq!(target.slots()[0].count(), 1);
    assert!(target.slots()[1].is_empty());
}
#[test]
fn reserved_destination_capacity_prevents_second_pull_before_first_packet_arrives() {
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 3, false);
    let mut target = rules::survival::furnace::Chest::default();
    for i in 0..27 {
        target
            .set_trusted(i, ItemStack::new(STONE, 64).unwrap())
            .unwrap();
    }
    target
        .set_trusted(0, ItemStack::new(rules::items::I_IRON_INGOT, 63).unwrap())
        .unwrap();
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.set_component(
        ComponentId(2),
        server::survival::chest_component(p(4, 2, 2), &target),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    for tick in 1..=40 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        assert_eq!(count(&world, 1), 2);
        assert!(runtime.packet_count() <= 1);
    }
    let target = rules::survival::furnace::Chest::decode(
        &world.component(ComponentId(2)).unwrap().data.payload,
    )
    .unwrap();
    assert_eq!(target.slots()[0].count(), 64);
    assert_eq!(runtime.packet_count(), 0);
    assert_eq!(target.slots()[0].count() + count(&world, 1), 66);
    assert_eq!(runtime.snapshot().transport.pulls[0].failures, 3);
    assert_eq!(runtime.snapshot().transport.pulls[0].due, Tick(44));
}
#[test]
fn exhausted_route_budget_never_extracts_source_and_retries_keep_original_age() {
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 3, false);
    let plan = runtime
        .plan_tick_with_payloads(
            &world,
            &ledger,
            Tick(1),
            Budget {
                route_nodes: 1,
                ..Budget::default()
            },
        )
        .unwrap();
    assert!(plan.transactions.is_empty());
    assert_eq!(plan.work.deferred, 1);
    assert_eq!(plan.work.route_nodes, 1);
    assert_eq!(count(&world, 1), 3);
    assert_eq!(runtime.packet_count(), 0);
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    let pending = runtime.snapshot().transport.pulls[0];
    assert_eq!(pending.due, Tick(1));
    assert_eq!(pending.first_due, Tick(1));
    let plan = runtime
        .plan_tick_with_payloads(&world, &ledger, Tick(2), Budget::default())
        .unwrap();
    assert!(plan.work.oldest_age >= 1);
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    assert_eq!(count(&world, 1), 2);
    assert_eq!(runtime.packet(1).unwrap().progress, 5);
}
#[test]
fn unknown_packet_page_defers_atomically_and_preserves_progress_and_retry_age() {
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 1, true);
    for tick in 1..=5 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    let page = PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let cells = world.page(page).unwrap().cells();
    let records: Vec<_> = [ComponentId(1), ComponentId(2)]
        .into_iter()
        .map(|id| (id, world.component(id).unwrap().data.clone()))
        .collect();
    let mut tx = world::Transaction::new(Tick(6), SpaceId(0));
    for (id, _) in &records {
        tx.remove_component(*id, core_types::ChannelMask::CONTENTS);
    }
    tx.remove_page(page);
    world.commit(tx).unwrap();
    for tick in 6..=8 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        assert_eq!(plan.work.unknown, 1);
        assert!(plan.transactions.is_empty());
        assert!(plan.item_spawns.is_empty());
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        let packet = runtime.packet(1).unwrap();
        assert_eq!(packet.progress, 25);
        assert_eq!(packet.last_tick, Tick(5));
        assert_eq!(packet.first_due, Tick(6));
        assert_eq!(packet.due, Tick(tick + 1));
    }
    let mut tx = world::Transaction::new(Tick(9), SpaceId(0));
    tx.create_page(page, cells.to_vec());
    for (id, data) in records {
        tx.set_component(id, data, core_types::ChannelMask::CONTENTS);
    }
    world.commit(tx).unwrap();
    for tick in 9..=43 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        assert_eq!(count(&world, 2), u16::from(tick == 43));
    }
    assert_eq!(runtime.packet_count(), 0);
    assert!(items.entities().is_empty());
}
#[test]
fn home_fallback_may_return_through_pull_arm_but_normal_route_cannot_push_through_it() {
    let mut f = field();
    let state = arms::arms_of(f.cells[&p(2, 2, 2)].state());
    f.cells.insert(
        p(2, 2, 2),
        Cell::from_parts(
            LOGISTICAL_TRANSPORTER,
            arms::with_arms(0, (state & !(3 << (2 * 4))) | (3 << (2 * 4))),
        ),
    );
    let stack = ItemStack::new(rules::items::I_IRON_INGOT, 1).unwrap();
    let budget = RouteBudget {
        nodes: 16,
        candidates: 32,
    };
    let home = find_route(
        &mut f,
        p(2, 2, 2),
        0,
        stack,
        None,
        Some(p(1, 2, 2)),
        None,
        budget,
        &mut RouteWork::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(home.path, [p(2, 2, 2), p(1, 2, 2)]);
    f.capacity.remove(&p(3, 3, 2));
    f.capacity.remove(&p(4, 2, 2));
    assert!(
        find_route(
            &mut f,
            p(2, 2, 2),
            0,
            stack,
            None,
            None,
            None,
            budget,
            &mut RouteWork::default()
        )
        .unwrap()
        .is_none()
    );
}
#[test]
fn actual_store_restores_inflight_typed_packet_pull_clock_and_delivery_tick() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let parent =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/transport-tests");
    std::fs::create_dir_all(&parent).unwrap();
    let directory = Directory(parent.join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 1, true);
    for tick in 1..=19 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    let (mut store, _) = save::Store::open(&directory.0).unwrap();
    store
        .checkpoint(&save::Snapshot {
            record_sequence: 0,
            server_tick: Tick(19),
            public_seed: 1,
            generator_version: 4,
            rules_version: 1,
            world: world.checkpoint().to_restore_state(),
            identities: Vec::new(),
            players: Vec::new(),
            dedup: Vec::new(),
            runtimes: vec![
                save::RuntimeRecord {
                    kind: save::RUNTIME_ITEMS,
                    payload: items.snapshot().encode().unwrap(),
                },
                save::RuntimeRecord {
                    kind: save::RUNTIME_MACHINES,
                    payload: runtime.snapshot().encode().unwrap(),
                },
                save::RuntimeRecord {
                    kind: save::RUNTIME_PAYLOADS,
                    payload: ledger
                        .snapshot()
                        .encode(rules::item_payload::Config::default())
                        .unwrap(),
                },
            ],
        })
        .unwrap();
    for tick in 20..=25 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        runtime.validate(&world, &plan).unwrap();
        let payload_delta = ledger
            .extend(ledger.plan().unwrap(), plan.payloads, [])
            .unwrap();
        let item_delta = items
            .extend_spawn(items.plan_activity(&[]).unwrap(), plan.item_spawns, true)
            .unwrap();
        let mut batch = save::Batch::empty(Tick(tick));
        for tx in plan.transactions {
            batch.changes.push(world.commit(tx).unwrap());
        }
        batch.runtimes.push(save::RuntimeRecord {
            kind: save::RUNTIME_ITEMS,
            payload: item_delta.encode().unwrap(),
        });
        batch.runtimes.push(save::RuntimeRecord {
            kind: save::RUNTIME_MACHINES,
            payload: plan.delta.encode().unwrap(),
        });
        if !payload_delta.is_empty() {
            batch.runtimes.push(save::RuntimeRecord {
                kind: save::RUNTIME_PAYLOADS,
                payload: payload_delta.encode().unwrap(),
            });
        }
        store.append(&batch).unwrap();
        runtime.acknowledge(plan.delta).unwrap();
        items.acknowledge(item_delta).unwrap();
        if !payload_delta.is_empty() {
            ledger.acknowledge(payload_delta).unwrap();
        }
    }
    drop(store);
    let (_, recovery) = save::Store::open(&directory.0).unwrap();
    let snapshot = recovery.snapshot.unwrap();
    let mut recovered_world = world::World::restore(
        rules::properties,
        world::JournalLimits::default(),
        snapshot.world,
    )
    .unwrap();
    let record = |kind| {
        snapshot
            .runtimes
            .iter()
            .find(|r| r.kind == kind)
            .unwrap()
            .payload
            .as_slice()
    };
    let mut recovered = MachineRuntime::restore(
        Config::default(),
        &RuntimeSnapshot::decode(record(save::RUNTIME_MACHINES), Config::default()).unwrap(),
    )
    .unwrap();
    let mut recovered_items = server::entities::items::ItemRuntime::restore(
        100,
        &server::entities::items::Snapshot::decode(record(save::RUNTIME_ITEMS), 100).unwrap(),
    )
    .unwrap();
    let mut recovered_ledger = rules::item_payload::Ledger::restore(
        rules::item_payload::Config::default(),
        &rules::item_payload::Snapshot::decode(
            record(save::RUNTIME_PAYLOADS),
            rules::item_payload::Config::default(),
        )
        .unwrap(),
    )
    .unwrap();
    for (_, batch) in recovery.batches {
        for change in batch.changes {
            recovered_world.apply_recovered_change(&change).unwrap();
        }
        for r in batch.runtimes {
            match r.kind {
                save::RUNTIME_ITEMS => recovered_items
                    .acknowledge(
                        server::entities::items::ItemDelta::decode(&r.payload, 100).unwrap(),
                    )
                    .unwrap(),
                save::RUNTIME_MACHINES => recovered
                    .acknowledge(RuntimeDelta::decode(&r.payload, Config::default()).unwrap())
                    .unwrap(),
                save::RUNTIME_PAYLOADS => recovered_ledger
                    .acknowledge(rules::item_payload::Delta::decode(&r.payload).unwrap())
                    .unwrap(),
                _ => panic!(),
            }
        }
    }
    assert_eq!(recovered_world.canonical_hash(), world.canonical_hash());
    assert_eq!(recovered.snapshot(), runtime.snapshot());
    assert_eq!(recovered_ledger.snapshot(), ledger.snapshot());
    assert_eq!(recovered_items.snapshot(), items.snapshot());
    for tick in 26..=40 {
        let plan = recovered
            .plan_tick_with_payloads(
                &recovered_world,
                &recovered_ledger,
                Tick(tick),
                Budget::default(),
            )
            .unwrap();
        commit(
            &mut recovered_world,
            &mut recovered,
            &mut recovered_ledger,
            &mut recovered_items,
            plan,
        );
        assert_eq!(count(&recovered_world, 2), u16::from(tick == 40));
    }
    let destination = rules::survival::furnace::Chest::decode(
        &recovered_world
            .component(ComponentId(2))
            .unwrap()
            .data
            .payload,
    )
    .unwrap();
    let stack = destination.slots()[0];
    assert_eq!(
        recovered_ledger
            .resolve(stack.payload().unwrap(), stack.item())
            .unwrap()
            .joules(stack.item()),
        Ok(180_000)
    );
    assert_eq!(recovered.packet_count(), 0);
}
#[test]
fn zero_work_registration_and_exhausted_spawn_budget_keep_packet_without_freezing_machine() {
    use rules::{
        blocks::MekKind,
        machines::{Machine, Slot},
    };
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 1, true);
    for tick in 1..=5 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    let before = runtime.snapshot();
    let plan = runtime
        .plan_tick_with_payloads(
            &world,
            &ledger,
            Tick(6),
            Budget {
                events: 0,
                item_spawns: 0,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!(plan.work, Work::default());
    assert!(plan.delta.is_empty());
    assert_eq!(runtime.snapshot(), before);
    let mut machine = Machine::new(MekKind::Smelter);
    machine.energy = 10_000;
    machine.slots[0] = Slot::new(ItemStack::new(rules::items::I_DUST_IRON, 1).unwrap());
    let mut tx = world::Transaction::new(Tick(5), SpaceId(0));
    tx.set_cell(p(2, 2, 2), Cell::AIR)
        .set_cell(p(6, 2, 2), Cell::from_parts(ENERGIZED_SMELTER, 3))
        .set_component(
            ComponentId(3),
            machine_component(p(6, 2, 2), &machine),
            core_types::ChannelMask::CONTENTS,
        );
    world.commit(tx).unwrap();
    let delta = runtime
        .extend_registration(
            runtime.noop_delta().unwrap(),
            &world,
            [ComponentId(3)],
            Tick(5),
        )
        .unwrap();
    runtime.acknowledge(delta).unwrap();
    for tick in 6..=7 {
        let plan = runtime
            .plan_tick_with_payloads(
                &world,
                &ledger,
                Tick(tick),
                Budget {
                    item_spawns: 0,
                    ..Budget::default()
                },
            )
            .unwrap();
        assert!(plan.item_spawns.is_empty());
        assert_eq!(plan.work.deferred, 1);
        assert_eq!(plan.work.machines, 1);
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        assert_eq!(
            read_machine(&world, ComponentId(3)).unwrap().progress,
            (tick - 5) as u32
        );
        let packet = runtime.packet(1).unwrap();
        assert_eq!(packet.progress, 25);
        assert_eq!(packet.first_due, Tick(6));
        assert_eq!(packet.due, Tick(tick + 1));
        assert!(items.entities().is_empty());
    }
    let plan = runtime
        .plan_tick_with_payloads(
            &world,
            &ledger,
            Tick(8),
            Budget {
                item_spawns: 1,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!(plan.item_spawns.len(), 1);
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    assert_eq!(runtime.packet_count(), 0);
    assert_eq!(items.entities().len(), 1);
    assert_eq!(read_machine(&world, ComponentId(3)).unwrap().progress, 3);
}
#[test]
fn page_pause_changes_only_clock_and_restores_exact_remaining_delivery_time() {
    let (mut world, mut runtime, mut ledger, mut items) = pipeline(false, 1, true);
    for tick in 1..=5 {
        let plan = runtime
            .plan_tick_with_payloads(&world, &ledger, Tick(tick), Budget::default())
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    }
    let page = p(2, 2, 2).page();
    let inactive = |_: BlockPos| false;
    let plan = runtime
        .plan_tick_with_context(
            &world,
            PhaseContext {
                payloads: Some(&ledger),
                active: Some(&inactive),
                activity: &[(page, false)],
            },
            Tick(6),
            Budget::default(),
        )
        .unwrap();
    assert_eq!(plan.work.events, 0);
    let delta = plan.delta.transport.as_ref().unwrap();
    assert_eq!(delta.packets.len(), 0);
    assert_eq!(delta.pulls.len(), 0);
    assert_eq!(delta.clocks.len(), 1);
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    assert!(!runtime.transport_due(Tick(99)));
    assert_eq!(runtime.packet(1).unwrap().progress, 25);
    assert_eq!(
        runtime.packet_epoch(1, Tick(99)).unwrap(),
        (Tick(99), false)
    );
    let snapshot =
        RuntimeSnapshot::decode(&runtime.snapshot().encode().unwrap(), Config::default()).unwrap();
    runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let active = |_: BlockPos| true;
    let plan = runtime
        .plan_tick_with_context(
            &world,
            PhaseContext {
                payloads: Some(&ledger),
                active: Some(&active),
                activity: &[(page, true)],
            },
            Tick(100),
            Budget::default(),
        )
        .unwrap();
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    assert_eq!(runtime.packet(1).unwrap().progress, 30);
    assert_eq!(
        runtime.packet_epoch(1, Tick(100)).unwrap(),
        (Tick(100), true)
    );
    assert_eq!(runtime.page_clock(page).unwrap().offset, 94);
    for tick in 101..=134 {
        let plan = runtime
            .plan_tick_with_context(
                &world,
                PhaseContext {
                    payloads: Some(&ledger),
                    active: Some(&active),
                    activity: &[],
                },
                Tick(tick),
                Budget::default(),
            )
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        assert_eq!(count(&world, 2), u16::from(tick == 134));
    }
}

#[test]
fn packet_crosses_into_an_already_dormant_page_with_its_independent_clock() {
    use rules::survival::furnace::Chest;
    let mut world = scene();
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p(16, 2, 2).page(), vec![Cell::AIR; core_types::PAGE_VOLUME]);
    let arms = arms::arms_from_mask((1 << 4) | (1 << 5));
    for x in 15..=16 {
        tx.set_cell(
            p(x, 2, 2),
            Cell::from_parts(LOGISTICAL_TRANSPORTER, arms::with_arms(0, arms)),
        );
    }
    tx.set_cell(p(17, 2, 2), Cell::from_parts(CHEST, 0));
    tx.set_component(
        ComponentId(2),
        server::survival::chest_component(p(17, 2, 2), &Chest::default()),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut snapshot = MachineRuntime::new(Config::default()).unwrap().snapshot();
    snapshot.transport.next_packet = 2;
    snapshot.transport.next_pull_order = 1;
    snapshot.transport.packets.push(Packet {
        id: 1,
        stack: ItemStack::new(rules::items::I_IRON_INGOT, 1).unwrap(),
        color: 0,
        from: p(14, 2, 2),
        path: vec![p(15, 2, 2), p(16, 2, 2), p(17, 2, 2)],
        step: 0,
        progress: 95,
        goal: Goal::Destination,
        home: None,
        idle_direction: None,
        destination: Some(ComponentId(2)),
        last_tick: Tick(19),
        due: Tick(20),
        first_due: Tick(20),
    });
    // Another dormant row already owns page1's frozen local clock (time9).
    snapshot.transport.pulls.push(Pull {
        position: p(18, 2, 2),
        order: 0,
        failures: 0,
        due: Tick(11),
        first_due: Tick(11),
    });
    snapshot.transport.clocks = vec![
        ClockRecord {
            page: p(15, 2, 2).page(),
            clock: PageClock::default(),
        },
        ClockRecord {
            page: p(16, 2, 2).page(),
            clock: PageClock::default().change(false, Tick(10)).unwrap(),
        },
    ];
    let mut runtime = MachineRuntime::restore(Config::default(), &snapshot).unwrap();
    let mut ledger =
        rules::item_payload::Ledger::new(rules::item_payload::Config::default()).unwrap();
    let mut items = server::entities::items::ItemRuntime::new(100, 9).unwrap();
    let page0only = |pos: BlockPos| pos.page() == p(15, 2, 2).page();
    let plan = runtime
        .plan_tick_with_context(
            &world,
            PhaseContext {
                payloads: Some(&ledger),
                active: Some(&page0only),
                activity: &[],
            },
            Tick(20),
            Budget::default(),
        )
        .unwrap();
    commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
    let packet = runtime.packet(1).unwrap();
    assert_eq!(packet.at(), Some(p(16, 2, 2)));
    assert_eq!(
        (packet.progress, packet.last_tick, packet.due),
        (0, Tick(9), Tick(10))
    );
    assert!(!runtime.transport_due(Tick(99)));
    runtime = MachineRuntime::restore(
        Config::default(),
        &RuntimeSnapshot::decode(&runtime.snapshot().encode().unwrap(), Config::default()).unwrap(),
    )
    .unwrap();
    let active = |_: BlockPos| true;
    let wake = [(p(16, 2, 2).page(), true)];
    for tick in 100..=119 {
        let plan = runtime
            .plan_tick_with_context(
                &world,
                PhaseContext {
                    payloads: Some(&ledger),
                    active: Some(&active),
                    activity: if tick == 100 { &wake } else { &[] },
                },
                Tick(tick),
                Budget::default(),
            )
            .unwrap();
        commit(&mut world, &mut runtime, &mut ledger, &mut items, plan);
        assert_eq!(count(&world, 2), u16::from(tick == 119));
    }
    assert_eq!(runtime.packet_count(), 0);
}

#[test]
fn rejected_charged_ejection_keeps_numeric_slot_and_creates_no_orphan_payload() {
    use rules::{machines::Slot, survival::furnace::Chest};
    let (mut world, mut runtime, ledger, _) = pipeline(true, 1, false);
    let mut source = read_machine(&world, ComponentId(1)).unwrap();
    source.slots[1] = Slot::tablet(180_000);
    let mut chest = Chest::default();
    for i in 0..27 {
        chest
            .set_trusted(i, ItemStack::new(STONE, 64).unwrap())
            .unwrap();
    }
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        machine_component(p(1, 2, 2), &source),
        core_types::ChannelMask::CONTENTS,
    );
    tx.set_component(
        ComponentId(2),
        server::survival::chest_component(p(4, 2, 2), &chest),
        core_types::ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let plan = runtime
        .plan_tick_with_payloads(&world, &ledger, Tick(1), Budget::default())
        .unwrap();
    assert!(plan.payloads.is_empty());
    assert!(plan.item_spawns.is_empty());
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    runtime.acknowledge(plan.delta).unwrap();
    assert_eq!(
        read_machine(&world, ComponentId(1)).unwrap().slots[1],
        Slot::tablet(180_000)
    );
    assert_eq!(runtime.packet_count(), 0);
}
