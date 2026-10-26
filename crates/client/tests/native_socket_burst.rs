//! Actual native worker + actual authoritative server; no mock socket or GPU.
#![cfg(not(target_arch = "wasm32"))]
#[path = "../../client-native/src/remote_transport.rs"]
mod native_socket;
use client::{network::NetworkConfig, remote::RemoteSession};
use core_types::{Cell, PageId, SpaceId, Tick};
use net::{Action, ActionId, ActionStatus, PlayerId, window::WindowIntent};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn drain(
    session: &mut RemoteSession,
    accepted: &mut BTreeSet<ActionId>,
    replies: &mut [u64; 4],
) -> usize {
    let c = session.connection_mut();
    let mut bulk = 0;
    while let Some(reply) = c.take_reply() {
        match reply.status {
            ActionStatus::Accepted => {
                if let Some(count) = replies.get_mut(reply.id.0 as usize) {
                    *count += 1;
                }
                accepted.insert(reply.id);
            }
            ActionStatus::Rejected(reason) => {
                panic!("sentinel action{} rejected {reason:?}", reply.id.0)
            }
            ActionStatus::Pending => {}
        }
    }
    bulk += c.take_page_updates(128).len();
    while c.take_light_update().is_some() {
        bulk += 1;
    }
    bulk += c.take_geometry_updates(32).len();
    while c.take_height_update().is_some() {
        bulk += 1;
    }
    while c.take_props_update().is_some() {
        bulk += 1;
    }
    while c.take_water_update().is_some() {
        bulk += 1;
    }
    bulk
}
enum Driver {
    Legacy(native_socket::Socket),
    Shared(native_socket::SharedSocket),
}
impl Driver {
    fn pump(&mut self, session: &mut RemoteSession, now: u64) -> Result<(), String> {
        match self {
            Self::Legacy(socket) => socket.pump(session, now),
            Self::Shared(socket) => socket.pump(),
        }
    }
}
fn actual_burst(gui_interval: Duration, shared: bool) {
    let dir = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "native-socket-burst-{}-{}-{shared}",
                std::process::id(),
                gui_interval.as_millis()
            )),
    );
    std::fs::create_dir_all(&dir.0).unwrap();
    let mut config = server::ServerConfig::new(&dir.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 1., 8.5]);
    config.spawn_protection_radius = 0;
    let mut world = world::World::new(rules::properties, 4096);
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    for y in -1..=1 {
        for z in 0..8 {
            for x in 0..8 {
                let mut cells = vec![Cell::AIR; 4096];
                if y == 0 {
                    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
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
    let server = server::Server::open_with_world(config, world).unwrap();
    let authority = server::transport::RunningServer::from_server(
        server,
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let mut session = RemoteSession::new(NetworkConfig {
        near_radius: 2,
        retain_radius: 3,
        maximum_pages: 256,
        ..Default::default()
    })
    .unwrap();
    session
        .connection_mut()
        .hello("native-socket-sentinel".into(), PlayerId::default(), None)
        .unwrap();
    let start = Instant::now();
    let shared_session = Arc::new(Mutex::new(session));
    let url = format!("ws://{}/", authority.ws_address());
    let mut socket = if shared {
        Driver::Shared(
            native_socket::SharedSocket::connect(url, Arc::clone(&shared_session), start).unwrap(),
        )
    } else {
        Driver::Legacy(native_socket::Socket::connect(url).unwrap())
    };
    let mut session = shared_session.lock().unwrap();
    let deadline = start + Duration::from_secs(30);
    let mut accepted = BTreeSet::new();
    let mut replies = [0; 4];
    let mut bulk = 0;
    let mut first = None;
    loop {
        assert!(
            Instant::now() < deadline,
            "sentinel first handshake/action timed out; transport={:?}",
            authority.counters()
        );
        let now = start.elapsed().as_millis() as u64;
        socket.pump(&mut session, now).unwrap();
        let authority_tick = session.connection().acknowledged_tick();
        session.connection_mut().advance_to(authority_tick).unwrap();
        session.connection_mut().update_interest(now).unwrap();
        bulk += drain(&mut session, &mut accepted, &mut replies);
        if session.connection().welcome().is_some() && first.is_none() {
            session.connection_mut().update_interest(now).unwrap();
            first = Some(
                session
                    .connection_mut()
                    .action(Action::Window(WindowIntent::OpenInventory), false, now)
                    .unwrap(),
            );
        }
        session.connection_mut().poll(now).unwrap();
        if first.is_some_and(|id| accepted.contains(&id)) && session.connection().window().is_some()
        {
            break;
        }
        drop(session);
        std::thread::sleep(Duration::from_millis(4));
        session = shared_session.lock().unwrap();
    }
    assert_eq!(first, Some(ActionId(1)));
    let path = dir.0.join("identity.local");
    assert!(native_socket::load_identity(&path).unwrap().is_none());
    let hello = net::Hello {
        name: "native-socket-sentinel".into(),
        identity: session.connection().welcome().unwrap().identity,
        credential: session.connection().credential(),
    };
    native_socket::save_identity(&path, &hello).unwrap();
    assert!(
        native_socket::load_identity(&path)
            .unwrap()
            .is_some_and(
                |saved| saved.identity == hello.identity && saved.credential == hello.credential
            )
    );
    let sent_before = authority.counters();
    let mut height_index = 0;
    while height_index < 648 {
        let now = start.elapsed().as_millis() as u64;
        socket.pump(&mut session, now).unwrap();
        let authority_tick = session.connection().acknowledged_tick();
        session.connection_mut().advance_to(authority_tick).unwrap();
        session.connection_mut().update_interest(now).unwrap();
        bulk += drain(&mut session, &mut accepted, &mut replies);
        for _ in 0..32 {
            if height_index >= 648 || session.connection().queue_depths().outgoing >= 192 {
                break;
            }
            session
                .connection_mut()
                .request_height(
                    net::height::HeightKey {
                        space: 0,
                        level: 0,
                        x: height_index % 27,
                        z: height_index / 27,
                    },
                    now,
                )
                .unwrap();
            height_index += 1;
        }
        session.connection_mut().poll(now).unwrap();
        drop(session);
        std::thread::sleep(Duration::from_millis(8));
        session = shared_session.lock().unwrap();
    }
    let now = start.elapsed().as_millis() as u64;
    for z in 0..8 {
        for x in 0..8 {
            session
                .connection_mut()
                .request_geometry(
                    PageId {
                        space: SpaceId(0),
                        x: x * 2,
                        y: 0,
                        z: z * 2,
                    },
                    now,
                )
                .unwrap();
        }
    }
    let before_bulk = bulk;
    let bulk_deadline = Instant::now() + Duration::from_secs(15);
    while bulk - before_bulk < 8 {
        assert!(
            Instant::now() < bulk_deadline,
            "fixture has not produced actual far data; bulk={bulk} before={before_bulk} transport={:?}",
            authority.counters()
        );
        let now = start.elapsed().as_millis() as u64;
        socket.pump(&mut session, now).unwrap();
        let authority_tick = session.connection().acknowledged_tick();
        session.connection_mut().advance_to(authority_tick).unwrap();
        session.connection_mut().update_interest(now).unwrap();
        bulk += drain(&mut session, &mut accepted, &mut replies);
        session.connection_mut().poll(now).unwrap();
        drop(session);
        std::thread::sleep(Duration::from_millis(8));
        session = shared_session.lock().unwrap();
    }
    // Queue enough real requests before the new controls to exercise the exact
    // FIFO and bounded33-frame native ingress, including paused GUI consumption.
    socket.pump(&mut session, now).unwrap();
    drop(session);
    std::thread::sleep(gui_interval);
    session = shared_session.lock().unwrap();
    let now = start.elapsed().as_millis() as u64;
    let guard = session.connection().window().unwrap().guard;
    let second = session
        .connection_mut()
        .action(Action::Window(WindowIntent::Close { guard }), false, now)
        .unwrap();
    let third = session
        .connection_mut()
        .action(Action::Chat("socket sentinel".into()), false, now)
        .unwrap();
    assert_eq!((second, third), (ActionId(2), ActionId(3)));
    let mut pumps = 0;
    let mut legacy_hol_observed = false;
    let control_start = Instant::now();
    let control_deadline = control_start + Duration::from_secs(20);
    while !(accepted.contains(&second) && accepted.contains(&third)) {
        if Instant::now() >= control_deadline {
            let counters = authority.counters();
            assert!(
                !shared && gui_interval == Duration::from_millis(4000),
                "shared controls starved: pumps={pumps} accepted={accepted:?} queues={:?} transport={counters:?}",
                session.connection().queue_depths()
            );
            assert!(counters.received_actions_123[1] > 0 && counters.received_actions_123[2] > 0);
            assert!(
                counters.staged_accepted_actions_123[1] > 0
                    && counters.staged_accepted_actions_123[2] > 0,
                "server must already have accepted both IDs"
            );
            eprintln!(
                "NATIVE_SOCKET_LEGACY_HOL_NEGATIVE gui_ms4000 elapsed_ms={} accepted_gui={accepted:?} server_received={:?} server_accepted={:?} outgoing={}",
                control_start.elapsed().as_millis(),
                counters.received_actions_123,
                counters.staged_accepted_actions_123,
                session.connection().queue_depths().outgoing
            );
            legacy_hol_observed = true;
            let recovery = Instant::now() + Duration::from_secs(5);
            while !(accepted.contains(&second) && accepted.contains(&third)) {
                assert!(
                    Instant::now() < recovery,
                    "accepted controls were lost, rather than delayed"
                );
                let now = start.elapsed().as_millis() as u64;
                socket.pump(&mut session, now).unwrap();
                bulk += drain(&mut session, &mut accepted, &mut replies);
                drop(session);
                std::thread::sleep(Duration::from_millis(8));
                session = shared_session.lock().unwrap();
            }
            break;
        }
        assert!(!authority.is_finished(), "actual server worker exited");
        let now = start.elapsed().as_millis() as u64;
        socket.pump(&mut session, now).unwrap();
        let authority_tick = session.connection().acknowledged_tick();
        session.connection_mut().advance_to(authority_tick).unwrap();
        session.connection_mut().update_interest(now).unwrap();
        bulk += drain(&mut session, &mut accepted, &mut replies);
        session.connection_mut().poll(now).unwrap();
        pumps += 1;
        drop(session);
        std::thread::sleep(gui_interval);
        session = shared_session.lock().unwrap();
    }
    let after = authority.counters();
    if !shared && gui_interval == Duration::from_millis(4000) {
        assert!(
            legacy_hol_observed,
            "legacy fixture must expose the GUI head-of-line delay"
        );
    }
    assert!(
        bulk > 0 && after.sent_frames > sent_before.sent_frames,
        "the real courier must run concurrently"
    );
    eprintln!(
        "NATIVE_SOCKET_ACTION_BURST_OK shared={shared} gui_interval_ms={} pumps={} elapsed_ms={} accepted_ids=1,2,3 bulk_updates={} server_frames_delta={} queue_caps_unchanged=true height_requests648=true accepted_reply_counts={replies:?} final_queues={:?}",
        gui_interval.as_millis(),
        pumps,
        control_start.elapsed().as_millis(),
        bulk,
        after.sent_frames - sent_before.sent_frames,
        session.connection().queue_depths()
    );
    drop(session);
    drop(socket);
    drop(authority);
}
#[test]
fn actual_native_socket_shared_owner_keeps_control_admission_independent_of_gui_and_preserves_legacy_negative()
 {
    actual_burst(Duration::from_millis(8), false);
    actual_burst(Duration::from_millis(4000), false);
    for ms in [8, 250, 1000, 4000] {
        actual_burst(Duration::from_millis(ms), true);
    }
}
