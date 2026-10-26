//! Real localhost UDP and the production authenticated channel. The profile
//! delays actual datagrams in both directions; no in-memory authority shortcut.
use net::emulator::{NetworkProfile, SimulatedLink};
use net::udp::{Channel, Limits, MAX_DATAGRAM, Side, UdpError};
use net::{
    Action, ActionId, ClientMessage, Hello, PageUpdate, Replica, ReplicaError, SecretToken,
    ServerMessage,
};
use std::{
    collections::VecDeque,
    io,
    net::{SocketAddr, UdpSocket},
};
#[derive(Clone, Copy, Debug, Default)]
pub struct Traffic {
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub sent_packets: u64,
    pub received_packets: u64,
    pub entity_frames: u64,
    pub entity_poses: u64,
    pub pongs: u64,
    pub reordered: u64,
    pub receive_pressure: u64,
}
pub struct Bot {
    socket: UdpSocket,
    channel: Option<Channel>,
    credential: SecretToken,
    upstream: SimulatedLink<Vec<u8>>,
    downstream: SimulatedLink<Vec<u8>>,
    outgoing: VecDeque<Vec<u8>>,
    hello: Vec<u8>,
    next_hello: u64,
    next_ping: u64,
    pub traffic: Traffic,
    pub joined: bool,
    pub page_received: bool,
    pub mode: u8,
    pub positioned: bool,
    target: [f64; 3],
    own: net::PlayerId,
    entity_revision: u64,
    replica: Replica,
    next_action: u64,
    last_packet: u64,
}
impl Bot {
    pub fn new(address: SocketAddr, hello: Hello, ordinal: u64, target: [f64; 3]) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(address).unwrap();
        socket.set_nonblocking(true).unwrap();
        Self {
            socket,
            channel: None,
            credential: hello.credential.expect("trusted fixture identity"),
            upstream: SimulatedLink::new(NetworkProfile::default(), ordinal * 41 + 1, 512, 524_288),
            downstream: SimulatedLink::new(
                NetworkProfile::default(),
                ordinal * 41 + 2,
                512,
                524_288,
            ),
            outgoing: VecDeque::new(),
            hello: net::encode_client(&ClientMessage::Hello(hello.clone())).unwrap(),
            next_hello: 0,
            next_ping: 0,
            traffic: Traffic::default(),
            joined: false,
            page_received: false,
            mode: 0,
            positioned: false,
            target,
            own: hello.identity,
            entity_revision: 0,
            replica: Replica::new(1),
            next_action: 1,
            last_packet: 0,
        }
    }
    fn queue(&mut self, message: ClientMessage) {
        assert!(self.outgoing.len() < 128, "bounded bot application queue");
        self.outgoing
            .push_back(net::encode_client(&message).unwrap());
    }
    fn packets(&mut self, packets: Vec<Vec<u8>>, now: u64) {
        for packet in packets {
            self.upstream.send(now, packet.len(), packet).unwrap();
        }
    }
    pub fn command(&mut self, text: &str) {
        let id = ActionId(self.next_action);
        self.next_action += 1;
        self.queue(ClientMessage::Action {
            id,
            action: Action::Command(text.into()),
        });
    }
    fn message(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::Welcome(w) if self.channel.is_none() => {
                self.joined = true;
                self.next_action = w.next_action.0;
                self.mode = w.player.mode;
                self.channel = Some(Channel::new(
                    w.session,
                    w.new_credential.unwrap_or(self.credential),
                    Side::Client,
                    Limits::default(),
                ));
                self.command(&format!(
                    "/tp {} {} {}",
                    self.target[0], self.target[1], self.target[2]
                ));
            }
            ServerMessage::Welcome(_) => {}
            ServerMessage::Pong(_) => self.traffic.pongs += 1,
            ServerMessage::Page(update) => {
                let p = match &update {
                    PageUpdate::Snapshot(s) => s.page,
                    PageUpdate::Delta(s) => s.page,
                };
                match self.replica.apply(update) {
                    Ok(_) => {
                        self.page_received = true;
                        self.queue(ClientMessage::AckPage {
                            page: p,
                            version: self.replica.page(p).unwrap().version,
                        });
                    }
                    Err(ReplicaError::MissingBase { .. }) => {
                        self.queue(ClientMessage::RequestPage {
                            page: p,
                            acknowledged: None,
                        })
                    }
                    Err(error) => panic!("replica: {error:?}"),
                }
            }
            ServerMessage::Entities(frame) => {
                self.traffic.entity_frames += 1;
                self.traffic.entity_poses += frame.upserts.len() as u64;
                if frame.revision <= self.entity_revision {
                    self.queue(ClientMessage::Entities(net::entities::EntityRequest::Ack {
                        revision: self.entity_revision,
                    }));
                } else if frame.base.is_none() || frame.base == Some(self.entity_revision) {
                    self.entity_revision = frame.revision;
                    self.queue(ClientMessage::Entities(net::entities::EntityRequest::Ack {
                        revision: frame.revision,
                    }));
                } else {
                    self.queue(ClientMessage::Entities(
                        net::entities::EntityRequest::Full {
                            have: Some(self.entity_revision),
                        },
                    ));
                }
            }
            ServerMessage::Presence(net::presence::PresenceServerEvent::Ping { nonce, .. }) => self
                .queue(ClientMessage::Presence(
                    net::presence::PresenceClientEvent::Pong { nonce },
                )),
            ServerMessage::Prediction { player, .. } | ServerMessage::State { player, .. }
                if player.identity == self.own =>
            {
                assert_eq!(player.health, 20.);
                self.mode = player.mode;
                if !self.positioned && player.position == self.target {
                    self.positioned = true;
                    let p = self.target.map(|v| v.floor() as i32);
                    self.queue(ClientMessage::RequestPage {
                        page: core_types::PageId {
                            space: player.space,
                            x: p[0].div_euclid(16),
                            y: p[1].div_euclid(16),
                            z: p[2].div_euclid(16),
                        },
                        acknowledged: None,
                    });
                }
            }
            ServerMessage::Action(reply) => assert_eq!(
                reply.status,
                net::ActionStatus::Accepted,
                "fixture command rejected"
            ),
            ServerMessage::Error(reason) => panic!("scale bot rejected {reason:?}"),
            _ => {}
        }
    }
    pub fn step(&mut self, now: u64) {
        if self.channel.is_none() && now >= self.next_hello {
            self.upstream
                .send(now, self.hello.len(), self.hello.clone())
                .unwrap();
            self.next_hello = now + 300;
        }
        if self.channel.is_some() && now >= self.next_ping {
            self.queue(ClientMessage::Ping(now));
            self.next_ping = now + 1000;
        }
        while let Some(bytes) = self.outgoing.front() {
            let Some(channel) = &mut self.channel else {
                break;
            };
            match channel.send(bytes.clone(), true, now) {
                Ok(packets) => {
                    self.outgoing.pop_front();
                    self.packets(packets, now);
                }
                Err(UdpError::Capacity) => break,
                Err(error) => panic!("bot channel: {error:?}"),
            }
        }
        if let Some(channel) = &mut self.channel {
            let packets = channel.tick(now).unwrap_or_else(|error| {
                panic!(
                    "bot reliability error={error:?} expired={:?} pending={} counters={:?}",
                    channel.last_expired(),
                    channel.pending_messages(),
                    channel.counters()
                )
            });
            self.packets(packets, now);
        }
        while let Some(packet) = self.upstream.receive(now) {
            self.socket.send(&packet).unwrap();
            self.traffic.sent_bytes += packet.len() as u64;
            self.traffic.sent_packets += 1;
        }
        let mut buffer = [0; MAX_DATAGRAM + 1];
        loop {
            match self.socket.recv(&mut buffer) {
                Ok(n) => {
                    assert!(n <= MAX_DATAGRAM);
                    self.traffic.received_bytes += n as u64;
                    self.traffic.received_packets += 1;
                    self.downstream.send(now, n, buffer[..n].to_vec()).unwrap();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("bot socket {e}"),
            }
        }
        while let Some(packet) = self.downstream.receive(now) {
            if packet.starts_with(b"CUBI") {
                self.message(net::decode_server(&packet).unwrap());
                continue;
            }
            if let Some(channel) = &mut self.channel {
                let sequence = u64::from_le_bytes(packet[12..20].try_into().unwrap());
                self.traffic.reordered += u64::from(sequence < self.last_packet);
                self.last_packet = self.last_packet.max(sequence);
                let received = match channel.receive(&packet, now) {
                    Ok(received) => received,
                    Err(UdpError::Capacity) => {
                        self.traffic.receive_pressure += 1;
                        continue;
                    }
                    Err(error) => panic!("authenticated UDP receive: {error:?}"),
                };
                self.packets(received.replies, now);
                for bytes in received.frames {
                    self.message(net::decode_server(&bytes).unwrap());
                }
            }
        }
    }
    pub fn discarded_snapshots(&self) -> u64 {
        self.channel
            .as_ref()
            .map_or(0, |c| c.counters().discarded_unreliable_assemblies)
    }
    pub fn loss(&self) -> u64 {
        self.upstream.counters().lost + self.downstream.counters().lost
    }
    pub fn queue_peak_bytes(&self) -> usize {
        self.upstream
            .counters()
            .peak_bytes
            .max(self.downstream.counters().peak_bytes)
    }
}

/// Bots run independently of the owner: a long owner poll cannot prevent a
/// localhost client from servicing its actual channel/retransmission clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct PeerStats {
    pub traffic: Traffic,
    pub lost: u64,
    pub peak_link_bytes: usize,
    pub discarded_unreliable: u64,
}
pub struct Fleet {
    stats: std::sync::Arc<std::sync::Mutex<Vec<PeerStats>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pages: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    spectators: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    command: std::sync::mpsc::SyncSender<()>,
    handle: Option<std::thread::JoinHandle<Vec<Bot>>>,
}
impl Fleet {
    pub fn start(mut bots: Vec<Bot>, started: std::time::Instant) -> Self {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        let stats = Arc::new(std::sync::Mutex::new(vec![
            PeerStats::default();
            bots.len()
        ]));
        let worker_stats = Arc::clone(&stats);
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicUsize::new(0));
        let pages = Arc::new(AtomicUsize::new(0));
        let worker_pages = Arc::clone(&pages);
        let spectators = Arc::new(AtomicUsize::new(0));
        let worker_spectators = Arc::clone(&spectators);
        let (command, commands) = std::sync::mpsc::sync_channel(1);
        let worker_stop = Arc::clone(&stop);
        let worker_ready = Arc::clone(&ready);
        let handle = std::thread::spawn(move || {
            let mut next_stats = 0;
            while !worker_stop.load(Ordering::Acquire) {
                if commands.try_recv().is_ok() {
                    for bot in &mut bots {
                        bot.command("/gamemode spectator");
                    }
                }
                let now = started.elapsed().as_millis() as u64;
                for bot in &mut bots {
                    bot.step(now);
                }
                if now >= next_stats {
                    if let Ok(mut out) = worker_stats.lock() {
                        for (row, bot) in out.iter_mut().zip(&bots) {
                            *row = PeerStats {
                                traffic: bot.traffic,
                                lost: bot.loss(),
                                peak_link_bytes: bot.queue_peak_bytes(),
                                discarded_unreliable: bot.discarded_snapshots(),
                            };
                        }
                    }
                    next_stats = now + 100;
                }
                worker_ready.store(
                    bots.iter()
                        .filter(|b| b.joined && b.positioned && b.traffic.pongs > 0)
                        .count(),
                    Ordering::Release,
                );
                worker_pages.store(
                    bots.iter().filter(|b| b.page_received).count(),
                    Ordering::Release,
                );
                worker_spectators.store(
                    bots.iter()
                        .filter(|b| b.mode == sim::GameMode::Spectator as u8)
                        .count(),
                    Ordering::Release,
                );
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
            bots
        });
        Self {
            stats,
            stop,
            ready,
            pages,
            spectators,
            command,
            handle: Some(handle),
        }
    }
    pub fn stats(&self) -> Vec<PeerStats> {
        self.stats.lock().expect("numeric bot stats").clone()
    }
    pub fn ready(&self, expected: usize) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire) == expected
    }
    pub fn pages_ready(&self) -> usize {
        self.pages.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn spectate(&self) {
        self.command.try_send(()).expect("one fleet transition");
    }
    pub fn spectators(&self) -> usize {
        self.spectators.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn check(&self) {
        assert!(
            !self.handle.as_ref().is_some_and(|h| h.is_finished()),
            "independent UDP bot failed"
        );
    }
    pub fn finish(&mut self) -> Vec<Bot> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.handle
            .take()
            .expect("fleet running")
            .join()
            .expect("UDP fleet result")
    }
}
impl Drop for Fleet {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
            let _ = self.handle.take().unwrap().join();
        }
    }
}
