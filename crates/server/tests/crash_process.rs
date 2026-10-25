//! Crash the real authority binary immediately after its wire acknowledgement.
//! Neither graceful shutdown nor an in-process Server drop can flush the WAL.

use core_types::{SpaceId, SpaceTransform};
use net::{Action, ActionId, ActionReply, ActionStatus, ClientMessage, Hello, ServerMessage};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{Error as WebSocketError, Message, WebSocket};
use world::{JournalLimits, World};

const DEADLINE: Duration = Duration::from_secs(3);
const MOVE_COMMAND: &str = "/space_move 0 12 0 0";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "crash-process-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn owner(&self) -> Hello {
        let mut file = File::open(self.0.join("owner.identity")).unwrap();
        assert!(file.metadata().unwrap().len() <= 512);
        let mut bytes = Vec::new();
        file.by_ref().take(513).read_to_end(&mut bytes).unwrap();
        assert!(bytes.len() <= 512);
        let ClientMessage::Hello(hello) = net::decode_client(&bytes).unwrap() else {
            panic!("private owner identity must contain an authenticated Hello");
        };
        assert!(hello.credential.is_some());
        hello
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Process {
    child: Option<Child>,
    ws_address: SocketAddr,
}
impl Process {
    fn start(directory: &Directory) -> Self {
        let ready_file = directory.0.join("public-addresses.txt");
        match std::fs::remove_file(&ready_file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove stale public addresses: {error}"),
        }
        let child = Command::new(env!("CARGO_BIN_EXE_server"))
            .arg("--data")
            .arg(&directory.0)
            .args([
                "--ws",
                "127.0.0.1:0",
                "--udp",
                "127.0.0.1:0",
                "--no-generation",
                "--spawn",
                "8.5,1,8.5",
                "--ready-file",
            ])
            .arg(&ready_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Construct the guard before any readiness assertion so unwinding also
        // kills/reaps this exact child, including startup failure and timeout.
        let mut process = Self {
            child: Some(child),
            ws_address: "127.0.0.1:0".parse().unwrap(),
        };
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(address) = read_public_address(&ready_file) {
                process.ws_address = address;
                return process;
            }
            if let Some(status) = process.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("authority exited before readiness: {status}");
            }
            assert!(Instant::now() < deadline, "authority readiness timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn crash(&mut self) {
        let child = self.child.as_mut().expect("live owned authority child");
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        self.child = None;
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn read_public_address(path: &Path) -> Option<SocketAddr> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > 512 {
        return None;
    }
    let mut text = String::new();
    file.take(513).read_to_string(&mut text).ok()?;
    let mut lines = text.lines();
    let ws: SocketAddr = lines.next()?.strip_prefix("ws=")?.parse().ok()?;
    let udp: SocketAddr = lines.next()?.strip_prefix("udp=")?.parse().ok()?;
    (lines.next().is_none()
        && ws.ip().is_loopback()
        && udp.ip().is_loopback()
        && ws.port() != 0
        && udp.port() != 0)
        .then_some(ws)
}

struct Bot {
    socket: WebSocket<TcpStream>,
    transforms: BTreeMap<SpaceId, SpaceTransform>,
}
impl Bot {
    fn connect(address: SocketAddr, owner: Hello, next_action: u64) -> Self {
        let socket = TcpStream::connect_timeout(&address, DEADLINE).unwrap();
        socket.set_read_timeout(Some(DEADLINE)).unwrap();
        socket.set_write_timeout(Some(DEADLINE)).unwrap();
        let (mut socket, _) = tungstenite::client(format!("ws://{address}/"), socket).unwrap();
        socket
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut bot = Self {
            socket,
            transforms: BTreeMap::new(),
        };
        bot.send(ClientMessage::Hello(owner));
        let welcome = bot.until(|message| matches!(message, ServerMessage::Welcome(_)));
        let ServerMessage::Welcome(welcome) = welcome else {
            unreachable!();
        };
        assert_eq!(welcome.role, net::Role::Owner);
        assert_eq!(welcome.next_action, ActionId(next_action));
        bot
    }
    fn send(&mut self, message: ClientMessage) {
        self.socket
            .send(Message::Binary(
                net::encode_client(&message).unwrap().into(),
            ))
            .unwrap();
    }
    fn until(&mut self, predicate: impl Fn(&ServerMessage) -> bool) -> ServerMessage {
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            match self.socket.read() {
                Ok(Message::Binary(bytes)) => {
                    let message = net::decode_server(&bytes).unwrap();
                    if let ServerMessage::Error(reason) = message {
                        panic!("authority rejected the owner: {reason:?}");
                    }
                    if let ServerMessage::Transform {
                        space, transform, ..
                    } = &message
                    {
                        self.transforms.insert(*space, *transform);
                    }
                    if predicate(&message) {
                        return message;
                    }
                }
                Ok(_) => {}
                Err(WebSocketError::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("authority socket: {error}"),
            }
        }
        panic!("authority protocol response timed out");
    }
    fn action(&mut self) -> ActionReply {
        self.send(ClientMessage::Action {
            id: ActionId(1),
            action: Action::Command(MOVE_COMMAND.into()),
        });
        let response = self.until(|message| {
            matches!(message, ServerMessage::Action(reply)
                if reply.id == ActionId(1) && reply.status != ActionStatus::Pending)
        });
        let ServerMessage::Action(reply) = response else {
            unreachable!();
        };
        reply
    }
}

fn verify_durable_transaction(directory: &Directory, owner: &Hello, accepted: ActionReply) {
    // Open only after the child has been killed and reaped: no competing store
    // owner, server drop, checkpoint request or graceful flush can affect proof.
    let (_store, recovered) = save::Store::open(&directory.0).unwrap();
    let snapshot = recovered.snapshot.expect("initial durable baseline");
    let mut world = World::restore(rules::properties, JournalLimits::default(), snapshot.world)
        .expect("restore durable baseline");
    let mut transactions = 0;
    let mut replies = 0;
    for (_, batch) in recovered.batches {
        for change in batch.changes {
            transactions += usize::from(Some(change.tx) == accepted.transaction);
            world.apply_recovered_change(&change).unwrap();
        }
        for (identity, reply) in batch.actions {
            if identity == owner.identity && reply.id == accepted.id {
                assert_eq!(reply, accepted);
                replies += 1;
            }
        }
    }
    assert_eq!(
        transactions, 1,
        "one durable world transaction for ActionId 1"
    );
    assert_eq!(replies, 1, "one durable terminal reply for ActionId 1");
    assert_eq!(world.version(), accepted.source_version);
    assert_eq!(
        world.space(SpaceId(0)).unwrap().transform.translation,
        [12.0, 0.0, 0.0]
    );
}

#[test]
fn acknowledged_action_survives_real_process_kill_and_replays_exactly_once() {
    let directory = Directory::new();
    let mut process = Process::start(&directory);
    let owner = directory.owner();
    let mut bot = Bot::connect(process.ws_address, owner.clone(), 1);
    let accepted = bot.action();
    assert_eq!(accepted.status, ActionStatus::Accepted);
    assert!(accepted.transaction.is_some());
    // Abrupt termination immediately follows the accepted wire reply.
    process.crash();
    drop(bot);
    verify_durable_transaction(&directory, &owner, accepted);

    let mut restarted = Process::start(&directory);
    let mut bot = Bot::connect(restarted.ws_address, owner.clone(), 2);
    if !bot.transforms.contains_key(&SpaceId(0)) {
        bot.until(|message| {
            matches!(
                message,
                ServerMessage::Transform {
                    space: SpaceId(0),
                    ..
                }
            )
        });
    }
    assert_eq!(bot.transforms[&SpaceId(0)].translation, [12.0, 0.0, 0.0]);
    assert_eq!(
        bot.action(),
        accepted,
        "same ActionId recovers the exact reply"
    );
    restarted.crash();
    drop(bot);
    verify_durable_transaction(&directory, &owner, accepted);
}
