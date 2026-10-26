//! Actual partial Smelter WAL, with an uninterrupted independent owner.
use crate::*;
use core_types::{Cell, ChannelMask, ComponentId};
use rules::{blocks::MekKind, ids::*, machines::Machine, stack::ItemStack};
use rules::{items::I_DUST_IRON, machines::Slot};
use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use world::Transaction;

struct Directories(PathBuf, PathBuf);
impl Drop for Directories {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir_all(&self.1);
    }
}
fn config(path: &Path) -> ServerConfig {
    let mut c = ServerConfig::new(path);
    c.generation_enabled = false;
    c.spawn_override = Some([24.5, 1., 24.5]);
    c.spawn_protection_radius = 0;
    c
}
fn position(i: usize) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 2 + (i % 44) as i32,
        y: 5 + (i / (44 * 44)) as i32,
        z: 2 + (i / 44 % 44) as i32,
    }
}
fn world() -> World {
    let mut w = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for z in 0..3 {
        for x in 0..3 {
            let mut cells = vec![Cell::AIR; 4096];
            cells[..256].fill(Cell::from_parts(STONE, 0));
            cells[4 * 256..5 * 256].fill(Cell::from_parts(STONE, 0));
            tx.create_page(
                PageId {
                    space: SpaceId(0),
                    x,
                    y: 0,
                    z,
                },
                cells,
            );
        }
    }
    // Non-emissive cubes isolate captured tick context from derived LIGHT's
    // independently rebuilt propagation frontier. Recipe fairness is covered
    // separately by the 10k Smelter/Store barrier tests.
    for i in 0..2049 {
        let p = position(i);
        tx.set_cell(p, Cell::from_parts(ENERGIZED_SMELTER, 2));
        tx.set_component(
            ComponentId(i as u64 + 1),
            machines::machine_component(p, &Machine::new(MekKind::Smelter)),
            ChannelMask::CONTENTS,
        );
    }
    w.commit(tx).unwrap();
    w
}
fn acknowledge(server: &mut Server) {
    let until = Instant::now() + Duration::from_secs(30);
    while server.pending.is_some() {
        server.finish_durability();
        assert!(!server.failed, "{:?}", server.failure_reason());
        assert!(Instant::now() < until, "actual writer completion");
        thread::sleep(Duration::from_micros(200));
    }
}
fn complete_held(server: &mut Server, tick: Tick) {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        acknowledge(server);
        server.restore_tick_context().unwrap();
        if server.runtimes.machines.owner_pending() {
            assert!(server.finish_machine_phase().unwrap());
        } else if server.mob_owner.due {
            server.finish_mob_phase().unwrap();
        } else {
            break;
        }
        assert_eq!(server.tick, tick);
        assert!(!server.failed, "{:?}", server.failure_reason());
        assert!(
            Instant::now() < until,
            "finite captured tick {:?}",
            server.load_metrics()
        );
    }
}
#[test]
fn emissive_machine_partial_wal_preserves_same_tick_light_frontier() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/tmp");
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dirs = Directories(
        root.join(format!("captured-machine-live-{suffix}")),
        root.join(format!("captured-machine-reopen-{suffix}")),
    );
    std::fs::create_dir_all(&dirs.0).unwrap();
    std::fs::create_dir_all(&dirs.1).unwrap();
    let mut live = Server::open_with_world(config(&dirs.0), world()).unwrap();
    let hello = net::decode_client(&std::fs::read(dirs.0.join("owner.identity")).unwrap()).unwrap();
    live.handle(1, hello, 0);
    let until = Instant::now() + Duration::from_secs(60);
    let mut now = 0;
    loop {
        now += 50;
        live.tick(now);
        acknowledge(&mut live);
        if live.runtimes.machines.owner_pending() {
            let held = live.tick;
            complete_held(&mut live, held);
        }
        let c = live.counters();
        if live.peers.get(&1).is_some_and(|p| p.ready)
            && live.height_ready()
            && live.runtimes.topology_bootstrap.is_empty()
            && c.light_pending_cells == 0
            && c.light_pending_seed_pages == 0
            && c.light_pending_bootstrap_pages == 0
            && c.light_pending_sky_pages == 0
            && live.runtimes.machines.event_count() == 0
        {
            break;
        }
        assert!(!live.failed, "{:?}", live.failure_reason());
        assert!(
            Instant::now() < until,
            "warm actual owner {:?}",
            live.load_metrics()
        );
    }
    let identity = live.peers[&1].identity;
    let mut item = rules::entities::item::ItemEntity::new(
        rules::entities::EntityId(999_001),
        SpaceId(0),
        ItemStack::new(STONE, 3).unwrap(),
        [24.5, 1., 24.5],
        123,
        2,
    )
    .unwrap();
    item.velocity = [0.; 3];
    item.last_tick = live.tick;
    live.runtimes.items.insert_initial(item, true).unwrap();
    assert!(live.request_checkpoint());
    acknowledge(&mut live);
    let activation_tick = live.tick;
    let mut tx = Transaction::new(live.tick, SpaceId(0));
    for i in 0..2049 {
        let mut m = Machine::new(MekKind::Smelter);
        m.energy = 10_000;
        m.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
        tx.set_component(
            ComponentId(i as u64 + 1),
            machines::machine_component(position(i), &m),
            ChannelMask::CONTENTS,
        );
    }
    assert!(live.queue_transaction(tx).is_ok());
    loop {
        now += 50;
        live.tick(now);
        acknowledge(&mut live);
        if live.runtimes.machines.owner_pending() {
            break;
        }
        assert!(!live.failed, "{:?}", live.failure_reason());
        assert!(Instant::now() < until, "open actual partial cohort");
    }
    let held = live.tick;
    assert_eq!(live.runtimes.machines.owner_context_tick(), Some(held));
    assert_eq!(
        live.runtimes.machines.owner_observers(),
        Some([identity].as_slice())
    );
    assert_eq!(
        live.runtimes.items.entities().len(),
        1,
        "pickup must wait for the late phase"
    );
    // Writer is quiescent here: copy exactly its durable snapshot+WAL, retaining
    // the original live owner as the independent uninterrupted reference.
    for entry in std::fs::read_dir(&dirs.0).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), dirs.1.join(entry.file_name())).unwrap();
        }
    }
    let mut recovered = Server::open(config(&dirs.1)).unwrap();
    assert!(recovered.peers.is_empty());
    assert_eq!(recovered.tick, held);
    assert!(recovered.runtimes.machines.owner_pending());
    assert_eq!(
        recovered.runtimes.machines.owner_observers(),
        Some([identity].as_slice())
    );
    assert_eq!(
        live.world.canonical_hash(),
        recovered.world.canonical_hash(),
        "opening durable World"
    );
    complete_held(&mut live, held);
    complete_held(&mut recovered, held);
    if live.world.canonical_hash() != recovered.world.canonical_hash() {
        let cells = live
            .world
            .pages()
            .flat_map(|(id, page)| {
                (0..4096).map(move |i| (id, page, core_types::LocalIdx::new(i).unwrap()))
            })
            .filter(|(id, page, idx)| {
                let other = recovered.world.page(*id).unwrap();
                page.cell(*idx) != other.cell(*idx) || page.light(*idx) != other.light(*idx)
            })
            .count();
        let components = live
            .world
            .components()
            .filter(|(id, rec)| {
                recovered
                    .world
                    .component(*id)
                    .is_none_or(|other| other.data != rec.data || other.version != rec.version)
            })
            .count();
        eprintln!(
            "recovery_diff cells={cells} components={components} live_world_tick={:?} recovered_world_tick={:?} live_world_version={:?} recovered_world_version={:?}",
            live.world.tick(),
            recovered.world.tick(),
            live.world.version(),
            recovered.world.version()
        );
    }
    assert_eq!(
        live.world.canonical_hash(),
        recovered.world.canonical_hash()
    );
    let records = |s: &Server| {
        s.runtimes
            .snapshots()
            .unwrap()
            .into_iter()
            .map(|r| (r.kind, r.payload))
            .collect::<Vec<_>>()
    };
    assert_eq!(records(&live), records(&recovered));
    assert_eq!(
        live.lighting.snapshot().unwrap().payload,
        recovered.lighting.snapshot().unwrap().payload,
        "exact durable LIGHT queues/free ordering/inputs/sky obligations"
    );
    let before = &live.saved_players[&identity];
    let after = &recovered.saved_players[&identity];
    assert_eq!(before.state, after.state);
    assert_eq!(before.place_ready_at, after.place_ready_at);
    assert_eq!(before.break_ready_at, after.break_ready_at);
    assert_eq!(before.inventory, after.inventory);
    assert_eq!(before.window, after.window);
    assert_eq!(before.simulation, after.simulation);
    let inventory =
        rules::survival::Inventory::decode(&recovered.saved_players[&identity].inventory).unwrap();
    assert_eq!(inventory.get(0).unwrap(), ItemStack::new(STONE, 3).unwrap());
    assert!(live.runtimes.items.entities().is_empty());
    assert!(recovered.runtimes.items.entities().is_empty());
    for i in 0..2049 {
        let actual = machines::read_machine(&recovered.world, ComponentId(i as u64 + 1)).unwrap();
        let elapsed = u32::try_from(held.0 - activation_tick.0).unwrap();
        assert!(
            elapsed > 0 && elapsed < 200,
            "fixture remains inside one recipe"
        );
        let expected = machines::read_machine(&live.world, ComponentId(i as u64 + 1)).unwrap();
        assert_eq!(actual, expected, "the entire durable machine must match");
        assert!(actual.progress > 0 && actual.progress <= elapsed);
        assert_eq!(actual.energy, 10_000 - 50 * u64::from(actual.progress));
        assert_eq!(
            recovered
                .runtimes
                .machines
                .machine_meta(ComponentId(i as u64 + 1))
                .unwrap()
                .last_tick,
            held
        );
    }
}
