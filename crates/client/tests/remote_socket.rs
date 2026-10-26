//! Runs the exact native transport worker and shared remote scene owner through
//! actual localhost WebSocket frames, authority ticks and durable world storage.
#![cfg(not(target_arch = "wasm32"))]
#[path = "../../client-native/src/remote_transport.rs"]
mod transport;
use client::{ClientScene, network::NetworkConfig, remote::RemoteSession, view::RemoteInput};
use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use net::{Hello, PlayerId, ReplicaRead};
use server::{Server, ServerConfig, transport::Runtime};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use world::{CellRead, Transaction, World};
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 256);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    // The requested5³ halo is an outdoor fixture. Keep the known AIR chain to
    // profile top127 so certified projection can distinguish it from a sealed
    // private cavity; these extra server pages are never requested by this peer.
    for y in -2..=7 {
        for z in -2..=2 {
            for x in -2..=2 {
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
                if [x, y, z] == [0, 0, 0] {
                    cells[8 + 11 * 16 + 2 * 256] = Cell::from_parts(rules::ids::STONE, 0);
                    cells[8 + 12 * 16 + 2 * 256] = Cell::from_parts(rules::ids::IRON_ORE, 0);
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
#[test]
fn native_websocket_pump_imports_real_authority_without_client_generation() {
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("remote-socket-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let mut config = ServerConfig::new(&directory.0);
    config.generation_enabled = false;
    config.spawn_override = Some([8.5, 1.0, 8.5]);
    let server = Server::open_with_world(config, fixture()).unwrap();
    let mut runtime = Runtime::bind(
        server,
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    // Prepare the authored outdoor world using the real bounded migration.
    // That has dedicated recovery tests; this test measures the
    // socket/backpressure phase against its original15s deadline.
    let setup_deadline = Instant::now() + Duration::from_secs(60);
    let mut authority_offset = 0;
    while !runtime.server().height_ready_for_publication() {
        assert!(
            Instant::now() < setup_deadline,
            "fixture height certificate"
        );
        runtime.poll(authority_offset).unwrap();
        authority_offset += server::TICK_MILLIS;
        assert!(!runtime.server().is_failed());
        thread::sleep(Duration::from_millis(2));
    }
    let started = Instant::now();
    let mut socket =
        transport::Socket::connect(format!("ws://{}/", runtime.ws_address().unwrap())).unwrap();
    let mut remote = RemoteSession::new(NetworkConfig {
        input_lead_ticks: 2,
        ..NetworkConfig::remote_scene()
    })
    .unwrap();
    remote
        .connection_mut()
        .hello("actual-native".into(), PlayerId::default(), None)
        .unwrap();
    let mut scene = ClientScene::new(RemoteSession::scene_config()).unwrap();
    assert_eq!(scene.stats().generation_published, 0);
    scene.phase(Tick(0), 8).unwrap();
    assert!(
        scene
            .take_task(Tick(0), &mut ClientScene::default_budget())
            .is_none(),
        "initial remote requests never enqueue Generation tasks"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut complete = false;
    let mut persisted = false;
    let mut stalled = false;
    for _ in 0..30_000 {
        if Instant::now() >= deadline {
            let missing: Vec<_> = scene
                .requested_pages()
                .filter(|p| scene.world().page(*p).is_none())
                .map(|p| {
                    let at = BlockPos {
                        space: p.space,
                        x: p.x * 16,
                        y: p.y * 16,
                        z: p.z * 16,
                    };
                    (p, remote.connection().read(at))
                })
                .collect();
            eprintln!(
                "remote-socket missing={missing:?} client={:?} queues={:?}",
                remote.connection().counters(),
                remote.connection().queue_depths()
            );
        }
        assert!(
            Instant::now() < deadline,
            "real WebSocket/fsync deadline: loaded={} player={:?} tick={} counters={:?}",
            scene.stats().loaded_pages,
            remote.connection().player().map(|p| p.pos),
            runtime.server().current_tick().0,
            runtime.server().counters(),
        );
        let now = started.elapsed().as_millis() as u64;
        runtime.poll(authority_offset + now).unwrap();
        if persisted && !stalled && now >= 500 {
            // Reproduce a slow first GPU/pipeline frame: owner keeps ticking
            // while the render thread does not drain its32-frame queue. TCP
            // backpressure must retain all reliable pages and the session.
            let resume = Instant::now() + Duration::from_millis(800);
            while Instant::now() < resume {
                runtime
                    .poll(authority_offset + started.elapsed().as_millis() as u64)
                    .unwrap();
                thread::sleep(Duration::from_millis(2));
            }
            stalled = true;
        }
        socket.pump(&mut remote, now).unwrap();
        let loaded = scene.stats().loaded_pages;
        remote
            .pump_scene(
                &mut scene,
                RemoteInput {
                    keys: if loaded >= 125 {
                        net::InputFrame::RIGHT
                    } else {
                        0
                    },
                    ..RemoteInput::default()
                },
                now,
            )
            .unwrap();
        socket.pump(&mut remote, now).unwrap();
        assert_eq!(
            scene.stats().generation_published,
            0,
            "remote never publishes locally generated pages"
        );
        assert!(
            !runtime.server().is_failed(),
            "heightready={} indexedcells={} columns={} loaded={} counters={:?}",
            runtime.server().height_ready_for_publication(),
            runtime.server().height_authority().index().cell_count(),
            runtime.server().height_authority().index().column_count(),
            runtime.server().world().loaded_pages(),
            runtime.server().counters()
        );
        if !persisted && let Some(welcome) = remote.connection().welcome() {
            let hello = Hello {
                identity: welcome.identity,
                name: "actual-native".into(),
                credential: remote.connection().credential(),
            };
            let identity = directory.0.join("client.identity");
            transport::save_identity(&identity, &hello).unwrap();
            let restored = transport::load_identity(&identity).unwrap().unwrap();
            assert_eq!(restored, hello);
            persisted = true;
        }
        if scene.stats().loaded_pages >= 125
            && remote.connection().player().is_some_and(|p| p.pos[0] < 7.0)
        {
            complete = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        complete,
        "loaded125 confirmed halo pages and moved using server-owned physics"
    );
    assert!(persisted);
    assert!(
        stalled,
        "actual owner continued through the receive backpressure window"
    );
    let ore = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 2,
        z: 12,
    };
    assert_eq!(
        remote.connection().read(ore),
        ReplicaRead::Known(Cell::from_parts(rules::ids::STONE, 0)),
        "sealed ore redacted before scene import"
    );
    assert_eq!(
        scene.world().read(ore),
        CellRead::Known(Cell::from_parts(rules::ids::STONE, 0))
    );
    assert_eq!(
        scene.world().read(BlockPos {
            space: SpaceId(0),
            x: 200,
            y: 1,
            z: 200
        }),
        CellRead::Unknown
    );
    assert!(remote.counters().imported_pages >= 125);
    assert!(runtime.server().counters().input_steps > 0);
    assert_eq!(runtime.counters().accepted_ws, 1);
    assert_eq!(runtime.counters().queue_overflows, 0);
    eprintln!(
        "native WS real scene pages={} cells={} prediction_steps={} authority_input_steps={} bytes_in={} bytes_out={}",
        scene.stats().loaded_pages,
        remote.counters().imported_cells,
        remote.connection().counters().prediction_steps,
        runtime.server().counters().input_steps,
        runtime.counters().received_bytes,
        runtime.counters().sent_bytes
    );
}
