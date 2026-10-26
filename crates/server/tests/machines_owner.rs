//! Canonical Server/WAL integration, using trusted world fixtures rather than
//! client-supplied inventory or a detached phase demonstration.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use net::{ClientMessage, Hello, ServerMessage};
use rules::{
    blocks::MekKind,
    ids::*,
    items::I_ENERGY_TABLET,
    machines::{Machine, SideMode, Slot},
    stack::ItemStack,
    survival::furnace::Chest,
};
use server::{Server, ServerConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
static NEXT: AtomicU64 = AtomicU64::new(1);
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
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
                "machines-owner-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.generation_enabled = false;
        c.spawn_override = Some([2.5, 1., 8.5]);
        c
    }
    fn owner(&self) -> Hello {
        let ClientMessage::Hello(hello) =
            net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
        else {
            panic!("owner identity")
        };
        hello
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    for x in 0..16 {
        for z in 0..16 {
            cells[x + 16 * z] = Cell::from_parts(STONE, 0);
        }
    }
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
    let mut machine = Machine::new(MekKind::Smelter);
    machine.eject = true;
    machine.sides = [SideMode::Output; 6];
    machine.slots[1] = Slot {
        stack: ItemStack::new(I_ENERGY_TABLET, 1).unwrap(),
        charge: 180_000,
    };
    tx.set_cell(p(9), Cell::from_parts(ENERGIZED_SMELTER, 3));
    tx.set_component(
        ComponentId(1),
        server::machines::machine_component(p(9), &machine),
        ChannelMask::CONTENTS,
    );
    for x in 10..=11 {
        tx.set_cell(p(x), Cell::from_parts(LOGISTICAL_TRANSPORTER, 0));
    }
    tx.set_cell(p(12), Cell::from_parts(CHEST, 0));
    tx.set_component(
        ComponentId(2),
        server::survival::chest_component(p(12), &Chest::default()),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    world
}
struct H {
    server: Server,
    now: u64,
}
impl H {
    fn step(&mut self) {
        self.now += 10;
        self.server.tick(self.now);
        assert!(!self.server.is_failed());
        thread::sleep(Duration::from_micros(500));
    }
    fn join(&mut self, owner: Hello) {
        let mut welcome = self
            .server
            .handle(1, ClientMessage::Hello(owner), self.now)
            .into_iter()
            .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !welcome {
            self.now += 10;
            welcome = self
                .server
                .tick(self.now)
                .into_iter()
                .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
            assert!(!self.server.is_failed());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_micros(500));
        }
    }
    fn durable(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.server.awaiting_durability() {
            self.server.tick(self.now);
            assert!(!self.server.is_failed());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_micros(500));
        }
    }
    fn source_taken(&mut self) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let source =
                server::machines::read_machine(self.server.world(), ComponentId(1)).unwrap();
            if source.slots[1].stack.is_empty() {
                self.durable();
                return self.server.current_tick().0;
            }
            assert!(Instant::now() < deadline, "source never ejected");
            self.step();
        }
    }
    fn advance_to(&mut self, tick: u64) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.server.current_tick().0 < tick {
            assert!(Instant::now() < deadline);
            self.step();
        }
        self.durable();
    }
    fn target(&self) -> ItemStack {
        Chest::decode(
            &self
                .server
                .world()
                .component(ComponentId(2))
                .unwrap()
                .data
                .payload,
        )
        .unwrap()
        .slots()[0]
    }
}
#[test]
fn canonical_typed_packet_crash_recovery_preserves_exact_active_delivery_time() {
    let directory = Directory::new();
    let mut h = H {
        server: Server::open_with_world(directory.config(), fixture()).unwrap(),
        now: 0,
    };
    let owner = directory.owner();
    h.join(owner.clone());
    let taken = h.source_taken();
    h.advance_to(taken + 15);
    assert!(h.target().is_empty());
    assert_eq!(h.server.item_payloads().snapshot().records.len(), 1);
    assert!(h.server.item_entities().is_empty());
    drop(h);
    let mut h = H {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
    };
    h.join(owner);
    assert!(h.target().is_empty());
    // Authentication can consume a logical tick with no ready observer. That
    // tick deliberately sleeps the page; account for the persisted page clock,
    // rather than charging an offline tick against in-flight travel time.
    while !h
        .server
        .machine_runtime()
        .page_clock(p(10).page())
        .unwrap()
        .active
    {
        h.step();
    }
    let offset = h
        .server
        .machine_runtime()
        .page_clock(p(10).page())
        .unwrap()
        .offset;
    let delivery = taken + 39 + offset;
    h.advance_to(delivery - 1);
    assert!(h.target().is_empty());
    h.advance_to(delivery);
    let stack = h.target();
    assert_eq!(stack.item(), I_ENERGY_TABLET);
    assert_eq!(stack.count(), 1);
    assert_eq!(stack.damage(), 0);
    let id = stack
        .payload()
        .expect("charge payload survives transport and crash");
    assert_eq!(
        h.server
            .item_payloads()
            .resolve(id, I_ENERGY_TABLET)
            .unwrap()
            .joules(I_ENERGY_TABLET),
        Ok(180_000)
    );
    assert!(h.server.item_entities().is_empty());
    h.durable();
    drop(h);
    let reopened = Server::open(directory.config()).unwrap();
    let stack = Chest::decode(
        &reopened
            .world()
            .component(ComponentId(2))
            .unwrap()
            .data
            .payload,
    )
    .unwrap()
    .slots()[0];
    assert_eq!(stack.payload(), Some(id));
    assert_eq!(
        reopened
            .item_payloads()
            .resolve(id, I_ENERGY_TABLET)
            .unwrap()
            .joules(I_ENERGY_TABLET),
        Ok(180_000)
    );
}

#[test]
fn canonical_removed_pipe_spawns_one_durable_typed_item_without_overwriting_replacement() {
    let directory = Directory::new();
    let mut h = H {
        server: Server::open_with_world(directory.config(), fixture()).unwrap(),
        now: 0,
    };
    h.join(directory.owner());
    let taken = h.source_taken();
    h.advance_to(taken + 5);
    let packet = h.server.machine_runtime().packet(1).unwrap().clone();
    let at = packet.at().unwrap();
    let now = h.server.current_tick().0;
    let mut tx = Transaction::new(Tick(now + 1), SpaceId(0));
    tx.set_cell(at, Cell::from_parts(ENERGY_CUBE, 3));
    tx.set_component(
        ComponentId(3),
        server::machines::machine_component(at, &Machine::new(MekKind::Cube)),
        ChannelMask::CONTENTS,
    );
    assert!(h.server.queue_transaction(tx).is_ok());
    h.advance_to(now + 1);
    assert_eq!(
        h.server.world().read(at),
        world::CellRead::Known(Cell::from_parts(ENERGY_CUBE, 3))
    );
    assert_eq!(h.server.machine_runtime().packet_count(), 0);
    assert_eq!(h.server.item_entities().len(), 1);
    let item = h.server.item_entities().rows()[0];
    assert_eq!(item.stack, packet.stack);
    assert_eq!(
        h.server
            .item_payloads()
            .resolve(item.stack.payload().unwrap(), I_ENERGY_TABLET)
            .unwrap()
            .joules(I_ENERGY_TABLET),
        Ok(180_000)
    );
    assert!(h.target().is_empty());
    drop(h);
    let reopened = Server::open(directory.config()).unwrap();
    assert_eq!(reopened.machine_runtime().packet_count(), 0);
    assert_eq!(reopened.item_entities().rows(), [item]);
    assert_eq!(
        reopened.world().read(at),
        world::CellRead::Known(Cell::from_parts(ENERGY_CUBE, 3))
    );
}
