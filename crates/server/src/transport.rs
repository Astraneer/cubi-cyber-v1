//! Bounded development transports. These listeners deliberately bind only to
//! loopback: remote deployment requires TLS for WebSocket and encrypted UDP
//! establishment (for example QUIC), neither of which this module advertises.
//!
//! Socket workers own no world state. `Runtime::poll` is the sole caller of the
//! authoritative server and is intended to run on its owning thread.

use crate::{Outgoing, Server, ServerConfig, ServerError};
use net::udp::{Channel, Limits, MAX_DATAGRAM, Side, UdpError};
use net::{ClientMessage, MAX_FRAME_BYTES, ServerMessage};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error as WebSocketError, Message};

const MAX_CONNECTIONS: usize = 64;
const INCOMING_CAPACITY: usize = 256;
const WS_OUTGOING_CAPACITY: usize = 16;
const WS_STAGED_FRAMES: usize = 256;
const WS_STAGED_BYTES: usize = 8 * MAX_FRAME_BYTES;
const ACCEPT_BUDGET: usize = 8;
const RECEIVE_BUDGET: usize = 256;
const SEND_BUDGET: usize = 4096;
const UDP_QUEUE_PACKETS: usize = 512;
const UDP_QUEUE_FRAMES: usize = 64;
const UDP_QUEUE_BYTES: usize = 2 * MAX_FRAME_BYTES;
const IDLE_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    Server(ServerError),
    NonLoopback,
    WorkerPanic,
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "local transport I/O: {error}"),
            Self::Server(error) => write!(f, "server: {error}"),
            Self::NonLoopback => f.write_str("development transports require loopback addresses"),
            Self::WorkerPanic => f.write_str("local server worker panicked"),
        }
    }
}
impl std::error::Error for TransportError {}
impl From<io::Error> for TransportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<ServerError> for TransportError {
    fn from(error: ServerError) -> Self {
        Self::Server(error)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TransportCounters {
    pub accepted_ws: u64,
    pub accepted_udp: u64,
    pub disconnected: u64,
    pub received_frames: u64,
    pub sent_frames: u64,
    pub received_datagrams: u64,
    pub sent_datagrams: u64,
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub rejected_packets: u64,
    pub queue_overflows: u64,
    pub active_connections: usize,
    pub peak_connections: usize,
    pub peak_udp_queue_packets: usize,
    pub peak_ws_staged_bytes: usize,
    pub ingress_backpressure: u64,
}

enum SocketEvent {
    Frame(u64, Vec<u8>),
    Closed(u64),
}

struct WsPeer {
    outgoing: SyncSender<Vec<u8>>,
    socket: TcpStream,
    worker: JoinHandle<()>,
    last_seen: u64,
    frames: VecDeque<Vec<u8>>,
    queued_bytes: usize,
}
struct UdpPeer {
    address: SocketAddr,
    channel: Option<Channel>,
    session: Option<u64>,
    outgoing: VecDeque<Vec<u8>>,
    frames: VecDeque<(Vec<u8>, bool)>,
    queued_frame_bytes: usize,
    last_seen: u64,
}

/// One authoritative world owner plus bounded socket queues.
pub struct Runtime {
    server: Server,
    ws_listener: TcpListener,
    udp_socket: UdpSocket,
    ws: BTreeMap<u64, WsPeer>,
    udp: BTreeMap<u64, UdpPeer>,
    udp_addresses: BTreeMap<SocketAddr, u64>,
    incoming_tx: SyncSender<SocketEvent>,
    incoming_rx: Receiver<SocketEvent>,
    retired_workers: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    next_connection: u64,
    counters: TransportCounters,
    send_cursor: u64,
}

impl Runtime {
    pub fn bind(
        server: Server,
        ws_address: SocketAddr,
        udp_address: SocketAddr,
    ) -> Result<Self, TransportError> {
        if !ws_address.ip().is_loopback() || !udp_address.ip().is_loopback() {
            return Err(TransportError::NonLoopback);
        }
        let ws_listener = TcpListener::bind(ws_address)?;
        ws_listener.set_nonblocking(true)?;
        let udp_socket = UdpSocket::bind(udp_address)?;
        udp_socket.set_nonblocking(true)?;
        let (incoming_tx, incoming_rx) = mpsc::sync_channel(INCOMING_CAPACITY);
        Ok(Self {
            server,
            ws_listener,
            udp_socket,
            ws: BTreeMap::new(),
            udp: BTreeMap::new(),
            udp_addresses: BTreeMap::new(),
            incoming_tx,
            incoming_rx,
            retired_workers: Vec::new(),
            stop: Arc::new(AtomicBool::new(false)),
            next_connection: 1,
            counters: TransportCounters::default(),
            send_cursor: 0,
        })
    }

    pub fn ws_address(&self) -> io::Result<SocketAddr> {
        self.ws_listener.local_addr()
    }
    pub fn udp_address(&self) -> io::Result<SocketAddr> {
        self.udp_socket.local_addr()
    }
    pub fn server(&self) -> &Server {
        &self.server
    }
    pub fn server_mut(&mut self) -> &mut Server {
        &mut self.server
    }
    pub fn counters(&self) -> TransportCounters {
        self.counters
    }

    fn allocate_connection(&mut self) -> Option<u64> {
        if self.ws.len() + self.udp.len() + self.retired_workers.len() >= MAX_CONNECTIONS {
            return None;
        }
        let connection = self.next_connection;
        self.next_connection = self.next_connection.checked_add(1)?;
        Some(connection)
    }

    fn update_connection_counters(&mut self) {
        self.counters.active_connections = self.ws.len() + self.udp.len();
        self.counters.peak_connections = self
            .counters
            .peak_connections
            .max(self.counters.active_connections);
    }

    /// Bounded nonblocking socket work followed by the server's scheduled tick.
    /// The domain owns the 50 ms tick gate, so frequent polling improves I/O
    /// latency without advancing simulation faster than 20 Hz.
    pub fn poll(&mut self, now_ms: u64) -> Result<(), TransportError> {
        self.reap_workers();
        self.accept_ws(now_ms)?;
        for _ in 0..RECEIVE_BUDGET {
            if self.server.inbox_capacity() == 0 {
                self.counters.ingress_backpressure += 1;
                break;
            }
            match self.incoming_rx.try_recv() {
                Ok(SocketEvent::Frame(connection, bytes)) => {
                    if let Some(peer) = self.ws.get_mut(&connection) {
                        peer.last_seen = now_ms;
                        self.counters.received_bytes += bytes.len() as u64;
                        self.receive_frame(connection, &bytes, now_ms);
                    }
                }
                Ok(SocketEvent::Closed(connection)) => self.disconnect(connection),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        self.receive_udp(now_ms)?;
        let outgoing = self.server.tick(now_ms);
        self.dispatch(outgoing, now_ms);
        let mut expired = Vec::new();
        for (&connection, peer) in &mut self.udp {
            if now_ms.saturating_sub(peer.last_seen) > IDLE_TIMEOUT_MS {
                expired.push(connection);
                continue;
            }
            if let Some(channel) = &mut peer.channel
                && peer.outgoing.is_empty()
            {
                match channel.tick(now_ms) {
                    Ok(packets) => {
                        if !queue_packets(peer, packets, &mut self.counters) {
                            expired.push(connection);
                        }
                    }
                    Err(_) => expired.push(connection),
                }
            }
            if !pump_udp_frames(peer, now_ms, &mut self.counters) {
                expired.push(connection);
            }
        }
        expired.extend(self.ws.iter().filter_map(|(&connection, peer)| {
            (now_ms.saturating_sub(peer.last_seen) > IDLE_TIMEOUT_MS).then_some(connection)
        }));
        for connection in expired {
            self.disconnect(connection);
        }
        self.flush_ws();
        self.flush_udp()?;
        self.update_connection_counters();
        Ok(())
    }

    fn accept_ws(&mut self, now_ms: u64) -> Result<(), TransportError> {
        for _ in 0..ACCEPT_BUDGET {
            let (socket, address) = match self.ws_listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            };
            if !address.ip().is_loopback() {
                self.counters.rejected_packets += 1;
                continue;
            }
            let Some(connection) = self.allocate_connection() else {
                self.counters.rejected_packets += 1;
                continue;
            };
            socket.set_nonblocking(false)?;
            socket.set_nodelay(true)?;
            socket.set_read_timeout(Some(Duration::from_secs(2)))?;
            socket.set_write_timeout(Some(Duration::from_millis(100)))?;
            let control_socket = socket.try_clone()?;
            let incoming = self.incoming_tx.clone();
            let stop = Arc::clone(&self.stop);
            let (outgoing, rx) = mpsc::sync_channel(WS_OUTGOING_CAPACITY);
            let worker = thread::Builder::new()
                .name(format!("cubi-ws-{connection}"))
                .spawn(move || websocket_worker(connection, socket, incoming, rx, stop))?;
            self.ws.insert(
                connection,
                WsPeer {
                    outgoing,
                    socket: control_socket,
                    worker,
                    last_seen: now_ms,
                    frames: VecDeque::new(),
                    queued_bytes: 0,
                },
            );
            self.counters.accepted_ws += 1;
        }
        Ok(())
    }

    fn receive_frame(&mut self, connection: u64, bytes: &[u8], now_ms: u64) {
        self.counters.received_frames += 1;
        match net::decode_client(bytes) {
            Ok(message) => {
                let outgoing = self.server.handle(connection, message, now_ms);
                self.dispatch(outgoing, now_ms);
            }
            Err(_) => {
                self.counters.rejected_packets += 1;
                self.disconnect(connection);
            }
        }
    }

    fn receive_udp(&mut self, now_ms: u64) -> Result<(), TransportError> {
        // One extra byte detects and rejects truncated/oversize datagrams.
        let mut buffer = [0; MAX_DATAGRAM + 1];
        for _ in 0..RECEIVE_BUDGET {
            // One UDP datagram completes at most one application frame. Do not
            // acknowledge it until a canonical inbox slot can retain that frame.
            if self.server.inbox_capacity() == 0 {
                self.counters.ingress_backpressure += 1;
                break;
            }
            let (length, address) = match self.udp_socket.recv_from(&mut buffer) {
                Ok(received) => received,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            };
            self.counters.received_datagrams += 1;
            self.counters.received_bytes += length as u64;
            if length > MAX_DATAGRAM || !address.ip().is_loopback() {
                self.counters.rejected_packets += 1;
                continue;
            }
            let bytes = &buffer[..length];
            if bytes.starts_with(b"CUBI") {
                // Only Hello is permitted before authenticated establishment.
                let Ok(ClientMessage::Hello(hello)) = net::decode_client(bytes) else {
                    self.counters.rejected_packets += 1;
                    continue;
                };
                let connection = if let Some(&existing) = self.udp_addresses.get(&address) {
                    existing
                } else {
                    let Some(connection) = self.allocate_connection() else {
                        self.counters.rejected_packets += 1;
                        continue;
                    };
                    self.udp_addresses.insert(address, connection);
                    self.udp.insert(
                        connection,
                        UdpPeer {
                            address,
                            channel: None,
                            session: None,
                            outgoing: VecDeque::new(),
                            frames: VecDeque::new(),
                            queued_frame_bytes: 0,
                            last_seen: now_ms,
                        },
                    );
                    self.counters.accepted_udp += 1;
                    connection
                };
                if let Some(peer) = self.udp.get_mut(&connection) {
                    peer.last_seen = now_ms;
                }
                self.counters.received_frames += 1;
                let outgoing = self
                    .server
                    .handle(connection, ClientMessage::Hello(hello), now_ms);
                self.dispatch(outgoing, now_ms);
                continue;
            }
            let Some(&connection) = self.udp_addresses.get(&address) else {
                self.counters.rejected_packets += 1;
                continue;
            };
            let Some(peer) = self.udp.get_mut(&connection) else {
                continue;
            };
            let Some(channel) = &mut peer.channel else {
                self.counters.rejected_packets += 1;
                continue;
            };
            match channel.receive(bytes, now_ms) {
                Ok(received) => {
                    peer.last_seen = now_ms;
                    // ACKs are sent even for retransmitted/deduplicated messages.
                    if !queue_packets(peer, received.replies, &mut self.counters) {
                        self.disconnect(connection);
                        continue;
                    }
                    for frame in received.frames {
                        self.receive_frame(connection, &frame, now_ms);
                    }
                }
                Err(_) => self.counters.rejected_packets += 1,
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, outgoing: Vec<Outgoing>, now_ms: u64) {
        for outgoing in outgoing {
            let connection = outgoing.connection;
            let Ok(bytes) = net::encode_server(&outgoing.message) else {
                self.counters.rejected_packets += 1;
                self.disconnect(connection);
                continue;
            };
            if let Some(peer) = self.ws.get_mut(&connection) {
                let length = bytes.len();
                if peer.frames.len() >= WS_STAGED_FRAMES
                    || peer.queued_bytes.saturating_add(length) > WS_STAGED_BYTES
                {
                    self.counters.queue_overflows += 1;
                    self.disconnect(connection);
                } else {
                    peer.frames.push_back(bytes);
                    peer.queued_bytes += length;
                    self.counters.peak_ws_staged_bytes =
                        self.counters.peak_ws_staged_bytes.max(peer.queued_bytes);
                    self.counters.sent_frames += 1;
                    self.counters.sent_bytes += length as u64;
                }
                continue;
            }
            let session = self.server.session(connection);
            let Some(peer) = self.udp.get_mut(&connection) else {
                continue;
            };
            let bootstrap = matches!(outgoing.message, ServerMessage::Welcome(_));
            if let Some((session, key)) = session
                && peer.session != Some(session)
            {
                peer.channel = Some(Channel::new(session, key, Side::Server, Limits::default()));
                peer.session = Some(session);
            }
            if bootstrap || peer.channel.is_none() {
                if bytes.len() > MAX_DATAGRAM {
                    self.counters.rejected_packets += 1;
                    continue;
                }
                if !queue_packets(peer, vec![bytes], &mut self.counters) {
                    self.disconnect(connection);
                } else {
                    self.counters.sent_frames += 1;
                }
            } else if peer.frames.len() >= UDP_QUEUE_FRAMES
                || peer.queued_frame_bytes.saturating_add(bytes.len()) > UDP_QUEUE_BYTES
            {
                self.counters.queue_overflows += 1;
                self.disconnect(connection);
            } else {
                peer.queued_frame_bytes += bytes.len();
                peer.frames.push_back((bytes, outgoing.reliable));
                if !pump_udp_frames(peer, now_ms, &mut self.counters) {
                    self.counters.queue_overflows += 1;
                    self.disconnect(connection);
                }
            }
        }
    }

    fn flush_ws(&mut self) {
        let mut disconnected = Vec::new();
        for (&connection, peer) in &mut self.ws {
            for _ in 0..WS_OUTGOING_CAPACITY {
                let Some(frame) = peer.frames.pop_front() else {
                    break;
                };
                let length = frame.len();
                match peer.outgoing.try_send(frame) {
                    Ok(()) => peer.queued_bytes -= length,
                    Err(TrySendError::Full(frame)) => {
                        peer.frames.push_front(frame);
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        disconnected.push(connection);
                        break;
                    }
                }
            }
        }
        for connection in disconnected {
            self.disconnect(connection);
        }
    }

    fn flush_udp(&mut self) -> Result<(), TransportError> {
        // Round-robin packets across peers prevents one large snapshot from
        // excluding another client's input/ACK traffic from the work budget.
        let mut connections: Vec<_> = self.udp.keys().copied().collect();
        if connections.is_empty() {
            return Ok(());
        }
        let start = connections.partition_point(|connection| *connection <= self.send_cursor);
        let rotation = start % connections.len();
        connections.rotate_left(rotation);
        let mut remaining = SEND_BUDGET;
        loop {
            let mut progress = false;
            for &connection in &connections {
                let Some(peer) = self.udp.get_mut(&connection) else {
                    continue;
                };
                let Some(packet) = peer.outgoing.front() else {
                    continue;
                };
                match self.udp_socket.send_to(packet, peer.address) {
                    Ok(length) => {
                        self.counters.sent_datagrams += 1;
                        self.counters.sent_bytes += length as u64;
                        peer.outgoing.pop_front();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
                progress = true;
                self.send_cursor = connection;
                remaining -= 1;
                if remaining == 0 {
                    return Ok(());
                }
            }
            if !progress {
                return Ok(());
            }
        }
    }

    fn disconnect(&mut self, connection: u64) {
        let mut removed = false;
        if let Some(peer) = self.ws.remove(&connection) {
            let _ = peer.socket.shutdown(Shutdown::Both);
            self.retired_workers.push(peer.worker);
            removed = true;
        }
        if let Some(peer) = self.udp.remove(&connection) {
            self.udp_addresses.remove(&peer.address);
            removed = true;
        }
        if removed {
            self.server.disconnect(connection);
            self.counters.disconnected += 1;
        }
    }

    fn reap_workers(&mut self) {
        let completed: Vec<_> = self
            .ws
            .iter()
            .filter_map(|(&connection, peer)| peer.worker.is_finished().then_some(connection))
            .collect();
        for connection in completed {
            self.disconnect(connection);
        }
        let mut index = 0;
        while index < self.retired_workers.len() {
            if self.retired_workers[index].is_finished() {
                let worker = self.retired_workers.swap_remove(index);
                let _ = worker.join();
            } else {
                index += 1;
            }
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let connections: Vec<_> = self.ws.keys().chain(self.udp.keys()).copied().collect();
        for connection in connections {
            self.disconnect(connection);
        }
        for worker in self.retired_workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn queue_packets(
    peer: &mut UdpPeer,
    packets: Vec<Vec<u8>>,
    counters: &mut TransportCounters,
) -> bool {
    if peer.outgoing.len().saturating_add(packets.len()) > UDP_QUEUE_PACKETS {
        counters.queue_overflows += 1;
        return false;
    }
    peer.outgoing.extend(packets);
    counters.peak_udp_queue_packets = counters.peak_udp_queue_packets.max(peer.outgoing.len());
    true
}

fn pump_udp_frames(peer: &mut UdpPeer, now_ms: u64, counters: &mut TransportCounters) -> bool {
    // Reserve enough room for one maximum-size fragmented message. Frames wait
    // for reliable ACK capacity rather than disconnecting a healthy client when
    // an AOI update contains more than sixteen pages.
    while peer.outgoing.len() <= UDP_QUEUE_PACKETS - MAX_FRAME_BYTES.div_ceil(1100) {
        let Some((frame, reliable)) = peer.frames.front() else {
            break;
        };
        let Some(channel) = &mut peer.channel else {
            return false;
        };
        if *reliable && channel.pending_messages() >= Limits::default().maximum_messages {
            break;
        }
        match channel.send(frame.clone(), *reliable, now_ms) {
            Ok(packets) => {
                peer.queued_frame_bytes -= frame.len();
                peer.frames.pop_front();
                if !queue_packets(peer, packets, counters) {
                    return false;
                }
                counters.sent_frames += 1;
            }
            Err(UdpError::Capacity) => break,
            Err(_) => return false,
        }
    }
    true
}

fn loopback_origin(origin: &str) -> bool {
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https") || authority.contains(['/', '?', '#', '@']) {
        return false;
    }
    let authority = authority.to_ascii_lowercase();
    if matches!(authority.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
        return true;
    }
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
        && !port.is_empty()
        && port.parse::<u16>().is_ok()
}

// The tungstenite Callback contract fixes this unboxed HTTP response type.
#[allow(clippy::result_large_err)]
fn check_origin(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    let allowed = match request.headers().get("Origin") {
        None => true,
        Some(origin) => origin.to_str().is_ok_and(loopback_origin),
    };
    if allowed {
        Ok(response)
    } else {
        let mut response = ErrorResponse::new(Some("loopback origin required".into()));
        *response.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
        Err(response)
    }
}

fn websocket_worker(
    connection: u64,
    socket: TcpStream,
    incoming: SyncSender<SocketEvent>,
    outgoing: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
) {
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME_BYTES + 4096)
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    if let Ok(mut websocket) =
        tungstenite::accept_hdr_with_config(socket, check_origin, Some(config))
    {
        if websocket.get_mut().set_nonblocking(true).is_err() {
            return;
        }
        let mut pending_ingress = None;
        let mut pending_outgoing = None;
        'connection: while !stop.load(Ordering::Acquire) {
            let mut progress = false;
            let writable = match websocket.flush() {
                Ok(()) => true,
                Err(WebSocketError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                    false
                }
                Err(_) => break,
            };
            if writable {
                for _ in 0..WS_OUTGOING_CAPACITY {
                    let message = if let Some(message) = pending_outgoing.take() {
                        message
                    } else {
                        match outgoing.try_recv() {
                            Ok(bytes) => Message::Binary(bytes.into()),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => break 'connection,
                        }
                    };
                    match websocket.write(message) {
                        Ok(()) => progress = true,
                        // Tungstenite retains this accepted message internally.
                        Err(WebSocketError::Io(error))
                            if error.kind() == io::ErrorKind::WouldBlock =>
                        {
                            progress = true;
                            break;
                        }
                        Err(WebSocketError::WriteBufferFull(message)) => {
                            pending_outgoing = Some(*message);
                            break;
                        }
                        Err(error) => {
                            eprintln!("authority websocket write failure: {error}");
                            break 'connection;
                        }
                    }
                }
                match websocket.flush() {
                    Ok(()) => {}
                    Err(WebSocketError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                    }
                    Err(_) => break,
                }
            }
            let mut can_read = true;
            if let Some(event) = pending_ingress.take() {
                match incoming.try_send(event) {
                    Ok(()) => progress = true,
                    Err(TrySendError::Full(event)) => {
                        pending_ingress = Some(event);
                        can_read = false;
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            if can_read {
                for _ in 0..WS_OUTGOING_CAPACITY {
                    match websocket.read() {
                        Ok(Message::Binary(bytes)) => {
                            progress = true;
                            match incoming.try_send(SocketEvent::Frame(connection, bytes.to_vec()))
                            {
                                Ok(()) => {}
                                Err(TrySendError::Full(event)) => {
                                    pending_ingress = Some(event);
                                    break;
                                }
                                Err(TrySendError::Disconnected(_)) => break 'connection,
                            }
                        }
                        Ok(Message::Ping(_) | Message::Pong(_)) => progress = true,
                        Ok(_) => break 'connection,
                        Err(WebSocketError::Io(error))
                            if error.kind() == io::ErrorKind::WouldBlock =>
                        {
                            break;
                        }
                        Err(_) => break 'connection,
                    }
                }
            }
            thread::sleep(Duration::from_millis(if progress { 1 } else { 4 }));
        }
    }
    let _ = incoming.try_send(SocketEvent::Closed(connection));
}

/// A local headless server with a single owned simulation thread.
pub struct RunningServer {
    ws_address: SocketAddr,
    udp_address: SocketAddr,
    stop: Arc<AtomicBool>,
    counters: Arc<Mutex<TransportCounters>>,
    worker: Option<JoinHandle<Result<TransportCounters, TransportError>>>,
}
impl RunningServer {
    pub fn spawn(
        config: ServerConfig,
        ws_address: SocketAddr,
        udp_address: SocketAddr,
    ) -> Result<Self, TransportError> {
        Self::from_server(Server::open(config)?, ws_address, udp_address)
    }
    pub fn from_server(
        server: Server,
        ws_address: SocketAddr,
        udp_address: SocketAddr,
    ) -> Result<Self, TransportError> {
        let mut runtime = Runtime::bind(server, ws_address, udp_address)?;
        let ws_address = runtime.ws_address()?;
        let udp_address = runtime.udp_address()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let counters = Arc::new(Mutex::new(TransportCounters::default()));
        let worker_counters = Arc::clone(&counters);
        let worker = thread::Builder::new()
            .name("cubi-authority".into())
            .spawn(move || {
                let start = Instant::now();
                while !worker_stop.load(Ordering::Acquire) {
                    let now_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                    runtime.poll(now_ms)?;
                    if let Ok(mut counters) = worker_counters.lock() {
                        *counters = runtime.counters();
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(runtime.counters())
            })?;
        Ok(Self {
            ws_address,
            udp_address,
            stop,
            counters,
            worker: Some(worker),
        })
    }
    pub fn ws_address(&self) -> SocketAddr {
        self.ws_address
    }
    pub fn udp_address(&self) -> SocketAddr {
        self.udp_address
    }
    pub fn counters(&self) -> TransportCounters {
        self.counters
            .lock()
            .map(|counters| *counters)
            .unwrap_or_default()
    }
    pub fn stop(mut self) -> Result<TransportCounters, TransportError> {
        self.stop.store(true, Ordering::Release);
        self.join()
    }
    fn join(&mut self) -> Result<TransportCounters, TransportError> {
        self.worker.take().map_or(Ok(self.counters()), |worker| {
            worker.join().map_err(|_| TransportError::WorkerPanic)?
        })
    }
}
impl Drop for RunningServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::{TransportCounters, UdpPeer, loopback_origin, pump_udp_frames};
    use net::SecretToken;
    use net::udp::{Channel, Limits, Side};
    use std::collections::VecDeque;

    #[test]
    fn origins_are_exact_loopback_authorities() {
        for origin in [
            "http://localhost",
            "http://localhost:3000",
            "https://127.0.0.1:443",
            "http://[::1]:8080",
        ] {
            assert!(loopback_origin(origin), "{origin}");
        }
        for origin in [
            "null",
            "http://localhost.evil",
            "http://localhost@evil",
            "http://evil@localhost",
            "http://localhost/evil",
            "http://localhost:99999",
            "file://localhost",
            "http://192.168.0.1",
            "http://[::1].evil",
        ] {
            assert!(!loopback_origin(origin), "{origin}");
        }
    }

    #[test]
    fn real_websocket_retains_burst_while_the_canonical_inbox_is_full() {
        use core_types::{Cell, PageId, SpaceId, Tick};
        use net::{ClientMessage, Hello, PlayerId, ServerMessage};
        use std::{
            collections::BTreeSet,
            net::TcpStream,
            path::PathBuf,
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };
        use tungstenite::{Error as WsError, HandshakeError, Message};
        use world::{Transaction, World};
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Directory(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/tmp")
                .join(format!("inbox-backpressure-{}", std::process::id())),
        );
        std::fs::create_dir_all(&directory.0).unwrap();
        let mut config = crate::ServerConfig::new(&directory.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 1., 8.5]);
        let mut world = World::new(rules::properties, 4096);
        let mut cells = vec![Cell::AIR; 4096];
        cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
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
        world.commit(tx).unwrap();
        let server = crate::Server::open_with_world(config, world).unwrap();
        let mut runtime = super::Runtime::bind(
            server,
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let address = runtime.ws_address().unwrap();
        let (joined_tx, joined_rx) = mpsc::channel();
        let (start_tx, start_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let client = thread::spawn(move || {
            let socket = TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            // A short read timeout may interrupt the HTTP upgrade while the
            // canonical owner performs real work. Resume the same handshake;
            // dropping its buffered state would make this load test flaky.
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut handshake = tungstenite::client(format!("ws://{address}/"), socket);
            let mut socket = loop {
                match handshake {
                    Ok((socket, _)) => break socket,
                    Err(HandshakeError::Interrupted(pending)) => {
                        assert!(Instant::now() < deadline, "real socket handshake deadline");
                        handshake = pending.handshake();
                    }
                    Err(HandshakeError::Failure(error)) => {
                        panic!("real socket handshake failed: {error:?}")
                    }
                }
            };
            let hello = ClientMessage::Hello(Hello {
                identity: PlayerId::default(),
                name: "ingress-pressure".into(),
                credential: None,
            });
            socket
                .send(Message::Binary(net::encode_client(&hello).unwrap().into()))
                .unwrap();
            let mut joined = false;
            let mut pongs = BTreeSet::new();
            loop {
                assert!(Instant::now() < deadline, "real socket ingress deadline");
                match socket.read() {
                    Ok(Message::Binary(bytes)) => match net::decode_server(&bytes).unwrap() {
                        ServerMessage::Welcome(_) if !joined => {
                            joined = true;
                            joined_tx.send(()).unwrap();
                            start_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                            for nonce in 10_000..10_600 {
                                socket
                                    .send(Message::Binary(
                                        net::encode_client(&ClientMessage::Ping(nonce))
                                            .unwrap()
                                            .into(),
                                    ))
                                    .unwrap();
                            }
                        }
                        ServerMessage::Pong(nonce) if nonce >= 10_000 => {
                            assert!(pongs.insert(nonce));
                            if pongs.len() == 600 {
                                done_tx.send(pongs.len()).unwrap();
                                return;
                            }
                        }
                        ServerMessage::Error(reason) => {
                            panic!("valid retained ingress was rejected: {reason:?}")
                        }
                        _ => {}
                    },
                    Err(WsError::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    other => panic!("real socket closed before600retained requests: {other:?}"),
                }
            }
        });
        let started = Instant::now();
        let deadline = started + Duration::from_secs(15);
        let mut filled = false;
        loop {
            assert!(Instant::now() < deadline, "real authority ingress deadline");
            let now = started.elapsed().as_millis() as u64;
            runtime.poll(now).unwrap();
            assert!(
                !runtime.server.is_failed(),
                "authority failure {:?}",
                runtime.server.counters()
            );
            if !filled && joined_rx.try_recv().is_ok() {
                // A bounded deterministic owner pause supplies the same pressure
                // as a slow durable checkpoint, without changing socket quotas.
                assert!(runtime.server.inbox.is_empty());
                runtime.server.next_tick_at = now + 300;
                for nonce in 0..crate::MAX_INBOX {
                    runtime
                        .server
                        .inbox
                        .push_back((1, ClientMessage::Ping(nonce as u64), now));
                }
                assert_eq!(runtime.server.inbox_capacity(), 0);
                filled = true;
                start_tx.send(()).unwrap();
            }
            match done_rx.try_recv() {
                Ok(count) => {
                    assert_eq!(count, 600);
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => panic!(
                    "socket exited: {:?} {:?}",
                    runtime.counters(),
                    runtime.server.counters()
                ),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            thread::sleep(Duration::from_millis(1));
        }
        client.join().unwrap();
        assert!(filled);
        assert!(!runtime.server.is_failed());
        assert!(runtime.counters.ingress_backpressure > 0);
        assert_eq!(runtime.counters.queue_overflows, 0);
        assert_eq!(runtime.server.counters.protocol_rejections, 0);
        assert_eq!(
            runtime.server.counters.inbox_backpressure, 0,
            "transport retained each message before admission"
        );
        assert!(runtime.server.counters.inbox_high_water <= crate::MAX_INBOX);
    }

    #[test]
    fn reliable_page_burst_waits_for_ack_capacity_without_losing_frames() {
        let key = SecretToken::new([7; 32]);
        let mut sender = UdpPeer {
            address: "127.0.0.1:1".parse().unwrap(),
            channel: Some(Channel::new(1, key, Side::Server, Limits::default())),
            session: Some(1),
            outgoing: VecDeque::new(),
            frames: (0..40).map(|id| (vec![id; 300], true)).collect(),
            queued_frame_bytes: 40 * 300,
            last_seen: 0,
        };
        let mut receiver = Channel::new(1, key, Side::Client, Limits::default());
        let mut counters = TransportCounters::default();
        assert!(pump_udp_frames(&mut sender, 0, &mut counters));
        assert_eq!(sender.frames.len(), 24);
        let mut received = Vec::new();
        for now_ms in 1..=4 {
            while let Some(packet) = sender.outgoing.pop_front() {
                let result = receiver.receive(&packet, now_ms).unwrap();
                received.extend(result.frames);
                for ack in result.replies {
                    sender
                        .channel
                        .as_mut()
                        .unwrap()
                        .receive(&ack, now_ms)
                        .unwrap();
                }
            }
            assert!(pump_udp_frames(&mut sender, now_ms, &mut counters));
        }
        assert_eq!(
            received,
            (0..40).map(|id| vec![id; 300]).collect::<Vec<_>>()
        );
        assert_eq!(sender.queued_frame_bytes, 0);
        assert!(sender.frames.is_empty());
        assert_eq!(counters.queue_overflows, 0);
        assert_eq!(counters.sent_frames, 40);
    }
}
