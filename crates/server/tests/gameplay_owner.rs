//! Actual Server phases, WAL barriers and owned windows, beyond detached planners.
use core_types::{BlockPos, Cell, ComponentId, SpaceId, Tick};
use net::{Action, ActionId, ActionStatus, ClientMessage, Hello, ServerMessage};
use rules::{ids::*, items::*, stack::ItemStack};
use server::{Outgoing, Server, ServerConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
static NEXT: AtomicU64 = AtomicU64::new(1);
const COMPONENT: ComponentId = ComponentId(500);
fn target() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 2,
        z: 11,
    }
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "gameplay-owner-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.generation_enabled = false;
        c.spawn_override = Some([8.5, 1., 8.5]);
        c
    }
    fn owner(&self) -> Hello {
        let ClientMessage::Hello(h) =
            net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
        else {
            panic!("owner")
        };
        h
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(block: u16, data: world::ComponentData) -> World {
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    for x in 0..16 {
        for z in 0..16 {
            cells[x + 16 * z] = Cell::from_parts(STONE, 0);
        }
    }
    cells[target().local_index().get() as usize] = Cell::from_parts(block, 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(target().page(), cells);
    tx.set_component(COMPONENT, data, core_types::ChannelMask::CONTENTS);
    world.commit(tx).unwrap();
    world
}
struct H {
    server: Server,
    now: u64,
    out: Vec<Outgoing>,
}
impl H {
    fn step(&mut self) {
        self.now += 10;
        self.out.extend(self.server.tick(self.now));
        assert!(!self.server.is_failed(), "canonical phase failed");
        thread::sleep(Duration::from_micros(500));
    }
    fn until(&mut self, mut p: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(20);
        for _ in 0..10_000 {
            if let Some(i) = self.out.iter().position(&mut p) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < deadline, "real durable phase deadline");
            self.step();
        }
        panic!("expected canonical server output");
    }
    fn send(&mut self, msg: ClientMessage) {
        self.out.extend(self.server.handle(1, msg, self.now));
    }
    fn join(&mut self, hello: Hello) {
        self.send(ClientMessage::Hello(hello));
        self.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    }
    fn action(&mut self, id: u64, action: Action) {
        self.send(ClientMessage::Action {
            id: ActionId(id),
            action,
        });
        let out=self.until(|o|matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        let ServerMessage::Action(r) = out.message else {
            unreachable!()
        };
        assert_eq!(r.status, ActionStatus::Accepted, "action {id}");
    }
    fn window(
        &mut self,
        mut p: impl FnMut(&net::window::WindowSnapshot) -> bool,
    ) -> net::window::WindowSnapshot {
        let out = self.until(|o| matches!(&o.message,ServerMessage::Window(w) if p(w)));
        let ServerMessage::Window(w) = out.message else {
            unreachable!()
        };
        *w
    }
}
#[test]
fn canonical_furnace_tick200_output_and_owned_window_recover_with_wal() {
    let directory = Directory::new();
    let mut furnace = rules::survival::furnace::Furnace::default();
    furnace
        .set_trusted(0, ItemStack::new(SAND, 1).unwrap())
        .unwrap();
    furnace
        .set_trusted(1, ItemStack::new(I_COAL, 1).unwrap())
        .unwrap();
    let world = fixture(
        FURNACE,
        server::survival::furnace_component(target(), &furnace),
    );
    let mut h = H {
        server: Server::open_with_world(directory.config(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner = directory.owner();
    h.join(owner.clone());
    let world::CellRead::Known(expected) = h.server.world().read(target()) else {
        panic!("known furnace")
    };
    h.action(
        1,
        Action::Window(net::window::WindowIntent::OpenBlock {
            position: target(),
            expected,
        }),
    );
    let start = rules::survival::furnace::Furnace::decode(
        &h.server.world().component(COMPONENT).unwrap().data.payload,
    )
    .unwrap();
    let expected = h.server.current_tick().0 + 200 - u64::from(start.cook());
    for _ in 0..10_000 {
        let value = rules::survival::furnace::Furnace::decode(
            &h.server.world().component(COMPONENT).unwrap().data.payload,
        )
        .unwrap();
        if !value.slots()[2].is_empty() {
            assert_eq!(h.server.current_tick().0, expected);
            assert_eq!(value.slots()[2].item(), GLASS);
            assert_eq!(value.slots()[2].count(), 1);
            assert_eq!(value.burn(), 1401);
            break;
        }
        h.step();
    }
    h.window(|w| {
        w.container
            .get(2)
            .is_some_and(|s| s.item == GLASS && s.count == 1)
    });
    assert!(h.server.counters().survival_events >= 200);
    drop(h);
    let mut h = H {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.join(owner);
    let window = h.window(|w| {
        w.container
            .get(2)
            .is_some_and(|s| s.item == GLASS && s.count == 1)
    });
    h.action(
        2,
        Action::Window(net::window::WindowIntent::Click {
            guard: window.guard,
            slot: 43,
            right: false,
            shift: false,
        }),
    );
    let cursor = h.window(|w| w.cursor.item == GLASS && w.cursor.count == 1);
    h.action(
        3,
        Action::Window(net::window::WindowIntent::Close {
            guard: cursor.guard,
        }),
    );
    h.window(|w| w.closed);
    let out=h.until(|o|matches!(&o.message,ServerMessage::Inventory{data} if rules::survival::Inventory::decode(data).is_ok_and(|i|i.get(0).is_some_and(|s|s.item()==GLASS&&s.count()==1))));
    assert!(matches!(out.message, ServerMessage::Inventory { .. }));
    let after = rules::survival::furnace::Furnace::decode(
        &h.server.world().component(COMPONENT).unwrap().data.payload,
    )
    .unwrap();
    assert!(after.slots()[2].is_empty());
}
#[test]
fn canonical_machine_spends_exact10000j_over200_ticks_and_restores_output() {
    use rules::{
        blocks::MekKind,
        machines::{Machine, Slot},
    };
    let directory = Directory::new();
    let mut machine = Machine::new(MekKind::Smelter);
    machine.energy = 10_000;
    machine.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
    let world = fixture(
        ENERGIZED_SMELTER,
        server::machines::machine_component(target(), &machine),
    );
    let mut h = H {
        server: Server::open_with_world(directory.config(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner = directory.owner();
    h.join(owner.clone());
    let world::CellRead::Known(expected) = h.server.world().read(target()) else {
        panic!("known machine")
    };
    h.action(
        1,
        Action::Window(net::window::WindowIntent::OpenBlock {
            position: target(),
            expected,
        }),
    );
    let start = server::machines::read_machine(h.server.world(), COMPONENT).unwrap();
    let consumed = (10_000 - start.energy) / 50;
    let expected = h.server.current_tick().0 + 200 - consumed;
    for _ in 0..10_000 {
        let value = server::machines::read_machine(h.server.world(), COMPONENT).unwrap();
        if !value.slots[1].stack.is_empty() {
            assert_eq!(h.server.current_tick().0, expected);
            assert_eq!(value.energy, 0);
            assert_eq!(value.slots[1].stack.item(), I_IRON_INGOT);
            assert_eq!(value.slots[1].stack.count(), 1);
            break;
        }
        h.step();
    }
    h.window(|w| {
        w.container
            .get(1)
            .is_some_and(|s| s.item == I_IRON_INGOT && s.count == 1)
    });
    assert!(h.server.counters().machine_steps >= 200);
    drop(h);
    let mut h = H {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.join(owner);
    let frame = h.window(|w| {
        w.container
            .get(1)
            .is_some_and(|s| s.item == I_IRON_INGOT && s.count == 1)
    });
    h.action(
        2,
        Action::Window(net::window::WindowIntent::Click {
            guard: frame.guard,
            slot: 42,
            right: false,
            shift: false,
        }),
    );
    h.window(|w| w.cursor.item == I_IRON_INGOT && w.cursor.count == 1);
    assert!(
        server::machines::read_machine(h.server.world(), COMPONENT)
            .unwrap()
            .slots[1]
            .stack
            .is_empty()
    );
}

#[test]
fn charged_tablet_moves_machine_cursor_inventory_and_restart_without_damage_tokens() {
    use net::window::WindowIntent as W;
    use rules::{
        blocks::MekKind,
        machines::{Machine, Slot},
    };
    let directory = Directory::new();
    let mut machine = Machine::new(MekKind::Heat);
    machine.slots[1] = Slot {
        stack: ItemStack::new(I_ENERGY_TABLET, 1).unwrap(),
        charge: 180_000,
    };
    let world = fixture(
        HEAT_GENERATOR,
        server::machines::machine_component(target(), &machine),
    );
    let mut h = H {
        server: Server::open_with_world(directory.config(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner = directory.owner();
    h.join(owner.clone());
    let world::CellRead::Known(expected) = h.server.world().read(target()) else {
        panic!("known generator")
    };
    h.action(
        1,
        Action::Window(W::OpenBlock {
            position: target(),
            expected,
        }),
    );
    let opened = h.window(|w| {
        w.container
            .get(1)
            .is_some_and(|s| s.item == I_ENERGY_TABLET && s.payload != 0)
    });
    assert_eq!(
        h.server.item_payloads().len(),
        0,
        "cache-only charge view never interns owned data"
    );
    // A persisted projected container is a cache, not another owned item root.
    // It may carry a deterministic charge ID without a ledger entry.
    drop(h);
    let mut h = H {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.join(owner.clone());
    let restored_cache = h.window(|w| {
        w.container
            .get(1)
            .is_some_and(|s| s.payload == opened.container[1].payload)
    });
    assert_eq!(h.server.item_payloads().len(), 0);
    let opened = restored_cache;
    h.action(
        2,
        Action::Window(W::Click {
            guard: opened.guard,
            slot: 42,
            right: false,
            shift: false,
        }),
    );
    let taken = h.window(|w| w.cursor.item == I_ENERGY_TABLET && w.cursor.payload != 0);
    let id = rules::item_payload::PayloadId(taken.cursor.payload);
    assert_eq!(
        taken.cursor.damage, 0,
        "charge is never encoded as durability"
    );
    assert_eq!(
        h.server
            .item_payloads()
            .resolve(id, I_ENERGY_TABLET)
            .unwrap()
            .joules(I_ENERGY_TABLET)
            .unwrap(),
        180_000
    );
    let empty = server::machines::read_machine(h.server.world(), COMPONENT).unwrap();
    assert!(empty.slots[1].stack.is_empty());
    assert_eq!(empty.slots[1].charge, 0);
    assert_eq!(h.server.item_payloads().len(), 1);
    h.action(3, Action::Window(W::Close { guard: taken.guard }));
    h.window(|w| w.closed);
    let inv=h.until(|o|matches!(&o.message,ServerMessage::Inventory{data} if rules::survival::Inventory::decode(data).is_ok_and(|i|i.get(0).is_some_and(|s|s.payload()==Some(id)))));
    assert!(matches!(inv.message, ServerMessage::Inventory { .. }));
    drop(h);
    let mut h = H {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.join(owner);
    assert_eq!(
        h.server
            .item_payloads()
            .resolve(id, I_ENERGY_TABLET)
            .unwrap()
            .joules(I_ENERGY_TABLET)
            .unwrap(),
        180_000
    );
    let world::CellRead::Known(expected) = h.server.world().read(target()) else {
        panic!("known generator")
    };
    h.action(
        4,
        Action::Window(W::OpenBlock {
            position: target(),
            expected,
        }),
    );
    let open = h.window(|w| !w.closed && w.guard.id >= 3);
    h.action(
        5,
        Action::Window(W::Click {
            guard: open.guard,
            slot: 0,
            right: false,
            shift: false,
        }),
    );
    let cursor = h.window(|w| w.cursor.payload == id.0);
    h.action(
        6,
        Action::Window(W::Click {
            guard: cursor.guard,
            slot: 42,
            right: false,
            shift: false,
        }),
    );
    h.window(|w| {
        w.cursor.item == 0
            && w.container
                .get(1)
                .is_some_and(|s| s.item == I_ENERGY_TABLET && s.payload == id.0)
    });
    let restored = server::machines::read_machine(h.server.world(), COMPONENT).unwrap();
    assert_eq!(restored.slots[1].charge, 180_000);
    assert_eq!(restored.slots[1].stack.payload(), None);
    assert_eq!(restored.slots[1].stack.damage(), 0);
    assert_eq!(
        h.server.item_payloads().len(),
        1,
        "identical content is reused, never deleted on one transfer"
    );
}

#[test]
fn canonical_emission_light_is_durable_and_unknown_sky_stays_dark() {
    let directory = Directory::new();
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(target().page(), vec![Cell::AIR; 4096]);
    tx.set_cell(target(), Cell::from_parts(TORCH, 0));
    world.commit(tx).unwrap();
    let adjacent = BlockPos {
        x: target().x + 1,
        ..target()
    };
    let mut h = H {
        server: Server::open_with_world(directory.config(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        h.step();
        let light = h
            .server
            .world()
            .page(adjacent.page())
            .unwrap()
            .light(adjacent.local_index());
        if light.block > 0 && h.server.request_checkpoint() {
            break;
        }
        assert!(Instant::now() < deadline, "authority emission convergence");
    }
    let expected = h
        .server
        .world()
        .page(adjacent.page())
        .unwrap()
        .light(adjacent.local_index());
    assert!(expected.block >= 12);
    assert_eq!(
        expected.sky, 0,
        "no certified sky across unknown authority gaps"
    );
    assert!(h.server.counters().light_updates > 0);
    drop(h);
    let server = Server::open(directory.config()).unwrap();
    let recovered = server
        .world()
        .page(adjacent.page())
        .unwrap()
        .light(adjacent.local_index());
    assert_eq!(
        recovered, expected,
        "derived light was in the durable checkpoint/WAL"
    );
}

#[test]
fn canonical_stateful_machine_mine_pickup_restart_and_place_preserve_one_payload() {
    use net::{InputFrame, InventoryGuard};
    use rules::{
        blocks::MekKind,
        machines::{Machine, Slot},
    };
    let directory = Directory::new();
    let mut machine = Machine::new(MekKind::Cube);
    machine.energy = 180_000;
    machine.slots[0] = Slot {
        stack: ItemStack::new(I_ENERGY_TABLET, 1).unwrap(),
        charge: 4020,
    };
    let mut world = fixture(
        ENERGY_CUBE,
        server::machines::machine_component(target(), &machine),
    );
    let mut support = Transaction::new(Tick(0), SpaceId(0));
    support.set_cell(
        BlockPos {
            z: target().z + 1,
            ..target()
        },
        Cell::from_parts(STONE, 0),
    );
    world.commit(support).unwrap();
    let mut h = H {
        server: Server::open_with_world(directory.config(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner = directory.owner();
    h.join(owner.clone());
    h.action(1, Action::Command(format!("/give {I_STEEL_PICKAXE} 1")));
    h.send(ClientMessage::Action {
        id: ActionId(2),
        action: Action::Mine {
            position: target(),
            expected: Cell::from_parts(ENERGY_CUBE, 0),
            tool: I_STEEL_PICKAXE,
        },
    });
    for sequence in 1..=240 {
        h.send(ClientMessage::Inputs(vec![InputFrame {
            sequence,
            keys: InputFrame::ATTACK,
            ..Default::default()
        }]));
        for _ in 0..5 {
            h.step();
        }
        if h.out.iter().any(|o|matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(2)&&r.status!=ActionStatus::Pending)){break;}
    }
    let reply=h.until(|o|matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(2)&&r.status!=ActionStatus::Pending));
    assert!(
        matches!(reply.message,ServerMessage::Action(r) if r.status==ActionStatus::Accepted),
        "{:?}",
        reply.message
    );
    for _ in 0..500 {
        if !h.server.item_entities().is_empty() {
            break;
        }
        h.step();
    }
    let item = *h
        .server
        .item_entities()
        .rows()
        .first()
        .expect("one durable machine item");
    assert_eq!(h.server.item_entities().len(), 1);
    assert_eq!(item.stack.item(), ENERGY_CUBE);
    assert_eq!(item.stack.count(), 1);
    let reference = item
        .stack
        .payload()
        .expect("immutable donor-retained machine state");
    let payload = h
        .server
        .item_payloads()
        .resolve(reference, ENERGY_CUBE)
        .unwrap();
    let saved = payload.resolve_machine(ENERGY_CUBE).unwrap();
    assert_eq!(saved.energy + u64::from(saved.slots[0].charge), 184_020);
    assert_eq!(saved.slots[0].stack.item(), I_ENERGY_TABLET);
    assert_eq!(saved.progress, 0);
    h.action(
        3,
        Action::Command(format!("/tp {} 1 {}", item.position[0], item.position[2])),
    );
    let output=h.until(|o|matches!(&o.message,ServerMessage::Inventory{data} if rules::survival::Inventory::decode(data).is_ok_and(|inv|inv.slots().iter().any(|s|s.payload()==Some(reference)))));
    let ServerMessage::Inventory { data } = output.message else {
        unreachable!()
    };
    let inventory = rules::survival::Inventory::decode(&data).unwrap();
    assert_eq!(
        inventory.get(0).unwrap().damage(),
        1,
        "one owned pickaxe wear"
    );
    assert!(h.server.item_entities().is_empty());
    drop(h);
    let mut h = H {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.join(owner.clone());
    let output=h.until(|o|matches!(&o.message,ServerMessage::Inventory{data} if rules::survival::Inventory::decode(data).is_ok_and(|inv|inv.slots().iter().any(|s|s.payload()==Some(reference)))));
    let ServerMessage::Inventory { data } = output.message else {
        unreachable!()
    };
    let inventory = rules::survival::Inventory::decode(&data).unwrap();
    let selected = inventory
        .slots()
        .iter()
        .position(|s| s.payload() == Some(reference))
        .unwrap() as u8;
    h.action(4, Action::Command("/tp 8.5 1 8.5".into()));
    h.action(5, Action::SelectSlot(selected));
    let output=h.until(|o|matches!(&o.message,ServerMessage::Inventory{data} if rules::survival::Inventory::decode(data).is_ok_and(|i|i.selected()==usize::from(selected))));
    let ServerMessage::Inventory { data } = output.message else {
        unreachable!()
    };
    let inventory = rules::survival::Inventory::decode(&data).unwrap();
    let held = inventory.hand();
    h.action(
        6,
        Action::PlaceOwned {
            position: target(),
            expected: Cell::AIR,
            cell: Cell::from_parts(ENERGY_CUBE, 0),
            inventory: InventoryGuard {
                revision: inventory.revision(),
                selected,
                item: held.item(),
                count: held.count(),
                damage: held.damage(),
                payload: reference.0,
            },
        },
    );
    let record = h
        .server
        .world()
        .components()
        .find(|(_, r)| {
            r.data.anchor == target() && r.data.kind == server::machines::MACHINE_COMPONENT
        })
        .unwrap();
    let restored = server::machines::read_machine(h.server.world(), record.0).unwrap();
    assert_eq!(
        restored.energy + u64::from(restored.slots[0].charge),
        184_020
    );
    assert_eq!(restored.slots[0].stack.item(), I_ENERGY_TABLET);
    assert_eq!(
        h.server
            .item_payloads()
            .resolve(reference, ENERGY_CUBE)
            .unwrap()
            .id(),
        reference,
        "immutable shared payload is retained for other identical roots"
    );
    drop(h);
    let server = Server::open(directory.config()).unwrap();
    let record = server
        .world()
        .components()
        .find(|(_, r)| {
            r.data.anchor == target() && r.data.kind == server::machines::MACHINE_COMPONENT
        })
        .unwrap();
    let restored = server::machines::read_machine(server.world(), record.0).unwrap();
    assert_eq!(
        restored.energy + u64::from(restored.slots[0].charge),
        184_020
    );
}

#[test]
fn wrong_tool_and_creative_machine_break_scatter_only_owned_slots() {
    use net::InputFrame;
    use rules::{
        blocks::MekKind,
        machines::{Machine, Slot},
    };
    for creative in [false, true] {
        let directory = Directory::new();
        let mut machine = Machine::new(MekKind::Smelter);
        machine.energy = 10_000;
        machine.slots[1] = Slot {
            stack: ItemStack::new(I_IRON_INGOT, 3).unwrap(),
            charge: 0,
        };
        let world = fixture(
            ENERGIZED_SMELTER,
            server::machines::machine_component(target(), &machine),
        );
        let mut h = H {
            server: Server::open_with_world(directory.config(), world).unwrap(),
            now: 0,
            out: Vec::new(),
        };
        h.join(directory.owner());
        let action = if creative {
            h.action(1, Action::Command("/gamemode creative".into()));
            2
        } else {
            1
        };
        h.send(ClientMessage::Action {
            id: ActionId(action),
            action: Action::Mine {
                position: target(),
                expected: Cell::from_parts(ENERGIZED_SMELTER, 0),
                tool: 0,
            },
        });
        for sequence in 1..=1000 {
            h.send(ClientMessage::Inputs(vec![InputFrame {
                sequence,
                keys: InputFrame::ATTACK,
                ..Default::default()
            }]));
            for _ in 0..5 {
                h.step();
            }
            if h.out.iter().any(|o|matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(action)&&r.status!=ActionStatus::Pending)){break;}
        }
        let reply=h.until(|o|matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(action)&&r.status!=ActionStatus::Pending));
        assert!(
            matches!(reply.message,ServerMessage::Action(r) if r.status==ActionStatus::Accepted),
            "{creative}: {:?}",
            reply.message
        );
        for _ in 0..500 {
            if !h.server.item_entities().is_empty() {
                break;
            }
            h.step();
        }
        assert_eq!(h.server.item_entities().len(), 1);
        let item = h.server.item_entities().rows()[0];
        assert_eq!(item.stack, ItemStack::new(I_IRON_INGOT, 3).unwrap());
        assert_eq!(
            h.server.item_payloads().len(),
            0,
            "no saved machine block/energy on donor wrong-tool or creative break"
        );
        assert_eq!(
            h.server.world().read(target()),
            world::CellRead::Known(Cell::AIR)
        );
    }
}
