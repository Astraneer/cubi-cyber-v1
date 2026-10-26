//! Fault injection verifies first-failure provenance without exposing component
//! payload bytes or disguising a stopped authority as a live transport.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use server::{Server, ServerConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{ComponentData, Transaction, World};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn malformed_trusted_owner_component_reports_first_phase_not_private_payload() {
    let d = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "failure-provenance-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
    );
    std::fs::create_dir_all(&d.0).unwrap();
    let mut config = ServerConfig::new(&d.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 1., 8.5]);
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        vec![Cell::AIR; 4096],
    );
    world.commit(tx).unwrap();
    let mut server = Server::open_with_world(config, world).unwrap();
    assert_eq!(server.failure_reason(), None);
    let end = Instant::now() + Duration::from_secs(15);
    let mut now = 0;
    while !server.height_ready_for_publication() {
        now += 10;
        server.tick(now);
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_micros(300));
    }
    let p = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 1,
        z: 11,
    };
    let mut bad = Transaction::new(server.current_tick(), SpaceId(0));
    bad.set_cell(p, Cell::from_parts(rules::ids::ENERGIZED_SMELTER, 2));
    bad.set_component(
        ComponentId(77),
        ComponentData {
            anchor: p,
            kind: server::machines::MACHINE_COMPONENT,
            payload: b"private-corrupt-payload-sentinel".to_vec(),
        },
        ChannelMask::CONTENTS,
    );
    assert!(server.queue_transaction(bad).is_ok());
    while !server.is_failed() {
        now += 10;
        server.tick(now);
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_micros(300));
    }
    let reason = server
        .failure_reason()
        .expect("first failing owner operation");
    assert_eq!(reason.operation, "tick_machines");
    assert!(reason.detail.is_some());
    assert!(reason.source_file.ends_with("server/src/lib.rs"));
    assert!(reason.source_line > 0);
    let public = format!("{reason:?}");
    assert!(public.len() < 512);
    assert!(!public.contains("private-corrupt-payload-sentinel"));
    server.tick(now + 1000);
    assert_eq!(server.failure_reason(), Some(reason));
    let local = "127.0.0.1:0".parse().unwrap();
    let mut transport = server::transport::Runtime::bind(server, local, local).unwrap();
    assert!(matches!(
        transport.poll(now + 1001),
        Err(server::transport::TransportError::Server(
            server::ServerError::Invalid("authority entered failed state")
        ))
    ));
    assert_eq!(transport.server().failure_reason(), Some(reason));
}
