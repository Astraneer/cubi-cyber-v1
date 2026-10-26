//! Egg sound comes from lay_egg, shares egg/mob WAL and crosses only after fsync.
use core_types::{Cell, PageId, SpaceId, Tick};
use net::{
    ClientMessage, ServerMessage,
    audio_cue::{Cue, Kind},
};
use rules::{
    entities::{EntityId, MobKind, mob::Mob},
    stack::ItemStack,
};
use server::{Outgoing, Server, ServerConfig};
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
    let mut c = ServerConfig::new(&d.0);
    c.generation_enabled = false;
    c.spawn_override = Some([8.5, 1., 8.5]);
    c
}
fn owner(d: &Directory) -> ClientMessage {
    net::decode_client(&std::fs::read(d.0.join("owner.identity")).unwrap()).unwrap()
}
struct H {
    server: Server,
    now: u64,
    out: Vec<Outgoing>,
}
impl H {
    fn step(&mut self) {
        self.now += 10;
        self.out.extend(self.server.tick(self.now));
        assert!(!self.server.is_failed(), "{:?}", self.server.counters());
        thread::sleep(Duration::from_micros(300));
    }
    fn send(&mut self, m: ClientMessage) {
        self.out.extend(self.server.handle(1, m, self.now));
    }
    fn until(&mut self, mut f: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(i) = self.out.iter().position(&mut f) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < deadline, "{:?}", self.server.counters());
            self.step();
        }
    }
}
#[test]
fn real_egg_phase_publishes_near_cue_after_same_wal_and_never_infers_it_from_existing_eggs() {
    let d = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("egg-cue-owner-{}", std::process::id())),
    );
    std::fs::create_dir_all(&d.0).unwrap();
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 0..5 {
        let mut cells = vec![Cell::AIR; 4096];
        cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
        tx.create_page(
            PageId {
                space: SpaceId(0),
                x,
                y: 0,
                z: 0,
            },
            cells,
        );
    }
    world.commit(tx).unwrap();
    let server = Server::open_with_world(config(&d), world).unwrap();
    let mut h = H {
        server,
        now: 0,
        out: Vec::new(),
    };
    // Complete the legacy readiness checkpoint before exercising the WAL
    // record; a later checkpoint legitimately compacts transient old events.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !h.server.height_ready_for_publication() {
        assert!(Instant::now() < deadline);
        h.step();
    }
    for (id, x) in [(90, 8.5), (91, 60.5)] {
        let mut chicken = Mob::new(EntityId(id), MobKind::Chicken, [x, 1., 11.5]).unwrap();
        chicken.egg_remaining = 1;
        assert!(h.server.queue_mob_spawn(chicken).is_ok());
    }
    h.send(owner(&d));
    let frame = h.until(|o| matches!(&o.message, ServerMessage::AudioCue(_)));
    let ServerMessage::AudioCue(cue) =
        net::decode_server(&net::encode_server(&frame.message).unwrap()).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(cue.kind, Kind::ChickenEgg);
    assert_eq!(cue.actor, 90);
    assert_eq!(cue.sequence, cue.tick.0);
    assert!(
        h.server
            .mob_entities()
            .get(EntityId(90))
            .unwrap()
            .egg_remaining
            >= 6000
    );
    assert!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .any(|item| item.stack.item() == rules::items::I_EGG && item.position == cue.position),
        "cue cannot precede egg ledger fsync"
    );
    let end = h.server.current_tick().0 + 8;
    while h.server.current_tick().0 < end {
        h.step();
    }
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(&o.message, ServerMessage::AudioCue(_))),
        "far chicken or same event must not create another cue"
    );
    let now = h.now;
    drop(h);
    let (store, recovery) = save::Store::open(&d.0).unwrap();
    let batch = recovery
        .batches
        .iter()
        .find_map(|(_, batch)| {
            batch
                .changes
                .iter()
                .flat_map(|c| &c.events)
                .any(|event| Cue::decode(&event.payload) == Ok(cue))
                .then_some(batch)
        })
        .expect("cue persisted in accepted world event");
    assert!(batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_MOBS));
    assert!(batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_ITEMS));
    drop(store);
    let mut h = H {
        server: Server::open(config(&d)).unwrap(),
        now,
        out: Vec::new(),
    };
    h.send(owner(&d));
    h.until(|o| matches!(&o.message, ServerMessage::Welcome(_)));
    let anchor = core_types::BlockPos {
        space: SpaceId(0),
        x: 7,
        y: 1,
        z: 8,
    };
    let drop = server::survival::plan_drops(
        h.server.world(),
        anchor,
        [7; 16],
        999,
        h.server.current_tick(),
        &[ItemStack::new(rules::items::I_EGG, 1).unwrap()],
        10,
        None,
    )
    .unwrap();
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    drop.append_to(&mut tx);
    assert!(h.server.queue_transaction(tx).is_ok());
    let end = h.server.current_tick().0 + 30;
    while h.server.current_tick().0 < end {
        h.step();
    }
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(&o.message, ServerMessage::AudioCue(_))),
        "recovery or a nearby ordinary egg never infers lay_egg"
    );
    assert_eq!(h.server.counters().audio_cues_sent, 0);
}
