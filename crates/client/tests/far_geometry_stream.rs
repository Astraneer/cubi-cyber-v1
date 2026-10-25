//! Real UDP + shared Connection → public LOD cache. The selected leaf is1024
//! chunks away; geometry workers and packet loss are exercised without raw pages.
#![cfg(not(target_arch = "wasm32"))]
use client::{
    lodcache::{BuiltProxy, LodCacheConfig, PublicLodCache, PublishDisposition},
    network::{Connection, NetworkConfig},
};
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, LocalIdx, PageId, SpaceId, Tick};
use net::{
    ClientMessage, Hello, PageUpdate, ServerMessage,
    emulator::{NetworkProfile, SimulatedLink},
    udp::{Channel, Limits, MAX_DATAGRAM, Side},
};
use render::lod::{GeometrySource, LodKey, LodObject, ObjectSelection};
use server::{Server, ServerConfig, transport::Runtime};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{ComponentData, Transaction, World};
fn far() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1024,
        y: 0,
        z: 0,
    }
}
fn unknown() -> PageId {
    PageId { y: 1, ..far() }
}
fn key() -> LodKey {
    LodKey::page(0, 1024, 0, 0)
}
fn opening() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 1024 * 16 + 7,
        y: 4,
        z: 8,
    }
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("far-geometry-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        let mut config = ServerConfig::new(&self.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 1., 8.5]);
        config.geometry_result_delay_ticks = 4;
        config
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 512);
    let mut ground = vec![Cell::AIR; 4096];
    for z in 0..16 {
        for x in 0..16 {
            ground[x + 16 * z] = Cell::from_parts(rules::ids::STONE, 0);
        }
    }
    let mut arch = ground.clone();
    for y in 1..=8 {
        for x in 4..=11 {
            if x == 4 || x == 11 || y == 8 {
                arch[x + 16 * (8 + 16 * y)] = Cell::from_parts(rules::ids::STONE, 0);
            }
        }
    }
    for (i, id) in [
        rules::ids::COAL_ORE,
        rules::ids::IRON_ORE,
        rules::ids::OSMIUM_ORE,
        rules::ids::TIN_ORE,
        rules::ids::COPPER_ORE,
        rules::ids::GOLD_ORE,
        rules::ids::REDSTONE_ORE,
    ]
    .into_iter()
    .enumerate()
    {
        arch[i + 16 * (1 + 16)] = Cell::from_parts(id, 65535);
    }
    let mut sentinel = vec![Cell::AIR; 4096];
    sentinel[0] = Cell::from_raw(u32::MAX);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        ground,
    )
    .create_page(far(), arch)
    .create_page(unknown(), sentinel);
    world.commit(tx).unwrap();
    world
}
struct Bot {
    socket: UdpSocket,
    channel: Option<Channel>,
    client: Connection,
    up: SimulatedLink<Vec<u8>>,
    down: SimulatedLink<Vec<u8>>,
    cache: PublicLodCache,
    held: Option<BuiltProxy>,
    requested: bool,
    baseline: bool,
    unknown_seen: bool,
    far_page_denied: bool,
    secret: [u8; 32],
    secret_seen: bool,
    reordered: u64,
    last_packet: u64,
    geometry_frames: u64,
}
impl Bot {
    fn new(address: SocketAddr, hello: Hello, secret: [u8; 32], index: u64) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(address).unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut client = Connection::new(NetworkConfig {
            near_radius: 0,
            retain_radius: 1,
            ..NetworkConfig::default()
        })
        .unwrap();
        client
            .hello(hello.name, hello.identity, hello.credential)
            .unwrap();
        Self {
            socket,
            channel: None,
            client,
            up: SimulatedLink::new(NetworkProfile::default(), 0x19 + index, 4096, 2_097_152),
            down: SimulatedLink::new(NetworkProfile::default(), 0x91 + index, 4096, 2_097_152),
            cache: PublicLodCache::new(LodCacheConfig::default()).unwrap(),
            held: None,
            requested: false,
            baseline: false,
            unknown_seen: false,
            far_page_denied: false,
            secret,
            secret_seen: false,
            reordered: 0,
            last_packet: 0,
            geometry_frames: 0,
        }
    }
    fn packets(&mut self, packets: Vec<Vec<u8>>, now: u64) {
        for packet in packets {
            self.up.send(now, packet.len(), packet).unwrap();
        }
    }
    fn send(&mut self, message: ClientMessage, now: u64) {
        let bytes = net::encode_client(&message).unwrap();
        if matches!(message, ClientMessage::Hello(_)) {
            self.up.send(now, bytes.len(), bytes).unwrap();
        } else if let Some(channel) = &mut self.channel {
            let reliable = !matches!(message, ClientMessage::Inputs(_));
            let packets = channel.send(bytes, reliable, now).unwrap();
            self.packets(packets, now);
        }
    }
    fn message(&mut self, message: ServerMessage, now: u64) {
        match &message {
            ServerMessage::Welcome(welcome) => {
                if self.channel.is_none() {
                    let credential = welcome.new_credential.or(self.client.credential()).unwrap();
                    self.channel = Some(Channel::new(
                        welcome.session,
                        credential,
                        Side::Client,
                        Limits::default(),
                    ));
                }
            }
            ServerMessage::Page(PageUpdate::Snapshot(snapshot)) => assert_ne!(
                snapshot.page,
                far(),
                "far protected request leaked a raw page"
            ),
            ServerMessage::Page(PageUpdate::Delta(delta)) => assert_ne!(
                delta.page,
                far(),
                "far protected request leaked raw-cell deltas"
            ),
            ServerMessage::PageUnavailable(page) if *page == far() => self.far_page_denied = true,
            _ => {}
        }
        self.client.receive(message, now).unwrap();
    }
    fn step(&mut self, now: u64) {
        let mut bytes = [0u8; MAX_DATAGRAM];
        for _ in 0..512 {
            match self.socket.recv(&mut bytes) {
                Ok(length) => {
                    let packet = bytes[..length].to_vec();
                    self.down.send(now, length, packet).unwrap();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("far UDP socket:{e}"),
            }
        }
        while let Some(packet) = self.down.receive(now) {
            self.secret_seen |= packet.windows(32).any(|bytes| bytes == self.secret);
            if packet.starts_with(b"CUBI") {
                self.message(net::decode_server(&packet).unwrap(), now);
            } else if let Some(channel) = &mut self.channel {
                let sequence = u64::from_le_bytes(packet[12..20].try_into().unwrap());
                self.reordered += u64::from(sequence < self.last_packet);
                self.last_packet = self.last_packet.max(sequence);
                let received = channel.receive(&packet, now).unwrap();
                self.packets(received.replies, now);
                for frame in received.frames {
                    self.secret_seen |= frame.windows(32).any(|bytes| bytes == self.secret);
                    self.message(net::decode_server(&frame).unwrap(), now);
                }
            }
        }
        if let Some(channel) = &mut self.channel {
            let packets = channel.tick(now).unwrap();
            self.packets(packets, now);
        }
        self.client.poll(now).unwrap();
        if self.client.welcome().is_some() {
            self.client
                .advance_to(self.client.acknowledged_tick())
                .unwrap();
        }
        for message in self.client.take_outgoing(64) {
            self.send(message, now);
        }
        while let Some(packet) = self.up.receive(now) {
            self.socket.send(&packet).unwrap();
        }
        if self.client.player().is_some() && !self.requested {
            self.client.request_geometry(far(), now).unwrap();
            self.client.request_geometry(unknown(), now).unwrap();
            self.send(
                ClientMessage::RequestPage {
                    page: far(),
                    acknowledged: None,
                },
                now,
            );
            self.requested = true;
        }
        for geometry in self.client.take_geometry_updates(32) {
            self.geometry_frames += 1;
            geometry.validate().unwrap();
            if geometry.page == far() {
                assert!(geometry.known.iter().all(|byte| *byte == 255));
                assert_eq!(geometry.palette.len(), 1);
                assert_eq!(
                    geometry.palette[0].atlas_layer, 3,
                    "all seven private ore materials must be host stone"
                );
                self.cache.ingest_wire_geometry(&geometry).unwrap();
                if !self.baseline {
                    assert_eq!(geometry.palette_index(opening().local_index()), None);
                    let selection = self.selection();
                    self.cache.queue_selection(&selection).unwrap();
                    self.held = Some(self.cache.take_task().unwrap().execute().unwrap());
                    self.baseline = true;
                }
            } else if geometry.page == unknown() {
                assert!(!geometry.is_known(LocalIdx::new(0).unwrap()));
                self.cache.ingest_wire_geometry(&geometry).unwrap();
                let key = LodKey::page(0, 1024, 1, 0);
                assert!(!self.cache.forest().get(key).unwrap().complete);
                self.unknown_seen = true;
            }
        }
        self.client.take_page_updates(128);
    }
    fn selection(&self) -> ObjectSelection {
        ObjectSelection {
            objects: vec![LodObject {
                key: key(),
                revision: self.cache.forest().get(key()).unwrap().revision,
                origin_relative: [0.; 3],
                cell_scale: 1,
                geometry: GeometrySource::FinePage,
            }],
            coverage_complete: true,
            ..ObjectSelection::default()
        }
    }
    fn closed(&self) -> bool {
        self.client
            .geometry(far())
            .is_some_and(|geometry| geometry.palette_index(opening().local_index()).is_some())
    }
}
#[test]
fn selected_far_geometry_stays_public_and_converges_with_loss_and_stale_workers() {
    let directory = Directory::new();
    let server = Server::open_with_world(directory.config(), fixture()).unwrap();
    let owner = std::fs::read(directory.0.join("owner.identity")).unwrap();
    let ClientMessage::Hello(hello) = net::decode_client(&owner).unwrap() else {
        unreachable!()
    };
    let secret: [u8; 32] = std::fs::read(directory.0.join("generation.key"))
        .unwrap()
        .try_into()
        .unwrap();
    let mut runtime = Runtime::bind(
        server,
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let address = runtime.udp_address().unwrap();
    let guest = Hello {
        name: "far-geometry-guest".into(),
        identity: net::PlayerId::default(),
        credential: None,
    };
    let mut bots = [
        Bot::new(address, hello, secret, 0),
        Bot::new(address, guest, secret, 1),
    ];
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut phase = 0;
    let mut baseline_submitted = 0;
    let mut baseline_ready = None;
    let mut contents_started = 0;
    let mut force_started = 0;
    let mut edit_started = false;
    let mut finished = false;
    for now in (0..=30_000u64).step_by(5) {
        assert!(
            Instant::now() < deadline,
            "actual far streaming deadline phase{phase}"
        );
        for bot in &mut bots {
            bot.step(now);
        }
        runtime.poll(now).unwrap();
        for bot in &mut bots {
            bot.step(now);
            if bot.requested && now % 100 == 0 {
                bot.send(ClientMessage::Ping(now), now);
            }
        }
        let all_baseline = bots
            .iter()
            .all(|bot| bot.baseline && bot.unknown_seen && bot.far_page_denied);
        if phase == 0 && all_baseline && baseline_ready.is_none() {
            baseline_ready = Some(now);
        }
        if phase == 0 && baseline_ready.is_some_and(|ready| now >= ready + 1000) {
            baseline_submitted = runtime.server().counters().geometry_submitted;
            let mut transaction =
                Transaction::new(Tick(runtime.server().current_tick().0 + 1), SpaceId(0));
            transaction.set_component(
                ComponentId(991),
                ComponentData {
                    anchor: opening(),
                    kind: 7,
                    payload: vec![1, 2, 3],
                },
                ChannelMask::CONTENTS,
            );
            assert!(runtime.server_mut().queue_transaction(transaction).is_ok());
            contents_started = now;
            phase = 1;
        }
        if phase == 1
            && now >= contents_started + 500
            && runtime
                .server()
                .world()
                .component(ComponentId(991))
                .is_some()
        {
            assert_eq!(
                runtime.server().counters().geometry_submitted,
                baseline_submitted,
                "contents-only transaction scheduled protected geometry"
            );
            for bot in &mut bots {
                bot.send(
                    ClientMessage::RequestGeometry {
                        page: far(),
                        known: None,
                    },
                    now,
                );
            }
            force_started = runtime.server().counters().geometry_submitted;
            phase = 2;
        }
        if phase == 2
            && runtime.server().counters().geometry_submitted > force_started
            && !edit_started
        {
            let mut transaction =
                Transaction::new(Tick(runtime.server().current_tick().0 + 1), SpaceId(0));
            transaction.set_cell(opening(), Cell::from_parts(rules::ids::STONE, 0));
            assert!(runtime.server_mut().queue_transaction(transaction).is_ok());
            edit_started = true;
            phase = 3;
        }
        if phase == 3
            && bots.iter().all(Bot::closed)
            && runtime.server().counters().geometry_discarded > 0
        {
            for bot in &mut bots {
                assert_eq!(
                    bot.cache.complete(bot.held.take().unwrap()).unwrap(),
                    PublishDisposition::Stale
                );
                let selection = bot.selection();
                bot.cache.queue_selection(&selection).unwrap();
                let built = bot.cache.take_task().unwrap().execute().unwrap();
                assert!(built.payload.voxel([7, 4, 8]).occupied());
                bot.cache.complete(built).unwrap();
                assert!(bot.client.replica().page(far()).is_none());
                assert!(!bot.secret_seen);
            }
            phase = 4;
        }
        if phase == 4
            && bots
                .iter()
                .map(|bot| bot.up.counters().lost + bot.down.counters().lost)
                .sum::<u64>()
                > 0
            && bots.iter().map(|bot| bot.reordered).sum::<u64>() > 0
        {
            finished = true;
            break;
        }
        thread::sleep(Duration::from_micros(100));
    }
    assert!(
        finished,
        "far streamdidnotfinish phase{phase}, bots={:?},server={:?},transport={:?}",
        bots.iter()
            .map(|bot| (
                bot.requested,
                bot.baseline,
                bot.unknown_seen,
                bot.far_page_denied,
                bot.geometry_frames,
                bot.client.player().is_some()
            ))
            .collect::<Vec<_>>(),
        runtime.server().counters(),
        runtime.counters()
    );
    let counters = runtime.server().counters();
    let transport = runtime.counters();
    assert_eq!(transport.queue_overflows, 0);
    assert!(counters.geometry_cells >= 4 * 4096);
    for (index, bot) in bots.iter().enumerate() {
        let lost = bot.up.counters().lost + bot.down.counters().lost;
        println!(
            "M5FARUDP bot={index} geometryFrames={} loss={lost} reordered={} cacheStale={} receivedBytes={} sentBytes={} queueOverflow=0",
            bot.geometry_frames,
            bot.reordered,
            bot.cache.stale_results(),
            bot.channel.as_ref().unwrap().counters().received_bytes,
            bot.channel.as_ref().unwrap().counters().sent_bytes
        );
    }
    println!(
        "M5FARUDP pages=1024chunks jobs={} published={} discarded={} cells={} tintSamples={} transportIn={} transportOut={} contentsExtraJobs=0 rawFarPages=0 privateSeedLeak=0",
        counters.geometry_submitted,
        counters.geometry_published,
        counters.geometry_discarded,
        counters.geometry_cells,
        counters.geometry_tint_samples,
        transport.received_bytes,
        transport.sent_bytes
    );
}
