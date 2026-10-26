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
use server::{Outgoing, Server, ServerConfig};
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
                "packet-stream-{}-{}",
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
    out: Vec<Outgoing>,
    raw: net::Replica,
}
impl H {
    fn step(&mut self) {
        self.now += 10;
        self.out.extend(self.server.tick(self.now));
        assert!(!self.server.is_failed());
        thread::sleep(Duration::from_micros(500));
    }
    fn send(&mut self, message: ClientMessage) {
        self.out.extend(self.server.handle(1, message, self.now));
    }
    fn until(&mut self, mut predicate: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(i) = self.out.iter().position(&mut predicate) {
                return self.out.remove(i);
            }
            assert!(
                Instant::now() < deadline,
                "packet courier deadline {:?}",
                self.server.packet_stream_counters()
            );
            self.step();
        }
    }
    fn join(&mut self, owner: Hello) {
        self.send(ClientMessage::Hello(owner));
        self.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    }
    fn raw_page(&mut self) -> u64 {
        let out=self.until(|o|matches!(&o.message,ServerMessage::Page(update) if match update{net::PageUpdate::Snapshot(s)=>s.page==p(10).page(),net::PageUpdate::Delta(d)=>d.page==p(10).page()}));
        let ServerMessage::Page(update) = out.message else {
            unreachable!()
        };
        if self.raw.apply(update).is_err() {
            self.send(ClientMessage::RequestPage {
                page: p(10).page(),
                acknowledged: None,
            });
            return self.raw_page();
        }
        self.raw.page(p(10).page()).unwrap().version
    }
    fn ack_page(&mut self, view: u64) {
        self.send(ClientMessage::AckPage {
            page: p(10).page(),
            version: view,
        });
    }
    fn packet(&mut self) -> net::machine_packets::Frame {
        loop {
            let out = self.until(|o| {
                matches!(
                    o.message,
                    ServerMessage::MachinePackets(_) | ServerMessage::Page(_)
                )
            });
            match out.message {
                ServerMessage::MachinePackets(frame) => {
                    let wire = net::encode_server(&ServerMessage::MachinePackets(frame)).unwrap();
                    let ServerMessage::MachinePackets(frame) = net::decode_server(&wire).unwrap()
                    else {
                        unreachable!()
                    };
                    return *frame;
                }
                ServerMessage::Page(update) => {
                    if self.raw.apply(update).is_err() {
                        self.send(ClientMessage::RequestPage {
                            page: p(10).page(),
                            acknowledged: None,
                        });
                    } else {
                        self.ack_page(self.raw.page(p(10).page()).unwrap().version);
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    fn advance(&mut self, tick: u64) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.server.current_tick().0 < tick {
            assert!(Instant::now() < deadline);
            self.step();
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.server.awaiting_durability() {
            self.out.extend(self.server.tick(self.now));
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_micros(500));
        }
    }
}
fn open(d: &Directory, w: World) -> H {
    let mut h = H {
        server: Server::open_with_world(d.config(), w).unwrap(),
        now: 0,
        out: vec![],
        raw: net::Replica::new(8),
    };
    h.join(d.owner());
    h
}
#[test]
fn canonical_pipe_packets_wait_for_raw_ack_and_credit_then_remove_against_confirmed_base() {
    let d = Directory::new();
    let mut h = open(&d, fixture());
    h.send(ClientMessage::RequestPage {
        page: p(10).page(),
        acknowledged: None,
    });
    let view = h.raw_page();
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(o.message, ServerMessage::MachinePackets(_)))
    );
    h.ack_page(view);
    let full = h.packet();
    assert!(full.base.is_none());
    assert_eq!(
        full.view_version.0,
        h.raw.page(p(10).page()).unwrap().version
    );
    assert_eq!(full.upserts.len(), 1);
    assert_eq!(full.upserts[0].item, I_ENERGY_TABLET);
    assert_eq!(full.upserts[0].count, 1);
    assert!(full.removes.is_empty());
    assert!(
        h.out
            .iter()
            .all(|o| !matches!(o.message, ServerMessage::MachinePackets(_)))
    );
    let bytes = full.encode().unwrap();
    assert_eq!(
        bytes.len(),
        net::machine_packets::HEADER_BYTES + net::machine_packets::ROW_BYTES
    );
    let payload = h
        .server
        .machine_runtime()
        .packet(1)
        .unwrap()
        .stack
        .payload()
        .unwrap()
        .0
        .to_le_bytes();
    assert!(!bytes.windows(8).any(|slice| slice == payload));
    h.advance(full.tick.0 + 45);
    assert_eq!(h.server.machine_runtime().packet_count(), 0);
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(o.message, ServerMessage::MachinePackets(_))),
        "ordinary updates must wait for page credit"
    );
    assert!(h.server.packet_stream_counters().credit_waits > 0);
    h.send(ClientMessage::MachinePackets(
        net::machine_packets::Request::Ack {
            page: full.page,
            view_version: full.view_version,
            revision: full.revision,
        },
    ));
    let removed = h.packet();
    assert_eq!(removed.base, Some(full.revision));
    assert!(removed.upserts.is_empty());
    assert_eq!(removed.removes, [1]);
    assert!(removed.revision > full.revision);
}
#[test]
fn packet_recovery_full_and_forged_view_requests_do_not_advance_confirmed_source() {
    let d = Directory::new();
    let mut h = open(&d, fixture());
    h.send(ClientMessage::RequestPage {
        page: p(10).page(),
        acknowledged: None,
    });
    let view = h.raw_page();
    h.ack_page(view);
    let full = h.packet();
    let before = h.server.packet_stream_counters().frames;
    h.send(ClientMessage::MachinePackets(
        net::machine_packets::Request::Full {
            page: full.page,
            view_version: core_types::Version(view + 999),
            have: None,
        },
    ));
    h.advance(h.server.current_tick().0 + 2);
    assert_eq!(h.server.packet_stream_counters().frames, before);
    h.send(ClientMessage::MachinePackets(
        net::machine_packets::Request::Full {
            page: full.page,
            view_version: full.view_version,
            have: None,
        },
    ));
    let recovery = h.packet();
    assert!(recovery.base.is_none());
    assert!(recovery.revision > full.revision);
    assert_eq!(recovery.view_version, full.view_version);
    assert!(recovery.tick >= full.tick);
}
#[test]
fn newly_created_pipe_location_is_withheld_until_exact_current_raw_cells_are_confirmed() {
    let d = Directory::new();
    let mut world = fixture();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_cell(p(10), Cell::from_parts(STONE, 0));
    let mut machine = server::machines::read_machine(&world, ComponentId(1)).unwrap();
    machine.eject = false;
    tx.set_component(
        ComponentId(1),
        server::machines::machine_component(p(9), &machine),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut h = open(&d, world);
    h.send(ClientMessage::RequestPage {
        page: p(10).page(),
        acknowledged: None,
    });
    let view = h.raw_page();
    assert_eq!(
        h.raw.read(p(10)),
        net::ReplicaRead::Known(Cell::from_parts(STONE, 0))
    );
    h.ack_page(view);
    h.advance(h.server.current_tick().0 + 2);
    let now = h.server.current_tick().0;
    let mut tx = Transaction::new(Tick(now + 1), SpaceId(0));
    tx.set_cell(p(10), Cell::from_parts(LOGISTICAL_TRANSPORTER, 0));
    let mut machine = server::machines::read_machine(h.server.world(), ComponentId(1)).unwrap();
    machine.eject = true;
    tx.set_component(
        ComponentId(1),
        server::machines::machine_component(p(9), &machine),
        ChannelMask::CONTENTS,
    );
    assert!(h.server.queue_transaction(tx).is_ok());
    // Ejection has the donor's 11-tick clock; topology repair and this clock
    // both remain live while the client deliberately keeps its old raw image.
    for tick in now + 1..=now + 20 {
        h.advance(tick);
        if h.server.machine_runtime().packet(1).is_some() {
            break;
        }
    }
    assert!(
        h.server.machine_runtime().packet(1).is_some(),
        "source {:?}, pending {:?}",
        server::machines::read_machine(h.server.world(), ComponentId(1)).unwrap(),
        h.server.machine_runtime().snapshot().events,
    );
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(o.message, ServerMessage::MachinePackets(_))),
        "unconfirmed model location must not leak via item pose"
    );
    // Consume and ACK the newest real raw updates. The packet source compares each
    // current conduit cell, including final arms, to that exact confirmed image.
    let frame = h.packet();
    assert!(!frame.upserts.is_empty());
    assert_eq!(
        frame.view_version.0,
        h.raw.page(frame.page).unwrap().version
    );
    for pose in &frame.upserts {
        let cell = h.raw.page(frame.page).unwrap().cells[usize::from(pose.index.get())];
        assert_eq!(cell.id(), LOGISTICAL_TRANSPORTER);
    }
}

#[test]
fn empty_startup_pages_create_no_packet_images_or_ack_pressure() {
    let d = Directory::new();
    let mut world = fixture();
    let mut machine = server::machines::read_machine(&world, ComponentId(1)).unwrap();
    machine.slots[1] = Slot::default();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        server::machines::machine_component(p(9), &machine),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut h = open(&d, world);
    h.send(ClientMessage::RequestPage {
        page: p(10).page(),
        acknowledged: None,
    });
    let view = h.raw_page();
    h.ack_page(view);
    h.advance(h.server.current_tick().0 + 30);
    let counters = h.server.packet_stream_counters();
    assert_eq!(counters.frames, 0);
    assert_eq!(counters.probes, 0);
    assert_eq!(counters.credit_waits, 0);
    assert!(
        h.out
            .iter()
            .all(|out| !matches!(out.message, ServerMessage::MachinePackets(_)))
    );
}
