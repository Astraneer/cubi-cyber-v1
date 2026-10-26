use super::*;
use core_types::{ComponentId, PAGE_VOLUME};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "spawn-showcase-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        ServerConfig::new(&self.0)
    }
    fn hello(&self) -> Hello {
        let ClientMessage::Hello(hello) =
            net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
        else {
            panic!("Owner identity");
        };
        hello
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn owner(config: &ServerConfig) -> Owner {
    Owner::new(
        config,
        r#gen::Generator::new(r#gen::TerrainSeed(config.public_seed))
            .spawn()
            .center,
    )
    .unwrap()
}
fn fixture(owner: &Owner) -> World {
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    let mut pages: BTreeSet<_> = owner.pages.iter().copied().collect();
    for part in owner.parts.iter().filter(|p| p.container()) {
        for y in 0..8 {
            pages.insert(PageId {
                space: SpaceId(0),
                x: (part.position.x + 3).div_euclid(16),
                y,
                z: part.position.z.div_euclid(16),
            });
        }
    }
    for &page in &pages {
        let mut cells = vec![Cell::AIR; PAGE_VOLUME];
        if page.y == owner.center[1].div_euclid(16) {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + z * 16 + owner.center[1].rem_euclid(16) as usize * 256] =
                        Cell::from_parts(STONE, 0);
                }
            }
        }
        tx.create_page(page, cells);
    }
    world.commit(tx).unwrap();

    world
}
struct Harness {
    server: Server,
    now: u64,
    out: Vec<Outgoing>,
    deadline: Instant,
    cold_wind_seen: bool,
}
impl Harness {
    fn new(server: Server) -> Self {
        Self {
            server,
            now: 0,
            out: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(90),
            cold_wind_seen: false,
        }
    }
    fn step(&mut self) {
        self.now += 10;
        self.out.extend(self.server.tick(self.now));
        if let Some(owner) = &self.server.spawn_showcase {
            let wind = owner
                .parts
                .iter()
                .find(|p| p.cell.id() == WIND_GENERATOR)
                .unwrap()
                .position;
            let head = BlockPos {
                y: wind.y + 4,
                ..wind
            };
            if let Some(id) = self
                .server
                .world
                .page(wind.page())
                .and_then(|p| p.component(wind.local_index()))
                && let Some(record) = self.server.world.component(id)
                && let Ok(machine) = Machine::decode(&record.data.payload)
            {
                let sky = self
                    .server
                    .world
                    .page(head.page())
                    .unwrap()
                    .light(head.local_index())
                    .sky;
                self.cold_wind_seen |= sky < 15 && machine.produced == 0;
            }
        }
        assert!(
            !self.server.is_failed(),
            "server failed: {:?}",
            self.server.failure_reason()
        );
        assert!(
            Instant::now() < self.deadline,
            "bounded real server deadline"
        );
        thread::sleep(Duration::from_micros(200));
    }
    fn until(&mut self, predicate: impl Fn(&Server) -> bool) {
        while !predicate(&self.server) {
            self.step();
        }
    }
    fn durable(&mut self) {
        self.until(|s| s.pending.is_none());
    }
    fn ready(&mut self) {
        self.until(|s| {
            s.pending.is_none()
                && s.spawn_showcase.as_ref().unwrap().ready
                && s.runtimes
                    .machines
                    .transmitter_config(s.spawn_showcase.as_ref().unwrap().pull)
                    .is_some_and(|c| c.modes == 2 << (2 * 3))
        });
    }
    fn send(&mut self, message: ClientMessage) {
        self.out.extend(self.server.handle(1, message, self.now));
    }
    fn action(&mut self, id: u64, action: Action, aim: Option<[f32; 2]>) -> ActionReply {
        self.send(if let Some(aim) = aim {
            ClientMessage::AimedAction {
                id: ActionId(id),
                action,
                yaw: aim[0],
                pitch: aim[1],
            }
        } else {
            ClientMessage::Action {
                id: ActionId(id),
                action,
            }
        });
        loop {
            if let Some(index)=self.out.iter().position(|o|matches!(o.message,ServerMessage::Action(r) if r.id==ActionId(id)&&r.status!=ActionStatus::Pending)) {
                let ServerMessage::Action(reply)=self.out.remove(index).message else { unreachable!(); }; return reply;
            }
            self.step();
        }
    }
}
fn component(world: &World, p: BlockPos) -> (ComponentId, &world::ComponentData) {
    let id = world
        .page(p.page())
        .unwrap()
        .component(p.local_index())
        .unwrap();
    (id, &world.component(id).unwrap().data)
}
#[test]
fn canonical_display_runs_three_real_lines_without_observers_and_reopens_exactly() {
    let dir = Directory::new();
    let config = dir.config();
    let plan = owner(&config);
    assert!(plan.parts.len() <= 64 && plan.pages.len() <= 64 && plan.active.len() <= 8);
    let mut h = Harness::new(Server::open_with_world(config.clone(), fixture(&plan)).unwrap());
    h.ready();
    assert!(h.server.peers.is_empty());
    for part in &plan.parts {
        assert!(
            matches!(h.server.world.read(part.position),CellRead::Known(cell) if part.matches(cell))
        );
    }
    let mut outputs = BTreeSet::new();
    let mut saw_packet = false;
    let wind = plan
        .parts
        .iter()
        .find(|p| p.cell.id() == WIND_GENERATOR)
        .unwrap()
        .position;
    let head = BlockPos {
        y: wind.y + 4,
        ..wind
    };
    let mut dark_before_ready = h.cold_wind_seen;
    let mut lit_at = None;
    let mut powered_after_ready = false;
    while h.server.current_tick().0 < 6000 && outputs.len() < 3 {
        h.step();
        saw_packet |= h.server.runtimes.machines.packet_count() > 0;
        if h.server.pending.is_none() {
            let machine = Machine::decode(&component(&h.server.world, wind).1.payload).unwrap();
            let sky = h
                .server
                .world
                .page(head.page())
                .unwrap()
                .light(head.local_index())
                .sky;
            dark_before_ready |= sky < 15 && machine.produced == 0;
            if sky == 15 {
                lit_at.get_or_insert(h.server.current_tick().0);
            }
            powered_after_ready |= sky == 15 && machine.produced > 0;
            if let Some(at) = lit_at {
                assert!(
                    powered_after_ready || h.server.current_tick().0 <= at + 40,
                    "committed sky15 must causally wake the blocked wind generator; tick={:?} litAt={at} ticker={} produced={}",
                    h.server.current_tick(),
                    machine.ticker,
                    machine.produced
                );
            }
        }
        for part in plan
            .parts
            .iter()
            .filter(|p| p.cell.id() == CHEST && !p.source)
        {
            let (_, data) = component(&h.server.world, part.position);
            let chest = Chest::decode(&data.payload).unwrap();
            if chest.slots().iter().any(|s| !s.is_empty()) {
                outputs.insert(part.hall as u8);
            }
        }
    }
    if outputs.len() != 3 {
        eprintln!(
            "SHOWCASE outputs={outputs:?} packets={saw_packet} tick={:?}",
            h.server.current_tick()
        );
        for part in plan.parts.iter().filter(|p| p.container()) {
            let (id, data) = component(&h.server.world, part.position);
            if data.kind == machines::MACHINE_COMPONENT {
                let machine = Machine::decode(&data.payload).unwrap();
                eprintln!(
                    "SHOWCASE {:?} {:?} kind={:?} energy={} progress={} produced={} last={:?} tank={:?} slots={:?} meta={:?}",
                    part.hall,
                    part.position,
                    machine.kind,
                    machine.energy,
                    machine.progress,
                    machine.produced,
                    machine.last,
                    machine.tank,
                    machine.slots,
                    h.server.runtimes.machines.machine_meta(id)
                );
            } else {
                eprintln!(
                    "SHOWCASE {:?} chest {:?}",
                    part.hall,
                    Chest::decode(&data.payload).unwrap().slots()
                );
            }
        }
    }
    assert_eq!(
        outputs.len(),
        3,
        "all actual wind/steel/doubling outputs must arrive"
    );
    assert!(
        saw_packet,
        "actual source chest must use transporter packets"
    );
    assert!(
        dark_before_ready && powered_after_ready,
        "cold blocked wind must resume only after committed skylight converges"
    );
    h.durable();
    eprintln!(
        "SHOWCASE_OWNER_OK cells={} components={} active_pages={} generated_pages={} tick={} outputs=3 packets=true no_observers=true cold_sky=true",
        plan.parts.len(),
        plan.parts.iter().filter(|p| p.container()).count(),
        plan.active.len(),
        plan.pages.len(),
        h.server.current_tick().0
    );
    let exact: Vec<_> = plan
        .parts
        .iter()
        .filter(|p| p.container())
        .map(|p| {
            let (id, data) = component(&h.server.world, p.position);
            (p.position, id, data.payload.clone())
        })
        .collect();
    let tick = h.server.current_tick();
    drop(h);
    let reopened = Server::open(config).unwrap();
    assert_eq!(reopened.current_tick(), tick);
    for (position, id, bytes) in exact {
        let (actual, data) = component(reopened.world(), position);
        assert_eq!(actual, id);
        assert_eq!(
            data.payload, bytes,
            "reopen must retain progress, energy, slots and output"
        );
    }
    assert!(
        reopened.item_entities().is_empty(),
        "showcase never leaks item drops"
    );
}
#[test]
fn physical_showcase_owner_window_is_read_only_even_for_admin_and_has_durable_landmark() {
    let dir = Directory::new();
    let config = dir.config();
    let plan = owner(&config);
    let target = plan
        .parts
        .iter()
        .find(|p| p.hall == Hall::Double && p.source)
        .unwrap()
        .position;
    let mut h = Harness::new(Server::open_with_world(config, fixture(&plan)).unwrap());
    h.ready();
    // Existing authoritative fixture contains all dependencies. Avoid unrelated
    // spawn-neighbour jobs after authentication in this focused owner test.
    h.server.generation = None;
    h.send(ClientMessage::Hello(dir.hello()));
    h.until(|s| s.peers.get(&1).is_some_and(|p| p.ready) && s.pending.is_none());
    let welcome = h
        .out
        .iter()
        .position(|o| matches!(o.message, ServerMessage::Welcome(_)))
        .unwrap();
    let landmark = h
        .out
        .iter()
        .position(|o| matches!(o.message, ServerMessage::SpawnLandmark(_)))
        .unwrap();
    assert!(landmark > welcome);
    assert_eq!(
        h.action(
            1,
            Action::Command(format!(
                "/tp {} {} {}",
                target.x as f64 + 2.5,
                target.y as f64,
                target.z as f64 + 0.5
            )),
            None
        )
        .status,
        ActionStatus::Accepted
    );
    let CellRead::Known(expected) = h.server.world.read(target) else {
        panic!("source chest");
    };
    assert_eq!(
        h.action(
            2,
            Action::Window(net::window::WindowIntent::OpenBlock {
                position: target,
                expected
            }),
            Some([90., 25.])
        )
        .status,
        ActionStatus::Accepted
    );
    let peer = &h.server.peers[&1];
    assert!(h.out.iter().any(
        |o| matches!(&o.message,ServerMessage::Window(w) if w.binding==Some(target) && w.read_only)
    ));
    assert_eq!(peer.window.binding.unwrap().anchor, target);
    assert!(
        peer.window
            .state
            .container()
            .iter()
            .any(|s| s.item() == I_RAW_IRON)
    );
    let before = peer.inventory;
    let guard = peer.window.state.guard(&peer.inventory);
    let g = net::window::WindowGuard {
        id: guard.id,
        revision: guard.revision,
        inventory_revision: guard.inventory_revision,
    };
    for (id, intent) in [
        (
            3,
            net::window::WindowIntent::Click {
                guard: g,
                slot: 41,
                right: false,
                shift: false,
            },
        ),
        (
            4,
            net::window::WindowIntent::MachineSide {
                guard: g,
                face: 0,
                mode: 0,
            },
        ),
        (
            5,
            net::window::WindowIntent::RecipeFill {
                guard: g,
                recipe: 0,
                all: false,
            },
        ),
        (
            6,
            net::window::WindowIntent::TakeCraft {
                guard: g,
                all: false,
            },
        ),
    ] {
        assert_eq!(
            h.action(id, Action::Window(intent), None).status,
            ActionStatus::Rejected(RejectReason::Protected)
        );
        assert_eq!(h.server.peers[&1].inventory, before);
        assert!(h.server.peers[&1].window.state.cursor().is_empty());
    }
    let guard = h.server.peers[&1].window.state.guard(&before);
    let g = net::window::WindowGuard {
        id: guard.id,
        revision: guard.revision,
        inventory_revision: guard.inventory_revision,
    };
    assert_eq!(
        h.action(
            7,
            Action::Window(net::window::WindowIntent::Close { guard: g }),
            None
        )
        .status,
        ActionStatus::Accepted
    );
    assert!(h.server.peers[&1].window.state.closed());
}
#[test]
fn reopened_authored_chest_is_read_only_before_other_showcase_pages_exist() {
    let dir = Directory::new();
    let config = dir.config();
    let plan = owner(&config);
    let target = *plan
        .parts
        .iter()
        .find(|p| p.hall == Hall::Double && p.source)
        .unwrap();
    let missing = *plan.pages.iter().find(|page| page.y == 7).unwrap();
    let mut world = fixture(&plan);
    let mut chest = Chest::default();
    chest
        .set_trusted(0, ItemStack::new(I_RAW_IRON, 23).unwrap())
        .unwrap();
    let component_id =
        windows::fresh_component(&world, target.position, survival::CHEST_COMPONENT).unwrap();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.remove_page(missing)
        .set_cell(target.position, target.cell)
        .set_component(
            component_id,
            survival::chest_component(target.position, &chest),
            CHANNELS,
        );
    world.commit(tx).unwrap();
    // Persist an ordinary partial-world baseline, then use the actual Store
    // recovery path. No showcase initialization/tick has happened yet.
    drop(Server::open_with_world(config.clone(), world).unwrap());
    let mut recovered = Server::open(config).unwrap();
    assert!(recovered.world.page(missing).is_none());
    assert!(!recovered.spawn_showcase.as_ref().unwrap().ready);
    assert_eq!(component(&recovered.world, target.position).0, component_id);
    // Keep this genuinely partial startup state deterministic while the real
    // authentication/WAL/window path runs; unrelated worker jobs are omitted.
    recovered.generation = None;
    let mut h = Harness::new(recovered);
    h.send(ClientMessage::Hello(dir.hello()));
    h.until(|s| s.peers.get(&1).is_some_and(|p| p.ready) && s.pending.is_none());
    assert_eq!(
        h.server.identities[&h.server.peers[&1].identity].role,
        Role::Owner
    );
    assert_eq!(
        h.action(
            1,
            Action::Command(format!(
                "/tp {} {} {}",
                target.position.x as f64 + 2.5,
                target.position.y as f64,
                target.position.z as f64 + 0.5
            )),
            None,
        )
        .status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action(
            2,
            Action::Window(net::window::WindowIntent::OpenBlock {
                position: target.position,
                expected: target.cell,
            }),
            Some([90., 25.]),
        )
        .status,
        ActionStatus::Accepted
    );
    assert!(!h.server.spawn_showcase.as_ref().unwrap().ready);
    assert!(h.server.world.page(missing).is_none());
    assert!(h.out.iter().any(
        |o| matches!(&o.message,ServerMessage::Window(w) if w.binding==Some(target.position) && w.read_only)
    ));
    let peer = &h.server.peers[&1];
    let before = peer.inventory;
    let guard = peer.window.state.guard(&before);
    let guard = net::window::WindowGuard {
        id: guard.id,
        revision: guard.revision,
        inventory_revision: guard.inventory_revision,
    };
    assert_eq!(
        h.action(
            3,
            Action::Window(net::window::WindowIntent::Click {
                guard,
                slot: 41,
                right: false,
                shift: true,
            }),
            None,
        )
        .status,
        ActionStatus::Rejected(RejectReason::Protected)
    );
    assert_eq!(h.server.peers[&1].inventory, before);
    assert!(h.server.peers[&1].window.state.cursor().is_empty());
    assert_eq!(
        Chest::decode(&component(&h.server.world, target.position).1.payload).unwrap(),
        chest
    );
    // A read-only policy change itself must invalidate the window cache even
    // when the item revision and all page/component versions stay unchanged.
    h.server.publish_windows();
    assert!(h.server.publish_windows().is_empty());
    let site = h.server.spawn_showcase.take();
    assert!(h.server.publish_windows().iter().any(
        |o| matches!(&o.message,ServerMessage::Window(w) if w.binding==Some(target.position) && !w.read_only)
    ));
    h.server.spawn_showcase = site;
    assert!(h.server.publish_windows().iter().any(
        |o| matches!(&o.message,ServerMessage::Window(w) if w.binding==Some(target.position) && w.read_only)
    ));
    h.durable();
    drop(h);
    let reopened = Server::open(dir.config()).unwrap();
    assert_eq!(
        Chest::decode(&component(&reopened.world, target.position).1.payload).unwrap(),
        chest,
        "rejected extraction cannot enter the recovered Store"
    );
}
#[test]
fn custom_spawn_and_unprotected_fixtures_never_install_infinite_display() {
    let dir = Directory::new();
    let mut c = dir.config();
    let center = [0, 64, 0];
    c.spawn_override = Some([0., 65., 0.]);
    assert!(Owner::new(&c, center).is_none());
    c.spawn_override = None;
    c.spawn_protection_radius = 31;
    assert!(Owner::new(&c, center).is_none());
    c.spawn_protection_radius = 32;
    c.generation_enabled = false;
    assert!(Owner::new(&c, center).is_none());
    let mut h = Harness::new(Server::open(c).unwrap());
    assert!(h.server.showcase_center().is_none());
    h.send(ClientMessage::Hello(dir.hello()));
    h.until(|s| s.peers.get(&1).is_some_and(|p| p.ready) && s.pending.is_none());
    assert!(
        h.out
            .iter()
            .any(|o| matches!(o.message, ServerMessage::Welcome(_)))
    );
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(o.message, ServerMessage::SpawnLandmark(_)))
    );
}
