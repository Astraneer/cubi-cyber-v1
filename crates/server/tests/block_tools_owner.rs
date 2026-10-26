//! Guarded use-on-block crosses the real action codec and fsync frontier.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use net::{
    Action, ActionId, ActionStatus, ClientMessage, InventoryGuard, RejectReason, ServerMessage,
};
use rules::{
    blocks::MekKind,
    ids::*,
    items::*,
    machines::{Machine, SideMode, Slot},
    stack::ItemStack,
    survival::Inventory,
};
use server::{Outgoing, Server, ServerConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "block-tools-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.generation_enabled = false;
        c.spawn_override = Some([8.5, 1., 8.5]);
        c.spawn_protection_radius = 0;
        c
    }
    fn owner(&self) -> ClientMessage {
        net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn at() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 2,
        z: 11,
    }
}
fn fixture(block: u16, machine: Option<Machine>) -> World {
    let mut w = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(STONE, 0));
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        cells,
    );
    tx.set_cell(at(), Cell::from_parts(block, 2));
    if let Some(m) = machine {
        tx.set_component(
            ComponentId(1),
            server::machines::machine_component(at(), &m),
            ChannelMask::CONTENTS,
        );
    }
    w.commit(tx).unwrap();
    w
}
struct H {
    server: Server,
    now: u64,
    out: Vec<Outgoing>,
    next: u64,
}
impl H {
    fn start(d: &Directory, block: u16, machine: Option<Machine>) -> Self {
        let mut h = Self {
            server: Server::open_with_world(d.config(), fixture(block, machine)).unwrap(),
            now: 0,
            out: Vec::new(),
            next: 1,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !h.server.height_ready_for_publication() {
            assert!(Instant::now() < deadline);
            h.step();
        }
        h.send(d.owner());
        h.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
        h.settle();
        h
    }
    fn step(&mut self) {
        self.now += 10;
        self.out.extend(self.server.tick(self.now));
        assert!(!self.server.is_failed(), "{:?}", self.server.counters());
        thread::sleep(Duration::from_micros(200));
    }
    fn send(&mut self, m: ClientMessage) {
        let bytes = net::encode_client(&m).unwrap();
        self.out.extend(
            self.server
                .handle(1, net::decode_client(&bytes).unwrap(), self.now),
        );
    }
    fn until(&mut self, mut f: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(i) = self.out.iter().position(&mut f) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < end, "{:?}", self.server.counters());
            self.step();
        }
    }
    fn action(&mut self, action: Action) -> net::ActionReply {
        let id = self.next;
        self.next += 1;
        self.action_id(id, action, [0., 0.])
    }
    fn action_id(&mut self, id: u64, action: Action, aim: [f32; 2]) -> net::ActionReply {
        self.send(ClientMessage::AimedAction {
            id: ActionId(id),
            action,
            yaw: aim[0],
            pitch: aim[1],
        });
        let out=self.until(|o|matches!(&o.message,ServerMessage::Action(r)if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        let ServerMessage::Action(reply) =
            net::decode_server(&net::encode_server(&out.message).unwrap()).unwrap()
        else {
            panic!()
        };
        reply
    }
    fn inventory(&self) -> Inventory {
        self.out
            .iter()
            .rev()
            .find_map(|o| match &o.message {
                ServerMessage::Inventory { data } => Some(Inventory::decode(data).unwrap()),
                _ => None,
            })
            .unwrap()
    }
    fn guard(&self) -> InventoryGuard {
        let i = self.inventory();
        let h = i.hand();
        InventoryGuard {
            revision: i.revision(),
            selected: i.selected() as u8,
            item: h.item(),
            count: h.count(),
            damage: h.damage(),
            payload: h.payload().map_or(0, |id| id.0),
        }
    }
    fn tool(&mut self, sneaking: bool) -> net::ActionReply {
        let action = self.intent(sneaking);
        let r = self.action(action);
        self.settle();
        r
    }
    fn intent(&self, sneaking: bool) -> Action {
        let world::CellRead::Known(expected) = self.server.world().read(at()) else {
            panic!()
        };
        Action::UseBlockTool {
            position: at(),
            expected,
            sneaking,
            inventory: self.guard(),
        }
    }
    fn give(&mut self, item: u16, count: u16) {
        assert_eq!(
            self.action(Action::Command(format!("/give {item} {count}")))
                .status,
            ActionStatus::Accepted
        );
        self.settle();
    }
    fn mode(&mut self, count: usize) {
        for _ in 0..count {
            let g = self.guard();
            assert_eq!(
                self.action(Action::CycleToolMode {
                    back: false,
                    inventory: g
                })
                .status,
                ActionStatus::Accepted
            );
            self.settle();
        }
    }
    fn machine(&self) -> Machine {
        Machine::decode(
            &self
                .server
                .world()
                .component(ComponentId(1))
                .unwrap()
                .data
                .payload,
        )
        .unwrap()
    }
    fn settle(&mut self) {
        let until = self.server.current_tick().0 + 5;
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.server.current_tick().0 < until {
            assert!(Instant::now() < deadline);
            self.step();
        }
        while self.server.awaiting_durability() {
            self.out.extend(self.server.tick(self.now));
            thread::sleep(Duration::from_micros(200));
        }
    }
}
#[test]
fn owned_item_side_rotate_and_gas_are_guarded_and_survive_reopen() {
    let d = Directory::new();
    let mut h = H::start(
        &d,
        ELECTROLYTIC_SEPARATOR,
        Some(Machine::new(MekKind::Separator)),
    );
    h.give(I_CONFIGURATOR, 1);
    let original = h.machine();
    assert_eq!(h.tool(false).status, ActionStatus::Accepted);
    assert_eq!(h.machine().sides, original.sides); // ordinary RMB opens window
    assert_eq!(h.tool(true).status, ActionStatus::Accepted);
    assert_eq!(h.machine().sides[0], SideMode::Energy); // default front INPUT -> ENERGY
    let old = h.intent(true);
    let id = h.next;
    h.next += 1;
    assert_eq!(
        h.action_id(id, old.clone(), [0., 0.]).status,
        ActionStatus::Accepted
    );
    h.settle();
    let after = h.machine().sides;
    assert_eq!(
        h.action_id(id, old.clone(), [0., 0.]).status,
        ActionStatus::Accepted
    );
    assert_eq!(h.machine().sides, after);
    h.mode(2);
    assert_eq!(h.tool(false).status, ActionStatus::Accepted);
    let world::CellRead::Known(c) = h.server.world().read(at()) else {
        panic!()
    };
    assert_eq!(c.state() & 7, 5); // N->E
    h.mode(2);
    assert_eq!(h.tool(true).status, ActionStatus::Accepted);
    let stored = h.machine();
    assert_eq!(stored.gas_sides[2], 0); // hitNorth is relativeRight: OXYGEN(2) -> NONE(0) when facingEast
    let before = h.server.world().canonical_hash();
    drop(h);
    let s = Server::open(d.config()).unwrap();
    assert_eq!(s.world().canonical_hash(), before);
    assert_eq!(
        Machine::decode(&s.world().component(ComponentId(1)).unwrap().data.payload).unwrap(),
        stored
    );
}
#[test]
fn empty_preserves_numeric_charge_and_same_action_does_not_repeat() {
    let d = Directory::new();
    let mut m = Machine::new(MekKind::Smelter);
    m.slots[0] = Slot::new(ItemStack::new(IRON_ORE, 3).unwrap());
    m.slots[1] = Slot::tablet(180_000);
    let mut h = H::start(&d, ENERGIZED_SMELTER, Some(m));
    h.give(I_CONFIGURATOR, 1);
    h.mode(1);
    let action = h.intent(true);
    let id = h.next;
    h.next += 1;
    assert_eq!(
        h.action_id(id, action.clone(), [0., 0.]).status,
        ActionStatus::Accepted
    );
    h.settle();
    assert!(h.machine().slots.iter().all(|s| s.stack.is_empty()));
    let items = h.server.item_entities().rows();
    assert_eq!(items.len(), 2);
    let tablet = items
        .iter()
        .find(|i| i.stack.item() == I_ENERGY_TABLET)
        .unwrap()
        .stack;
    assert_eq!(
        h.server
            .item_payloads()
            .resolve(tablet.payload().unwrap(), tablet.item())
            .unwrap()
            .joules(tablet.item())
            .unwrap(),
        180_000
    );
    assert!(
        items
            .iter()
            .all(|i| i.position[2] < 11. && i.pickup_delay <= 10)
    );
    assert_eq!(
        h.action_id(id, action, [0., 0.]).status,
        ActionStatus::Accepted
    );
    h.settle();
    // After delay10 the real pickup phase may move a row into inventory.
    // Conservation includes both authoritative owners instead of freezing AI.
    for (item, expected) in [(IRON_ORE, 3), (I_ENERGY_TABLET, 1)] {
        let inventory: u64 = h
            .inventory()
            .slots()
            .iter()
            .filter(|s| s.item() == item)
            .map(|s| u64::from(s.count()))
            .sum();
        let rows: u64 = h
            .server
            .item_entities()
            .rows()
            .iter()
            .filter(|r| r.stack.item() == item)
            .map(|r| u64::from(r.stack.count()))
            .sum();
        assert_eq!(inventory + rows, expected);
    }
    let rows = h.server.item_entities().len();
    drop(h);
    let s = Server::open(d.config()).unwrap();
    assert_eq!(s.item_entities().len(), rows);
    assert_eq!(
        s.item_payloads()
            .resolve(tablet.payload().unwrap(), tablet.item())
            .unwrap()
            .joules(tablet.item())
            .unwrap(),
        180_000
    );
}
#[test]
fn wrench_preserves_one_machine_item_without_tool_wear() {
    let d = Directory::new();
    let mut m = Machine::new(MekKind::Smelter);
    m.energy = 4020;
    m.slots[1] = Slot::tablet(180_000);
    let mut h = H::start(&d, ENERGIZED_SMELTER, Some(m));
    h.give(I_CONFIGURATOR, 1);
    h.mode(3);
    assert_eq!(h.tool(true).status, ActionStatus::Accepted);
    assert_eq!(
        h.server.world().read(at()),
        world::CellRead::Known(Cell::AIR)
    );
    assert!(h.server.world().component(ComponentId(1)).is_none());
    let i = h.inventory();
    assert_eq!(i.hand().damage(), 0);
    let saved = *i
        .slots()
        .iter()
        .find(|s| s.item() == ENERGIZED_SMELTER)
        .unwrap();
    let restored = h
        .server
        .item_payloads()
        .resolve(saved.payload().unwrap(), saved.item())
        .unwrap()
        .resolve_machine(saved.item())
        .unwrap();
    assert_eq!(restored.energy, 4020);
    assert_eq!(restored.slots[1].charge, 180_000);
    assert!(h.server.item_entities().is_empty());
    drop(h);
    let s = Server::open(d.config()).unwrap();
    assert_eq!(s.world().read(at()), world::CellRead::Known(Cell::AIR));
    assert!(
        s.item_payloads()
            .resolve(saved.payload().unwrap(), saved.item())
            .is_ok()
    );
}
#[test]
fn upgrade_shift_installs_up_to_eight_and_consumes_only_installed() {
    let d = Directory::new();
    let mut m = Machine::new(MekKind::Smelter);
    m.upgrades[0] = 6;
    let mut h = H::start(&d, ENERGIZED_SMELTER, Some(m));
    h.give(I_UPGRADE_SPEED, 5);
    assert_eq!(h.tool(true).status, ActionStatus::Accepted);
    assert_eq!(h.machine().upgrades[0], 8);
    assert_eq!(h.inventory().hand().count(), 3);
    assert_eq!(h.tool(true).status, ActionStatus::Accepted);
    assert_eq!(h.inventory().hand().count(), 3);
}
#[test]
fn connector_center_color_and_arm_modes_are_durable_without_free_flow() {
    let d = Directory::new();
    let mut h = H::start(&d, LOGISTICAL_TRANSPORTER, None);
    h.give(I_CONFIGURATOR, 1);
    assert_eq!(h.tool(true).status, ActionStatus::Accepted);
    let c = h.server.machine_runtime().transmitter_config(at()).unwrap();
    assert_eq!(c.color, 1);
    assert_eq!(c.modes, 0);
    for _ in 1..19 {
        assert_eq!(h.tool(true).status, ActionStatus::Accepted);
    }
    assert_eq!(
        h.server
            .machine_runtime()
            .transmitter_config(at())
            .unwrap()
            .color,
        0
    );
    drop(h);
    let s = Server::open(d.config()).unwrap();
    assert_eq!(
        s.machine_runtime().transmitter_config(at()).unwrap().color,
        0
    );
}
#[test]
fn stale_remote_or_occluded_tool_requests_leave_every_owner_unchanged() {
    let d = Directory::new();
    let mut h = H::start(&d, ENERGIZED_SMELTER, Some(Machine::new(MekKind::Smelter)));
    h.give(I_CONFIGURATOR, 1);
    let before = h.machine();
    let guard = h.guard();
    let rejected = h.action(Action::UseBlockTool {
        position: BlockPos { x: 12, ..at() },
        expected: Cell::from_parts(ENERGIZED_SMELTER, 2),
        sneaking: true,
        inventory: guard,
    });
    assert_eq!(
        rejected.status,
        ActionStatus::Rejected(RejectReason::Occluded)
    );
    let rejected = h.action(Action::UseBlockTool {
        position: at(),
        expected: Cell::from_parts(ENERGIZED_SMELTER, 2),
        sneaking: true,
        inventory: InventoryGuard {
            revision: guard.revision + 1,
            ..guard
        },
    });
    assert_eq!(rejected.status, ActionStatus::Rejected(RejectReason::Stale));
    assert_eq!(h.machine(), before);
}

#[test]
fn transmitter_real_upper_arm_changes_only_that_side_not_center_color() {
    let d = Directory::new();
    let mut h = H::start(&d, LOGISTICAL_TRANSPORTER, None);
    h.give(I_CONFIGURATOR, 1);
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    tx.set_cell(
        BlockPos { y: 3, ..at() },
        Cell::from_parts(LOGISTICAL_TRANSPORTER, 0),
    );
    assert!(h.server.queue_transaction(tx).is_ok());
    h.settle();
    for expected in [1, 2, 3] {
        let action = h.intent(true);
        let id = h.next;
        h.next += 1;
        assert_eq!(
            h.action_id(id, action, [0., -6.]).status,
            ActionStatus::Accepted
        );
        h.settle();
        let config = h.server.machine_runtime().transmitter_config(at()).unwrap();
        assert_eq!((config.modes >> 2) & 3, expected); // actual upper arm, Face::Up
        assert_eq!(config.modes & !12, 0);
        assert_eq!(config.color, 0);
    }
}
#[test]
fn all_nonitem_transmitter_centers_cycle_clicked_face_normal_push_pull_none() {
    for block in [UNIVERSAL_CABLE, MECHANICAL_PIPE, PRESSURIZED_TUBE] {
        let d = Directory::new();
        let mut h = H::start(&d, block, None);
        h.give(I_CONFIGURATOR, 1);
        for expected in [1, 2, 3, 0] {
            assert_eq!(h.tool(true).status, ActionStatus::Accepted);
            let config = h.server.machine_runtime().transmitter_config(at()).unwrap();
            assert_eq!((config.modes >> 4) & 3, expected); // hit North
            assert_eq!(config.modes & !48, 0);
            assert_eq!(config.color, 0);
        }
    }
}
#[test]
fn wrench_full_inventory_throws_one_tagged_item_with_forty_tick_delay() {
    let d = Directory::new();
    let mut machine = Machine::new(MekKind::Smelter);
    machine.energy = 4020;
    let mut h = H::start(&d, ENERGIZED_SMELTER, Some(machine));
    h.give(I_CONFIGURATOR, 1);
    h.mode(3);
    for _ in 0..35 {
        h.give(STONE, 64);
    }
    assert!(h.inventory().main().iter().all(|s| !s.is_empty()));
    let action = h.intent(true);
    let id = h.next;
    h.next += 1;
    assert_eq!(
        h.action_id(id, action.clone(), [0., 0.]).status,
        ActionStatus::Accepted
    );
    let item = *h
        .server
        .item_entities()
        .rows()
        .first()
        .expect("same durable item row");
    assert_eq!(item.stack.item(), ENERGIZED_SMELTER);
    assert!(item.thrown);
    assert_eq!(item.pickup_delay, 40);
    assert_eq!(
        item.position[1],
        1. + f64::from(1.62_f32) - f64::from(0.3_f32)
    );
    assert_eq!(
        h.server
            .item_payloads()
            .resolve(item.stack.payload().unwrap(), item.stack.item())
            .unwrap()
            .resolve_machine(item.stack.item())
            .unwrap()
            .energy,
        4020
    );
    h.settle();
    assert_eq!(
        h.action_id(id, action, [0., 0.]).status,
        ActionStatus::Accepted
    );
    assert_eq!(h.server.item_entities().len(), 1);
}
#[test]
fn unknown_neighbor_rejects_connector_intent_before_config_or_inventory_changes() {
    let d = Directory::new();
    let mut config = d.config();
    config.spawn_override = Some([12.5, 1., 8.5]);
    let mut w = fixture(STONE, None);
    let target = BlockPos {
        x: 15,
        y: 2,
        z: 8,
        space: SpaceId(0),
    };
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(target, Cell::from_parts(UNIVERSAL_CABLE, 0));
    w.commit(tx).unwrap();
    let mut h = H {
        server: Server::open_with_world(config, w).unwrap(),
        now: 0,
        out: Vec::new(),
        next: 1,
    };
    h.send(d.owner());
    h.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    h.settle();
    h.give(I_CONFIGURATOR, 1);
    let before = h.server.machine_runtime().transmitter_config(target);
    let inventory = h.inventory();
    let world::CellRead::Known(expected) = h.server.world().read(target) else {
        panic!()
    };
    let id = h.next;
    h.next += 1;
    assert_eq!(
        h.action_id(
            id,
            Action::UseBlockTool {
                position: target,
                expected,
                sneaking: true,
                inventory: h.guard()
            },
            [-90., 0.]
        )
        .status,
        ActionStatus::Rejected(RejectReason::Unknown)
    );
    assert_eq!(
        h.server.machine_runtime().transmitter_config(target),
        before
    );
    assert_eq!(h.inventory(), inventory);
}

#[test]
fn connector_durable_boundaries_never_omit_an_unrelated_recipe_tick() {
    let d = Directory::new();
    let mut h = H::start(&d, LOGISTICAL_TRANSPORTER, None);
    h.give(I_CONFIGURATOR, 1);
    let p = BlockPos { x: 4, ..at() };
    let mut machine = Machine::new(MekKind::Smelter);
    machine.energy = 20_000;
    machine.slots[0] = Slot::new(ItemStack::new(IRON_ORE, 1).unwrap());
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    tx.set_cell(p, Cell::from_parts(ENERGIZED_SMELTER, 2));
    tx.set_component(
        ComponentId(2),
        server::machines::machine_component(p, &machine),
        ChannelMask::CONTENTS,
    );
    assert!(h.server.queue_transaction(tx).is_ok());
    h.settle();
    for _ in 0..8 {
        let before = Machine::decode(
            &h.server
                .world()
                .component(ComponentId(2))
                .unwrap()
                .data
                .payload,
        )
        .unwrap();
        let tick = h.server.current_tick();
        assert_eq!(h.tool(true).status, ActionStatus::Accepted);
        let after = Machine::decode(
            &h.server
                .world()
                .component(ComponentId(2))
                .unwrap()
                .data
                .payload,
        )
        .unwrap();
        let elapsed = h.server.current_tick().0 - tick.0;
        assert_eq!(
            u64::from(after.progress - before.progress),
            elapsed,
            "connector edits must not pause unrelated manufacture"
        );
        assert_eq!(before.energy - after.energy, 50 * elapsed);
    }
}
