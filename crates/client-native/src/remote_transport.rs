//! Bounded development WebSocket transport. Blocking connect/handshake and all
//! socket I/O live on one worker; the render thread only tries bounded queues.
use client::remote::RemoteSession;
use std::{
    io,
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::Duration,
};
use tungstenite::{Error, Message, protocol::WebSocketConfig};
const QUEUE: usize = 32;
pub struct Socket {
    outgoing: SyncSender<Vec<u8>>,
    incoming: Receiver<Result<Vec<u8>, String>>,
    pending: Option<Vec<u8>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Socket {
    pub fn connect(url: String) -> Result<Self, String> {
        let address = local_address(&url)?;
        let (outgoing, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE);
        let (tx, incoming) = mpsc::sync_channel(QUEUE);
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::Builder::new()
            .name("cubi-websocket-client".into())
            .spawn(move || {
                if let Err(error) = run(url, address, rx, &tx, &done) {
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
        })
    }
    pub fn pump(&mut self, session: &mut RemoteSession, now_ms: u64) -> Result<(), String> {
        for _ in 0..QUEUE {
            match self.incoming.try_recv() {
                Ok(Ok(frame)) => session.receive_frame(&frame, now_ms)?,
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
                Ok(()) => {}
                Err(TrySendError::Full(frame)) => {
                    self.pending = Some(frame);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err("server WebSocket writer disconnected".into());
                }
            }
        }
        Ok(())
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
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
fn run(
    url: String,
    address: SocketAddr,
    rx: Receiver<Vec<u8>>,
    tx: &SyncSender<Result<Vec<u8>, String>>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(net::MAX_FRAME_BYTES))
        .max_frame_size(Some(net::MAX_FRAME_BYTES))
        .write_buffer_size(0)
        .max_write_buffer_size(2 * net::MAX_FRAME_BYTES);
    let (mut socket, _) = tungstenite::client::client_with_config(url, stream, Some(config))
        .map_err(|e| e.to_string())?;
    socket
        .get_mut()
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    let mut pending_incoming = None;
    while !stop.load(Ordering::Acquire) {
        for _ in 0..QUEUE {
            match rx.try_recv() {
                Ok(bytes) => match socket.write(Message::Binary(bytes.into())) {
                    Ok(()) => {}
                    Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.to_string()),
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match socket.flush() {
            Ok(()) => {}
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
                Ok(Message::Binary(bytes)) => match tx.try_send(Ok(bytes.to_vec())) {
                    Ok(()) => {}
                    Err(TrySendError::Full(frame)) => {
                        // Keep exactly one extra bounded frame; stop reading
                        // TCP so shader/loading stalls apply backpressure.
                        pending_incoming = Some(frame);
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => return Ok(()),
                },
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
    #[test]
    fn plaintext_transport_is_loopback_only() {
        assert!(local_address("ws://127.0.0.1:7777/").is_ok());
        assert!(local_address("ws://[::1]:7777/").is_ok());
        assert!(local_address("ws://203.0.113.1:7777/").is_err());
        assert!(local_address("wss://127.0.0.1:7777/").is_err());
    }
}
