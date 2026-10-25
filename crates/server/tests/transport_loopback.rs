//! Real socket integration: the emulator delays actual UDP datagrams, while
//! each bot uses the same bounded authenticated channel as a native client.

use core_types::{PageId, SpaceId};
use net::emulator::{NetworkProfile, SimulatedLink};
use net::udp::{Channel, Limits, MAX_DATAGRAM, Side};
use net::{ClientMessage, Hello, PlayerId, ServerMessage};
use server::ServerConfig;
use server::transport::{RunningServer, TransportError};
use std::io;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "transport-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
    fn config(&self) -> ServerConfig {
        let mut config = ServerConfig::new(&self.0);
        config.maximum_players = 32;
        config.generation_enabled = false;
        config.spawn_override = Some([0.5, 2.0, 0.5]);
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
fn missing_page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 0,
        y: 1,
        z: 0,
    }
}
fn hello(name: String) -> ClientMessage {
    ClientMessage::Hello(Hello {
        identity: PlayerId::default(),
        name,
        credential: None,
    })
}
fn connect_ws(address: SocketAddr, origin: Option<&str>) -> WebSocket<TcpStream> {
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut request = format!("ws://{address}/").into_client_request().unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", origin.parse().unwrap());
    }
    tungstenite::client(request, socket).unwrap().0
}
fn send_ws(socket: &mut WebSocket<TcpStream>, message: ClientMessage) {
    socket
        .send(Message::Binary(
            net::encode_client(&message).unwrap().into(),
        ))
        .unwrap();
}
fn receive_ws(
    socket: &mut WebSocket<TcpStream>,
    predicate: impl Fn(&ServerMessage) -> bool,
) -> ServerMessage {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Message::Binary(bytes) = socket.read().unwrap() {
            let message = net::decode_server(&bytes).unwrap();
            if let ServerMessage::Error(reason) = message {
                panic!("WebSocket bot rejected: {reason:?}");
            }
            if predicate(&message) {
                return message;
            }
        }
    }
    panic!("expected protocol response before deadline");
}

#[test]
fn websocket_real_hello_ping_and_page_request() {
    let directory = Directory::new();
    let running = RunningServer::spawn(directory.config(), local(), local()).unwrap();
    let mut socket = connect_ws(running.ws_address(), Some("http://localhost:8080"));
    send_ws(&mut socket, hello("web-bot".into()));
    let welcome = receive_ws(&mut socket, |message| {
        matches!(message, ServerMessage::Welcome(_))
    });
    assert!(matches!(welcome, ServerMessage::Welcome(welcome) if welcome.session != 0));
    send_ws(&mut socket, ClientMessage::Ping(1234));
    receive_ws(&mut socket, |message| {
        matches!(message, ServerMessage::Pong(1234))
    });
    send_ws(
        &mut socket,
        ClientMessage::RequestPage {
            page: missing_page(),
            acknowledged: None,
        },
    );
    receive_ws(
        &mut socket,
        |message| matches!(message, ServerMessage::PageUnavailable(page) if *page == missing_page()),
    );
    let counters = running.stop().unwrap();
    assert_eq!(counters.accepted_ws, 1);
    assert!(counters.received_frames >= 3);
    assert_eq!(counters.queue_overflows, 0);
}

#[test]
fn websocket_rejects_foreign_origin_and_oversize_frames() {
    let directory = Directory::new();
    let running = RunningServer::spawn(directory.config(), local(), local()).unwrap();
    let socket = TcpStream::connect(running.ws_address()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut request = format!("ws://{}/", running.ws_address())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", "http://localhost.evil.example".parse().unwrap());
    assert!(tungstenite::client(request, socket).is_err());

    let mut socket = connect_ws(running.ws_address(), None);
    if socket
        .send(Message::Binary(vec![0; net::MAX_FRAME_BYTES + 1].into()))
        .is_ok()
    {
        assert!(matches!(socket.read(), Err(_) | Ok(Message::Close(_))));
    }
    assert_eq!(running.stop().unwrap().received_frames, 0);
}

#[test]
fn remote_bind_is_rejected_before_listening() {
    let directory = Directory::new();
    assert!(matches!(
        RunningServer::spawn(directory.config(), "0.0.0.0:0".parse().unwrap(), local()),
        Err(TransportError::NonLoopback)
    ));
}

struct UdpBot {
    socket: UdpSocket,
    channel: Option<Channel>,
    upstream: SimulatedLink<Vec<u8>>,
    downstream: SimulatedLink<Vec<u8>>,
    hello: Vec<u8>,
    next_hello: u64,
    next_ping: u64,
    pong: bool,
    page_received: bool,
    id: u64,
}
impl UdpBot {
    fn new(server: SocketAddr, id: u64) -> Self {
        let socket = UdpSocket::bind(local()).unwrap();
        socket.connect(server).unwrap();
        socket.set_nonblocking(true).unwrap();
        Self {
            socket,
            channel: None,
            upstream: SimulatedLink::new(NetworkProfile::default(), id * 41 + 1, 512, 524_288),
            downstream: SimulatedLink::new(NetworkProfile::default(), id * 41 + 2, 512, 524_288),
            hello: net::encode_client(&hello(format!("udp-bot-{id}"))).unwrap(),
            next_hello: 0,
            next_ping: 0,
            pong: false,
            page_received: false,
            id,
        }
    }
    fn packets(&mut self, packets: Vec<Vec<u8>>, now_ms: u64) {
        for packet in packets {
            self.upstream.send(now_ms, packet.len(), packet).unwrap();
        }
    }
    fn send(&mut self, message: ClientMessage, now_ms: u64) {
        let packets = self
            .channel
            .as_mut()
            .unwrap()
            .send(net::encode_client(&message).unwrap(), true, now_ms)
            .unwrap();
        self.packets(packets, now_ms);
    }
    fn step(&mut self, now_ms: u64) {
        if self.channel.is_none() && now_ms >= self.next_hello {
            self.upstream
                .send(now_ms, self.hello.len(), self.hello.clone())
                .unwrap();
            self.next_hello = now_ms + 300;
        }
        while let Some(packet) = self.upstream.receive(now_ms) {
            self.socket.send(&packet).unwrap();
        }
        let mut buffer = [0; MAX_DATAGRAM + 1];
        loop {
            match self.socket.recv(&mut buffer) {
                Ok(length) => {
                    self.downstream
                        .send(now_ms, length, buffer[..length].to_vec())
                        .unwrap();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("bot socket: {error}"),
            }
        }
        while let Some(packet) = self.downstream.receive(now_ms) {
            if packet.starts_with(b"CUBI") {
                match net::decode_server(&packet).unwrap() {
                    ServerMessage::Welcome(welcome) if self.channel.is_none() => {
                        self.channel = Some(Channel::new(
                            welcome.session,
                            welcome.new_credential.expect("new guest credential"),
                            Side::Client,
                            Limits::default(),
                        ));
                    }
                    ServerMessage::Error(reason) => {
                        panic!("UDP bot bootstrap rejected: {reason:?}")
                    }
                    _ => {}
                }
                continue;
            }
            if let Some(channel) = &mut self.channel {
                let received = channel.receive(&packet, now_ms).unwrap();
                self.packets(received.replies, now_ms);
                for frame in received.frames {
                    match net::decode_server(&frame).unwrap() {
                        ServerMessage::Pong(id) if id == self.id => self.pong = true,
                        ServerMessage::PageUnavailable(page) if page == missing_page() => {
                            self.page_received = true;
                        }
                        ServerMessage::Error(reason) => panic!("UDP bot rejected: {reason:?}"),
                        _ => {}
                    }
                }
            }
        }
        if self.channel.is_some() && now_ms >= self.next_ping {
            if !self.pong {
                self.send(ClientMessage::Ping(self.id), now_ms);
            }
            if !self.page_received {
                self.send(
                    ClientMessage::RequestPage {
                        page: missing_page(),
                        acknowledged: None,
                    },
                    now_ms,
                );
            }
            self.next_ping = now_ms + 300;
        }
        if let Some(channel) = &mut self.channel {
            let packets = channel.tick(now_ms).unwrap();
            self.packets(packets, now_ms);
        }
    }
}

#[test]
fn thirty_two_real_udp_bots_under_delay_jitter_loss_and_reorder() {
    run_udp_bots(32);
}

#[test]
fn two_real_udp_bots_under_delay_jitter_loss_and_reorder() {
    run_udp_bots(2);
}

fn run_udp_bots(count: usize) {
    let directory = Directory::new();
    let running = RunningServer::spawn(directory.config(), local(), local()).unwrap();
    let mut bots: Vec<_> = (1..=count as u64)
        .map(|id| UdpBot::new(running.udp_address(), id))
        .collect();
    // Unauthenticated payloads cannot allocate a peer or reach the domain.
    bots[0].socket.send(b"CUDP forged session").unwrap();
    let start = Instant::now();
    let deadline = Duration::from_secs(10);
    loop {
        let elapsed = start.elapsed();
        let now_ms = elapsed.as_millis() as u64;
        for bot in &mut bots {
            bot.step(now_ms);
        }
        if bots.iter().all(|bot| bot.pong && bot.page_received) {
            break;
        }
        assert!(
            elapsed < deadline,
            "{}/{count} bots completed the real socket round trip",
            bots.iter()
                .filter(|bot| bot.pong && bot.page_received)
                .count()
        );
        thread::sleep(Duration::from_millis(2));
    }
    let counters = running.stop().unwrap();
    assert_eq!(counters.accepted_udp, count as u64);
    assert_eq!(counters.peak_connections, count);
    assert_eq!(counters.queue_overflows, 0);
    assert!(counters.rejected_packets >= 1);
    assert!(counters.received_datagrams >= 3 * count as u64);
    let lost: u64 = bots
        .iter()
        .map(|bot| bot.upstream.counters().lost + bot.downstream.counters().lost)
        .sum();
    if count == 32 {
        assert!(lost > 0, "profile must actually discard packets");
    }
    assert!(
        bots.iter()
            .all(|bot| bot.upstream.counters().peak_bytes <= 524_288)
    );
    eprintln!(
        "loopback UDP bots={count} profile=75+/-20ms/2%loss lost={lost} traffic_in={} traffic_out={} datagrams_in={} datagrams_out={} queue_overflows={} peak_packets_per_peer={}",
        counters.received_bytes,
        counters.sent_bytes,
        counters.received_datagrams,
        counters.sent_datagrams,
        counters.queue_overflows,
        counters.peak_udp_queue_packets,
    );
}
