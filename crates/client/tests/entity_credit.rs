//! Real authority and Connection retain one confirmed-base entity credit.
#![cfg(not(target_arch = "wasm32"))]
use client::network::{Connection, NetworkConfig};
use core_types::{Cell, PageId, SpaceId, Tick};
use net::{
    ClientMessage, ServerMessage,
    entities::{EntityFrame, EntityRequest},
};
use rules::entities::{EntityId, MobKind, mob::Mob};
use server::{Server, ServerConfig};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn route(
    client: &mut Connection,
    messages: Vec<server::Outgoing>,
    now: u64,
    frames: &mut Vec<EntityFrame>,
) {
    for out in messages {
        let message = net::decode_server(&net::encode_server(&out.message).unwrap()).unwrap();
        if let ServerMessage::Entities(frame) = &message {
            frames.push((**frame).clone());
        }
        client.receive(message, now).unwrap();
    }
}
#[test]
fn delayed_entity_ack_bounds_stream_and_query_work_then_sends_one_current_based_delta() {
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("entity-credit-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
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
    let mut config = ServerConfig::new(&directory.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 1., 8.5]);
    let mut server = Server::open_with_world(config, world).unwrap();
    assert!(
        server
            .queue_mob_spawn(
                Mob::spawned(EntityId(90), MobKind::Pig, SpaceId(0), [8.5, 1., 11.5], 17).unwrap()
            )
            .is_ok()
    );
    let mut client = Connection::new(NetworkConfig::default()).unwrap();
    let mut frames = Vec::new();
    let mut now = 0;
    let owner =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    route(&mut client, server.handle(1, owner, now), now, &mut frames);
    let deadline = Instant::now() + Duration::from_secs(20);
    while frames.is_empty() {
        now += 10;
        route(&mut client, server.tick(now), now, &mut frames);
        assert!(!server.is_failed());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(300));
    }
    let first = frames[0].clone();
    assert_eq!(client.entity_replica().revision(), Some(first.revision));
    let initial_work = server.counters().entity_poses;
    let start = server.current_tick();
    let mut deferred = None;
    while server.current_tick().0 < start.0 + 40 {
        for message in client.take_outgoing(256) {
            match message {
                ClientMessage::Entities(request) => deferred = Some(request),
                other => {
                    route(&mut client, server.handle(1, other, now), now, &mut frames);
                }
            }
        }
        now += 10;
        route(&mut client, server.tick(now), now, &mut frames);
        assert!(!server.is_failed());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(300));
    }
    assert_eq!(
        frames.len(),
        1,
        "no second application image before the first credit ACK"
    );
    assert_eq!(
        server.counters().entity_poses,
        initial_work,
        "slow renderer adds no repeated spatial/pose work"
    );
    assert!(
        server.counters().mob_steps > 20,
        "actual authority continued bounded physics/AI"
    );
    let ack = deferred.expect("application ACK retained by the slow owner");
    assert_eq!(
        ack,
        EntityRequest::Ack {
            revision: first.revision
        }
    );
    route(
        &mut client,
        server.handle(1, ClientMessage::Entities(ack), now),
        now,
        &mut frames,
    );
    while frames.len() < 2 {
        now += 10;
        route(&mut client, server.tick(now), now, &mut frames);
        assert!(!server.is_failed());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(300));
    }
    assert_eq!(frames[1].base, Some(first.revision));
    assert_eq!(client.entity_replica().revision(), Some(frames[1].revision));
    // Explicit repair is a separate credit and must not await the prior delta ACK.
    route(
        &mut client,
        server.handle(
            1,
            ClientMessage::Entities(EntityRequest::Full { have: None }),
            now,
        ),
        now,
        &mut frames,
    );
    while frames.len() < 3 {
        now += 10;
        route(&mut client, server.tick(now), now, &mut frames);
        assert!(!server.is_failed());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(300));
    }
    assert_eq!(frames[2].base, None);
    assert_eq!(client.entity_replica().revision(), Some(frames[2].revision));
}
