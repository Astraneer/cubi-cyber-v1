//! Actual authenticated UDP datagrams, delayed/lost delivery, the canonical WAL
//! owner, shared Connection, and the same pipe GPU-record bridge used by clients.
#![cfg(not(target_arch = "wasm32"))]
use client::{
    entities::{ItemVisual, ItemVisuals},
    network::{Connection, NetworkConfig},
    packet_source::{PacketGpuSource, View},
};
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use net::{
    PlayerId, ServerMessage,
    emulator::{NetworkProfile, SimulatedLink},
    udp::{Channel, Limits, MAX_DATAGRAM, Side},
};
use rules::{
    blocks::MekKind,
    ids::*,
    items::I_ENERGY_TABLET,
    machines::{Machine, SideMode, Slot},
    stack::ItemStack,
    survival::furnace::Chest,
};
use server::{Server, ServerConfig, transport::Runtime};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
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
                "packet-socket-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.generation_enabled = false;
        c.spawn_override = Some([2.5, 1., 8.5]);
        c
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn p(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y: 2,
        z: 11,
    }
}
fn local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
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
    machine.eject = false;
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
struct Icons;
impl ItemVisuals for Icons {
    fn visual(&self, item: u16) -> Option<ItemVisual> {
        (item == I_ENERGY_TABLET).then_some(ItemVisual {
            kind: 7,
            atlas_layer: 13,
        })
    }
}
struct Bot {
    socket: UdpSocket,
    channel: Option<Channel>,
    up: SimulatedLink<Vec<u8>>,
    down: SimulatedLink<Vec<u8>>,
    connection: Connection,
    bootstrap: Vec<u8>,
    next_hello: u64,
    source: PacketGpuSource,
    saw_birth: bool,
    saw_remove: bool,
    saw_gpu: bool,
    received_poses: u64,
}
impl Bot {
    fn new(address: SocketAddr, id: u64) -> Self {
        let socket = UdpSocket::bind(local()).unwrap();
        socket.connect(address).unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut connection = Connection::new(NetworkConfig::default()).unwrap();
        connection
            .hello(format!("pipe-udp-{id}"), PlayerId::default(), None)
            .unwrap();
        let bootstrap = net::encode_client(&connection.take_outgoing(1).pop().unwrap()).unwrap();
        Self {
            socket,
            channel: None,
            up: SimulatedLink::new(NetworkProfile::default(), id * 41 + 1, 512, 524288),
            down: SimulatedLink::new(NetworkProfile::default(), id * 41 + 2, 512, 524288),
            connection,
            bootstrap,
            next_hello: 0,
            source: PacketGpuSource::new(8).unwrap(),
            saw_birth: false,
            saw_remove: false,
            saw_gpu: false,
            received_poses: 0,
        }
    }
    fn enqueue(&mut self, packets: Vec<Vec<u8>>, now: u64) {
        for p in packets {
            self.up.send(now, p.len(), p).unwrap();
        }
    }
    fn message(&mut self, message: ServerMessage, now: u64) {
        if let ServerMessage::MachinePackets(frame) = &message {
            assert_eq!(frame.page, p(10).page());
            assert!(
                frame
                    .upserts
                    .iter()
                    .all(|pose| pose.item == I_ENERGY_TABLET && pose.count == 1)
            );
            self.received_poses += frame.upserts.len() as u64;
            self.saw_birth |= !frame.upserts.is_empty();
            self.saw_remove |= !frame.removes.is_empty();
        }
        self.connection.receive(message, now).unwrap();
    }
    fn step(&mut self, now: u64, tick: Tick) {
        if self.channel.is_none() && now >= self.next_hello {
            self.up
                .send(now, self.bootstrap.len(), self.bootstrap.clone())
                .unwrap();
            self.next_hello = now + 300;
        }
        while let Some(p) = self.up.receive(now) {
            self.socket.send(&p).unwrap();
        }
        let mut buffer = [0; MAX_DATAGRAM + 1];
        loop {
            match self.socket.recv(&mut buffer) {
                Ok(len) => self.down.send(now, len, buffer[..len].to_vec()).unwrap(),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("socket:{e}"),
            }
        }
        while let Some(packet) = self.down.receive(now) {
            if packet.starts_with(b"CUBI") {
                let m = net::decode_server(&packet).unwrap();
                if let ServerMessage::Welcome(w) = m {
                    if self.channel.is_none() {
                        self.channel = Some(Channel::new(
                            w.session,
                            w.new_credential.expect("fresh guest credential"),
                            Side::Client,
                            Limits::default(),
                        ));
                        self.message(ServerMessage::Welcome(w), now);
                        self.connection.request_page(p(10).page(), now).unwrap();
                    }
                } else if let ServerMessage::Error(reason) = m {
                    panic!("bootstrap:{reason:?}");
                }
                continue;
            }
            if let Some(ch) = self.channel.as_mut() {
                let received = ch.receive(&packet, now).unwrap();
                self.enqueue(received.replies, now);
                for frame in received.frames {
                    self.message(net::decode_server(&frame).unwrap(), now);
                }
            }
        }
        if self.channel.is_some() {
            for m in self.connection.take_outgoing(16) {
                let packets = self
                    .channel
                    .as_mut()
                    .unwrap()
                    .send(net::encode_client(&m).unwrap(), true, now)
                    .unwrap();
                self.enqueue(packets, now);
            }
            let packets = self.channel.as_mut().unwrap().tick(now).unwrap();
            self.enqueue(packets, now);
        }
        let work = self
            .source
            .prepare(
                self.connection.packet_scene(),
                View {
                    space: SpaceId(0),
                    tick,
                    alpha: 0.,
                    origin: [0; 3],
                },
                &|page, version, index| {
                    self.connection.replica().page(page).is_some_and(|raw| {
                        raw.version == version.0
                            && raw.cells[usize::from(index.get())].id() == LOGISTICAL_TRANSPORTER
                    })
                },
                &Icons,
                |_| None,
            )
            .unwrap();
        assert_eq!(work.cpu_vertices, 0);
        if work.instances != 0 {
            assert_eq!(work.instances, 1);
            let instance = self.source.instances()[0];
            assert_eq!(instance.kind, 7);
            assert_eq!(instance.flags & 4096, 4096);
            assert_eq!(instance.flags & 1024, 0);
            assert_eq!(instance.position[1], 2.25);
            assert_eq!(instance.color, 13);
            self.saw_gpu = true;
        }
    }
}
#[test]
fn two_actual_udp_connections_render_durable_charged_packet_and_remove_it_after_delivery_under_loss()
 {
    let d = Directory::new();
    let server = Server::open_with_world(d.config(), fixture()).unwrap();
    let mut runtime = Runtime::bind(server, local(), local()).unwrap();
    let address = runtime.udp_address().unwrap();
    let mut bots = [Bot::new(address, 1), Bot::new(address, 2)];
    let started = Instant::now();
    let deadline = Duration::from_secs(15);
    let mut enabled = false;
    let mut delivered = false;
    loop {
        let now = started.elapsed().as_millis() as u64;
        runtime.poll(now).unwrap();
        let tick = runtime.server().current_tick();
        for bot in &mut bots {
            bot.step(now, tick);
        }
        assert!(!runtime.server().is_failed());
        if !enabled
            && bots
                .iter()
                .all(|b| b.connection.replica().page(p(10).page()).is_some())
        {
            let mut m =
                server::machines::read_machine(runtime.server().world(), ComponentId(1)).unwrap();
            m.eject = true;
            let mut tx = Transaction::new(Tick(tick.0 + 1), SpaceId(0));
            tx.set_component(
                ComponentId(1),
                server::machines::machine_component(p(9), &m),
                ChannelMask::CONTENTS,
            );
            assert!(runtime.server_mut().queue_transaction(tx).is_ok());
            enabled = true;
        }
        let target = Chest::decode(
            &runtime
                .server()
                .world()
                .component(ComponentId(2))
                .unwrap()
                .data
                .payload,
        )
        .unwrap()
        .slots()[0];
        if !target.is_empty() && !runtime.server().awaiting_durability() {
            let reference = target.payload().unwrap();
            assert_eq!(
                runtime
                    .server()
                    .item_payloads()
                    .resolve(reference, I_ENERGY_TABLET)
                    .unwrap()
                    .joules(I_ENERGY_TABLET),
                Ok(180000)
            );
            delivered = true;
        }
        if delivered
            && bots.iter().all(|b| {
                b.saw_birth && b.saw_remove && b.saw_gpu && b.connection.packet_scene().is_empty()
            })
        {
            break;
        }
        assert!(
            started.elapsed() < deadline,
            "actual UDP pipe deadline: enabled={enabled} delivered={delivered} stream={:?} bots={:?}",
            runtime.server().packet_stream_counters(),
            bots.iter()
                .map(|b| (
                    b.channel.is_some(),
                    b.saw_birth,
                    b.saw_remove,
                    b.saw_gpu,
                    b.received_poses
                ))
                .collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(2));
    }
    let transport = runtime.counters();
    assert_eq!(transport.accepted_udp, 2);
    assert_eq!(transport.queue_overflows, 0);
    assert!(transport.received_datagrams > 10);
    assert!(bots.iter().all(|b| b.received_poses >= 2));
    let stream = runtime.server().packet_stream_counters();
    assert_eq!(
        stream.frames, 6,
        "birth, cell transfer, removal per observer"
    );
    assert_eq!((stream.upserts, stream.removes), (4, 2));
    let lost: u64 = bots
        .iter()
        .map(|b| b.up.counters().lost + b.down.counters().lost)
        .sum();
    assert!(lost > 0, "emulator must actually drop datagrams");
    assert!(bots.iter().all(|b|b.up.counters().peak_bytes<=524288&&b.down.counters().peak_bytes<=524288));
    drop(runtime);
    let restored = Server::open(d.config()).unwrap();
    let target = Chest::decode(
        &restored
            .world()
            .component(ComponentId(2))
            .unwrap()
            .data
            .payload,
    )
    .unwrap()
    .slots()[0];
    assert_eq!(target.item(), I_ENERGY_TABLET);
    assert_eq!(target.count(), 1);
    assert_eq!(restored.machine_runtime().packet_count(), 0);
    assert_eq!(
        restored
            .item_payloads()
            .resolve(target.payload().unwrap(), I_ENERGY_TABLET)
            .unwrap()
            .joules(I_ENERGY_TABLET),
        Ok(180000)
    );
    eprintln!(
        "actual UDP pipe 2 observers profile75+/-20ms/2%loss dropped={lost} datagrams_in={} datagrams_out={} poses={:?} public_frames={}",
        transport.received_datagrams,
        transport.sent_datagrams,
        bots.map(|b| b.received_poses),
        stream.frames
    );
}
