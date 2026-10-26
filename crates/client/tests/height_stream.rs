//! Actual UDP/loss, durable legacy migration and public tiles1024pages away.
#![cfg(not(target_arch = "wasm32"))]
use client::network::{Connection, NetworkConfig};
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use net::{
    ClientMessage, Hello, PageUpdate, ServerMessage,
    emulator::{NetworkProfile, SimulatedLink},
    height::{HeightKey, HeightTile, Replacement},
    udp::{Channel, Limits, MAX_DATAGRAM, Side},
};
use server::{Server, ServerConfig, transport::Runtime};
use std::{
    collections::BTreeMap,
    io,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{ComponentData, Transaction, World};
const PRIVATE: [u8; 32] = [37; 32];
const CANARY: &[u8] = b"PRIVATE_HEIGHT_COMPONENT_NEVER_NET";
fn leaf() -> HeightKey {
    HeightKey {
        space: 0,
        level: 0,
        x: 1024,
        z: 0,
    }
}
fn coarse() -> HeightKey {
    HeightKey {
        level: 9,
        x: 2,
        ..leaf()
    }
}
fn far(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1024,
        y,
        z: 0,
    }
}
fn point(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("height-live-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(directory: &Directory) -> (World, i32) {
    let (store, _) = save::Store::open(&directory.0).unwrap();
    store
        .load_or_create_private_entropy(|| Ok(PRIVATE))
        .unwrap();
    drop(store);
    let generator = gen_authority::ServerGenerator::new(
        terrain_gen::TerrainSeed(1337),
        gen_authority::PrivateSeed::new(PRIVATE),
    );
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in [0, 1024] {
        let col = generator.generate_column(x, 0).unwrap();
        for y in 0..8 {
            let p = PageId {
                space: SpaceId(0),
                x,
                y,
                z: 0,
            };
            let cells: Box<[Cell]> = col.page(p, None).unwrap().cells;
            tx.create_page(p, cells);
        }
    }
    let mut roof = vec![Cell::AIR; 4096];
    roof[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 16,
            z: 0,
        },
        roof,
    );
    world.commit(tx).unwrap();
    let base = (0..16)
        .flat_map(|z| (0..16).map(move |x| (x, z)))
        .map(|(x, z)| {
            generator
                .terrain()
                .public_terrain_column(16384 + x, z)
                .unwrap()
                .surface_y
        })
        .max()
        .unwrap()
        + 2;
    let buried = (0..16)
        .flat_map(|z| (0..16).map(move |x| (x, z)))
        .map(|(x, z)| {
            generator
                .terrain()
                .public_terrain_column(16384 + x, z)
                .unwrap()
                .ground_y
        })
        .min()
        .unwrap()
        .saturating_sub(4)
        .max(1);
    // Buried edits must neither leak ore materials nor punch surface holes.
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    for (x, id) in [
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
        let p = point(
            16384 + 3 + (x as i32 % 4) * 3,
            buried,
            3 + (x as i32 / 4) * 3,
        );
        for [dx, dy, dz] in [
            [0, 0, 0],
            [1, 0, 0],
            [-1, 0, 0],
            [0, 1, 0],
            [0, -1, 0],
            [0, 0, 1],
            [0, 0, -1],
        ] {
            tx.set_cell(
                point(p.x + dx, p.y + dy, p.z + dz),
                Cell::from_parts(rules::ids::STONE, 0),
            );
        }
        tx.set_cell(p, Cell::from_parts(id, 65535));
    }
    tx.set_component(
        ComponentId(991),
        ComponentData {
            anchor: point(16390, buried, 5),
            kind: 991,
            payload: CANARY.to_vec(),
        },
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    (world, base)
}
struct Bot {
    socket: UdpSocket,
    channel: Option<Channel>,
    client: Connection,
    up: SimulatedLink<Vec<u8>>,
    down: SimulatedLink<Vec<u8>>,
    tiles: BTreeMap<HeightKey, HeightTile>,
    requested: bool,
    raw_denied: bool,
    secret_seen: bool,
    last_packet: u64,
    reordered: u64,
}
impl Bot {
    fn new(address: SocketAddr, hello: Hello, index: u64) -> Self {
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
            up: SimulatedLink::new(NetworkProfile::default(), 0x72 + index, 4096, 2097152),
            down: SimulatedLink::new(NetworkProfile::default(), 0xb9 + index, 4096, 2097152),
            tiles: BTreeMap::new(),
            requested: false,
            raw_denied: false,
            secret_seen: false,
            last_packet: 0,
            reordered: 0,
        }
    }
    fn packets(&mut self, packets: Vec<Vec<u8>>, now: u64) {
        for packet in packets {
            self.up.send(now, packet.len(), packet).unwrap();
        }
    }
    fn send(&mut self, msg: ClientMessage, now: u64) {
        let raw = net::encode_client(&msg).unwrap();
        if matches!(msg, ClientMessage::Hello(_)) {
            self.up.send(now, raw.len(), raw).unwrap();
        } else if let Some(channel) = &mut self.channel {
            let packets = channel
                .send(raw, !matches!(msg, ClientMessage::Inputs(_)), now)
                .unwrap();
            self.packets(packets, now);
        }
    }
    fn message(&mut self, msg: ServerMessage, now: u64) {
        match &msg {
            ServerMessage::Welcome(w) if self.channel.is_none() => {
                self.channel = Some(Channel::new(
                    w.session,
                    w.new_credential.or(self.client.credential()).unwrap(),
                    Side::Client,
                    Limits::default(),
                ));
            }
            ServerMessage::Page(PageUpdate::Snapshot(p)) => {
                assert_ne!(p.page.x, 1024, "farheight leaked rawpage")
            }
            ServerMessage::Page(PageUpdate::Delta(p)) => {
                assert_ne!(p.page.x, 1024, "farheight leaked rawcells")
            }
            ServerMessage::PageUnavailable(p) if p.x == 1024 => self.raw_denied = true,
            ServerMessage::Height(tile) => {
                tile.validate().unwrap();
                let private = [
                    rules::ids::L_COAL_ORE,
                    rules::ids::L_IRON_ORE,
                    rules::ids::L_OSMIUM_ORE,
                    rules::ids::L_TIN_ORE,
                    rules::ids::L_COPPER_ORE,
                    rules::ids::L_GOLD_ORE,
                    rules::ids::L_REDSTONE_ORE,
                ];
                assert!(
                    !tile
                        .atlas
                        .iter()
                        .any(|id| private.contains(&u32::from(*id)))
                );
                self.tiles.insert(tile.key, (**tile).clone());
            }
            _ => {}
        }
        self.client.receive(msg, now).unwrap();
    }
    fn inspect(&mut self, bytes: &[u8]) {
        self.secret_seen |= bytes.windows(PRIVATE.len()).any(|v| v == PRIVATE)
            || bytes.windows(CANARY.len()).any(|v| v == CANARY);
    }
    fn step(&mut self, now: u64) {
        let mut buf = [0; MAX_DATAGRAM];
        for _ in 0..512 {
            match self.socket.recv(&mut buf) {
                Ok(n) => {
                    self.down.send(now, n, buf[..n].to_vec()).unwrap();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("UDPheight:{e}"),
            }
        }
        while let Some(packet) = self.down.receive(now) {
            self.inspect(&packet);
            if packet.starts_with(b"CUBI") {
                self.message(net::decode_server(&packet).unwrap(), now);
            } else if let Some(channel) = &mut self.channel {
                let sequence = u64::from_le_bytes(packet[12..20].try_into().unwrap());
                self.reordered += u64::from(sequence < self.last_packet);
                self.last_packet = self.last_packet.max(sequence);
                let got = channel.receive(&packet, now).unwrap();
                self.packets(got.replies, now);
                for frame in got.frames {
                    self.inspect(&frame);
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
        if self.client.player().is_some() && !self.requested {
            self.client.request_height(coarse(), now).unwrap();
            self.client.request_height(leaf(), now).unwrap();
            self.send(
                ClientMessage::RequestPage {
                    page: far(0),
                    acknowledged: None,
                },
                now,
            );
            self.requested = true;
        }
        for msg in self.client.take_outgoing(64) {
            self.send(msg, now);
        }
        while let Some(packet) = self.up.receive(now) {
            self.socket.send(&packet).unwrap();
        }
        while self.client.take_height_update().is_some() {}
        self.client.take_geometry_updates(128);
        self.client.take_page_updates(128);
    }
    fn manifest_ready(&self) -> bool {
        let Some(tile) = self.tiles.get(&leaf()) else {
            return false;
        };
        let Replacement::Column {
            geometry,
            min_y,
            max_y,
        } = &tile.replacement
        else {
            return false;
        };
        geometry.len() == (*max_y - *min_y + 1) as usize
            && !geometry.is_empty()
            && geometry.iter().all(|stamp| {
                self.client.geometry(stamp.page).is_some_and(|g| {
                    g.source_version == stamp.source && g.view_version == stamp.view
                })
            })
    }
}
#[test]
fn actual_legacy_index_and_distant_height_tiles_recover_without_ore_or_rawpage_leaks() {
    let directory = Directory::new();
    let (world, base) = fixture(&directory);
    let mut config = ServerConfig::new(&directory.0);
    config.public_seed = 1337;
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 90., 8.5]);
    let server = Server::open_with_world(config.clone(), world).unwrap();
    assert!(!server.height_ready_for_publication());
    let ClientMessage::Hello(owner) =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap()
    else {
        unreachable!()
    };
    let mut runtime = Runtime::bind(
        server,
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let guest = Hello {
        name: "height-guest".into(),
        identity: net::PlayerId::default(),
        credential: None,
    };
    let mut bots = [
        Bot::new(runtime.udp_address().unwrap(), owner, 0),
        Bot::new(runtime.udp_address().unwrap(), guest, 1),
    ];
    let started = Instant::now();
    let mut phase = 0;
    let mut old_source = 0;
    let mut done = false;
    let mut ended = 0;
    for now in (0..60000u64).step_by(5) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "height actualdeadline phase{phase}"
        );
        for bot in &mut bots {
            bot.step(now);
        }
        runtime.poll(now).unwrap();
        assert!(!runtime.server().is_failed(), "heightserverfailed");
        for bot in &mut bots {
            bot.step(now);
        }
        if phase == 0
            && bots.iter().all(|b| {
                b.tiles
                    .get(&leaf())
                    .is_some_and(|t| matches!(t.replacement, Replacement::None))
                    && b.tiles.contains_key(&coarse())
                    && b.raw_denied
            })
        {
            assert!(runtime.server().height_ready_for_publication());
            assert_eq!(
                runtime.server().counters().height_migration_cells,
                runtime.server().counters().height_migration_pages * 4096
            );
            let sky = runtime
                .server()
                .height_authority()
                .sky_proof(SpaceId(0), None)
                .unwrap();
            assert_eq!(
                world::lighting::SkyAccess::incoming_from_unknown_above(&sky, point(8, 127, 8)),
                None,
                "persistedroofacrossUnknowngap"
            );
            old_source = bots[0].tiles[&leaf()].source;
            let mut tx = Transaction::new(Tick(runtime.server().current_tick().0 + 1), SpaceId(0));
            for y in base..=base + 8 {
                for x in 4..=11 {
                    if x == 4 || x == 11 || y == base + 8 {
                        tx.set_cell(
                            point(16384 + x, y, 8),
                            Cell::from_parts(rules::ids::STONE, 0),
                        );
                    }
                }
            }
            assert!(runtime.server_mut().queue_transaction(tx).is_ok());
            phase = 1;
        }
        if phase == 1
            && bots.iter().all(|b| {
                b.manifest_ready()
                    && b.tiles[&leaf()].source > old_source
                    && matches!(b.tiles[&coarse()].replacement, Replacement::Refine)
            })
        {
            for bot in &bots {
                for y in 0..8 {
                    assert!(bot.client.replica().page(far(y)).is_none());
                }
                assert!(!bot.secret_seen);
            }
            assert!(runtime.server().counters().height_published >= 4);
            assert!(
                bots.iter()
                    .map(|b| b.up.counters().lost + b.down.counters().lost)
                    .sum::<u64>()
                    > 0
            );
            assert!(bots.iter().map(|b| b.reordered).sum::<u64>() > 0);
            done = true;
            ended = now;
            break;
        }
        thread::sleep(Duration::from_micros(200));
    }
    assert!(
        done,
        "heightstream incomplete phase{phase} server={:?} tilecounts={:?}",
        runtime.server().counters(),
        bots.iter()
            .map(|b| (
                b.tiles.len(),
                b.raw_denied,
                b.tiles.get(&leaf()).map(|t| (&t.replacement, t.source)),
                b.client.geometry(far(0)).is_some()
            ))
            .collect::<Vec<_>>()
    );
    let mut eviction = Transaction::new(Tick(runtime.server().current_tick().0 + 1), SpaceId(0));
    eviction.remove_page(PageId {
        space: SpaceId(0),
        x: 0,
        y: 16,
        z: 0,
    });
    assert!(runtime.server_mut().queue_transaction(eviction).is_ok());
    let mut evicted = false;
    for now in (ended + 5..ended + 5000).step_by(5) {
        for bot in &mut bots {
            bot.step(now);
        }
        runtime.poll(now).unwrap();
        assert!(!runtime.server().is_failed());
        if runtime
            .server()
            .world()
            .snapshot(PageId {
                space: SpaceId(0),
                x: 0,
                y: 16,
                z: 0,
            })
            .is_none()
            && !runtime.server().awaiting_durability()
        {
            evicted = true;
            break;
        }
        thread::sleep(Duration::from_micros(200));
    }
    assert!(
        evicted,
        "roof page eviction must cross its own durable boundary"
    );
    let sky = runtime
        .server()
        .height_authority()
        .sky_proof(SpaceId(0), None)
        .unwrap();
    assert_eq!(
        world::lighting::SkyAccess::incoming_from_unknown_above(&sky, point(8, 127, 8)),
        None,
        "eviction cannot erase the persisted roof"
    );
    let authority_key = server::height_index::TileKey {
        space: SpaceId(0),
        level: 0,
        x: 1024,
        z: 0,
    };
    let source = runtime
        .server()
        .height_authority()
        .index()
        .source(authority_key)
        .unwrap();
    let migration_cells = runtime.server().counters().height_migration_cells;
    drop(runtime);
    let recovered = Server::open(config).unwrap();
    assert!(
        recovered.height_ready_for_publication(),
        "durable rebuilt checkpoint must not restart a migration"
    );
    assert_eq!(recovered.counters().height_migration_cells, 0);
    assert_eq!(
        recovered
            .height_authority()
            .index()
            .source(authority_key)
            .unwrap(),
        source
    );
    assert!(
        recovered
            .world()
            .snapshot(PageId {
                space: SpaceId(0),
                x: 0,
                y: 16,
                z: 0
            })
            .is_none()
    );
    let sky = recovered
        .height_authority()
        .sky_proof(SpaceId(0), None)
        .unwrap();
    assert_eq!(
        world::lighting::SkyAccess::incoming_from_unknown_above(&sky, point(8, 127, 8)),
        None,
        "reopening with the roof page absent preserves darkness"
    );
    println!(
        "HEIGHT_STREAM_OK clients2 distance1024pages latency75+-20ms loss2percent manifestedColumns8 rawFarPages0 privateLeaks0 migrationCells{} evictionRestartExacttrue",
        migration_cells
    );
}
