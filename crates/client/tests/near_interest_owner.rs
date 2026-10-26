//! Actual canonical owner + shared Connection, with the GUI deliberately paused.
//! Socket/GPU timing is outside this bounded protocol-liveness fixture.
#![cfg(not(target_arch = "wasm32"))]
use client::{
    near_profile::{self, NearProfile},
    network::{Connection, NetworkConfig},
};
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use gen_authority::{PrivateSeed, ServerGenerator};
use net::{Action, ActionId, ActionStatus, ClientMessage, PageUpdate, ServerMessage};
use rules::{arms, ids::*, items::I_IRON_INGOT, stack::ItemStack, survival::furnace::Chest};
use server::{
    Server, ServerConfig,
    machines::{
        self,
        transport::{ClockRecord, Goal, Packet, PageClock},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
const PRIVATE: [u8; 32] = [37; 32];
static NEXT: AtomicU64 = AtomicU64::new(1);
fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn page(x: i32, y: i32, z: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn old_pipe() -> BlockPos {
    p(-47, 110, -47)
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "near-owner-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn prepare(d: &Directory) -> Server {
    let (mut store, _) = save::Store::open(&d.0).unwrap();
    store
        .load_or_create_private_entropy(|| Ok(PRIVATE))
        .unwrap();
    let generator = ServerGenerator::new(terrain_gen::TerrainSeed(1), PrivateSeed::new(PRIVATE));
    let mut w = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    let mut next = 1;
    for z in -3..=3 {
        for x in -3..=5 {
            let column = generator.generate_column(x, z).unwrap();
            for y in 0..8 {
                let id = page(x, y, z);
                let cells: Box<[Cell]> = column.page(id, None).unwrap().cells;
                tx.create_page(id, cells);
                let at = p(x * 16 + 2, y * 16 + 2, z * 16 + 2);
                tx.set_cell(at, Cell::from_parts(CHEST, 0));
                tx.set_component(
                    ComponentId(next),
                    server::survival::chest_component(at, &Chest::default()),
                    ChannelMask::CONTENTS,
                );
                next += 1;
            }
        }
    }
    let at = old_pipe();
    for x in at.x..=at.x + 2 {
        for y in at.y + 1..128 {
            tx.set_cell(p(x, y, at.z), Cell::AIR);
        }
    }
    for x in at.x..=at.x + 1 {
        tx.set_cell(
            p(x, at.y, at.z),
            Cell::from_parts(
                LOGISTICAL_TRANSPORTER,
                arms::with_arms(0, arms::arms_from_mask(63)),
            ),
        );
    }
    let target = p(at.x + 2, at.y, at.z);
    tx.set_cell(target, Cell::from_parts(CHEST, 0));
    tx.set_component(
        ComponentId(1_000_000),
        server::survival::chest_component(target, &Chest::default()),
        ChannelMask::CONTENTS,
    );
    // An exterior model on the new upper slab makes its public dependent
    // stream observable after moving vertically, without invented LIGHT.
    let upper = p(66, 126, 2);
    tx.set_cell(p(66, 127, 2), Cell::AIR)
        .set_cell(upper, Cell::from_parts(CHEST, 0));
    tx.set_component(
        ComponentId(1_000_001),
        server::survival::chest_component(upper, &Chest::default()),
        ChannelMask::CONTENTS,
    );
    w.commit(tx).unwrap();
    let mut image = machines::MachineRuntime::new(machines::Config::default())
        .unwrap()
        .snapshot();
    image.transport.next_packet = 2;
    image.transport.packets.push(Packet {
        id: 1,
        stack: ItemStack::new(I_IRON_INGOT, 1).unwrap(),
        color: 0,
        from: p(at.x - 1, at.y, at.z),
        path: vec![at, p(at.x + 1, at.y, at.z), target],
        step: 0,
        progress: 25,
        goal: Goal::Destination,
        home: None,
        idle_direction: None,
        destination: Some(ComponentId(1_000_000)),
        last_tick: Tick(0),
        due: Tick(1),
        first_due: Tick(1),
    });
    image.transport.clocks.push(ClockRecord {
        page: at.page(),
        clock: PageClock::default(),
    });
    for x in at.x..=at.x + 1 {
        image.configs.push(machines::TransmitterConfig {
            position: p(x, at.y, at.z),
            block: LOGISTICAL_TRANSPORTER,
            modes: 0,
            color: 0,
        });
    }
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
                payload: image.encode().unwrap(),
            }],
        })
        .unwrap();
    drop(store);
    let mut config = ServerConfig::new(&d.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 72., 8.5]);
    config.spawn_protection_radius = 0;
    Server::open(config).unwrap()
}
struct H {
    server: Server,
    client: Connection,
    now: u64,
    wanted: bool,
    replies: BTreeMap<ActionId, ActionStatus>,
    forgot: BTreeSet<PageId>,
    unavailable: BTreeSet<PageId>,
    old_raw: Option<PageUpdate>,
    old_light: Option<Box<net::light::Update>>,
    old_props: Option<Box<net::props::Frame>>,
    old_packets: Option<Box<net::machine_packets::Frame>>,
    max_lease: usize,
    max_gui: usize,
}
impl H {
    fn new(d: &Directory) -> Self {
        let server = prepare(d);
        let profile = NearProfile::plan(32, near_profile::Limits::default()).unwrap();
        assert_eq!(profile.raw_pages, 245);
        let mut config = NetworkConfig::remote_scene();
        profile.apply_network(&mut config);
        assert_eq!((config.maximum_pages, config.requested_pages()), (256, 245));
        let mut client = Connection::new(config).unwrap();
        let ClientMessage::Hello(hello) =
            net::decode_client(&std::fs::read(d.0.join("owner.identity")).unwrap()).unwrap()
        else {
            unreachable!()
        };
        client
            .hello(hello.name, hello.identity, hello.credential)
            .unwrap();
        Self {
            server,
            client,
            now: 0,
            wanted: false,
            replies: BTreeMap::new(),
            forgot: BTreeSet::new(),
            unavailable: BTreeSet::new(),
            old_raw: None,
            old_light: None,
            old_props: None,
            old_packets: None,
            max_lease: 0,
            max_gui: 0,
        }
    }
    fn collect(&mut self, rows: Vec<server::Outgoing>) {
        let old = old_pipe().page();
        for row in rows {
            let bytes = net::encode_server(&row.message).unwrap();
            assert!(!bytes.windows(32).any(|b| b == PRIVATE));
            let message = net::decode_server(&bytes).unwrap();
            match &message {
                ServerMessage::Action(reply) => {
                    self.replies.insert(reply.id, reply.status);
                }
                ServerMessage::Page(update)
                    if update_page(update) == old && self.old_raw.is_none() =>
                {
                    self.old_raw = Some(update.clone())
                }
                ServerMessage::Light(update)
                    if update.header().page == old && self.old_light.is_none() =>
                {
                    self.old_light = Some(update.clone())
                }
                ServerMessage::Props(frame) if frame.page == old && self.old_props.is_none() => {
                    self.old_props = Some(frame.clone())
                }
                ServerMessage::MachinePackets(frame)
                    if frame.page == old && self.old_packets.is_none() =>
                {
                    self.old_packets = Some(frame.clone())
                }
                ServerMessage::LightUnavailable(page) => {
                    self.unavailable.insert(*page);
                }
                _ => {}
            }
            self.client.receive(message, self.now).unwrap();
        }
        self.client
            .advance_to(self.client.acknowledged_tick())
            .unwrap();
        let queues = self.client.queue_depths();
        assert!(
            queues.cells <= 256
                && queues.light <= 256
                && queues.props <= 256
                && queues.outgoing <= 256
        );
        self.max_gui = self.max_gui.max(queues.cells);
        assert!(self.client.replica().len() <= 256 && self.client.light_replica().len() <= 256);
        if let Some(stats) = self.server.cold_projection_stats() {
            assert!(stats.raw_leases <= 256);
            self.max_lease = self.max_lease.max(stats.raw_leases);
        }
    }
    fn flush(&mut self) {
        // Network IO/ACK admission continues independently of the frozen GUI.
        for _ in 0..32 {
            let messages = self.client.take_outgoing(64);
            if messages.is_empty() {
                break;
            }
            for message in messages {
                if let ClientMessage::ForgetPage(page) = message {
                    self.forgot.insert(page);
                }
                let bytes = net::encode_client(&message).unwrap();
                let out = self
                    .server
                    .handle(1, net::decode_client(&bytes).unwrap(), self.now);
                self.collect(out);
            }
        }
    }
    fn step(&mut self) {
        self.now += 10;
        self.client.poll(self.now).unwrap();
        if self.wanted {
            self.client.update_interest(self.now).unwrap();
        }
        self.flush();
        let out = self.server.tick(self.now);
        self.collect(out);
        self.flush();
        assert!(
            !self.server.is_failed(),
            "{:?}",
            self.server.failure_reason()
        );
        thread::sleep(Duration::from_micros(500));
    }
    fn until(&mut self, mut ready: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !ready(self) {
            assert!(
                Instant::now() < deadline,
                "near liveness timeout tick{:?} client{} gui{:?} raw{:?} counters{:?}",
                self.server.current_tick(),
                self.client.replica().len(),
                self.client.queue_depths(),
                self.server.cold_projection_stats(),
                self.server.counters()
            );
            self.step();
        }
    }
    fn command(&mut self, text: &str) {
        let id = self
            .client
            .action(Action::Command(text.into()), false, self.now)
            .unwrap();
        self.flush();
        self.until(|h| {
            h.replies
                .get(&id)
                .is_some_and(|s| *s != ActionStatus::Pending)
        });
        assert_eq!(self.replies[&id], ActionStatus::Accepted);
    }
    fn wait_pages(&mut self, center: PageId) {
        let wanted = pages(center);
        self.until(|h| {
            wanted.iter().all(|p| {
                h.client.replica().page(*p).is_some_and(|raw| {
                    h.client
                        .light_replica()
                        .page(*p)
                        .is_some_and(|l| l.header.cell_view == raw.version)
                }) && h.client.props_frame(*p).is_some()
            })
        });
        assert!(
            self.client.queue_depths().cells >= 245,
            "GUI remains paused for the whole page volume"
        );
    }
}
fn update_page(update: &PageUpdate) -> PageId {
    match update {
        PageUpdate::Snapshot(s) => s.page,
        PageUpdate::Delta(d) => d.page,
    }
}
fn pages(center: PageId) -> BTreeSet<PageId> {
    let mut out = BTreeSet::new();
    for y in -2..=2 {
        for z in -3..=3 {
            for x in -3..=3 {
                out.insert(PageId {
                    x: center.x + x,
                    y: center.y + y,
                    z: center.z + z,
                    ..center
                });
            }
        }
    }
    out
}
#[test]
fn actual_245_page_profile_retires_old_x_and_y_slices_and_reenters_with_fresh_headers() {
    let d = Directory::new();
    let mut h = H::new(&d);
    h.until(|h| h.client.player().is_some());
    h.command("/gamemode spectator");
    h.wanted = true;
    h.wait_pages(page(0, 4, 0));
    let old = old_pipe().page();
    h.until(|h| {
        h.client
            .packet_scene()
            .page(old)
            .is_some_and(|p| p.get(1).is_some())
    });
    let before_raw = h.client.replica().page(old).unwrap().clone();
    let before_light = h.client.light_replica().page(old).unwrap().header;
    assert!(
        h.client
            .props_frame(old)
            .is_some_and(|f| !f.hints.is_empty())
    );
    let world_pages = h.server.world().loaded_pages();
    assert_eq!(world_pages, 9 * 7 * 8);
    h.command("/tp 24.5 72 8.5");
    h.until(|h| h.client.player().is_some_and(|p| p.pos[0] >= 24.));
    h.wait_pages(page(1, 4, 0));
    h.until(|h| h.forgot.contains(&old) && h.client.replica().page(old).is_none());
    assert!(
        h.client.light_replica().page(old).is_none()
            && h.client.props_frame(old).is_none()
            && h.client.packet_scene().page(old).is_none()
    );
    assert!(h.server.world().page(old).is_some());
    for message in [
        ServerMessage::Page(h.old_raw.clone().unwrap()),
        ServerMessage::Light(h.old_light.clone().unwrap()),
        ServerMessage::Props(h.old_props.clone().unwrap()),
        ServerMessage::MachinePackets(h.old_packets.clone().unwrap()),
    ] {
        let bytes = net::encode_server(&message).unwrap();
        h.client
            .receive(net::decode_server(&bytes).unwrap(), h.now)
            .unwrap();
    }
    assert!(
        h.client.replica().page(old).is_none()
            && h.client.light_replica().page(old).is_none()
            && h.client.props_frame(old).is_none()
            && h.client.packet_scene().page(old).is_none()
    );
    let request = ClientMessage::RequestLight(net::light::Request {
        page: old,
        cell_view: before_raw.version,
        acknowledged: None,
    });
    let out = h.server.handle(
        1,
        net::decode_client(&net::encode_client(&request).unwrap()).unwrap(),
        h.now,
    );
    h.collect(out);
    h.until(|h| h.unavailable.contains(&old));
    h.command("/tp 24.5 88 8.5");
    h.until(|h| h.client.player().is_some_and(|p| p.pos[1] >= 88.));
    h.wait_pages(page(1, 5, 0));
    assert!(
        h.forgot.iter().any(|p| p.y == 2),
        "old 49-page bottom slab must release capacity"
    );
    assert!(
        h.client
            .props_frame(page(4, 7, 0))
            .is_some_and(|f| !f.hints.is_empty())
    );
    h.command("/tp 8.5 72 8.5");
    h.until(|h| {
        h.client
            .player()
            .is_some_and(|p| p.pos[0] < 16. && p.pos[1] < 80.)
    });
    h.wait_pages(page(0, 4, 0));
    h.until(|h| {
        h.client
            .packet_scene()
            .page(old)
            .is_some_and(|p| p.get(1).is_some())
    });
    assert!(
        h.client.replica().page(old).unwrap().version > before_raw.version,
        "retirement removes the old Raw history without resetting the peer clock"
    );
    assert!(h.client.light_replica().page(old).unwrap().header.version > before_light.version);
    assert_eq!(h.server.world().loaded_pages(), world_pages);
    assert_eq!(h.server.counters().generation_submitted, 0);
    let queued = h.client.queue_depths().cells;
    let first = h.client.take_page_updates(usize::MAX);
    let second = h.client.take_page_updates(usize::MAX);
    assert_eq!(first.len(), 128);
    assert_eq!(first.len() + second.len(), queued);
    assert!(h.client.take_page_updates(128).is_empty());
    assert!(
        first
            .iter()
            .chain(&second)
            .all(|u| h.client.replica().page(update_page(u)).is_some())
    );
    eprintln!(
        "nearOwner wanted245 maxGui{} maxRawLease{} forgotten{} worldPages{} canonicalGeneration0",
        h.max_gui,
        h.max_lease,
        h.forgot.len(),
        world_pages
    );
}
