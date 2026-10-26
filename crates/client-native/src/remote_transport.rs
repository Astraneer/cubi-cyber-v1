//! Bounded development WebSocket transport. Blocking connect/handshake and all
//! socket I/O live on one worker; the render thread only tries bounded queues.
use client::remote::RemoteSession;
#[cfg(test)]
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::{
    io,
    net::{SocketAddr, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Error, Message, protocol::WebSocketConfig};
const QUEUE: usize = 32;
#[cfg(test)]
pub struct Socket {
    outgoing: SyncSender<Vec<u8>>,
    incoming: Receiver<Result<Vec<u8>, String>>,
    pending: Option<Vec<u8>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    diagnostics: bool,
    last_pump_ms: Option<u64>,
    last_report_ms: u64,
    reports: u32,
    max_pump_gap_ms: u64,
    delivered: u64,
    enqueued: u64,
}
#[cfg(test)]
impl Socket {
    pub fn connect(url: String) -> Result<Self, String> {
        let address = local_address(&url)?;
        let (outgoing, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE);
        let (tx, incoming) = mpsc::sync_channel(QUEUE);
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let diagnostics = std::env::var_os("CUBI_UI_DIAGNOSTICS").is_some();
        let worker = thread::Builder::new()
            .name("cubi-websocket-client".into())
            .spawn(move || {
                if let Err(error) = run(url, address, rx, &tx, &done, diagnostics) {
                    let _ = tx.try_send(Err(error));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            outgoing,
            incoming,
            pending: None,
            stop,
            worker: Some(worker),
            diagnostics,
            last_pump_ms: None,
            last_report_ms: 0,
            reports: 0,
            max_pump_gap_ms: 0,
            delivered: 0,
            enqueued: 0,
        })
    }
    pub fn pump(&mut self, session: &mut RemoteSession, now_ms: u64) -> Result<(), String> {
        if self.diagnostics
            && let Some(previous) = self.last_pump_ms.replace(now_ms)
        {
            self.max_pump_gap_ms = self.max_pump_gap_ms.max(now_ms.saturating_sub(previous));
        }
        for _ in 0..QUEUE {
            match self.incoming.try_recv() {
                Ok(Ok(frame)) => {
                    session.receive_frame(&frame, now_ms)?;
                    self.delivered = self.delivered.saturating_add(1);
                }
                Ok(Err(e)) => return Err(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err("server WebSocket disconnected".into());
                }
            }
        }
        for _ in 0..QUEUE {
            let Some(frame) = self.pending.take().or_else(|| session.take_outgoing()) else {
                break;
            };
            match self.outgoing.try_send(frame) {
                Ok(()) => {
                    self.enqueued = self.enqueued.saturating_add(1);
                }
                Err(TrySendError::Full(frame)) => {
                    self.pending = Some(frame);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err("server WebSocket writer disconnected".into());
                }
            }
        }
        if self.diagnostics
            && self.reports < 64
            && now_ms.saturating_sub(self.last_report_ms) >= 1000
        {
            println!(
                "NATIVE_WS_PUMP time_ms={now_ms} max_gap_ms={} delivered={} enqueued={} outbox={} pending={}",
                self.max_pump_gap_ms,
                self.delivered,
                self.enqueued,
                session.connection().queue_depths().outgoing,
                u8::from(self.pending.is_some())
            );
            self.last_report_ms = now_ms;
            self.reports += 1;
        }
        Ok(())
    }
}
#[cfg(test)]
impl Drop for Socket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
/// One typed session is shared by the network worker and short GUI CPU phases.
/// No raw-frame GUI queue can hold action replies behind world snapshots.
pub type SharedSession = Arc<Mutex<RemoteSession>>;
pub struct SharedSocket {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<String>>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl SharedSocket {
    pub fn connect(
        url: String,
        session: SharedSession,
        clock_origin: Instant,
    ) -> Result<Self, String> {
        let address = local_address(&url)?;
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let fault = Arc::new(Mutex::new(None));
        let worker_stop = stop.clone();
        let worker_finished = finished.clone();
        let worker_fault = fault.clone();
        let diagnostics = std::env::var_os("CUBI_UI_DIAGNOSTICS").is_some();
        let worker = thread::Builder::new()
            .name("cubi-shared-websocket".into())
            .spawn(move || {
                if let Err(error) = run_shared(
                    url,
                    address,
                    &session,
                    clock_origin,
                    &worker_stop,
                    diagnostics,
                ) && let Ok(mut fault) = worker_fault.lock()
                {
                    *fault = Some(error);
                }
                worker_finished.store(true, Ordering::Release);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            stop,
            finished,
            fault,
            worker: Some(worker),
        })
    }
    /// Poll only lifecycle errors. Network admission proceeds while the GUI is busy.
    pub fn pump(&mut self) -> Result<(), String> {
        if let Some(error) = self
            .fault
            .lock()
            .map_err(|_| "WebSocket fault lock poisoned")?
            .take()
        {
            return Err(error);
        }
        if self.finished.load(Ordering::Acquire) {
            return Err("server WebSocket worker stopped".into());
        }
        Ok(())
    }
}
impl Drop for SharedSocket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn run_shared(
    url: String,
    address: SocketAddr,
    session: &SharedSession,
    clock_origin: Instant,
    stop: &AtomicBool,
    diagnostics: bool,
) -> Result<(), String> {
    let mut socket = open_socket(url, address)?;
    // tungstenite returns an unaccepted message on WriteBufferFull. Retain exactly
    // that message; WouldBlock already transferred ownership to its write buffer.
    let mut pending_write = None;
    let mut action_pending = [false; QUEUE];
    let mut action_flushed = [false; QUEUE];
    let mut action_received = [0u8; QUEUE];
    let mut delivered = 0u64;
    let mut enqueued = 0u64;
    let mut previous_loop = clock_origin.elapsed();
    let mut max_gap_ms = 0u128;
    let mut last_report_ms = 0u128;
    let mut reports = 0u32;
    while !stop.load(Ordering::Acquire) {
        let elapsed = clock_origin.elapsed();
        max_gap_ms = max_gap_ms.max(elapsed.saturating_sub(previous_loop).as_millis());
        previous_loop = elapsed;
        // Each received RAW frame may enqueue both ACK and LIGHT request. Service
        // twice the incoming quantum before reading, without enlarging either cap.
        for _ in 0..64 {
            let message = if let Some(message) = pending_write.take() {
                message
            } else {
                let frame = session
                    .lock()
                    .map_err(|_| "shared session poisoned")?
                    .take_outgoing();
                let Some(bytes) = frame else {
                    break;
                };
                Message::Binary(bytes.into())
            };
            let action = if diagnostics {
                match &message {
                    Message::Binary(bytes) => diagnostic_action(bytes, true).map(|(id, _)| id),
                    _ => None,
                }
            } else {
                None
            };
            match socket.write(message) {
                Ok(()) => {
                    enqueued = enqueued.saturating_add(1);
                    if let Some(id) = action {
                        action_pending[(id - 1) as usize] = true;
                    }
                }
                Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                    enqueued = enqueued.saturating_add(1);
                    if let Some(id) = action {
                        action_pending[(id - 1) as usize] = true;
                    }
                    break;
                }
                Err(Error::WriteBufferFull(message)) => {
                    pending_write = Some(*message);
                    break;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        match socket.flush() {
            Ok(()) => {
                for index in 0..QUEUE {
                    if action_pending[index] && !action_flushed[index] {
                        println!("NATIVE_WS_ACTION_FLUSHED id={}", index + 1);
                        action_flushed[index] = true;
                    }
                    action_pending[index] = false;
                }
            }
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.to_string()),
        }
        // No session guard survives a socket call, sleep or GPU operation.
        for _ in 0..32 {
            match socket.read() {
                Ok(Message::Binary(bytes)) => {
                    let now_ms =
                        clock_origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                    session
                        .lock()
                        .map_err(|_| "shared session poisoned")?
                        .receive_frame(&bytes, now_ms)?;
                    delivered = delivered.saturating_add(1);
                    if diagnostics && let Some((id, status)) = diagnostic_action(&bytes, false) {
                        let code = match status {
                            "pending" => 1,
                            "accepted" => 2,
                            _ => 3,
                        };
                        if action_received[(id - 1) as usize] != code {
                            println!("NATIVE_WS_REPLY_RECEIVED id={id} status={status}");
                            action_received[(id - 1) as usize] = code;
                        }
                    }
                }
                Ok(Message::Close(_)) => return Err("server closed WebSocket".into()),
                Ok(_) => {}
                Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(Error::ConnectionClosed | Error::AlreadyClosed) => {
                    return Err("server WebSocket closed".into());
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        let now_ms = clock_origin.elapsed().as_millis();
        if diagnostics && reports < 64 && now_ms.saturating_sub(last_report_ms) >= 1000 {
            let outgoing = session
                .lock()
                .map_err(|_| "shared session poisoned")?
                .connection()
                .queue_depths()
                .outgoing;
            println!(
                "NATIVE_WS_SHARED time_ms={now_ms} max_gap_ms={max_gap_ms} delivered={delivered} enqueued={enqueued} outbox={outgoing} pending={}",
                u8::from(pending_write.is_some())
            );
            last_report_ms = now_ms;
            reports += 1;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = socket.close(None);
    Ok(())
}
fn open_socket(
    url: String,
    address: SocketAddr,
) -> Result<tungstenite::WebSocket<TcpStream>, String> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(net::MAX_FRAME_BYTES))
        .max_frame_size(Some(net::MAX_FRAME_BYTES))
        .write_buffer_size(0)
        .max_write_buffer_size(2 * net::MAX_FRAME_BYTES);
    let (socket, _) = tungstenite::client::client_with_config(url, stream, Some(config))
        .map_err(|error| error.to_string())?;
    socket
        .get_ref()
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    Ok(socket)
}
fn local_address(url: &str) -> Result<SocketAddr, String> {
    let authority = url
        .strip_prefix("ws://")
        .ok_or("development connection requires ws://loopback:port")?
        .split('/')
        .next()
        .ok_or("missing WebSocket authority")?;
    let address = authority
        .parse::<SocketAddr>()
        .map_err(|_| "use a numeric loopback address and port")?;
    if !address.ip().is_loopback() {
        return Err("unencrypted development WebSocket requires loopback".into());
    }
    Ok(address)
}
#[cfg(test)]
fn run(
    url: String,
    address: SocketAddr,
    rx: Receiver<Vec<u8>>,
    tx: &SyncSender<Result<Vec<u8>, String>>,
    stop: &AtomicBool,
    diagnostics: bool,
) -> Result<(), String> {
    let mut socket = open_socket(url, address)?;
    let mut pending_incoming = None;
    let mut action_pending = [false; QUEUE];
    let mut action_flushed = [false; QUEUE];
    let mut action_received = [0u8; QUEUE];
    while !stop.load(Ordering::Acquire) {
        for _ in 0..QUEUE {
            match rx.try_recv() {
                Ok(bytes) => {
                    let action = diagnostics
                        .then(|| diagnostic_action(&bytes, true))
                        .flatten()
                        .map(|(id, _)| id);
                    match socket.write(Message::Binary(bytes.into())) {
                        Ok(()) => {
                            if let Some(id) = action {
                                action_pending[(id - 1) as usize] = true;
                            }
                        }
                        Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                            if let Some(id) = action {
                                action_pending[(id - 1) as usize] = true;
                            }
                            break;
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match socket.flush() {
            Ok(()) => {
                for index in 0..QUEUE {
                    if action_pending[index] && !action_flushed[index] {
                        println!("NATIVE_WS_ACTION_FLUSHED id={}", index + 1);
                        action_flushed[index] = true;
                    }
                    action_pending[index] = false;
                }
            }
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
        if let Some(frame) = pending_incoming.take() {
            match tx.try_send(frame) {
                Ok(()) => {}
                Err(TrySendError::Full(frame)) => {
                    pending_incoming = Some(frame);
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(TrySendError::Disconnected(_)) => return Ok(()),
            }
        }
        for _ in 0..QUEUE {
            match socket.read() {
                Ok(Message::Binary(bytes)) => {
                    if diagnostics && let Some((id, status)) = diagnostic_action(&bytes, false) {
                        let code = match status {
                            "pending" => 1,
                            "accepted" => 2,
                            _ => 3,
                        };
                        if action_received[(id - 1) as usize] != code {
                            println!("NATIVE_WS_REPLY_RECEIVED id={id} status={status}");
                            action_received[(id - 1) as usize] = code;
                        }
                    }
                    match tx.try_send(Ok(bytes.to_vec())) {
                        Ok(()) => {}
                        Err(TrySendError::Full(frame)) => {
                            pending_incoming = Some(frame);
                            break;
                        }
                        Err(TrySendError::Disconnected(_)) => return Ok(()),
                    }
                }
                Ok(Message::Close(_)) => return Err("server closed WebSocket".into()),
                Ok(_) => {}
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(Error::ConnectionClosed | Error::AlreadyClosed) => {
                    return Err("server WebSocket closed".into());
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = socket.close(None);
    Ok(())
}

// Inspect only known action/reply envelopes when explicitly requested. The
// protocol decoder still validates the whole frame; payloads are never logged.
fn diagnostic_action(bytes: &[u8], outgoing: bool) -> Option<(u64, &'static str)> {
    let tag = *bytes.get(18)?;
    let (id, status) = if outgoing && matches!(tag, 2 | 9) {
        match net::decode_client(bytes).ok()? {
            net::ClientMessage::Action { id, .. } | net::ClientMessage::AimedAction { id, .. } => {
                (id.0, "queued")
            }
            _ => return None,
        }
    } else if !outgoing && tag == 6 {
        match net::decode_server(bytes).ok()? {
            net::ServerMessage::Action(reply) => (
                reply.id.0,
                match reply.status {
                    net::ActionStatus::Pending => "pending",
                    net::ActionStatus::Accepted => "accepted",
                    net::ActionStatus::Rejected(_) => "rejected",
                },
            ),
            _ => return None,
        }
    } else {
        return None;
    };
    (1..=QUEUE as u64).contains(&id).then_some((id, status))
}

pub fn load_identity(path: &std::path::Path) -> Result<Option<net::Hello>, String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > 4096 => return Err("client identity file exceeds bound".into()),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    match net::decode_client(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?
    {
        net::ClientMessage::Hello(hello) if hello.credential.is_some() => Ok(Some(hello)),
        _ => Err("invalid client identity file".into()),
    }
}
pub fn save_identity(path: &std::path::Path, hello: &net::Hello) -> Result<(), String> {
    use std::io::Write;
    if let Some(existing) = load_identity(path)? {
        if existing.identity == hello.identity && existing.credential == hello.credential {
            return Ok(());
        }
        return Err("client identity already belongs to another session".into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes =
        net::encode_client(&net::ClientMessage::Hello(hello.clone())).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("identity-{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Both adapters must surface a real peer's close. The legacy adapter remains
    // compiled only for the slow-GUI negative/recovery transport regression.
    #[test]
    fn legacy_and_shared_workers_surface_peer_close() {
        fn peer() -> (String, thread::JoinHandle<()>) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("ws://{}/", listener.local_addr().unwrap());
            let worker = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                socket.close(None).unwrap();
            });
            (url, worker)
        }
        let origin = Instant::now();
        let (url, peer_worker) = peer();
        let mut session =
            RemoteSession::new(client::network::NetworkConfig::remote_scene()).unwrap();
        let mut socket = Socket::connect(url).unwrap();
        while socket
            .pump(&mut session, origin.elapsed().as_millis() as u64)
            .is_ok()
        {
            assert!(origin.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(2));
        }
        drop(socket);
        peer_worker.join().unwrap();
        let (url, peer_worker) = peer();
        let session = Arc::new(Mutex::new(session));
        let mut socket = SharedSocket::connect(url, session, origin).unwrap();
        while socket.pump().is_ok() {
            assert!(origin.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(2));
        }
        drop(socket);
        peer_worker.join().unwrap();
    }
    #[test]
    fn plaintext_transport_is_loopback_only() {
        assert!(local_address("ws://127.0.0.1:7777/").is_ok());
        assert!(local_address("ws://[::1]:7777/").is_ok());
        assert!(local_address("ws://203.0.113.1:7777/").is_err());
        assert!(local_address("wss://127.0.0.1:7777/").is_err());
    }
}
