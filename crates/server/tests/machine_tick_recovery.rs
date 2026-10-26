//! An actual unfinished owner tick survives reopening before any Hello. The
//! fixture changes only trusted startup snapshots; all measured work uses Server.
use core_types::{Cell, PageId, SpaceId, Tick};
use net::{ClientMessage, Hello};
use rules::{
    entities::{EntityId, MobKind, item::ItemEntity, mob::Mob},
    items::I_IRON_INGOT,
    stack::ItemStack,
    survival::Inventory,
};
use server::{
    Server, ServerConfig,
    entities::{Config, MobRuntime, items::ItemRuntime},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};

const ACTORS: usize = 1025;
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "held-mob-recovery-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        let mut config = ServerConfig::new(&self.0);
        config.generation_enabled = false;
        config.spawn_protection_radius = 0;
        config.spawn_override = Some([32.5, 1., 16.5]);
        config
    }
    fn owner(&self) -> Hello {
        let ClientMessage::Hello(hello) =
            net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
        else {
            unreachable!()
        };
        hello
    }
    fn copy_from(source: &Self) -> Self {
        let target = Self::new();
        for entry in std::fs::read_dir(&source.0).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), target.0.join(entry.file_name())).unwrap();
            }
        }
        target
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn room() -> World {
    let mut world = World::new(rules::properties, 4096);
    let mut create = Transaction::new(Tick(0), SpaceId(0));
    for z in 0..5 {
        for x in 0..5 {
            let mut cells = vec![Cell::AIR; 4096];
            for lz in 0..16 {
                for lx in 0..16 {
                    let i = lx + 16 * lz;
                    cells[i] = Cell::from_parts(rules::ids::STONE, 0);
                    cells[i + 4 * 256] = Cell::from_parts(rules::ids::STONE, 0);
                    if x * 16 + lx == 0
                        || x * 16 + lx == 79
                        || z * 16 + lz == 0
                        || z * 16 + lz == 79
                    {
                        for y in 1..4 {
                            cells[i + y * 256] = Cell::from_parts(rules::ids::STONE, 0);
                        }
                    }
                }
            }
            create.create_page(
                PageId {
                    space: SpaceId(0),
                    x: x as i32,
                    y: 0,
                    z: z as i32,
                },
                cells,
            );
        }
    }
    world.commit(create).unwrap();
    world
}
fn poll(server: &mut Server, now: u64, deadline: Instant) {
    server.tick(now);
    assert!(!server.is_failed(), "{:?}", server.failure_reason());
    assert!(
        Instant::now() < deadline,
        "owner did not finish: {:?}",
        server.load_metrics()
    );
    thread::sleep(Duration::from_micros(300));
}
fn checkpoint(server: &mut Server, now: u64, deadline: Instant) {
    while !server.request_checkpoint() {
        poll(server, now, deadline);
    }
    while server.awaiting_durability() {
        poll(server, now, deadline);
    }
}
fn replace(snapshot: &mut save::Snapshot, kind: u16, payload: Vec<u8>) {
    if let Some(row) = snapshot.runtimes.iter_mut().find(|row| row.kind == kind) {
        row.payload = payload;
    } else {
        snapshot
            .runtimes
            .push(save::RuntimeRecord { kind, payload });
        snapshot.runtimes.sort_unstable_by_key(|row| row.kind);
    }
}
fn prepare(unknown_first: bool) -> Directory {
    let directory = Directory::new();
    let mut server = Server::open_with_world(directory.config(), room()).unwrap();
    server.handle(1, ClientMessage::Hello(directory.owner()), 0);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(90);
    let now = loop {
        let now = started.elapsed().as_millis() as u64;
        poll(&mut server, now, deadline);
        let counters = server.counters();
        if server.height_ready_for_publication()
            && server.current_tick().0 >= 64
            && counters.light_pending_cells == 0
            && counters.light_pending_seed_pages == 0
            && counters.light_pending_bootstrap_pages == 0
            && !server.awaiting_durability()
        {
            break now;
        }
    };
    checkpoint(&mut server, now, deadline);
    drop(server);
    let (mut store, recovery) = save::Store::open(&directory.0).unwrap();
    assert!(recovery.batches.is_empty());
    let mut snapshot = recovery.snapshot.unwrap();
    let tick = snapshot.server_tick;
    let mut inventory = Inventory::empty();
    inventory
        .set_trusted(0, ItemStack::new(I_IRON_INGOT, 5).unwrap())
        .unwrap();
    snapshot.players[0].inventory = inventory.encode().to_vec();
    snapshot.players[0].window.clear();
    let mut mobs = MobRuntime::new(Config {
        seed: 1234,
        ..Config::default()
    })
    .unwrap();
    for i in 0..ACTORS {
        let final_actor = i == ACTORS - 1;
        let mut mob = Mob::spawned(
            EntityId(10_000 + i as u64),
            if final_actor {
                MobKind::Zombie
            } else {
                MobKind::Pig
            },
            SpaceId(0),
            if unknown_first && i == 0 {
                // Still inside the observer AOI, but the body page is absent.
                // A visited Unknown actor has no row patch/last_tick witness.
                [2.5, 17., 2.5]
            } else if final_actor {
                [32.5, 1., 17.7]
            } else {
                [2.5 + 1.5 * (i % 40) as f64, 1., 2.5 + 1.5 * (i / 40) as f64]
            },
            100 + i as u64,
        )
        .unwrap();
        mob.last_tick = tick;
        // Keep path-worker completion timing outside this recovery oracle.
        // Actors still execute their real AI, collisions and durable pose step.
        mob.ai.goal_until = Tick(tick.0 + 100);
        mobs.insert_initial(mob, false).unwrap();
    }
    let mut items = ItemRuntime::new(8192, 9).unwrap();
    let mut item = ItemEntity::new(
        EntityId(90_000),
        SpaceId(0),
        ItemStack::new(I_IRON_INGOT, 2).unwrap(),
        [32.6, 1.05, 16.5],
        9,
        1,
    )
    .unwrap();
    item.last_tick = tick;
    item.velocity = [0.; 3];
    items.insert_initial(item, true).unwrap();
    replace(
        &mut snapshot,
        save::RUNTIME_MOBS,
        mobs.snapshot().encode().unwrap(),
    );
    replace(
        &mut snapshot,
        save::RUNTIME_ITEMS,
        items.snapshot().encode().unwrap(),
    );
    store.checkpoint(&snapshot).unwrap();
    drop(store);
    directory
}
fn stop_mid_mob(directory: &Directory) -> (Server, u64, Tick) {
    let mut server = Server::open(directory.config()).unwrap();
    server.handle(1, ClientMessage::Hello(directory.owner()), 0);
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut now = 0;
    loop {
        now += 1;
        poll(&mut server, now, deadline);
        if server.load_metrics().mob_phase_pending && server.counters().mob_steps > 0 {
            let tick = server.current_tick();
            assert!(
                server.counters().mob_steps < ACTORS as u64,
                "must stop inside the actual actor cohort"
            );
            assert_eq!(server.machine_runtime().machine_count(), 0);
            return (server, now, tick);
        }
    }
}
fn finish_held(server: &mut Server, now: u64, tick: Tick) {
    let deadline = Instant::now() + Duration::from_secs(45);
    while server.load_metrics().mob_phase_pending || server.load_metrics().machine_phase_pending {
        poll(server, now, deadline);
        assert_eq!(
            server.current_tick(),
            tick,
            "no later tick may hide a skipped actor"
        );
    }
    // The final portion may still await its ACK. Drop drains that write. Do not
    // poll the final ACK: a newly opened Server has next_tick_at=0 and would
    // legitimately begin its next global tick in that same public tick call.
}
fn saved(directory: &Directory) -> save::Snapshot {
    let mut server = Server::open(directory.config()).unwrap();
    assert!(server.request_checkpoint());
    // Checkpoint carries the fully replayed image. Writer drop confirms it
    // without starting another simulation tick or authenticating any actor.
    drop(server);
    let (_store, recovery) = save::Store::open(&directory.0).unwrap();
    assert!(
        recovery.batches.is_empty(),
        "comparison uses confirmed checkpoints"
    );
    recovery.snapshot.unwrap()
}

fn run_recovery(unknown_first: bool) {
    let prepared = prepare(unknown_first);
    let uninterrupted = Directory::copy_from(&prepared);
    let interrupted = Directory::copy_from(&prepared);
    let (mut baseline, _, tick) = stop_mid_mob(&uninterrupted);
    let (crashed, _, crash_tick) = stop_mid_mob(&interrupted);
    assert_eq!(crash_tick, tick);
    // SaveWorker::drop drains its bounded queue. This captures the durable
    // partial WAL even when the caller has not yet received that write's ACK.
    drop(crashed);
    let mut reopened = Server::open(interrupted.config()).unwrap();
    assert_eq!(reopened.current_tick(), tick);
    assert_eq!(
        reopened.load_metrics().peers,
        0,
        "no Hello or production fake peer"
    );
    assert!(reopened.load_metrics().mob_phase_pending);
    assert_eq!(
        reopened
            .mob_entities()
            .rows()
            .iter()
            .filter(|mob| mob.last_tick == tick)
            .count(),
        1024 - usize::from(unknown_first),
        "the captured WAL must contain exactly the first completed AI portion"
    );
    assert!(
        reopened
            .mob_entities()
            .get(EntityId(10_000 + ACTORS as u64 - 1))
            .unwrap()
            .last_tick
            < tick
    );
    assert_eq!(reopened.machine_runtime().owner_context_tick(), Some(tick));
    assert_eq!(
        reopened.machine_runtime().owner_observers().unwrap(),
        &[interrupted.owner().identity]
    );
    finish_held(&mut baseline, 0, tick);
    finish_held(&mut reopened, 0, tick);
    drop(baseline);
    drop(reopened);
    let expected = saved(&uninterrupted);
    let actual = saved(&interrupted);
    let baseline = Server::open(uninterrupted.config()).unwrap();
    let reopened = Server::open(interrupted.config()).unwrap();
    assert!(
        baseline
            .mob_entities()
            .rows()
            .iter()
            .all(|mob| (unknown_first && mob.id == EntityId(10_000)) || mob.last_tick == tick)
    );
    if unknown_first {
        assert!(
            baseline
                .mob_entities()
                .get(EntityId(10_000))
                .unwrap()
                .last_tick
                < tick
        );
    }
    assert_eq!(
        reopened.mob_entities().rows(),
        baseline.mob_entities().rows()
    );
    assert!(
        reopened.item_entities().is_empty(),
        "captured actor must pick up the owned item"
    );
    drop(baseline);
    drop(reopened);
    assert_eq!(actual.server_tick, expected.server_tick);
    let expected_world = World::restore(
        rules::properties,
        world::JournalLimits::default(),
        expected.world.clone(),
    )
    .unwrap();
    let actual_world = World::restore(
        rules::properties,
        world::JournalLimits::default(),
        actual.world.clone(),
    )
    .unwrap();
    assert_eq!(
        actual_world.canonical_hash(),
        expected_world.canonical_hash()
    );
    assert_eq!(actual.players.len(), expected.players.len());
    for (actual, expected) in actual.players.iter().zip(&expected.players) {
        assert_eq!(actual.state, expected.state, "offline actor pose/HP");
        assert_eq!(actual.place_ready_at, expected.place_ready_at);
        assert_eq!(actual.break_ready_at, expected.break_ready_at);
        assert_eq!(actual.inventory, expected.inventory, "owned inventory");
        assert_eq!(actual.window, expected.window, "owned window/cursor");
        assert_eq!(
            actual.simulation, expected.simulation,
            "simulation continuation"
        );
    }
    let inventory = Inventory::decode(&actual.players[0].inventory).unwrap();
    assert_eq!(
        inventory.slots()[0],
        ItemStack::new(I_IRON_INGOT, 7).unwrap()
    );
    assert!(
        actual.players[0].state.health < 20.,
        "last cohort actor must damage the captured offline player: player={:?}",
        actual.players[0].state
    );
    assert_eq!(actual.runtimes.len(), expected.runtimes.len());
    for (actual, expected) in actual.runtimes.iter().zip(&expected.runtimes) {
        assert_eq!(actual.kind, expected.kind);
        let differences: Vec<_> = actual
            .payload
            .iter()
            .zip(&expected.payload)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(16)
            .map(|(i, (&a, &b))| (i, a, b))
            .collect();
        assert!(
            actual.payload == expected.payload,
            "runtime {} differs: lengths {}/{}, first(offset,actual,expected)={differences:?}",
            actual.kind,
            actual.payload.len(),
            expected.payload.len()
        );
    }
}

#[test]
fn mob_only_held_tick_reopens_before_hello_with_exact_actors_and_owned_inventory() {
    run_recovery(false);
}

#[test]
fn visited_unknown_actor_is_not_revisited_after_mid_mob_reopen() {
    run_recovery(true);
}
