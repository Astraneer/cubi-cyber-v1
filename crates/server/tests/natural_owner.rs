//! Actual worker population and its durable kind2 metadata are one operation.
use core_types::{Cell, PageId, SpaceId, Tick};
use net::ServerMessage;
use server::{
    Server, ServerConfig,
    natural::{Column, State},
};
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
fn config(d: &Directory) -> ServerConfig {
    let mut config = ServerConfig::new(&d.0);
    config.public_seed = 3;
    config.generation_enabled = false;
    config.spawn_protection_radius = 0;
    let generator = r#gen::Generator::new(r#gen::TerrainSeed(3));
    config.spawn_override = Some([8.5, f64::from(generator.column_info(8, 8).height) + 1., 8.5]);
    config
}
fn fixture() -> World {
    let generator = r#gen::Generator::new(r#gen::TerrainSeed(3));
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for y in 0..8 {
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y,
            z: 0,
        };
        let mut cells = vec![Cell::AIR; 4096];
        for z in 0..16 {
            for x in 0..16 {
                let top = generator.column_info(x, z).height;
                for local_y in 0..16 {
                    let global_y = y * 16 + local_y;
                    if global_y <= top {
                        cells[(x + 16 * z + 256 * local_y) as usize] = Cell::from_parts(
                            if global_y == top {
                                rules::ids::GRASS
                            } else {
                                rules::ids::STONE
                            },
                            0,
                        );
                    }
                }
            }
        }
        tx.create_page(page, cells);
    }
    world.commit(tx).unwrap();
    world
}
#[test]
fn natural_herd_birth_and_population_flag_reopen_together_without_repeat() {
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("natural-owner-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let (store, _) = save::Store::open(&directory.0).unwrap();
    store
        .load_or_create_private_entropy(|| Ok([73; 32]))
        .unwrap();
    drop(store);
    let mut server = Server::open_with_world(config(&directory), fixture()).unwrap();
    let hello =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    let mut outgoing = server.handle(1, hello, 0);
    let started = Instant::now();
    let mut now = 0;
    loop {
        now += 10;
        outgoing.extend(server.tick(now));
        assert!(!server.is_failed(), "{:?}", server.counters());
        if server.counters().natural_completed > 0 && !server.awaiting_durability() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "natural owner {:?}",
            server.counters()
        );
        thread::sleep(Duration::from_micros(300));
    }
    assert!(
        outgoing
            .iter()
            .any(|o| matches!(o.message, ServerMessage::Welcome(_)))
    );
    assert_eq!(server.counters().natural_completed, 1);
    assert!(
        server.counters().natural_births > 0,
        "seed3 canonical biome {:?}, {:?}",
        r#gen::Generator::new(r#gen::TerrainSeed(3)).column_info(8, 8),
        server.counters()
    );
    let rows = server.mob_entities().rows().to_vec();
    assert_eq!(rows.len() as u64, server.counters().natural_births);
    assert!(rows.iter().all(|mob| mob.id.0 >> 62 == 1));
    // Stop before a checkpoint: rows and population/RNG continuation must
    // recover from the very same WAL record, with no regenerated births.
    drop(server);
    let (store, recovery) = save::Store::open(&directory.0).unwrap();
    let snapshot = recovery.snapshot.as_ref().unwrap();
    let durable_tick = recovery
        .batches
        .last()
        .map_or(snapshot.server_tick, |(_, b)| b.tick);
    let record = snapshot
        .runtimes
        .iter()
        .find(|r| r.kind == save::RUNTIME_MOBS)
        .unwrap();
    let length = u32::from_le_bytes(record.payload[4..8].try_into().unwrap()) as usize;
    let mut wal_state = State::decode(&record.payload[12 + length..]).unwrap();
    let mut atomic_birth_records = 0;
    for (_, batch) in &recovery.batches {
        for record in batch
            .runtimes
            .iter()
            .filter(|r| r.kind == save::RUNTIME_MOBS)
        {
            if record.payload.starts_with(b"MND1") {
                let mobs = u32::from_le_bytes(record.payload[4..8].try_into().unwrap()) as usize;
                let metadata = &record.payload[12 + mobs..];
                assert!(mobs > 0, "a nonempty herd must carry its entity delta");
                wal_state
                    .apply(server::natural::Delta::decode(metadata).unwrap())
                    .unwrap();
                atomic_birth_records += 1;
            }
        }
    }
    assert_eq!(atomic_birth_records, 1);
    assert_eq!(wal_state.len(), 1);
    assert_eq!(wal_state.next_id(), rows.len() as u64 + 1);
    drop(store);
    let mut server = Server::open(config(&directory)).unwrap();
    assert_eq!(server.current_tick(), durable_tick);
    assert_eq!(server.mob_entities().rows(), rows);
    assert!(server.request_checkpoint());
    while server.awaiting_durability() {
        server.tick(now);
        thread::sleep(Duration::from_micros(300));
    }
    drop(server);
    let (store, recovery) = save::Store::open(&directory.0).unwrap();
    let snapshot = recovery.snapshot.unwrap();
    let tick = snapshot.server_tick;
    let record = snapshot
        .runtimes
        .iter()
        .find(|r| r.kind == save::RUNTIME_MOBS)
        .unwrap();
    assert_eq!(&record.payload[..4], b"MNS1");
    let mob_length = u32::from_le_bytes(record.payload[4..8].try_into().unwrap()) as usize;
    let state = State::decode(&record.payload[12 + mob_length..]).unwrap();
    assert_eq!(state.len(), 1);
    assert!(state.populated(Column {
        space: SpaceId(0),
        x: 0,
        z: 0
    }));
    assert_eq!(state.next_id(), rows.len() as u64 + 1);
    let encoded = state.encode();
    drop(store);
    let mut reopened = Server::open(config(&directory)).unwrap();
    assert_eq!(reopened.current_tick(), tick);
    assert_eq!(reopened.mob_entities().rows(), rows);
    assert_eq!(reopened.counters().natural_births, 0);
    let hello =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    reopened.handle(1, hello, now);
    let until = now + 15000;
    while now < until {
        now += 10;
        reopened.tick(now);
        assert!(!reopened.is_failed(), "{:?}", reopened.counters());
        thread::sleep(Duration::from_micros(100));
    }
    assert_eq!(reopened.counters().natural_births, 0);
    assert_eq!(reopened.counters().natural_completed, 0);
    assert_eq!(reopened.mob_entities().len(), rows.len());
    while reopened.awaiting_durability() {
        reopened.tick(now);
        thread::sleep(Duration::from_micros(300));
    }
    assert!(reopened.request_checkpoint());
    while reopened.awaiting_durability() {
        reopened.tick(now);
        thread::sleep(Duration::from_micros(300));
    }
    drop(reopened);
    let (_store, recovery) = save::Store::open(&directory.0).unwrap();
    let snapshot = recovery.snapshot.unwrap();
    let record = snapshot
        .runtimes
        .iter()
        .find(|r| r.kind == save::RUNTIME_MOBS)
        .unwrap();
    let length = u32::from_le_bytes(record.payload[4..8].try_into().unwrap()) as usize;
    assert_eq!(&record.payload[12 + length..], encoded);
}
