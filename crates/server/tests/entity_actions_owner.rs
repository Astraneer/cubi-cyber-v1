//! Actual owned actions, guarded tools, public rejection and same-WAL entity loot.
use core_types::{Cell, SpaceId, Tick};
use net::{Action, ActionId, ActionStatus, ClientMessage, InventoryGuard, ServerMessage};
use rules::{
    entities::{EntityId, MobKind, mob::Mob},
    survival::{Inventory, health::DamageKind},
};
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
                "entity-action-owner-{}-{}",
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
    fn owner(&self) -> ClientMessage {
        net::decode_client(&std::fs::read(self.0.join("owner.identity")).unwrap()).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn world() -> World {
    let mut world = World::new(rules::properties, 4096);
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    cells[4 * 256..5 * 256].fill(Cell::from_parts(rules::ids::STONE, 0));
    for z in 0..16 {
        for x in 0..16 {
            if x == 0 || x == 15 || z == 0 || z == 15 {
                for y in 1..4 {
                    cells[x + 16 * z + 256 * y] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
        }
    }
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        core_types::PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        cells,
    );
    world.commit(tx).unwrap();
    world
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
    fn send(&mut self, message: ClientMessage) {
        self.send_as(1, message);
    }
    fn send_as(&mut self, connection: u64, message: ClientMessage) {
        self.out
            .extend(self.server.handle(connection, message, self.now));
    }
    fn until(&mut self, mut f: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(i) = self.out.iter().position(&mut f) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < end, "{:?}", self.server.counters());
            self.step();
        }
    }
    fn action(&mut self, id: u64, action: Action) -> net::ActionReply {
        self.action_as(1, id, action)
    }
    fn action_as(&mut self, connection: u64, id: u64, action: Action) -> net::ActionReply {
        // Cross the real outer binary contract in both directions.
        self.send_as(
            connection,
            net::decode_client(
                &net::encode_client(&ClientMessage::Action {
                    id: ActionId(id),
                    action,
                })
                .unwrap(),
            )
            .unwrap(),
        );
        let response=self.until(|o|o.connection==connection && matches!(&o.message,ServerMessage::Action(r)if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        let response = net::decode_server(&net::encode_server(&response.message).unwrap()).unwrap();
        let ServerMessage::Action(reply) = response else {
            unreachable!()
        };
        reply
    }
    fn inventory(&self) -> Inventory {
        self.out
            .iter()
            .rev()
            .filter(|o| o.connection == 1)
            .find_map(|o| match &o.message {
                ServerMessage::Inventory { data } => Some(Inventory::decode(data).unwrap()),
                _ => None,
            })
            .unwrap()
    }
    fn guard(&self) -> InventoryGuard {
        let inventory = self.inventory();
        let h = inventory.hand();
        InventoryGuard {
            revision: inventory.revision(),
            selected: inventory.selected() as u8,
            item: h.item(),
            count: h.count(),
            damage: h.damage(),
            payload: h.payload().map_or(0, |id| id.0),
        }
    }
    fn settle(&mut self) {
        for _ in 0..30 {
            self.step();
        }
        while self.server.awaiting_durability() {
            self.out.extend(self.server.tick(self.now));
            thread::sleep(Duration::from_micros(300));
        }
    }
    fn count_items(&self) -> u64 {
        self.server
            .item_entities()
            .rows()
            .iter()
            .map(|i| u64::from(i.stack.count()))
            .sum()
    }
}
fn start(d: &Directory, mut mob: Mob) -> H {
    mob.last_tick = Tick(0);
    let server = Server::open_with_world(d.config(), world()).unwrap();
    let mut h = H {
        server,
        now: 0,
        out: Vec::new(),
    };
    // Observe the actual action WAL after the one-time migration checkpoint;
    // compaction is allowed to remove earlier transient action/event records.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !h.server.height_ready_for_publication() {
        assert!(Instant::now() < deadline);
        h.step();
    }
    assert!(h.server.queue_mob_spawn(mob).is_ok());
    h.send(d.owner());
    h.until(|o| matches!(&o.message, ServerMessage::Welcome(_)));
    while h.server.mob_entities().is_empty() {
        h.step();
    }
    h
}
#[test]
fn actual_owned_shearing_is_once_stale_guard_safe_and_reopens_exact_tool_and_wool() {
    let d = Directory::new();
    let mut h = start(
        &d,
        Mob::new(EntityId(90), MobKind::Sheep, [8.5, 1., 11.5]).unwrap(),
    );
    assert_eq!(
        h.action(1, Action::Command("/give 294 1".into())).status,
        ActionStatus::Accepted
    );
    let guard = h.guard();
    let use_entity = Action::UseEntity {
        target: 90,
        inventory: guard,
    };
    assert_eq!(
        h.action(2, use_entity.clone()).status,
        ActionStatus::Accepted
    );
    h.settle();
    let count = h.count_items();
    assert!((1..=3).contains(&count));
    assert!(h.server.mob_entities().get(EntityId(90)).unwrap().sheared);
    assert_eq!(h.inventory().hand().damage(), 1);
    assert_eq!(
        h.action(2, use_entity.clone()).status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action(3, use_entity).status,
        ActionStatus::Rejected(net::RejectReason::Stale)
    );
    h.settle();
    assert_eq!(h.count_items(), count);
    assert_eq!(h.inventory().hand().damage(), 1);
    let second = Mob::new(EntityId(93), MobKind::Sheep, [9.5, 1., 11.5]).unwrap();
    assert!(h.server.queue_mob_spawn(second).is_ok());
    while h.server.mob_entities().get(EntityId(93)).is_none() {
        h.step();
    }
    let second_guard = h.guard();
    assert_eq!(
        h.action(
            4,
            Action::UseEntity {
                target: 93,
                inventory: second_guard
            }
        )
        .status,
        ActionStatus::Accepted
    );
    h.settle();
    let count2 = h.count_items();
    assert!((count + 1..=count + 3).contains(&count2));
    assert_eq!(h.inventory().hand().damage(), 2);
    let now = h.now;
    drop(h);
    let mut h = H {
        server: Server::open(d.config()).unwrap(),
        now,
        out: Vec::new(),
    };
    h.send(d.owner());
    h.until(|o| matches!(&o.message, ServerMessage::Welcome(_)));
    assert_eq!(h.inventory().hand().damage(), 2);
    assert_eq!(h.count_items(), count2);
    assert!(h.server.mob_entities().get(EntityId(90)).unwrap().sheared);
}
#[test]
fn accepted_lethal_attack_stages_owned_sword_and_mob_loot_in_one_durable_batch() {
    let d = Directory::new();
    let mut mob = Mob::new(EntityId(91), MobKind::Pig, [8.5, 1., 11.5]).unwrap();
    mob.life
        .damage(DamageKind::Generic, 9., (0, 0.), false)
        .unwrap();
    for _ in 0..10 {
        mob.life.begin_tick();
        mob.life.base_tick_end();
    }
    let mut h = start(&d, mob);
    assert_eq!(
        h.action(1, Action::Command("/give 285 1".into())).status,
        ActionStatus::Accepted
    );
    let attack = Action::AttackEntity {
        target: 91,
        inventory: h.guard(),
    };
    assert_eq!(h.action(2, attack.clone()).status, ActionStatus::Accepted);
    assert_eq!(h.inventory().hand().damage(), 1);
    let count = h.count_items();
    assert!(count > 0);
    assert_eq!(h.action(2, attack).status, ActionStatus::Accepted);
    assert_eq!(h.count_items(), count);
    h.settle();
    let count = h.count_items();
    drop(h);
    let (store, recovery) = save::Store::open(&d.0).unwrap();
    assert!(recovery.batches.iter().any(|(_, batch)| {
        batch
            .actions
            .iter()
            .any(|(_, r)| r.id == ActionId(2) && r.status == ActionStatus::Accepted)
            && batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_MOBS)
            && batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_ITEMS)
            && batch
                .players
                .iter()
                .any(|p| Inventory::decode(&p.inventory).is_ok_and(|i| i.hand().damage() == 1))
    }));
    drop(store);
    let reopened = Server::open(d.config()).unwrap();
    assert_eq!(
        reopened
            .item_entities()
            .rows()
            .iter()
            .map(|i| u64::from(i.stack.count()))
            .sum::<u64>(),
        count
    );
}

#[test]
fn death_waits_twenty_ticks_then_respawn_is_owned_idempotent_and_durable() {
    let d = Directory::new();
    let zombie = Mob::spawned(
        EntityId(92),
        MobKind::Zombie,
        SpaceId(0),
        [8.5, 1., 11.5],
        19,
    )
    .unwrap();
    let mut h = start(&d, zombie);
    assert_eq!(
        h.action(1, Action::Command("/give 260 3".into())).status,
        ActionStatus::Accepted
    );
    h.until(|o| matches!(&o.message,ServerMessage::Prediction{player,..}if player.health==0.));
    let died = h.server.current_tick();
    assert_eq!(
        h.action(2, Action::Respawn).status,
        ActionStatus::Rejected(net::RejectReason::Unauthorized)
    );
    assert_eq!(
        h.action(3, Action::Flight(true)).status,
        ActionStatus::Rejected(net::RejectReason::Unauthorized)
    );
    while h.server.current_tick().0 < died.0 + 22 {
        h.step();
    }
    assert_eq!(h.action(4, Action::Respawn).status, ActionStatus::Accepted);
    assert_eq!(h.action(4, Action::Respawn).status, ActionStatus::Accepted);
    assert!(h.inventory().slots().iter().all(|s| s.is_empty()));
    let state = h
        .out
        .iter()
        .rev()
        .find_map(|o| match o.message {
            ServerMessage::Prediction { player, .. } => Some(player),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.health, 20.);
    assert_eq!(state.position, [8.5, 1., 8.5]);
    assert!(!state.noclip);
    let now = h.now;
    drop(h);
    let mut h = H {
        server: Server::open(d.config()).unwrap(),
        now,
        out: Vec::new(),
    };
    h.send(d.owner());
    h.until(|o| matches!(&o.message, ServerMessage::Welcome(_)));
    assert!(h.inventory().slots().iter().all(|s| s.is_empty()));
    let state = h
        .out
        .iter()
        .rev()
        .find_map(|o| match o.message {
            ServerMessage::Prediction { player, .. } => Some(player),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.health, 20.);
}
#[test]
fn actual_fall_death_clears_owned_inventory_and_creates_rows_in_one_wal_without_mob_phase() {
    let d = Directory::new();
    let mut world = world();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(
        core_types::PageId {
            space: SpaceId(0),
            x: 0,
            y: 1,
            z: 0,
        },
        vec![Cell::AIR; 4096],
    );
    let mut high = vec![Cell::AIR; 4096];
    high[7 * 256..8 * 256].fill(Cell::from_parts(rules::ids::STONE, 0));
    tx.create_page(
        core_types::PageId {
            space: SpaceId(0),
            x: 0,
            y: 2,
            z: 0,
        },
        high,
    );
    world.commit(tx).unwrap();
    let mut config = d.config();
    config.spawn_override = Some([8.5, 40., 8.5]);
    let mut h = H {
        server: Server::open_with_world(config, world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.send(d.owner());
    h.until(|o| matches!(&o.message, ServerMessage::Welcome(_)));
    assert_eq!(
        h.action(1, Action::Command("/give 260 3".into())).status,
        ActionStatus::Accepted
    );
    while h.server.current_tick().0 < 75 {
        h.step();
    }
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    for z in 0..16 {
        for x in 0..16 {
            tx.set_cell(
                core_types::BlockPos {
                    space: SpaceId(0),
                    x,
                    y: 39,
                    z,
                },
                Cell::AIR,
            );
        }
    }
    assert!(h.server.queue_transaction(tx).is_ok());
    h.until(|o| matches!(&o.message,ServerMessage::Prediction{player,..}if player.health==0.));
    assert!(h.inventory().slots().iter().all(|s| s.is_empty()));
    assert_eq!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .filter(|i| i.stack.item() == 260)
            .map(|i| i.stack.count())
            .sum::<u16>(),
        3
    );
    drop(h);
    let (_store, recovery) = save::Store::open(&d.0).unwrap();
    assert!(
        recovery
            .batches
            .iter()
            .any(|(_, b)| b.players.iter().any(|p| p.state.health == 0.
                && Inventory::decode(&p.inventory)
                    .is_ok_and(|i| i.slots().iter().all(|s| s.is_empty())))
                && b.runtimes.iter().any(|r| r.kind == save::RUNTIME_ITEMS)
                && !b.runtimes.iter().any(|r| r.kind == save::RUNTIME_MOBS))
    );
}

#[test]
fn actual_two_player_strikes_drop_and_respawn_owned_items_without_duplicate_player_records() {
    let d = Directory::new();
    let mut h = H {
        server: Server::open_with_world(d.config(), world()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.send(d.owner());
    h.until(|o| o.connection == 1 && matches!(&o.message, ServerMessage::Welcome(_)));
    h.send_as(
        2,
        ClientMessage::Hello(net::Hello {
            identity: net::PlayerId([0; 16]),
            name: "Victim".into(),
            credential: None,
        }),
    );
    let joined = h.until(|o| o.connection == 2 && matches!(&o.message, ServerMessage::Welcome(_)));
    let ServerMessage::Welcome(victim) = joined.message else {
        unreachable!()
    };
    assert_eq!(
        h.action(1, Action::Command("/give 285 1".into())).status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action(2, Action::Command("/op Victim".into())).status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action_as(2, 1, Action::Command("/give 260 3".into()))
            .status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action(3, Action::Command("/deop Victim".into())).status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action_as(2, 2, Action::Command("/give 260 1".into()))
            .status,
        ActionStatus::Rejected(net::RejectReason::Unauthorized)
    );
    while h.server.current_tick().0 < 80 {
        h.step();
    }
    let target = net::player_entity_id(victim.identity);
    for id in 4..=7 {
        let attack = Action::AttackEntity {
            target,
            inventory: h.guard(),
        };
        assert_eq!(h.action(id, attack.clone()).status, ActionStatus::Accepted);
        if id == 7 {
            assert_eq!(h.action(id, attack).status, ActionStatus::Accepted);
        }
        let after = h.server.current_tick().0 + 14;
        while h.server.current_tick().0 < after {
            h.step();
        }
    }
    let health = h
        .out
        .iter()
        .rev()
        .filter(|o| o.connection == 2)
        .find_map(|o| match o.message {
            ServerMessage::Prediction { player, .. } => Some(player.health),
            _ => None,
        })
        .unwrap();
    assert_eq!(health, 0.);
    assert_eq!(h.inventory().hand().damage(), 4);
    assert_eq!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .filter(|i| i.stack.item() == 260)
            .map(|i| i.stack.count())
            .sum::<u16>(),
        3
    );
    let until = h.server.current_tick().0 + 10;
    while h.server.current_tick().0 < until {
        h.step();
    }
    assert_eq!(
        h.action_as(2, 3, Action::Respawn).status,
        ActionStatus::Accepted
    );
    let health = h
        .out
        .iter()
        .rev()
        .filter(|o| o.connection == 2)
        .find_map(|o| match o.message {
            ServerMessage::Prediction { player, .. } => Some(player.health),
            _ => None,
        })
        .unwrap();
    assert_eq!(health, 20.);
    h.settle();
    drop(h);
    let (_store, recovery) = save::Store::open(&d.0).unwrap();
    assert!(recovery.batches.iter().any(|(_, b)| {
        b.players
            .iter()
            .any(|p| p.state.identity == victim.identity && p.state.health == 0.)
            && b.runtimes.iter().any(|r| r.kind == save::RUNTIME_ITEMS)
    }));
    for (_, batch) in &recovery.batches {
        let mut ids = std::collections::BTreeSet::new();
        assert!(batch.players.iter().all(|p| ids.insert(p.state.identity)));
    }
}

#[test]
fn mob_actions_validate_reach_known_line_of_sight_and_owned_guard_before_consuming() {
    let d = Directory::new();
    let mut h = start(
        &d,
        Mob::new(EntityId(94), MobKind::Sheep, [8.5, 1., 11.5]).unwrap(),
    );
    assert_eq!(
        h.action(1, Action::Command("/give 294 1".into())).status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action(2, Action::Command("/tp 8.5 1 1.5".into())).status,
        ActionStatus::Accepted
    );
    let guard = h.guard();
    assert_eq!(
        h.action(
            3,
            Action::UseEntity {
                target: 94,
                inventory: guard
            }
        )
        .status,
        ActionStatus::Rejected(net::RejectReason::OutOfReach)
    );
    assert_eq!(
        h.action(4, Action::Command("/tp 8.5 1 8.5".into())).status,
        ActionStatus::Accepted
    );
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    for x in 1..15 {
        for y in 1..4 {
            tx.set_cell(
                core_types::BlockPos {
                    space: SpaceId(0),
                    x,
                    y,
                    z: 10,
                },
                Cell::from_parts(rules::ids::STONE, 0),
            );
        }
    }
    assert!(h.server.queue_transaction(tx).is_ok());
    h.settle();
    assert_eq!(
        h.action(
            5,
            Action::UseEntity {
                target: 94,
                inventory: guard
            }
        )
        .status,
        ActionStatus::Rejected(net::RejectReason::OutOfReach)
    );
    assert_eq!(
        h.action(
            6,
            Action::UseEntity {
                target: 94,
                inventory: InventoryGuard { damage: 1, ..guard }
            }
        )
        .status,
        ActionStatus::Rejected(net::RejectReason::Stale)
    );
    let page = core_types::PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let mut tx = Transaction::new(h.server.current_tick(), SpaceId(0));
    tx.remove_page(page);
    assert!(h.server.queue_transaction(tx).is_ok());
    h.settle();
    assert_eq!(
        h.action(
            7,
            Action::UseEntity {
                target: 94,
                inventory: guard
            }
        )
        .status,
        ActionStatus::Rejected(net::RejectReason::Unknown)
    );
    assert_eq!(h.inventory().hand().damage(), 0);
    assert_eq!(h.count_items(), 0);
    assert!(!h.server.mob_entities().get(EntityId(94)).unwrap().sheared);
}
#[test]
fn spawn_pvp_grace_blocks_damage_after_the_initial_health_shield_expires() {
    let d = Directory::new();
    let mut config = d.config();
    config.spawn_protection_radius = 16;
    let mut h = H {
        server: Server::open_with_world(config, world()).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    h.send(d.owner());
    h.until(|o| o.connection == 1 && matches!(&o.message, ServerMessage::Welcome(_)));
    h.send_as(
        2,
        ClientMessage::Hello(net::Hello {
            identity: net::PlayerId([0; 16]),
            name: "Protected".into(),
            credential: None,
        }),
    );
    let joined = h.until(|o| o.connection == 2 && matches!(&o.message, ServerMessage::Welcome(_)));
    let ServerMessage::Welcome(victim) = joined.message else {
        unreachable!()
    };
    assert_eq!(
        h.action(1, Action::Command("/give 285 1".into())).status,
        ActionStatus::Accepted
    );
    while h.server.current_tick().0 < 100 {
        h.step();
    }
    let target = net::player_entity_id(victim.identity);
    let guard = h.guard();
    assert_eq!(
        h.action(
            2,
            Action::AttackEntity {
                target,
                inventory: guard
            }
        )
        .status,
        ActionStatus::Accepted
    );
    h.settle();
    assert_eq!(h.inventory().hand().damage(), 0);
    let health = h
        .out
        .iter()
        .rev()
        .filter(|o| o.connection == 2)
        .find_map(|o| match o.message {
            ServerMessage::Prediction { player, .. } => Some(player.health),
            _ => None,
        })
        .unwrap();
    assert_eq!(health, 20.);
}
