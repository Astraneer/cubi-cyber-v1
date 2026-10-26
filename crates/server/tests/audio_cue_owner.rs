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
        self.send_as(1, m);
    }
    fn send_as(&mut self, connection: u64, m: ClientMessage) {
        self.out.extend(self.server.handle(connection, m, self.now));
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
    fn action(&mut self, id: u64, action: net::Action) -> net::ActionStatus {
        let message = ClientMessage::Action {
            id: net::ActionId(id),
            action,
        };
        self.send(net::decode_client(&net::encode_client(&message).unwrap()).unwrap());
        let output = self.until(|o| o.connection == 1 && matches!(o.message,
            ServerMessage::Action(r) if r.id == net::ActionId(id) && r.status != net::ActionStatus::Pending));
        let ServerMessage::Action(reply) = output.message else {
            unreachable!()
        };
        reply.status
    }
    fn guard(&self) -> net::InventoryGuard {
        let inventory = self
            .out
            .iter()
            .rev()
            .find_map(|o| match &o.message {
                ServerMessage::Inventory { data } if o.connection == 1 => {
                    Some(rules::survival::Inventory::decode(data).unwrap())
                }
                _ => None,
            })
            .unwrap();
        let stack = inventory.hand();
        net::InventoryGuard {
            revision: inventory.revision(),
            selected: inventory.selected() as u8,
            item: stack.item(),
            count: stack.count(),
            damage: stack.damage(),
            payload: stack.payload().map_or(0, |id| id.0),
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
    let drop_plan = server::survival::plan_drops(
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
    drop_plan.append_to(&mut tx);
    assert!(h.server.queue_transaction(tx).is_ok());
    let end = h.server.current_tick().0 + 30;
    while h.server.current_tick().0 < end {
        h.step();
    }
    let cues: Vec<_> = h
        .out
        .iter()
        .filter_map(|o| match &o.message {
            ServerMessage::AudioCue(cue) => Some(cue),
            _ => None,
        })
        .collect();
    assert!(
        cues.iter().all(|cue| cue.kind == Kind::ItemPickup),
        "recovery or a nearby ordinary egg never infers lay_egg; only actual pickup may sound: {cues:?}"
    );
    assert_eq!(h.server.counters().audio_cues_sent, cues.len() as u64);
}

#[test]
fn owned_chest_damage_and_pickup_cues_share_durable_gameplay_and_never_broadcast_or_replay() {
    use core_types::{BlockPos, ChannelMask, ComponentId};
    use net::{Action, ActionStatus, Hello, PlayerId, window::WindowIntent};
    use rules::{entities::EntityId, ids::*, survival::furnace::Chest};
    let d = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("owned-cue-owner-{}", std::process::id())),
    );
    std::fs::create_dir_all(&d.0).unwrap();
    let anchor = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 2,
        z: 11,
    };
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(STONE, 0));
    cells[4 * 256..5 * 256].fill(Cell::from_parts(STONE, 0));
    cells[usize::from(anchor.local_index().get())] = Cell::from_parts(CHEST, 0);
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(anchor.page(), cells);
    tx.set_component(
        ComponentId(777),
        server::survival::chest_component(anchor, &Chest::default()),
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let mut cfg = config(&d);
    cfg.spawn_protection_radius = 0;
    let mut h = H {
        server: Server::open_with_world(cfg.clone(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !h.server.height_ready_for_publication() {
        assert!(Instant::now() < deadline);
        h.step();
    }
    let hello = owner(&d);
    h.send(hello.clone());
    let welcome = h.until(|o| o.connection == 1 && matches!(o.message, ServerMessage::Welcome(_)));
    let ServerMessage::Welcome(welcome) = welcome.message else {
        unreachable!()
    };
    h.send_as(
        2,
        ClientMessage::Hello(Hello {
            identity: PlayerId::default(),
            name: "near-listener".into(),
            credential: None,
        }),
    );
    h.until(|o| o.connection == 2 && matches!(o.message, ServerMessage::Welcome(_)));
    let open = Action::Window(WindowIntent::OpenBlock {
        position: anchor,
        expected: Cell::from_parts(CHEST, 0),
    });
    assert_eq!(h.action(1, open.clone()), ActionStatus::Accepted);
    let window = h.until(|o| {
        o.connection == 1 && matches!(&o.message, ServerMessage::Window(w) if !w.closed)
    });
    let ServerMessage::Window(window) = window.message else {
        unreachable!()
    };
    assert_eq!(
        h.action(1, open),
        ActionStatus::Accepted,
        "retry retains original acceptance"
    );
    assert_eq!(
        h.action(
            2,
            Action::Window(WindowIntent::Close {
                guard: net::window::WindowGuard {
                    revision: u64::MAX,
                    ..window.guard
                }
            })
        ),
        ActionStatus::Rejected(net::RejectReason::Stale)
    );
    assert_eq!(
        h.action(
            3,
            Action::Window(WindowIntent::Close {
                guard: window.guard
            })
        ),
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action(
            4,
            Action::Command(format!("/give {} 1", rules::items::I_DIAMOND))
        ),
        ActionStatus::Accepted
    );
    let pig = Mob::new(EntityId(992), MobKind::Pig, [10.5, 1., 8.5]).unwrap();
    h.server.queue_mob_spawn(pig).unwrap();
    while h.server.mob_entities().get(pig.id).is_none() {
        h.step();
    }
    let end = h.server.current_tick().0 + 12;
    while h.server.current_tick().0 < end {
        h.step();
    }
    let attack = Action::AttackEntity {
        target: pig.id.0,
        inventory: h.guard(),
    };
    assert_eq!(h.action(5, attack.clone()), ActionStatus::Accepted);
    let damaged = h.server.mob_entities().get(pig.id).unwrap().life.hp();
    assert!(damaged < pig.life.hp());
    assert_eq!(h.action(5, attack), ActionStatus::Accepted);
    assert_eq!(
        h.action(
            6,
            Action::AttackEntity {
                target: pig.id.0,
                inventory: h.guard()
            }
        ),
        ActionStatus::Accepted
    );
    assert_eq!(
        h.server.mob_entities().get(pig.id).unwrap().life.hp(),
        damaged,
        "iframe consumes no further damage"
    );
    assert_eq!(
        h.action(
            7,
            Action::AttackEntity {
                target: u64::MAX,
                inventory: h.guard()
            }
        ),
        ActionStatus::Rejected(net::RejectReason::Unknown)
    );
    assert_eq!(
        h.action(8, Action::Command("/tp 4.5 1 8.5".into())),
        ActionStatus::Accepted
    );
    let drop_plan = server::survival::plan_drops(
        h.server.world(),
        BlockPos {
            x: 4,
            y: 1,
            z: 8,
            space: SpaceId(0),
        },
        [5; 16],
        234,
        h.server.current_tick(),
        &[ItemStack::new(COBBLE, 3).unwrap()],
        0,
        None,
    )
    .unwrap();
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    drop_plan.append_to(&mut tx);
    h.server
        .queue_transaction(tx)
        .unwrap_or_else(|_| panic!("drop admission"));
    let pickup_output = h.until(|o| {
        o.connection == 1
            && matches!(&o.message, ServerMessage::AudioCue(c) if c.kind == Kind::ItemPickup)
    });
    let ServerMessage::AudioCue(pickup_wire) =
        net::decode_server(&net::encode_server(&pickup_output.message).unwrap()).unwrap()
    else {
        unreachable!()
    };
    let mut cues: Vec<_> = h
        .out
        .iter()
        .filter_map(|o| match o.message {
            ServerMessage::AudioCue(c) => Some((o.connection, c)),
            _ => None,
        })
        .collect();
    assert_eq!(
        cues.iter().map(|(_, c)| c.kind).collect::<Vec<_>>(),
        [Kind::ChestOpen, Kind::ChestClose, Kind::AttackStrong]
    );
    assert!(cues.iter().all(|(connection, _)| *connection == 1));
    assert_eq!(h.server.counters().audio_cues_sent, 4);
    let now = h.now;
    drop(h);
    let (store, recovery) = save::Store::open(&d.0).unwrap();
    let mut pickup = None;
    for (_, batch) in &recovery.batches {
        for event in batch
            .changes
            .iter()
            .flat_map(|c| &c.events)
            .filter(|e| e.kind == 0x8502)
        {
            assert_eq!(
                &event.payload[..16],
                &welcome.identity.0,
                "recipient is private WAL data"
            );
            let cue = Cue::decode(&event.payload[16..]).unwrap();
            if cue.kind == Kind::ItemPickup {
                assert!(batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_ITEMS));
                assert!(batch.players.iter().any(|p| {
                    p.state.identity == welcome.identity
                        && rules::survival::Inventory::decode(&p.inventory)
                            .unwrap()
                            .slots()
                            .iter()
                            .any(|s| s.item() == COBBLE && s.count() == 3)
                }));
                pickup = Some(cue);
            } else {
                assert!(
                    batch
                        .actions
                        .iter()
                        .any(|(identity, r)| *identity == welcome.identity
                            && r.id.0 == cue.sequence
                            && r.status == ActionStatus::Accepted)
                );
            }
        }
    }
    assert_eq!(
        pickup,
        Some(pickup_wire),
        "wire cue matches the durable pickup exactly"
    );
    cues.push((1, pickup_wire));
    assert_eq!(cues.len(), 4);
    drop(store);
    let mut h = H {
        server: Server::open(cfg).unwrap(),
        now,
        out: Vec::new(),
    };
    h.send(hello);
    h.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    for _ in 0..30 {
        h.step();
    }
    assert!(
        !h.out
            .iter()
            .any(|o| matches!(o.message, ServerMessage::AudioCue(_))),
        "transient historical cues never replay after reopen"
    );
}
