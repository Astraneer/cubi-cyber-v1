//! Real owner phases, durable actors and private damage/death state.
use core_types::{BlockPos, Cell, SpaceId, Tick};
use net::{Action, ActionId, ActionStatus, ClientMessage, Hello, ServerMessage};
use rules::entities::{EntityId, MobKind, mob::Mob};
use server::{Outgoing, Server, ServerConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "mob-owner-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.generation_enabled = false;
        c.spawn_override = Some([8.5, 1., 8.5]);
        c.spawn_protection_radius = 0;
        c
    }
    fn owner(&self) -> Hello {
        let ClientMessage::Hello(h) =
            net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
        else {
            unreachable!()
        };
        h
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn pos() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 1,
        z: 8,
    }
}
fn fixture() -> World {
    let mut w = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    for z in 0..16 {
        for x in 0..16 {
            cells[x + 16 * z] = Cell::from_parts(rules::ids::STONE, 0);
            cells[x + 16 * z + 4 * 256] = Cell::from_parts(rules::ids::STONE, 0);
            // Keep knockback in a fully known room. An open one-page fixture
            // lets the victim retreat to Unknown, where pathfinding must stop.
            if x == 0 || x == 15 || z == 0 || z == 15 {
                for y in 1..4 {
                    cells[x + 16 * z + y * 256] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
        }
    }
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(pos().page(), cells);
    w.commit(tx).unwrap();
    w
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
        assert!(
            !self.server.is_failed(),
            "canonical mob phase failed {:?}",
            self.server.counters()
        );
        thread::sleep(Duration::from_micros(500));
    }
    fn send(&mut self, m: ClientMessage) {
        self.out.extend(self.server.handle(1, m, self.now));
    }
    fn until(&mut self, mut p: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(20);
        for _ in 0..20_000 {
            if let Some(i) = self.out.iter().position(&mut p) {
                return self.out.remove(i);
            }
            assert!(
                Instant::now() < deadline,
                "mob authority output deadline {:?}",
                self.server.counters()
            );
            self.step();
        }
        panic!(
            "missing mob output: latest={:?}, mobs={:?}, counters={:?}",
            self.out
                .iter()
                .rev()
                .find(|o| matches!(o.message, ServerMessage::Prediction { .. }))
                .map(|o| &o.message),
            self.server.mob_entities().rows(),
            self.server.counters()
        )
    }
    fn join(&mut self, h: Hello) {
        self.send(ClientMessage::Hello(h));
        self.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    }
    fn action(&mut self, id: u64, action: Action) {
        self.send(ClientMessage::Action {
            id: ActionId(id),
            action,
        });
        let out=self.until(|o|matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        assert!(
            matches!(out.message,ServerMessage::Action(r) if r.status==ActionStatus::Accepted),
            "{:?}",
            out.message
        );
    }
}
#[test]
fn trusted_birth_runs_actual_physics_ai_and_reopens_exact_durable_mob() {
    let d = Directory::new();
    let mut h = H {
        server: Server::open_with_world(d.config(), fixture()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner = d.owner();
    h.join(owner.clone());
    let mut pig = Mob::spawned(
        EntityId(9001),
        MobKind::Pig,
        SpaceId(0),
        [8.5, 1., 10.5],
        17,
    )
    .unwrap();
    pig.body.velocity = [0.12, 0.2, 0.];
    h.server.queue_mob_spawn(pig).unwrap();
    for _ in 0..2000 {
        h.step();
        if h.server
            .mob_entities()
            .get(pig.id)
            .is_some_and(|m| m.age >= 12)
        {
            break;
        }
    }
    let actual = *h.server.mob_entities().get(pig.id).unwrap();
    assert!(actual.age >= 12);
    assert_ne!(actual.body.position, pig.body.position);
    assert!(h.server.counters().mob_steps >= 12);
    assert!(h.server.counters().mob_cell_reads > 0);
    while h.server.awaiting_durability() {
        h.server.tick(h.now);
        thread::sleep(Duration::from_micros(300));
    }
    let expected = h
        .server
        .mob_entities()
        .get(pig.id)
        .unwrap()
        .encode()
        .unwrap();
    drop(h);
    let server = Server::open(d.config()).unwrap();
    assert_eq!(
        server.mob_entities().get(pig.id).unwrap().encode().unwrap(),
        expected,
        "same fsync frontier recovers complete actor life/body/AI/pose"
    );
}

#[test]
fn six_hundred_owned_actors_finish_their_budgeted_microbatches_at_each_global_tick() {
    let directory = Directory::new();
    let mut world = World::new(rules::properties, 4096);
    let mut create = Transaction::new(Tick(1), SpaceId(0));
    for z in 0..4 {
        for x in 0..4 {
            let mut cells = vec![Cell::AIR; 4096];
            cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
            create.create_page(
                core_types::PageId {
                    space: SpaceId(0),
                    x,
                    y: 0,
                    z,
                },
                cells,
            );
        }
    }
    world.commit(create).unwrap();
    let mut h = H {
        server: Server::open_with_world(directory.config(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.join(directory.owner());
    for i in 0..600 {
        let pig = Mob::spawned(
            EntityId(10_000 + i),
            MobKind::Pig,
            SpaceId(0),
            [5. + 1.5 * (i % 20) as f64, 1., 5. + 1.5 * (i / 20) as f64],
            i + 1,
        )
        .unwrap();
        h.server.queue_mob_spawn(pig).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while h.server.mob_entities().len() < 600 {
        assert!(Instant::now() < deadline);
        h.step();
    }
    let start = h.server.current_tick().0;
    loop {
        h.step();
        assert!(
            Instant::now() < deadline,
            "600actors must make bounded progress {:?}",
            h.server.counters()
        );
        if h.server.current_tick().0 >= start + 4 {
            while h.server.awaiting_durability() {
                h.out.extend(h.server.tick(h.now));
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_micros(300));
            }
        }
        let now = h.server.current_tick();
        if now.0 >= start + 4
            && !h.server.awaiting_durability()
            && h.server
                .mob_entities()
                .rows()
                .iter()
                .all(|mob| mob.last_tick == now)
        {
            break;
        }
    }
    assert!(
        h.server
            .mob_entities()
            .rows()
            .iter()
            .all(|mob| mob.age >= 4)
    );
    assert!(h.server.counters().mob_steps >= 2400);
    assert_eq!(h.server.mob_entities().len(), 600);
}
#[test]
fn actual_zombie_hits_clear_death_inventory_and_owned_cursor_in_same_wal() {
    let d = Directory::new();
    let mut h = H {
        server: Server::open_with_world(d.config(), fixture()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let owner = d.owner();
    h.join(owner.clone());
    h.action(
        1,
        Action::Command(format!("/give {} 3", rules::items::I_IRON_INGOT)),
    );
    h.action(2, Action::Window(net::window::WindowIntent::OpenInventory));
    let image = h.until(|o| matches!(&o.message,ServerMessage::Window(w) if !w.closed));
    let ServerMessage::Window(w) = image.message else {
        unreachable!()
    };
    h.action(
        3,
        Action::Window(net::window::WindowIntent::Click {
            guard: w.guard,
            slot: 0,
            right: false,
            shift: false,
        }),
    );
    h.until(|o|matches!(&o.message,ServerMessage::Window(w) if w.cursor.item==rules::items::I_IRON_INGOT&&w.cursor.count==3));
    h.out.clear();
    let zombie = Mob::spawned(
        EntityId(9002),
        MobKind::Zombie,
        SpaceId(0),
        [8.5, 1., 11.5],
        19,
    )
    .unwrap();
    h.server.queue_mob_spawn(zombie).unwrap();
    h.until(|o| matches!(&o.message,ServerMessage::Prediction{player,..} if player.health==0.));
    let closed =
        h.until(|o| matches!(&o.message,ServerMessage::Window(w) if w.closed&&w.cursor.count==0));
    assert!(matches!(closed.message, ServerMessage::Window(_)));
    let output=h.until(|o|matches!(&o.message,ServerMessage::Inventory{data} if rules::survival::Inventory::decode(data).is_ok_and(|i|i.slots().iter().all(|s|s.is_empty()))));
    assert!(matches!(output.message, ServerMessage::Inventory { .. }));
    let drops: Vec<_> = h
        .server
        .item_entities()
        .rows()
        .iter()
        .filter(|i| i.stack.item() == rules::items::I_IRON_INGOT)
        .copied()
        .collect();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].stack.count(), 3);
    assert_eq!(drops[0].pickup_delay, 40);
    drop(h);
    let (_store, recovery) = save::Store::open(&d.0).unwrap();
    let player = recovery
        .batches
        .iter()
        .flat_map(|(_, b)| &b.players)
        .rev()
        .find(|p| p.state.identity == owner.identity)
        .or_else(|| {
            recovery.snapshot.as_ref().and_then(|s| {
                s.players
                    .iter()
                    .find(|p| p.state.identity == owner.identity)
            })
        })
        .unwrap();
    assert_eq!(player.state.health, 0.);
    assert!(
        rules::survival::Inventory::decode(&player.inventory)
            .unwrap()
            .slots()
            .iter()
            .all(|s| s.is_empty())
    );
    assert!(
        server::windows::BoundWindow::decode(&player.window)
            .unwrap()
            .state
            .closed()
    );
}
