//! Movement through real native UDP sockets, with deterministic packet delay,
//! jitter, loss and reordering in both directions. Sequenced input prediction
//! measurements are reported explicitly; legacy timing is not a zero-correction
//! claim because neutral server ticks can precede delayed input frames.
use core_types::{Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use net::emulator::{NetworkProfile, SimulatedLink};
use net::udp::{Channel, Limits, MAX_DATAGRAM, Side};
use net::{
    Action, ActionId, ActionStatus, ClientMessage, Hello, InputFrame, PlayerId, PlayerState,
    ServerMessage,
};
use server::transport::Runtime;
use server::{Server, ServerConfig};
use sim::{Keys, Player, prediction::PredictionState};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use world::{CellRead, Transaction, World};

const INPUTS: u64 = 80;
const LINK_BYTES: usize = 524_288;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "movement-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
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
fn local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}
fn flat() -> World {
    let mut world = World::new(rules::properties, 256);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    // At least3x3 horizontal pages, with additional forward room for the flyer
    // and known vertical neighbors for the actual collision/fluid queries.
    for z in -1..=4 {
        for x in -1..=1 {
            for y in -1..=1 {
                let mut cells = vec![Cell::AIR; PAGE_VOLUME];
                for ly in 0..16 {
                    if y * 16 + ly <= 0 {
                        for lz in 0..16 {
                            for lx in 0..16 {
                                cells[lx + lz * 16 + ly as usize * 256] =
                                    Cell::from_parts(rules::ids::STONE, 0);
                            }
                        }
                    }
                }
                tx.create_page(
                    PageId {
                        space: SpaceId(0),
                        x,
                        y,
                        z,
                    },
                    cells,
                );
            }
        }
    }
    world.commit(tx).unwrap();
    world
}
fn flat_query(position: [i32; 3]) -> CellRead {
    CellRead::Known(if position[1] <= 0 {
        Cell::from_parts(rules::ids::STONE, 0)
    } else {
        Cell::AIR
    })
}
fn keys(bits: u8) -> Keys {
    Keys {
        fwd: bits & InputFrame::FORWARD != 0,
        back: bits & InputFrame::BACK != 0,
        left: bits & InputFrame::LEFT != 0,
        right: bits & InputFrame::RIGHT != 0,
        jump: bits & InputFrame::JUMP != 0,
        sneak: bits & InputFrame::SNEAK != 0,
        sprint: bits & InputFrame::SPRINT != 0,
    }
}

struct Bot {
    socket: UdpSocket,
    channel: Option<Channel>,
    upstream: SimulatedLink<Vec<u8>>,
    downstream: SimulatedLink<Vec<u8>>,
    hello: Vec<u8>,
    next_hello: u64,
    flying: bool,
    flight_sent: bool,
    flight_accepted: bool,
    state: Option<(Tick, PlayerState)>,
    exact_state: Option<(Tick, PredictionState)>,
    terminal: Option<(Tick, Player)>,
    terminal_checks: usize,
    history: Vec<InputFrame>,
    predicted: Option<Player>,
    predictions: BTreeMap<u64, [f64; 3]>,
    checked_predictions: BTreeSet<u64>,
    maximum_ack: u64,
    corrections: usize,
    maximum_prediction_error: f64,
    state_frames: u64,
    reordered_states: u64,
    last_upstream_packet: u64,
    last_downstream_packet: u64,
    reordered_packets: u64,
    prediction_tick: u64,
    scheduled_predictions: BTreeMap<u64, (u64, [f64; 3], [f64; 3])>,
    scheduled_checked: usize,
    scheduled_corrections: usize,
}
impl Bot {
    fn new(address: SocketAddr, id: u64, flying: bool) -> Self {
        let socket = UdpSocket::bind(local()).unwrap();
        socket.connect(address).unwrap();
        socket.set_nonblocking(true).unwrap();
        let hello = net::encode_client(&ClientMessage::Hello(Hello {
            identity: PlayerId::default(),
            name: format!("movement-{id}"),
            credential: None,
        }))
        .unwrap();
        Self {
            socket,
            channel: None,
            upstream: SimulatedLink::new(NetworkProfile::default(), id * 41 + 13, 512, LINK_BYTES),
            downstream: SimulatedLink::new(
                NetworkProfile::default(),
                id * 41 + 14,
                512,
                LINK_BYTES,
            ),
            hello,
            next_hello: 0,
            flying,
            flight_sent: false,
            flight_accepted: !flying,
            state: None,
            exact_state: None,
            terminal: None,
            terminal_checks: 0,
            history: Vec::new(),
            predicted: None,
            predictions: BTreeMap::new(),
            checked_predictions: BTreeSet::new(),
            maximum_ack: 0,
            corrections: 0,
            maximum_prediction_error: 0.0,
            state_frames: 0,
            reordered_states: 0,
            last_upstream_packet: 0,
            last_downstream_packet: 0,
            reordered_packets: 0,
            prediction_tick: 0,
            scheduled_predictions: BTreeMap::new(),
            scheduled_checked: 0,
            scheduled_corrections: 0,
        }
    }
    fn queue(&mut self, packets: Vec<Vec<u8>>, now: u64) {
        for packet in packets {
            self.upstream.send(now, packet.len(), packet).unwrap();
        }
    }
    fn send(&mut self, message: ClientMessage, reliable: bool, now: u64) {
        let frame = net::encode_client(&message).unwrap();
        let packets = self
            .channel
            .as_mut()
            .unwrap()
            .send(frame, reliable, now)
            .unwrap();
        self.queue(packets, now);
    }
    fn receive_message(&mut self, message: ServerMessage, now: u64) {
        match message {
            ServerMessage::Welcome(welcome) if self.channel.is_none() => {
                self.channel = Some(Channel::new(
                    welcome.session,
                    welcome.new_credential.expect("fresh guest credential"),
                    Side::Client,
                    Limits::default(),
                ));
                self.state = Some((welcome.tick, welcome.player));
            }
            ServerMessage::Prediction {
                tick,
                player,
                continuation,
            } => {
                let state = PredictionState::decode(&continuation).unwrap();
                let decoded = state.to_player().unwrap();
                assert_eq!(decoded.pos, player.position);
                assert_eq!(decoded.vel, player.velocity);
                if self.exact_state.is_none_or(|(previous, _)| tick > previous) {
                    self.exact_state = Some((tick, state));
                }
                // Sequence-only inputs have no promised application tick. Once
                // every input is consumed, validate subsequent authority ticks
                // against an independent neutral continuation instead of
                // comparing a repeated ACK with an earlier input's pose.
                if player.input_sequence == INPUTS {
                    if let Some((previous, mut predicted)) = self.terminal.take() {
                        if tick > previous {
                            for _ in previous.0..tick.0 {
                                predicted.tick(&flat_query, Keys::default());
                            }
                            assert_eq!(
                                PredictionState::capture(&predicted).unwrap(),
                                state,
                                "neutral convergence after the final consumed input at {tick:?}"
                            );
                            self.terminal_checks += 1;
                        } else {
                            self.terminal = Some((previous, predicted));
                            return;
                        }
                    }
                    self.terminal = Some((tick, decoded));
                }
            }
            ServerMessage::State { tick, player } => {
                self.state_frames += 1;
                assert!(net::valid_position(player.position));
                assert!(
                    player
                        .velocity
                        .iter()
                        .all(|v| v.is_finite() && v.abs() < 2.0)
                );
                assert!((0.999..16.0).contains(&player.position[1]), "{player:?}");
                assert!((8.0..64.0).contains(&player.position[2]), "{player:?}");
                assert!((player.position[0] - 8.5).abs() < 1e-9);
                assert!(player.input_sequence <= INPUTS);
                self.maximum_ack = self.maximum_ack.max(player.input_sequence);
                if let Some(&(sequence, position, velocity)) =
                    self.scheduled_predictions.get(&tick.0)
                {
                    assert_eq!(
                        player.input_sequence, sequence,
                        "scheduled input consumed at its server tick"
                    );
                    self.scheduled_checked += 1;
                    self.scheduled_corrections +=
                        usize::from(player.position != position || player.velocity != velocity);
                }
                if self.state.is_some_and(|(previous, _)| previous > tick) {
                    self.reordered_states += 1;
                    return;
                }
                let input_target = player
                    .input_sequence
                    .checked_sub(1)
                    .and_then(|index| self.history.get(index as usize))
                    .map_or(0, |frame| frame.target_tick.0);
                // An ACK may repeat while neutral physics advances. Scheduled
                // prediction compares the state for its exact server tick;
                // comparing that later state against an earlier input pose
                // would incorrectly label normal inertia as a correction.
                if player.input_sequence > 0
                    && (input_target == 0 || input_target == tick.0)
                    && self.checked_predictions.insert(player.input_sequence)
                    && let Some(predicted) = self.predictions.get(&player.input_sequence)
                {
                    let error = predicted
                        .iter()
                        .zip(player.position)
                        .map(|(a, b)| (a - b) * (a - b))
                        .sum::<f64>()
                        .sqrt();
                    self.maximum_prediction_error = self.maximum_prediction_error.max(error);
                    self.corrections += usize::from(error > 1e-6);
                }
                self.state = Some((tick, player));
            }
            ServerMessage::Action(reply) if reply.id == ActionId(1) => {
                assert_eq!(reply.status, ActionStatus::Accepted);
                self.flight_accepted = true;
            }
            ServerMessage::Error(reason) => panic!("movement bot rejected: {reason:?}"),
            _ => {}
        }
        if self.flying && self.channel.is_some() && !self.flight_sent {
            self.send(
                ClientMessage::Action {
                    id: ActionId(1),
                    action: Action::Flight(true),
                },
                true,
                now,
            );
            self.flight_sent = true;
        }
    }
    fn input(&mut self, sequence: u64, target: u64, now: u64) {
        let mut bits = InputFrame::FORWARD;
        if self.flying && sequence <= 12 {
            bits |= InputFrame::JUMP;
        }
        let frame = InputFrame {
            sequence,
            target_tick: Tick(target),
            keys: bits,
            yaw: 0.0,
            pitch: 0.0,
        };
        if self.predicted.is_none() {
            let (tick, state) = self.exact_state.expect("exact joined continuation");
            self.prediction_tick = tick.0;
            self.predicted = Some(state.to_player().unwrap());
        }
        let predicted = self.predicted.as_mut().unwrap();
        if target > 0 {
            while self.prediction_tick + 1 < target {
                predicted.tick(&flat_query, Keys::default());
                self.prediction_tick += 1;
            }
        }
        predicted.tick(&flat_query, keys(frame.keys));
        self.prediction_tick = if target > 0 {
            target
        } else {
            self.prediction_tick + 1
        };
        if target > 0 {
            self.scheduled_predictions
                .insert(target, (sequence, predicted.pos, predicted.vel));
        }
        self.predictions.insert(sequence, predicted.pos);
        self.history.push(frame);
        self.repeat_inputs(now);
        // New UDP message IDs containing already sent input sequences must not
        // cause a second physics step for those sequences.
        if sequence.is_multiple_of(7) {
            self.repeat_inputs(now);
        }
    }
    fn repeat_inputs(&mut self, now: u64) {
        let start = self.history.len().saturating_sub(net::MAX_INPUT_HISTORY);
        self.send(
            ClientMessage::Inputs(self.history[start..].to_vec()),
            false,
            now,
        );
    }
    fn step(&mut self, now: u64) {
        if self.channel.is_none() && now >= self.next_hello {
            self.upstream
                .send(now, self.hello.len(), self.hello.clone())
                .unwrap();
            self.next_hello = now + 300;
        }
        while let Some(packet) = self.upstream.receive(now) {
            self.reordered_packets +=
                usize::from(packet_reordered(&packet, &mut self.last_upstream_packet)) as u64;
            self.socket.send(&packet).unwrap();
        }
        let mut bytes = [0; MAX_DATAGRAM + 1];
        loop {
            match self.socket.recv(&mut bytes) {
                Ok(len) => self
                    .downstream
                    .send(now, len, bytes[..len].to_vec())
                    .unwrap(),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("movement UDP socket: {error}"),
            }
        }
        while let Some(packet) = self.downstream.receive(now) {
            self.reordered_packets +=
                usize::from(packet_reordered(&packet, &mut self.last_downstream_packet)) as u64;
            if packet.starts_with(b"CUBI") {
                self.receive_message(net::decode_server(&packet).unwrap(), now);
            } else if let Some(channel) = &mut self.channel {
                let received = channel.receive(&packet, now).unwrap();
                self.queue(received.replies, now);
                for frame in received.frames {
                    self.receive_message(net::decode_server(&frame).unwrap(), now);
                }
            }
        }
        if let Some(channel) = &mut self.channel {
            let packets = channel.tick(now).unwrap();
            self.queue(packets, now);
        }
    }
}

fn packet_reordered(packet: &[u8], previous: &mut u64) -> bool {
    if !packet.starts_with(b"CUDP") || packet.len() < 20 {
        return false;
    }
    let sequence = u64::from_le_bytes(packet[12..20].try_into().unwrap());
    let reordered = sequence < *previous;
    *previous = (*previous).max(sequence);
    reordered
}

#[test]
fn walk_and_flight_authority_consume_each_input_once_through_real_delayed_udp() {
    run_movement(false, false);
}
#[test]
fn scheduled_inputs_match_shared_simulation_at_authoritative_ticks() {
    // Repeat the exact tick oracle under different host scheduling. Network
    // jitter/loss and authority tick durations remain unchanged in both runs.
    run_movement(true, false);
    run_movement(true, true);
}
fn run_movement(scheduled: bool, host_delays: bool) {
    let directory = Directory::new();
    let server = Server::open_with_world(directory.config(), flat()).unwrap();
    let mut runtime = Runtime::bind(server, local(), local()).unwrap();
    let mut bots = [
        Bot::new(runtime.udp_address().unwrap(), 1, false),
        Bot::new(runtime.udp_address().unwrap(), 2, true),
    ];
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut next_input = None;
    let mut sent = 0;
    let mut first_target = 0;
    let mut duplicates_since = None;
    let mut next_duplicate = 0;
    let mut finished_at = None;
    for now in (0..=30_000).step_by(5) {
        assert!(
            Instant::now() < deadline,
            "real socket/save worker deadline"
        );
        for bot in &mut bots {
            bot.step(now);
        }
        if next_input.is_none()
            && bots.iter().all(|bot| {
                bot.channel.is_some()
                    && bot.flight_accepted
                    && bot
                        .exact_state
                        .is_some_and(|(_, state)| state.to_player().unwrap().flying == bot.flying)
            })
        {
            next_input = Some(now + 50);
            if scheduled {
                first_target = runtime.server().current_tick().0 + 12;
            }
        }
        if sent < INPUTS && next_input.is_some_and(|due| now >= due) {
            sent += 1;
            for bot in &mut bots {
                bot.input(
                    sent,
                    if scheduled {
                        first_target + sent - 1
                    } else {
                        0
                    },
                    now,
                );
            }
            // A client scheduling hiccup creates neutral authority ticks in
            // legacy mode. It makes the old sequence/pose oracle's ambiguity
            // deterministic while retaining the exact network profile.
            next_input = Some(now + if !scheduled && sent == 24 { 250 } else { 50 });
        }
        // Continue the redundant tail until both ACKs arrive; the final input
        // packet itself may have been one of the2% discarded packets.
        if sent == INPUTS && now >= next_duplicate {
            for bot in &mut bots {
                bot.repeat_inputs(now);
            }
            next_duplicate = now + 50;
        }
        let before = runtime.server().counters().ticks;
        runtime.poll(now).unwrap();
        let counters = runtime.server().counters();
        assert!(!runtime.server().is_failed());
        assert!(counters.ticks - before <= 1, "one owner tick per50ms gate");
        assert!(counters.ticks <= now / 50 + 1, "20Hz maximum: {counters:?}");
        assert!(counters.input_steps <= 2 * counters.ticks);
        for bot in &mut bots {
            bot.step(now);
        }
        if bots.iter().all(|bot| bot.maximum_ack == INPUTS) {
            let since = *duplicates_since.get_or_insert(now);
            if now >= since + 1_000 {
                finished_at = Some(now);
                break;
            }
        }
        // The worker performs real fsync. Yield bounded real time so virtual
        // network time does not starve it or turn scheduling speed into a test.
        thread::sleep(if host_delays && now % 85 == 0 {
            Duration::from_millis(3)
        } else {
            Duration::from_micros(500)
        });
    }
    let now = finished_at.expect("both bots acknowledged80input sequences within30s virtual time");
    let counters = runtime.server().counters();
    let transport = runtime.counters();
    assert_eq!(sent, INPUTS);
    assert_eq!(
        counters.input_steps,
        2 * INPUTS,
        "late duplicates cannot repeat input steps"
    );
    assert_eq!(transport.accepted_udp, 2);
    assert_eq!(transport.peak_connections, 2);
    assert_eq!(transport.queue_overflows, 0);
    assert_eq!(transport.rejected_packets, 0);
    assert!(transport.received_datagrams > 160 && transport.sent_datagrams > 160);
    assert!(bots[0].state.unwrap().1.position[2] > 20.0, "walker moved");
    assert!(
        bots[1].state.unwrap().1.flying,
        "F flight remains authoritative"
    );
    assert!(bots[1].state.unwrap().1.position[1] > 3.0, "flyer climbed");
    assert!(bots[1].state.unwrap().1.position[2] > bots[0].state.unwrap().1.position[2]);
    let lost: u64 = bots
        .iter()
        .map(|b| b.upstream.counters().lost + b.downstream.counters().lost)
        .sum();
    assert!(lost > 0, "the configured profile really lost packets");
    for bot in &bots {
        assert_eq!(bot.maximum_ack, INPUTS);
        assert_eq!(bot.predictions.len(), INPUTS as usize);
        assert!(bot.checked_predictions.len() > 60);
        assert!(
            bot.terminal_checks >= 5,
            "independent idle convergence checks"
        );
        if !scheduled {
            assert!(
                bot.corrections > 0,
                "legacy client hiccup exercises correction reporting"
            );
        }
        if scheduled {
            assert_eq!(
                bot.corrections, 0,
                "scheduled input pose is compared only at its exact authority tick"
            );
            assert_eq!(bot.maximum_prediction_error, 0.0);
            assert!(bot.scheduled_checked > 60);
            assert_eq!(
                bot.scheduled_corrections, 0,
                "position and velocity match at global server ticks"
            );
        }
        assert!(bot.upstream.counters().peak_bytes <= LINK_BYTES);
        assert!(bot.downstream.counters().peak_bytes <= LINK_BYTES);
    }
    let reordered: u64 = bots.iter().map(|bot| bot.reordered_packets).sum();
    assert!(reordered > 0, "jitter actually reordered datagrams");
    eprintln!(
        "movement UDP scheduled={scheduled} host_delays={host_delays} bots=2 inputs_per_bot={INPUTS} virtual_ms={now} profile=75+/-20ms/2%loss lost={lost} reordered={reordered} input_steps={} owner_ticks={} traffic_in={} traffic_out={} queue_overflows={} seq_prediction_corrections=[{},{}] max_prediction_error=[{:.6},{:.6}] scheduled_tick_checks=[{},{}] scheduled_tick_corrections=[{},{}]",
        counters.input_steps,
        counters.ticks,
        transport.received_bytes,
        transport.sent_bytes,
        transport.queue_overflows,
        bots[0].corrections,
        bots[1].corrections,
        bots[0].maximum_prediction_error,
        bots[1].maximum_prediction_error,
        bots[0].scheduled_checked,
        bots[1].scheduled_checked,
        bots[0].scheduled_corrections,
        bots[1].scheduled_corrections
    );
}
