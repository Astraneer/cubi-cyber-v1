//! A single 10,000-cell owner transaction reaches two confirmed UDP replicas.
//! This measures ordinary page replication, not a compact explosion command.
use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick, Version};
use net::emulator::{NetworkProfile, SimulatedLink};
use net::udp::{Channel, Limits, MAX_DATAGRAM, Side, UdpError};
use net::{
    ClientMessage, Hello, PageUpdate, PlayerId, Replica, ReplicaError, ReplicaRead, ServerMessage,
};
use server::transport::Runtime;
use server::{Server, ServerConfig};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use world::{CellRead, Transaction, World};

const LINK_BYTES: usize = 524_288;
const CELLS_PER_PAGE: usize = 1250;
const BASELINE_FENCE: u64 = 1;
const EXPLOSION_FENCE: u64 = 2;
// This protocol/work fixture edits public construction, outside the private
// generator profile. Closed underground AIR disclosure is tested separately.
const BASE_Y: i32 = 256;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "explosion-stream-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        let mut config = ServerConfig::new(&self.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 2.0 + f64::from(BASE_Y), 8.5]);
        config
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}
fn pages() -> Vec<PageId> {
    let mut result = Vec::new();
    for y in 0..=1 {
        for z in 0..=1 {
            for x in 0..=1 {
                result.push(PageId {
                    space: SpaceId(0),
                    x,
                    y: y + BASE_Y / 16,
                    z,
                });
            }
        }
    }
    result
}
fn position(page: PageId, index: usize) -> BlockPos {
    BlockPos {
        space: page.space,
        x: page.x * 16 + (index % 16) as i32,
        y: page.y * 16 + (index / 256) as i32,
        z: page.z * 16 + (index / 16 % 16) as i32,
    }
}
fn protected_chamber(position: BlockPos) -> bool {
    (6..=10).contains(&position.x)
        && (0..=6).contains(&(position.y - BASE_Y))
        && (6..=10).contains(&position.z)
}
fn fixture() -> (World, Vec<BlockPos>) {
    let mut world = World::new(rules::properties, 256);
    let stone = Cell::from_parts(rules::ids::STONE, 0);
    let mut create = Transaction::new(Tick(0), SpaceId(0));
    let mut targets = Vec::new();
    for page in pages() {
        let mut cells = vec![stone; PAGE_VOLUME];
        for (index, cell) in cells.iter_mut().enumerate() {
            let pos = position(page, index);
            if (7..=9).contains(&pos.x)
                && (2..=5).contains(&(pos.y - BASE_Y))
                && (7..=9).contains(&pos.z)
            {
                *cell = Cell::AIR;
            }
        }
        let selected: Vec<_> = (0..PAGE_VOLUME)
            .map(|index| position(page, index))
            .filter(|pos| !protected_chamber(*pos))
            .take(CELLS_PER_PAGE)
            .collect();
        assert_eq!(selected.len(), CELLS_PER_PAGE);
        targets.extend(selected);
        create.create_page(page, cells);
    }
    world.commit(create).unwrap();
    assert_eq!(targets.len(), 10_000);
    assert!(
        targets
            .iter()
            .all(|pos| world.read(*pos) == CellRead::Known(stone))
    );
    (world, targets)
}

#[derive(Clone, Copy, Default)]
struct Traffic {
    sent_bytes: u64,
    received_bytes: u64,
    sent_packets: u64,
    received_packets: u64,
    page_bytes: u64,
    light_bytes: u64,
    other_bytes: u64,
}
impl Traffic {
    fn since(self, before: Self) -> Self {
        Self {
            sent_bytes: self.sent_bytes - before.sent_bytes,
            received_bytes: self.received_bytes - before.received_bytes,
            sent_packets: self.sent_packets - before.sent_packets,
            received_packets: self.received_packets - before.received_packets,
            page_bytes: self.page_bytes - before.page_bytes,
            light_bytes: self.light_bytes - before.light_bytes,
            other_bytes: self.other_bytes - before.other_bytes,
        }
    }
}
struct Bot {
    socket: UdpSocket,
    channel: Option<Channel>,
    upstream: SimulatedLink<Vec<u8>>,
    downstream: SimulatedLink<Vec<u8>>,
    outgoing: VecDeque<Vec<u8>>,
    hello: Vec<u8>,
    next_hello: u64,
    replica: Replica,
    lights: net::light::Replica,
    fence_sent: u64,
    fence_received: u64,
    snapshots: u64,
    deltas: u64,
    traffic: Traffic,
    reordered: u64,
    last_packet: u64,
    baseline_headers: BTreeMap<PageId, (u64, Version)>,
}
impl Bot {
    fn new(address: SocketAddr, id: u64) -> Self {
        let socket = UdpSocket::bind(local()).unwrap();
        socket.connect(address).unwrap();
        socket.set_nonblocking(true).unwrap();
        Self {
            socket,
            channel: None,
            upstream: SimulatedLink::new(NetworkProfile::default(), id * 41 + 21, 512, LINK_BYTES),
            downstream: SimulatedLink::new(
                NetworkProfile::default(),
                id * 41 + 22,
                512,
                LINK_BYTES,
            ),
            outgoing: VecDeque::new(),
            hello: net::encode_client(&ClientMessage::Hello(Hello {
                identity: PlayerId::default(),
                name: format!("explosion-{id}"),
                credential: None,
            }))
            .unwrap(),
            next_hello: 0,
            replica: Replica::new(8),
            lights: net::light::Replica::new(8).unwrap(),
            fence_sent: 0,
            fence_received: 0,
            snapshots: 0,
            deltas: 0,
            traffic: Traffic::default(),
            reordered: 0,
            last_packet: 0,
            baseline_headers: BTreeMap::new(),
        }
    }
    fn enqueue(&mut self, message: ClientMessage) {
        assert!(self.outgoing.len() < 64, "bounded bot application queue");
        self.outgoing
            .push_back(net::encode_client(&message).unwrap());
    }
    fn packets(&mut self, packets: Vec<Vec<u8>>, now: u64) {
        for packet in packets {
            self.upstream.send(now, packet.len(), packet).unwrap();
        }
    }
    fn pending(&self) -> bool {
        !self.outgoing.is_empty()
            || self
                .channel
                .as_ref()
                .is_none_or(|channel| channel.pending_messages() != 0)
    }
    fn receive_message(&mut self, message: ServerMessage) {
        let bytes = net::encode_server(&message).unwrap().len() as u64;
        match &message {
            ServerMessage::Page(_) => self.traffic.page_bytes += bytes,
            ServerMessage::Light(_) => self.traffic.light_bytes += bytes,
            _ => self.traffic.other_bytes += bytes,
        }
        match message {
            ServerMessage::Welcome(welcome) if self.channel.is_none() => {
                self.channel = Some(Channel::new(
                    welcome.session,
                    welcome
                        .new_credential
                        .expect("new guest transport credential"),
                    Side::Client,
                    Limits::default(),
                ));
                for page in pages() {
                    self.enqueue(ClientMessage::RequestPage {
                        page,
                        acknowledged: None,
                    });
                }
            }
            ServerMessage::Page(update) => {
                let page = match &update {
                    PageUpdate::Snapshot(snapshot) => {
                        self.snapshots += 1;
                        snapshot.page
                    }
                    PageUpdate::Delta(delta) => {
                        self.deltas += 1;
                        delta.page
                    }
                };
                match self.replica.apply(update) {
                    Ok(_) => self.enqueue(ClientMessage::AckPage {
                        page,
                        version: self.replica.page(page).unwrap().version,
                    }),
                    Err(ReplicaError::MissingBase { .. }) => {
                        self.enqueue(ClientMessage::RequestPage {
                            page,
                            acknowledged: None,
                        })
                    }
                    Err(error) => panic!("explosion replica: {error:?}"),
                }
            }
            ServerMessage::Light(update) => {
                let header = update.header();
                let cell_view = self.replica.page(header.page).map(|page| page.version);
                match self.lights.apply(*update, cell_view) {
                    Ok(_) => {
                        if let Some(image) = self.lights.page(header.page)
                            && Some(image.header.cell_view) == cell_view
                        {
                            self.enqueue(ClientMessage::RequestLight(net::light::Request {
                                page: header.page,
                                cell_view: image.header.cell_view,
                                acknowledged: Some(image.header.version),
                            }));
                        }
                    }
                    Err(net::light::Error::MissingBase { .. }) => {
                        self.enqueue(ClientMessage::RequestLight(net::light::Request {
                            page: header.page,
                            cell_view: cell_view.expect("known fixture cell page"),
                            acknowledged: None,
                        }));
                    }
                    Err(net::light::Error::MissingCells { .. }) => {}
                    Err(error) => panic!("explosion light replica: {error:?}"),
                }
            }
            ServerMessage::LightUnavailable(_) => panic!("fixture LIGHT must be available"),
            ServerMessage::Pong(fence) => self.fence_received = self.fence_received.max(fence),
            ServerMessage::PageUnavailable(_) => {
                panic!("all eight fixture pages must be available")
            }
            ServerMessage::Error(reason) => panic!("explosion bot rejected: {reason:?}"),
            _ => {}
        }
    }
    fn fence(&mut self, value: u64) {
        // Transport ACK receipt proves all preceding reliable application ACKs
        // reached the owner's inbox. A subsequent Pong then proves they passed
        // through the authoritative FIFO, even though UDP delivery is unordered.
        if !self.pending() && self.fence_sent < value {
            self.enqueue(ClientMessage::Ping(value));
            self.fence_sent = value;
        }
    }
    fn step(&mut self, now: u64) {
        if self.channel.is_none() && now >= self.next_hello {
            self.upstream
                .send(now, self.hello.len(), self.hello.clone())
                .unwrap();
            self.next_hello = now + 300;
        }
        while let Some(frame) = self.outgoing.front() {
            let Some(channel) = &mut self.channel else {
                break;
            };
            if channel.pending_messages() >= Limits::default().maximum_messages {
                break;
            }
            match channel.send(frame.clone(), true, now) {
                Ok(packets) => {
                    self.outgoing.pop_front();
                    self.packets(packets, now);
                }
                Err(UdpError::Capacity) => break,
                Err(error) => panic!("bounded bot channel: {error:?}"),
            }
        }
        while let Some(packet) = self.upstream.receive(now) {
            self.socket.send(&packet).unwrap();
            self.traffic.sent_bytes += packet.len() as u64;
            self.traffic.sent_packets += 1;
        }
        let mut buffer = [0; MAX_DATAGRAM + 1];
        loop {
            match self.socket.recv(&mut buffer) {
                Ok(length) => {
                    assert!(length <= MAX_DATAGRAM);
                    self.traffic.received_bytes += length as u64;
                    self.traffic.received_packets += 1;
                    self.downstream
                        .send(now, length, buffer[..length].to_vec())
                        .unwrap();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("explosion UDP socket: {error}"),
            }
        }
        while let Some(packet) = self.downstream.receive(now) {
            if packet.starts_with(b"CUBI") {
                self.receive_message(net::decode_server(&packet).unwrap());
            } else if let Some(channel) = &mut self.channel {
                let sequence = u64::from_le_bytes(packet[12..20].try_into().unwrap());
                self.reordered += u64::from(sequence < self.last_packet);
                self.last_packet = self.last_packet.max(sequence);
                let received = channel.receive(&packet, now).unwrap();
                self.packets(received.replies, now);
                for frame in received.frames {
                    self.receive_message(net::decode_server(&frame).unwrap());
                }
            }
        }
        if let Some(channel) = &mut self.channel {
            let packets = channel.tick(now).unwrap();
            self.packets(packets, now);
        }
    }
    fn lights_bound(&self) -> bool {
        pages().iter().all(|page| {
            self.lights.page(*page).is_some_and(|image| {
                self.replica
                    .page(*page)
                    .is_some_and(|cells| image.header.cell_view == cells.version)
            })
        })
    }
    fn explosion_applied(&self, targets: &[BlockPos]) -> bool {
        self.replica.len() == 8
            && pages().iter().all(|page| {
                let current = self.replica.page(*page).unwrap();
                self.baseline_headers
                    .get(page)
                    .is_some_and(|&(view, source)| {
                        current.version > view && current.source_version > source
                    })
            })
            && targets
                .iter()
                .all(|position| self.replica.read(*position) == ReplicaRead::Known(Cell::AIR))
    }
}

#[test]
fn one_ten_thousand_cell_transaction_reaches_two_confirmed_lossy_udp_replicas() {
    let directory = Directory::new();
    let (world, targets) = fixture();
    let initial_version = world.version();
    let server = Server::open_with_world(directory.config(), world).unwrap();
    let mut runtime = Runtime::bind(server, local(), local()).unwrap();
    let mut bots = [
        Bot::new(runtime.udp_address().unwrap(), 1),
        Bot::new(runtime.udp_address().unwrap(), 2),
    ];
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut baseline = None;
    let mut finished = false;
    for now in (0..=30_000).step_by(5) {
        assert!(
            Instant::now() < deadline,
            "explosion real socket/worker deadline"
        );
        for bot in &mut bots {
            bot.step(now);
            if bot.replica.len() == 8 && bot.lights_bound() && baseline.is_none() {
                bot.fence(BASELINE_FENCE);
            }
        }
        runtime.poll(now).unwrap();
        assert!(!runtime.server().is_failed());
        for bot in &mut bots {
            bot.step(now);
        }
        if baseline.is_none() && bots.iter().all(|bot| bot.fence_received == BASELINE_FENCE) {
            for bot in &mut bots {
                assert!(bot.snapshots >= 8);
                assert!(
                    pages()
                        .iter()
                        .all(|page| bot.replica.page(*page).unwrap().source_version.0 > 0)
                );
                bot.baseline_headers = pages()
                    .into_iter()
                    .map(|page| {
                        let image = bot.replica.page(page).unwrap();
                        (page, (image.version, image.source_version))
                    })
                    .collect();
                assert!(targets.iter().all(|position| bot.replica.read(*position)
                    == ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0))));
            }
            baseline = Some((
                runtime.counters(),
                runtime.server().counters(),
                [bots[0].traffic, bots[1].traffic],
            ));
            let mut transaction =
                Transaction::new(Tick(runtime.server().current_tick().0 + 1), SpaceId(0));
            for page in pages() {
                transaction.expect_page(page, runtime.server().world().page_version(page));
            }
            for &position in &targets {
                transaction.set_cell(position, Cell::AIR);
            }
            runtime
                .server_mut()
                .queue_transaction(transaction)
                .unwrap_or_else(|_| panic!("owner transaction queue must accept one operation"));
        }
        if baseline.is_some() {
            for bot in &mut bots {
                if bot.explosion_applied(&targets) && bot.lights_bound() {
                    bot.fence(EXPLOSION_FENCE);
                }
            }
            if bots.iter().all(|bot| bot.fence_received == EXPLOSION_FENCE) {
                finished = true;
                break;
            }
        }
        thread::sleep(Duration::from_micros(500));
    }
    if !finished {
        for (index, bot) in bots.iter().enumerate() {
            let air = targets
                .iter()
                .filter(|&&p| bot.replica.read(p) == ReplicaRead::Known(Cell::AIR))
                .count();
            let stone = targets
                .iter()
                .filter(|&&p| {
                    bot.replica.read(p)
                        == ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0))
                })
                .count();
            let advanced = pages()
                .iter()
                .filter(|&&p| {
                    bot.replica.page(p).is_some_and(|raw| {
                        bot.baseline_headers.get(&p).is_some_and(|&(view, source)| {
                            raw.version > view && raw.source_version > source
                        })
                    })
                })
                .count();
            eprintln!(
                "explosion visibility diagnostic client{index} canonicalAir{} replicaAir{air} replicaStone{stone} advancedPages{advanced} tick{:?} rays{} pendingCells{}",
                targets
                    .iter()
                    .filter(|&&p| runtime.server().world().read(p) == CellRead::Known(Cell::AIR))
                    .count(),
                runtime.server().current_tick(),
                runtime.server().counters().visibility_rays,
                runtime.server().counters().light_pending_cells
            );
        }
        for position in [
            [6, 3, 8],
            [10, 3, 8],
            [8, 3, 6],
            [8, 3, 10],
            [8, 1, 8],
            [8, 6, 8],
        ] {
            eprintln!(
                "explosion enclosure {position:?} {:?}",
                runtime.server().world().read(BlockPos {
                    space: SpaceId(0),
                    x: position[0],
                    y: position[1] + BASE_Y,
                    z: position[2]
                })
            );
        }
    }
    assert!(
        finished,
        "both replicas must apply and acknowledge the explosion"
    );
    let (transport_before, domain_before, traffic_before) =
        baseline.expect("confirmed initial snapshots");
    let domain = runtime.server().counters();
    let transport = runtime.counters();
    assert_eq!(
        domain.committed_transactions - domain_before.committed_transactions,
        1
    );
    assert_eq!(
        runtime.server().world().version(),
        Version(initial_version.0 + 1)
    );
    assert_eq!(domain.rejected_transactions, 0);
    assert!(
        targets
            .iter()
            .all(|position| runtime.server().world().read(*position) == CellRead::Known(Cell::AIR))
    );
    assert_eq!(transport.queue_overflows, 0);
    let view_cells = domain.view_cells - domain_before.view_cells;
    let rays = domain.visibility_segments - domain_before.visibility_segments;
    assert!(
        (65_536..=131_072).contains(&view_cells),
        "work bounded by eight changed pages for each observer: {view_cells}"
    );
    assert_eq!(
        rays, 0,
        "stone/air fixture has no private-ore visibility rays"
    );
    for (index, bot) in bots.iter().enumerate() {
        let traffic = bot.traffic.since(traffic_before[index]);
        eprintln!(
            "explosion decoded client={index} page_bytes={} light_bytes={} other_bytes={} wire_bytes={}",
            traffic.page_bytes, traffic.light_bytes, traffic.other_bytes, traffic.received_bytes
        );
        // Fence the independently acknowledged initial LIGHT stream before
        // measuring the edit. The explosion still includes every wire byte,
        // including the new cell-view LIGHT bindings and transport retries.
        assert!(traffic.received_bytes > 0 && traffic.received_bytes < 131_072);
        assert!(traffic.received_packets > 0 && traffic.received_packets < 1024);
        assert!(
            bot.upstream.counters().peak_bytes <= LINK_BYTES
                && bot.downstream.counters().peak_bytes <= LINK_BYTES
        );
        eprintln!(
            "explosion cells=10000 transactions=1 client={} received_bytes={} received_datagrams={} sent_bytes={} sent_datagrams={} snapshots_total={} deltas_total={} lost_total={} reordered_total={}",
            index + 1,
            traffic.received_bytes,
            traffic.received_packets,
            traffic.sent_bytes,
            traffic.sent_packets,
            bot.snapshots,
            bot.deltas,
            bot.upstream.counters().lost + bot.downstream.counters().lost,
            bot.reordered
        );
    }
    eprintln!(
        "explosion total_server_bytes={} total_server_datagrams={} view_cells={} visibility_segments={} queue_overflows={}",
        transport.sent_bytes - transport_before.sent_bytes,
        transport.sent_datagrams - transport_before.sent_datagrams,
        view_cells,
        rays,
        transport.queue_overflows
    );
    assert!(
        bots.iter()
            .map(|bot| bot.upstream.counters().lost + bot.downstream.counters().lost)
            .sum::<u64>()
            > 0
    );
    assert!(bots.iter().map(|bot| bot.reordered).sum::<u64>() > 0);
}
