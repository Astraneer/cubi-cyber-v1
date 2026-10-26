//! Trusted, bounded startup data for the actual SPEC8.6 server. This helper is
//! deliberately separate from the measured owner loop: no captured fixture
//! construction, full snapshot or credential enters the per-tick work result.
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use net::{Hello, PlayerId, Role, SecretToken};
use rules::{
    blocks::MekKind,
    entities::{EntityId, MobKind, mob::Mob},
    ids::*,
    items::I_DUST_IRON,
    machines::{Machine, Slot},
    stack::ItemStack,
    survival::fluid::Liquid,
};
use server::{
    Server, ServerConfig,
    entities::{Config as MobConfig, MobRuntime},
    machines::{Config as MachineConfig, MachineRuntime},
    survival::{Event, SurvivalRuntime},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
pub const MOBS: usize = 5000;
pub const MACHINES: usize = 10_000;
pub const FLUIDS: usize = 100_000;
pub const BOTS: usize = 32;
const WIDTH: i32 = 128;
const DEPTH: i32 = 112;
static NEXT: AtomicU64 = AtomicU64::new(1);
pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "spec86-scale-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.maximum_players = BOTS;
        c.generation_enabled = false;
        c.public_seed = 1337;
        c.spawn_override = Some([2.5, 1., 2.5]);
        c.spawn_protection_radius = 0;
        c
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub struct Prepared {
    pub directory: Directory,
    pub hellos: Vec<Hello>,
    pub start_tick: Tick,
    pub measurement_tick: Tick,
    pub machine_ids: Vec<ComponentId>,
    pub water_cells: Vec<BlockPos>,
}
fn at(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn water_at(p: BlockPos) -> bool {
    (2..102).contains(&p.x) && (2..102).contains(&p.z) && (16..26).contains(&p.y)
}
fn cell(p: BlockPos) -> Cell {
    if p.y == 0
        || p.y == 4
        || ((p.x == 0 || p.x == WIDTH - 1 || p.z == 0 || p.z == DEPTH - 1) && (1..4).contains(&p.y))
    {
        return Cell::from_parts(STONE, 0);
    }
    if water_at(p) {
        return Cell::from_parts(WATER, 0);
    }
    if ((1..=102).contains(&p.x) && (1..=102).contains(&p.z) && p.y == 15)
        || ((1..=102).contains(&p.x)
            && (1..=102).contains(&p.z)
            && (16..26).contains(&p.y)
            && (p.x == 1 || p.x == 102 || p.z == 1 || p.z == 102))
    {
        return Cell::from_parts(STONE, 0);
    }
    Cell::AIR
}
fn world() -> (World, Vec<ComponentId>, Vec<BlockPos>) {
    let mut w = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    let mut waters = Vec::with_capacity(FLUIDS);
    for py in 0..4 {
        for pz in 0..DEPTH / 16 {
            for px in 0..WIDTH / 16 {
                let mut cells = Vec::with_capacity(4096);
                for i in 0..4096 {
                    let p = at(
                        px * 16 + (i % 16),
                        py * 16 + (i / 256),
                        pz * 16 + (i / 16 % 16),
                    );
                    let value = cell(p);
                    if water_at(p) {
                        waters.push(p);
                    }
                    cells.push(value);
                }
                tx.create_page(
                    PageId {
                        space: SpaceId(0),
                        x: px,
                        y: py,
                        z: pz,
                    },
                    cells,
                );
            }
        }
    }
    let mut ids = Vec::with_capacity(MACHINES);
    for i in 0..MACHINES {
        let p = at(2 + (i % 124) as i32, 5, 2 + (i / 124) as i32);
        let id = ComponentId(100_000 + i as u64);
        tx.set_cell(p, Cell::from_parts(ENERGIZED_SMELTER, 2));
        tx.set_component(
            id,
            server::machines::machine_component(p, &Machine::new(MekKind::Smelter)),
            ChannelMask::CONTENTS,
        );
        ids.push(id);
    }
    w.commit(tx).unwrap();
    assert_eq!(waters.len(), FLUIDS);
    (w, ids, waters)
}
fn replace(snapshot: &mut save::Snapshot, kind: u16, payload: Vec<u8>) {
    assert!(!payload.is_empty() && payload.len() <= save::MAX_RUNTIME_BYTES);
    if let Some(record) = snapshot.runtimes.iter_mut().find(|r| r.kind == kind) {
        record.payload = payload;
    } else {
        snapshot
            .runtimes
            .push(save::RuntimeRecord { kind, payload });
        snapshot.runtimes.sort_unstable_by_key(|r| r.kind);
    }
}
pub fn peer_position(i: usize) -> [f64; 3] {
    if i < 16 {
        [16.5 + 32. * (i % 4) as f64, 1., 14.5 + 28. * (i / 4) as f64]
    } else {
        match i % 4 {
            0 => [2.5, 1., 2.5],
            1 => [125.5, 1., 2.5],
            2 => [2.5, 1., 109.5],
            _ => [125.5, 1., 109.5],
        }
    }
}
pub fn prepare() -> Prepared {
    let directory = Directory::new();
    let (store, _) = save::Store::open(&directory.0).unwrap();
    store
        .load_or_create_private_entropy(|| Ok([37; 32]))
        .unwrap();
    drop(store);
    let (world, machine_ids, water_cells) = world();
    let mut warm = Server::open_with_world(directory.config(), world).unwrap();
    let owner_hello =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap();
    warm.handle(1, owner_hello, 0);
    let deadline = Instant::now() + Duration::from_secs(240);
    let started = Instant::now();
    let now = loop {
        let now = started.elapsed().as_millis() as u64;
        warm.tick(now);
        assert!(!warm.is_failed(), "warmup {:?}", warm.failure_reason());
        let c = warm.counters();
        if warm.height_ready_for_publication()
            && c.light_pending_cells == 0
            && c.light_pending_seed_pages == 0
            && c.light_pending_sky_pages == 0
            && c.light_pending_bootstrap_pages == 0
            && !warm.awaiting_durability()
        {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "bounded fixture warmup {:?}",
            warm.load_metrics()
        );
        thread::sleep(Duration::from_micros(300));
    };
    assert!(warm.request_checkpoint());
    while warm.awaiting_durability() {
        warm.tick(now);
        assert!(!warm.is_failed(), "{:?}", warm.failure_reason());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(300));
    }
    drop(warm);
    let (mut store, recovery) = save::Store::open(&directory.0).unwrap();
    assert!(
        recovery.batches.is_empty(),
        "bootstrap must start from its confirmed checkpoint"
    );
    let mut snapshot = recovery.snapshot.expect("actual warm checkpoint");
    let player_template = snapshot
        .players
        .first()
        .expect("actual authenticated actor continuation")
        .clone();
    let tick = snapshot.server_tick;
    let measurement_tick = Tick(tick.0 + 64);
    let mut world = World::restore(
        rules::properties,
        world::JournalLimits::default(),
        snapshot.world.clone(),
    )
    .unwrap();
    let mut tx = Transaction::new(tick, SpaceId(0));
    for &id in &machine_ids {
        let p = world.component(id).unwrap().data.anchor;
        let mut machine = Machine::new(MekKind::Smelter);
        machine.energy = 20_000;
        machine.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 64).unwrap());
        tx.set_component(
            id,
            server::machines::machine_component(p, &machine),
            ChannelMask::CONTENTS,
        );
    }
    let activation_change = world.commit(tx).unwrap();
    let mut activation = save::Batch::empty(tick);
    activation.changes.push(activation_change);
    store.append(&activation).unwrap();
    snapshot.record_sequence = store.last_sequence();
    let mut machinery = MachineRuntime::new(MachineConfig::default()).unwrap();
    let delta = machinery
        .extend_registration(
            machinery.noop_delta().unwrap(),
            &world,
            machine_ids.iter().copied(),
            tick,
        )
        .unwrap();
    machinery.acknowledge(delta).unwrap();
    assert_eq!(machinery.machine_count(), MACHINES);
    assert_eq!(machinery.event_count(), MACHINES);
    let mut survival = SurvivalRuntime::with_seed(100_000, 1337);
    for &p in &water_cells {
        assert!(
            survival
                .register(Event::Fluid(p, Liquid::Water), measurement_tick)
                .unwrap()
        );
    }
    assert_eq!(survival.len(), FLUIDS);
    let mut mobs = MobRuntime::new(MobConfig {
        seed: 1234,
        ..MobConfig::default()
    })
    .unwrap();
    for i in 0..MOBS {
        let mut mob = Mob::spawned(
            EntityId(i as u64 + 1),
            MobKind::Pig,
            SpaceId(0),
            [4. + 1.5 * (i % 80) as f64, 1., 4. + 1.5 * (i / 80) as f64],
            10_000 + i as u64,
        )
        .unwrap();
        mob.last_tick = tick;
        mobs.insert_initial(mob, true).unwrap();
    }
    assert_eq!(mobs.entities().active_len(), MOBS);
    replace(
        &mut snapshot,
        save::RUNTIME_SURVIVAL,
        survival.snapshot().encode(),
    );
    replace(
        &mut snapshot,
        save::RUNTIME_MACHINES,
        machinery.snapshot().encode().unwrap(),
    );
    replace(
        &mut snapshot,
        save::RUNTIME_MOBS,
        mobs.snapshot().encode().unwrap(),
    );
    let mut hellos = Vec::with_capacity(BOTS);
    for i in 0..BOTS {
        let id = PlayerId([101 + i as u8; 16]);
        let token = SecretToken::new([101 + i as u8; 32]);
        let name = format!("scale_{i}");
        snapshot.identities.push(save::Identity {
            id,
            name: name.clone(),
            credential_hash: blake3::derive_key(
                "cubi local identity credential v1",
                token.as_bytes(),
            ),
            role: Role::Admin,
            banned: false,
        });
        let mut saved = player_template.clone();
        saved.state.identity = id;
        // Reuse the real versioned continuation unchanged. Each authenticated
        // benchmark peer performs its actual /tp before the measured window.
        snapshot.players.push(saved);
        snapshot.dedup.push((id, save::DedupState::default()));
        hellos.push(Hello {
            identity: id,
            name,
            credential: Some(token),
        });
    }
    snapshot.world = world.checkpoint().to_restore_state();
    store.checkpoint(&snapshot).unwrap();
    drop(store);
    Prepared {
        directory,
        hellos,
        start_tick: tick,
        measurement_tick,
        machine_ids,
        water_cells,
    }
}
