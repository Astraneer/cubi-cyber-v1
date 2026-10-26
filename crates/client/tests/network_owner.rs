//! The actual shared client module owns prediction, confirmed images and edit
//! rollback while real UDP sockets carry delayed, lossy protocol datagrams.
#![cfg(not(target_arch = "wasm32"))]
use client::network::{Connection, NetworkConfig};
use core_types::{BlockPos, Cell, PAGE_VOLUME, SpaceId, Tick};
use net::emulator::{NetworkProfile, SimulatedLink};
use net::udp::{Channel, Limits, MAX_DATAGRAM, Side};
use net::{Action, ActionStatus, ClientMessage, Hello, InputFrame, ReplicaRead, ServerMessage};
use server::{Server, ServerConfig, transport::Runtime};
use sim::prediction::PredictionState;
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};

static DIRECTORY_NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "shared-network-{}-{}",
                std::process::id(),
                DIRECTORY_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        let mut config = ServerConfig::new(&self.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 1.0, 8.5]);
        config
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 256);
    let mut cells = vec![Cell::AIR; PAGE_VOLUME];
    for z in 0..16 {
        for x in 0..16 {
            cells[x + z * 16] = Cell::from_parts(rules::ids::STONE, 0);
        }
    }
    cells[8 + 13 * 16 + 2 * 256] = Cell::from_parts(rules::ids::STONE, 0);
    cells[8 + 14 * 16 + 2 * 256] = Cell::from_parts(rules::ids::IRON_ORE, 0);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(pos(0, 0, 0).page(), cells);
    world.commit(tx).unwrap();
    world
}
struct Bot {
    socket: UdpSocket,
    channel: Option<Channel>,
    up: SimulatedLink<Vec<u8>>,
    down: SimulatedLink<Vec<u8>>,
    client: Connection,
    key: [u8; 32],
    last_authority: Option<(Tick, PredictionState)>,
    secret_seen: bool,
    changed_input: bool,
    rejected_place: bool,
}
impl Bot {
    fn new(server: SocketAddr, hello: Hello, key: [u8; 32]) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(server).unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut client = Connection::new(NetworkConfig {
            near_radius: 0,
            retain_radius: 1,
            input_lead_ticks: 12,
            ..NetworkConfig::default()
        })
        .unwrap();
        client
            .hello(hello.name, hello.identity, hello.credential)
            .unwrap();
        Self {
            socket,
            channel: None,
            up: SimulatedLink::new(NetworkProfile::default(), 0x19, 4096, 1_048_576),
            down: SimulatedLink::new(NetworkProfile::default(), 0x91, 4096, 1_048_576),
            client,
            key,
            last_authority: None,
            secret_seen: false,
            changed_input: false,
            rejected_place: false,
        }
    }
    fn queue(&mut self, packets: Vec<Vec<u8>>, now: u64) {
        for packet in packets {
            self.up.send(now, packet.len(), packet).unwrap();
        }
    }
    fn message(&mut self, message: ServerMessage, now: u64) {
        if let ServerMessage::Prediction {
            tick, continuation, ..
        } = &message
            && self
                .last_authority
                .as_ref()
                .is_none_or(|(old, _)| tick > old)
        {
            self.last_authority = Some((*tick, PredictionState::decode(continuation).unwrap()));
        }
        if let ServerMessage::Action(reply) = &message {
            self.rejected_place |= matches!(reply.status, ActionStatus::Rejected(_));
        }
        self.client.receive(message, now).unwrap();
    }
    fn step(&mut self, now: u64) {
        let mut bytes = [0; MAX_DATAGRAM];
        for _ in 0..512 {
            match self.socket.recv(&mut bytes) {
                Ok(n) => {
                    let packet = bytes[..n].to_vec();
                    self.down.send(now, n, packet).unwrap();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("UDP {e}"),
            }
        }
        while let Some(packet) = self.down.receive(now) {
            self.secret_seen |= packet.windows(32).any(|b| b == self.key);
            if packet.starts_with(b"CUBI") {
                let message = net::decode_server(&packet).unwrap();
                if let ServerMessage::Welcome(welcome) = &message {
                    let key = welcome.new_credential.or(self.client.credential()).unwrap();
                    if self.channel.is_none() {
                        self.channel = Some(Channel::new(
                            welcome.session,
                            key,
                            Side::Client,
                            Limits::default(),
                        ));
                    }
                }
                self.message(message, now);
            } else if let Some(channel) = &mut self.channel {
                let received = channel.receive(&packet, now).unwrap();
                self.queue(received.replies, now);
                for frame in received.frames {
                    self.secret_seen |= frame.windows(32).any(|b| b == self.key);
                    self.message(net::decode_server(&frame).unwrap(), now);
                }
            }
        }
        if let Some(channel) = &mut self.channel {
            let packets = channel.tick(now).unwrap();
            self.queue(packets, now);
        }
        self.client.poll(now).unwrap();
        for mut message in self.client.take_outgoing(64) {
            if let ClientMessage::Inputs(frames) = &mut message {
                // A real input disagreement must cause a full-state rollback;
                // later retransmissions preserve exactly the same wire input.
                for frame in frames {
                    if frame.sequence == 8 {
                        frame.keys = InputFrame::LEFT;
                        self.changed_input = true;
                    }
                }
            }
            let bytes = net::encode_client(&message).unwrap();
            if matches!(message, ClientMessage::Hello(_)) {
                self.up.send(now, bytes.len(), bytes).unwrap();
            } else if let Some(channel) = &mut self.channel {
                let reliable = !matches!(message, ClientMessage::Inputs(_));
                let packets = channel.send(bytes, reliable, now).unwrap();
                self.queue(packets, now);
            }
        }
        while let Some(packet) = self.up.receive(now) {
            self.socket.send(&packet).unwrap();
        }
        // This is the owner handoff point for the unchanged View/Scene module.
        // A real render loop uploads these bounded page updates to Scene.
        self.client.take_page_updates(128);
    }
}

#[test]
fn shared_client_predicts_rolls_back_recovers_confirmed_base_and_never_receives_sealed_ore() {
    let directory = Directory::new();
    let server = Server::open_with_world(directory.config(), fixture()).unwrap();
    let owner = std::fs::read(directory.0.join("owner.identity")).unwrap();
    let ClientMessage::Hello(hello) = net::decode_client(&owner).unwrap() else {
        unreachable!()
    };
    let key: [u8; 32] = std::fs::read(directory.0.join("generation.key"))
        .unwrap()
        .try_into()
        .unwrap();
    let mut runtime = Runtime::bind(
        server,
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let mut bot = Bot::new(runtime.udp_address().unwrap(), hello, key);
    let page = pos(0, 0, 0).page();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut requested = false;
    let mut known = false;
    let mut sent = 0;
    let mut next_input = 0;
    let mut rejected = false;
    let mut opened = false;
    let mut exposed = false;
    let mut settled = None;
    let mut teleported = false;
    let mut creative = false;
    let mut placed = false;
    let mut finished = false;
    for now in (0..=30_000).step_by(5) {
        assert!(Instant::now() < deadline, "network worker deadline");
        bot.step(now);
        runtime.poll(now).unwrap();
        bot.step(now);
        if bot.client.player().is_some() && !requested {
            bot.client.request_page(page, now).unwrap();
            requested = true;
        }
        let baseline = bot.client.replica().page(page).is_some();
        if baseline && !known {
            assert_eq!(
                bot.client.read(pos(8, 2, 14)),
                ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0))
            );
            known = true;
            next_input = now + 50;
        }
        if known && sent < 40 && now >= next_input {
            bot.client
                .sample_input(now, InputFrame::FORWARD, 0.0, 0.0)
                .unwrap();
            sent += 1;
            next_input = now + 50;
        }
        if known && sent >= 2 && !rejected {
            bot.client
                .action(
                    Action::Place {
                        position: pos(9, 2, 9),
                        expected: Cell::AIR,
                        cell: Cell::from_parts(rules::ids::STONE, 0),
                    },
                    true,
                    now,
                )
                .unwrap();
            assert_eq!(
                bot.client.read(pos(9, 2, 9)),
                ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0))
            );
            rejected = true;
        }
        let target = if sent < 40 {
            Tick(bot.client.estimated_server_tick(now).0 + 12)
        } else {
            bot.client.acknowledged_tick()
        };
        bot.client.advance_to(target).unwrap();
        if known && sent == 40 && bot.client.pending_inputs() == 0 {
            let since = *settled.get_or_insert(now);
            if now >= since + 1_000 && !opened {
                assert!(bot.changed_input);
                assert!(bot.client.counters().corrections > 0);
                assert!(bot.rejected_place);
                assert_eq!(bot.client.predicted_edits(), 0);
                assert_eq!(bot.client.read(pos(9, 2, 9)), ReplicaRead::Known(Cell::AIR));
                let (tick, state) = bot.last_authority.unwrap();
                assert_eq!(bot.client.predicted_tick(), tick);
                assert_eq!(
                    PredictionState::capture(bot.client.player().unwrap()).unwrap(),
                    state
                );
                let mut tx =
                    Transaction::new(Tick(runtime.server().current_tick().0 + 1), SpaceId(0));
                tx.expect_page(page, runtime.server().world().page_version(page));
                tx.set_cell(pos(8, 2, 13), Cell::AIR);
                runtime
                    .server_mut()
                    .queue_transaction(tx)
                    .unwrap_or_else(|_| panic!("owner queue"));
                opened = true;
            }
        }
        if opened
            && !exposed
            && bot.client.read(pos(8, 2, 14))
                == ReplicaRead::Known(Cell::from_parts(rules::ids::IRON_ORE, 0))
        {
            exposed = true;
            // Deliberately losing the cache base forces actual full-image recovery.
            bot.client.request_snapshot(page, now).unwrap();
        }
        if exposed && !creative && bot.client.replica().page(page).is_some() {
            bot.client
                .action(Action::Command("/gamemode creative".into()), false, now)
                .unwrap();
            creative = true;
        }
        if creative
            && !placed
            && bot
                .client
                .player()
                .is_some_and(|p| p.mode == sim::GameMode::Creative)
            && bot.client.pending_actions() == 0
        {
            bot.client
                .action(
                    Action::Place {
                        position: pos(8, 2, 13),
                        expected: Cell::AIR,
                        cell: Cell::from_parts(rules::ids::STONE, 0),
                    },
                    true,
                    now,
                )
                .unwrap();
            placed = true;
            assert_eq!(bot.client.predicted_edits(), 1);
        }
        if placed
            && !teleported
            && bot.client.pending_actions() == 0
            && bot.client.predicted_edits() == 0
        {
            assert_eq!(
                bot.client.replica().read(pos(8, 2, 13)),
                ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0))
            );
            bot.client
                .action(Action::Command("/tp 104.5 1 8.5".into()), false, now)
                .unwrap();
            teleported = true;
        }
        if teleported && bot.client.player().is_some_and(|p| p.pos[0] > 100.0) {
            bot.client.update_interest(now).unwrap();
            assert_eq!(
                bot.client.replica().read(pos(8, 2, 14)),
                ReplicaRead::Unknown
            );
            assert!(bot.client.counters().evicted_pages > 0);
            finished = true;
            break;
        }
        thread::sleep(Duration::from_micros(500));
    }
    assert!(finished && known && exposed && teleported && placed);
    assert!(!bot.secret_seen);
    assert!(bot.up.counters().lost + bot.down.counters().lost > 0);
    assert_eq!(runtime.counters().queue_overflows, 0);
    eprintln!(
        "shared client corrections={} replay_steps={} page_cells={} full_snapshots={} traffic_in={} traffic_out={} loss={}",
        bot.client.counters().corrections,
        bot.client.counters().replay_steps,
        bot.client.counters().page_cells,
        bot.client.counters().full_snapshots,
        runtime.counters().received_bytes,
        runtime.counters().sent_bytes,
        bot.up.counters().lost + bot.down.counters().lost
    );
}

#[test]
fn public_light_crosses_actual_lossy_udp_and_repairs_its_own_base_without_cell_changes() {
    let directory = Directory::new();
    let mut world = fixture();
    let page = pos(0, 0, 0).page();
    let emitter = pos(2, 3, 2);
    let sample = pos(3, 3, 2);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(emitter, Cell::from_parts(rules::ids::TORCH, 0));
    world.commit(tx).unwrap();
    let server = Server::open_with_world(directory.config(), world).unwrap();
    let ClientMessage::Hello(owner) =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap()
    else {
        unreachable!()
    };
    let key: [u8; 32] = std::fs::read(directory.0.join("generation.key"))
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
        identity: net::PlayerId::default(),
        name: "light observer".into(),
        credential: None,
    };
    let mut bots = [Bot::new(address, owner, key), Bot::new(address, guest, key)];
    let mut scenes = std::array::from_fn::<_, 2, _>(|_| {
        client::ClientScene::new(client::SceneConfig {
            authoritative_remote: true,
            center: pos(8, 1, 8),
            ..client::SceneConfig::forest()
        })
        .unwrap()
    });
    let mut configured = [false; 2];
    let mut requested = [false; 2];
    let mut cell_views = [0; 2];
    let mut shape_versions = [core_types::Version(0); 2];
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut stable_at = None;
    let mut injected = false;
    let mut base_lost = false;
    let mut initial_snapshots = 0;
    let desired = world::LightSample {
        sky: 4,
        block: 5,
        rgb: [231, 17, 255],
    };
    let mut complete = false;
    for now in (0..=40_000).step_by(5) {
        assert!(Instant::now() < deadline, "actual UDP light deadline");
        runtime.poll(now).unwrap();
        for (index, bot) in bots.iter_mut().enumerate() {
            bot.step(now);
            bot.client
                .advance_to(bot.client.acknowledged_tick())
                .unwrap();
            if let Some(w) = bot.client.welcome()
                && !configured[index]
            {
                scenes[index]
                    .configure_authority(w.public_seed, pos(8, 1, 8))
                    .unwrap();
                configured[index] = true;
            }
            if bot.client.player().is_some() && !requested[index] {
                bot.client.request_page(page, now).unwrap();
                requested[index] = true;
            }
            if let Some(image) = bot.client.replica().page(page)
                && scenes[index]
                    .authoritative_version(page)
                    .is_none_or(|v| v.view != image.version)
            {
                scenes[index]
                    .import_replica_page(image, bot.client.acknowledged_tick())
                    .unwrap();
            }
            for _ in 0..16 {
                let Some(update) = bot.client.take_light_update() else {
                    break;
                };
                let result = scenes[index]
                    .import_authoritative_light(&update, bot.client.acknowledged_tick());
                assert!(
                    result.is_ok()
                        || matches!(
                            result,
                            Err(client::SceneError::Light(
                                net::light::Error::MissingCells { .. }
                                    | net::light::Error::MissingBase { .. }
                            ))
                        )
                );
            }
            if let Some(image) = bot.client.light_replica().page(page)
                && scenes[index]
                    .authoritative_version(page)
                    .is_some_and(|v| v.view == image.header.cell_view)
                && scenes[index]
                    .authoritative_light_version(page)
                    .is_none_or(|v| v.view < image.header.version)
            {
                scenes[index]
                    .import_replica_light(image, bot.client.acknowledged_tick())
                    .unwrap();
            }
        }
        let lit = scenes.iter().all(|s| {
            s.read_authoritative_light(sample)
                .is_some_and(|l| l.block >= 12)
        });
        if lit && !injected {
            let start = *stable_at.get_or_insert(now);
            if now >= start + 1500 {
                for i in 0..2 {
                    cell_views[i] = bots[i].client.replica().page(page).unwrap().version;
                    shape_versions[i] = scenes[i]
                        .world()
                        .page(page)
                        .unwrap()
                        .channel_version(core_types::ChannelMask::SHAPE);
                }
                initial_snapshots = runtime.server().counters().light_snapshot_cells;
                let mut tx =
                    Transaction::new(Tick(runtime.server().current_tick().0 + 1), SpaceId(0));
                tx.set_light(sample, desired);
                runtime
                    .server_mut()
                    .queue_transaction(tx)
                    .unwrap_or_else(|_| panic!("trusted light owner queue"));
                injected = true;
            }
        }
        if injected
            && scenes
                .iter()
                .all(|s| s.read_authoritative_light(sample) == Some(desired))
        {
            for i in 0..2 {
                assert_eq!(
                    bots[i].client.replica().page(page).unwrap().version,
                    cell_views[i]
                );
                assert_eq!(
                    scenes[i]
                        .world()
                        .page(page)
                        .unwrap()
                        .channel_version(core_types::ChannelMask::SHAPE),
                    shape_versions[i]
                );
            }
            if !base_lost {
                bots[0].client.request_light_snapshot(page).unwrap();
                assert!(bots[0].client.light_replica().page(page).is_none());
                base_lost = true;
            } else if bots[0].client.light_replica().page(page).is_some()
                && runtime.server().counters().light_snapshot_cells >= initial_snapshots + 4096
            {
                complete = true;
                break;
            }
        }
        thread::sleep(Duration::from_micros(300));
    }
    assert!(complete && base_lost);
    assert!(runtime.server().counters().light_deltas > 0);
    assert!(runtime.server().counters().light_delta_cells >= 2);
    assert!(bots.iter().all(|b| !b.secret_seen));
    for scene in scenes {
        assert_eq!(scene.read_authoritative_light(sample), Some(desired));
        assert_eq!(scene.world().loaded_pages(), 1);
    }
}
