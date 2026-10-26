use super::*;
use rules::{
    items::*,
    stack::ItemStack,
    survival::{Inventory, health::DamageKind},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Owner(Option<Server>);
impl std::ops::Deref for Owner {
    type Target = Server;
    fn deref(&self) -> &Server {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for Owner {
    fn deref_mut(&mut self) -> &mut Server {
        self.0.as_mut().unwrap()
    }
}
struct H {
    server: Owner,
    path: PathBuf,
    now: u64,
    out: Vec<Outgoing>,
    owner: net::ClientMessage,
}
impl H {
    fn new() -> Self {
        Self::variant(0)
    }
    fn variant(variant: u8) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "consumable-owner-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        let mut world = World::new(rules::properties, 4096);
        let mut cells = vec![Cell::AIR; 4096];
        cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
        cells[6 + 16 * 11 + 256] = Cell::from_parts(rules::ids::WATER, 0);
        for x in 9..=10 {
            cells[x + 16 * 11 + 256] = Cell::from_parts(rules::ids::STONE, 0);
        }
        cells[10 + 16 * 11 + 512] = Cell::from_parts(rules::ids::LAVA, 0);
        if variant == 3 {
            cells[10 + 16 * 11 + 512] = Cell::AIR;
            cells[9 + 16 * 11 + 512] = Cell::from_parts(rules::ids::TORCH, 0);
        }
        if variant == 1 {
            cells[7 + 16 * 9 + 512] = Cell::from_parts(rules::ids::STONE, 0);
        }
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        if variant != 2 {
            tx.create_page(
                PageId {
                    space: SpaceId(0),
                    x: 0,
                    y: 0,
                    z: 0,
                },
                cells,
            );
            world.commit(tx).unwrap();
        }
        let mut config = ServerConfig::new(&path);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 1., 8.5]);
        config.spawn_protection_radius = 0;
        let server = Server::open_with_world(config, world).unwrap();
        let owner =
            net::decode_client(&std::fs::read(path.join("owner.identity")).unwrap()).unwrap();
        let mut h = Self {
            server: Owner(Some(server)),
            path,
            now: 0,
            out: Vec::new(),
            owner,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !h.server.height_ready_for_publication() {
            assert!(Instant::now() < deadline);
            h.tick();
        }
        h.send(h.owner.clone());
        h.wait(|o| matches!(o.message, ServerMessage::Welcome(_)));
        h
    }
    fn peer(&self) -> &Peer {
        self.server.peers.get(&1).unwrap()
    }
    fn inventory(&self) -> Inventory {
        self.peer().inventory
    }
    fn guard(&self) -> net::InventoryGuard {
        let i = self.inventory();
        let h = i.hand();
        net::InventoryGuard {
            revision: i.revision(),
            selected: i.selected() as u8,
            item: h.item(),
            count: h.count(),
            damage: h.damage(),
            payload: h.payload().map_or(0, |p| p.0),
        }
    }
    fn seed_hand(&mut self, item: u16, count: u16) {
        let p = self.server.peers.get_mut(&1).unwrap();
        p.inventory = Inventory::empty();
        p.inventory
            .set_trusted(0, ItemStack::new(item, count).unwrap())
            .unwrap();
    }
    fn hurt(&mut self) {
        let p = self.server.peers.get_mut(&1).unwrap();
        p.health.set_spawn_protection(0);
        p.health
            .damage(DamageKind::Generic, 10., (0, 0.), false)
            .unwrap();
        p.player.health = p.health.hp();
    }
    fn tick(&mut self) {
        let want = self.server.current_tick().0 + 1;
        self.now = self.now.max(self.server.next_tick_at);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.out.extend(self.server.tick(self.now));
            assert!(
                !self.server.is_failed(),
                "{:?}",
                self.server.failure_reason()
            );
            if self.server.current_tick().0 >= want && !self.server.awaiting_durability() {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_micros(250));
        }
    }
    fn send(&mut self, message: ClientMessage) {
        let wire = net::encode_client(&message).unwrap();
        let message = net::decode_client(&wire).unwrap();
        self.out.extend(self.server.handle(1, message, self.now));
    }
    fn wait(&mut self, mut f: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(i) = self.out.iter().position(&mut f) {
                let out = self.out.remove(i);
                let encoded = net::encode_server(&out.message).unwrap();
                assert_eq!(net::decode_server(&encoded).unwrap(), out.message);
                return out;
            }
            assert!(
                Instant::now() < deadline,
                "tick {:?}",
                self.server.current_tick()
            );
            self.tick();
        }
    }
    fn action(&mut self, id: u64, action: Action) -> ActionReply {
        self.send(ClientMessage::Action {
            id: ActionId(id),
            action,
        });
        let out=self.wait(|o|matches!(o.message,ServerMessage::Action(r)if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        let ServerMessage::Action(r) = out.message else {
            unreachable!()
        };
        r
    }
    fn aimed(&mut self, id: u64, action: Action, point: [f64; 3]) -> ActionReply {
        let eye = self.peer().player.eye(1.);
        let d = std::array::from_fn::<_, 3, _>(|i| point[i] - eye[i]);
        let yaw = (-d[0].atan2(d[2]).to_degrees()) as f32;
        let pitch = (-d[1].atan2((d[0] * d[0] + d[2] * d[2]).sqrt()).to_degrees()) as f32;
        self.send(ClientMessage::AimedAction {
            id: ActionId(id),
            action,
            yaw,
            pitch,
        });
        let out=self.wait(|o|matches!(o.message,ServerMessage::Action(r)if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        let ServerMessage::Action(r) = out.message else {
            unreachable!()
        };
        r
    }
    fn reopen_inspect(&mut self, inspect: impl FnOnce(&save::Recovery)) {
        let mut config = ServerConfig::new(&self.path);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 1., 8.5]);
        config.spawn_protection_radius = 0;
        self.server.disconnect(1);
        drop(self.server.0.take());
        let (store, recovery) = save::Store::open(&self.path).unwrap();
        inspect(&recovery);
        drop(store);
        self.server.0 = Some(Server::open(config).unwrap());
        self.now = 0;
        self.out.clear();
        self.send(self.owner.clone());
        self.wait(|o| matches!(o.message, ServerMessage::Welcome(_)));
    }
}
impl Drop for H {
    fn drop(&mut self) {
        drop(self.server.0.take());
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
fn point(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn has_cue(batch: &save::Batch, kind: net::audio_cue::Kind) -> bool {
    batch.changes.iter().flat_map(|c| &c.events).any(|e| {
        e.payload.len() == 80
            && net::audio_cue::Cue::decode(&e.payload[16..]).is_ok_and(|c| c.kind == kind)
    })
}
#[test]
fn food_real_actions_hold32_cancel_switch_and_reopen_once() {
    let mut h = H::new();
    h.seed_hand(I_APPLE, 2);
    h.hurt();
    let start = Action::UseFood {
        held: true,
        inventory: h.guard(),
    };
    assert_eq!(h.action(1, start.clone()).status, ActionStatus::Accepted);
    let began = h.server.current_tick();
    assert_eq!(h.peer().health.hp(), 10.);
    assert_eq!(h.inventory().hand().count(), 2);
    for _ in 0..30 {
        h.tick();
        assert_eq!(h.peer().health.hp(), 10.);
        assert_eq!(h.inventory().hand().count(), 2);
    }
    h.tick();
    assert_eq!(h.server.current_tick().0, began.0 + 31);
    assert_eq!(h.peer().health.hp(), 14.);
    assert_eq!(h.inventory().hand().count(), 1);
    assert_eq!(h.action(1, start).status, ActionStatus::Accepted);
    assert_eq!(h.inventory().hand().count(), 1);
    let stale = net::InventoryGuard {
        revision: 0,
        ..h.guard()
    };
    assert_eq!(
        h.action(
            2,
            Action::UseFood {
                held: false,
                inventory: stale
            }
        )
        .status,
        ActionStatus::Accepted
    );
    for _ in 0..35 {
        h.tick();
    }
    assert_eq!(h.inventory().hand().count(), 1);
    assert_eq!(h.peer().health.hp(), 14.);
    assert_eq!(
        h.action(
            3,
            Action::UseFood {
                held: true,
                inventory: h.guard()
            }
        )
        .status,
        ActionStatus::Accepted
    );
    for _ in 0..8 {
        h.tick();
    }
    assert_eq!(
        h.action(4, Action::SelectSlot(1)).status,
        ActionStatus::Accepted
    );
    for _ in 0..32 {
        h.tick();
    }
    assert_eq!(h.inventory().get(0).unwrap().count(), 1);
    h.reopen_inspect(|recovery| {
        let eat_ticks = recovery
            .batches
            .iter()
            .flat_map(|(_, b)| &b.changes)
            .flat_map(|c| &c.events)
            .filter(|e| e.payload.len() == 80)
            .filter_map(|e| net::audio_cue::Cue::decode(&e.payload[16..]).ok())
            .filter(|c| {
                c.kind == net::audio_cue::Kind::Eat && c.tick >= began && c.tick.0 <= began.0 + 31
            })
            .map(|c| c.tick.0 - began.0)
            .collect::<Vec<_>>();
        assert_eq!(eat_ticks, [8, 12, 16, 20, 24, 28, 31]);
        assert!(recovery.batches.iter().any(|(_, batch)| has_cue(
            batch,
            net::audio_cue::Kind::EatDone
        )
            && batch.players.iter().any(|p| p.state.health == 14.
                && Inventory::decode(&p.inventory).is_ok_and(|i| i.get(0).unwrap().count() == 1))));
    });
    assert_eq!(h.inventory().get(0).unwrap().count(), 1);
    assert_eq!(h.peer().health.hp(), 14.);
    assert!(h.peer().use_item.eating.is_none());
}
#[test]
fn bucket_real_source_pickup_pour_and_immediate_hardening_are_atomic() {
    let mut h = H::new();
    h.seed_hand(I_BUCKET, 2);
    let water = point(6, 1, 11);
    let old = h.guard();
    let pickup = Action::UseBucket {
        position: water,
        expected: Cell::from_parts(rules::ids::WATER, 0),
        inventory: old,
    };
    let result = h.aimed(1, pickup.clone(), [6.5, 1.5, 11.5]);
    assert_eq!(result.status, ActionStatus::Accepted);
    assert!(result.transaction.is_some());
    assert_eq!(h.inventory().hand(), ItemStack::new(I_BUCKET, 1).unwrap());
    assert_eq!(
        h.inventory().get(1),
        Some(ItemStack::new(I_WATER_BUCKET, 1).unwrap())
    );
    assert_eq!(h.action(1, pickup.clone()).status, ActionStatus::Accepted);
    while h.server.current_tick() < h.peer().place_ready_at {
        h.tick();
    }
    assert_eq!(
        h.action(2, pickup).status,
        ActionStatus::Rejected(RejectReason::Stale)
    );
    assert_eq!(
        h.action(3, Action::SelectSlot(1)).status,
        ActionStatus::Accepted
    );
    while h.server.current_tick() < h.peer().place_ready_at {
        h.tick();
    }
    let target = point(9, 2, 11);
    let expected = match h.server.world.read(target) {
        CellRead::Known(c) => c,
        _ => panic!(),
    };
    let pour = Action::UseBucket {
        position: target,
        expected,
        inventory: h.guard(),
    };
    let result = h.aimed(4, pour.clone(), [9.5, 1.99, 11.5]);
    assert_eq!(result.status, ActionStatus::Accepted);
    assert_eq!(h.inventory().hand(), ItemStack::new(I_BUCKET, 1).unwrap());
    assert_eq!(
        h.server.world.read(point(10, 2, 11)),
        CellRead::Known(Cell::from_parts(rules::ids::OBSIDIAN, 0))
    );
    assert_eq!(h.action(4, pour).status, ActionStatus::Accepted);
    h.reopen_inspect(|recovery| {
        assert!(recovery.batches.iter().any(|(_, batch)| {
            batch
                .actions
                .iter()
                .any(|(_, r)| r.id == ActionId(4) && r.status == ActionStatus::Accepted)
                && has_cue(batch, net::audio_cue::Kind::BucketEmpty)
                && batch
                    .changes
                    .iter()
                    .flat_map(|c| &c.cells)
                    .any(|c| c.old.id() == rules::ids::LAVA && c.new.id() == rules::ids::OBSIDIAN)
                && batch.players.iter().any(|p| {
                    Inventory::decode(&p.inventory)
                        .is_ok_and(|i| i.hand() == ItemStack::new(I_BUCKET, 1).unwrap())
                })
        }));
    });
    assert_eq!(h.inventory().hand(), ItemStack::new(I_BUCKET, 1).unwrap());
    assert_eq!(
        h.server.world.read(point(10, 2, 11)),
        CellRead::Known(Cell::from_parts(rules::ids::OBSIDIAN, 0))
    );
}
#[test]
fn bucket_full_inventory_spills_once_and_same_wal_contains_all_owners() {
    let mut h = H::new();
    h.seed_hand(I_BUCKET, 2);
    for slot in 1..36 {
        h.server
            .peers
            .get_mut(&1)
            .unwrap()
            .inventory
            .set_trusted(slot, ItemStack::new(rules::ids::COBBLE, 64).unwrap())
            .unwrap();
    }
    let target = point(6, 1, 11);
    let action = Action::UseBucket {
        position: target,
        expected: Cell::from_parts(rules::ids::WATER, 0),
        inventory: h.guard(),
    };
    assert_eq!(
        h.aimed(1, action.clone(), [6.5, 1.5, 11.5]).status,
        ActionStatus::Accepted
    );
    assert_eq!(h.inventory().hand().count(), 1);
    assert_eq!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .filter(|r| r.stack.item() == I_WATER_BUCKET)
            .count(),
        1
    );
    assert_eq!(h.action(1, action.clone()).status, ActionStatus::Accepted);
    h.reopen_inspect(|recovery| {
        assert!(recovery.batches.iter().any(|(_, batch)| {
            batch
                .actions
                .iter()
                .any(|(_, r)| r.id == ActionId(1) && r.status == ActionStatus::Accepted)
                && has_cue(batch, net::audio_cue::Kind::BucketFill)
                && batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_ITEMS)
                && batch.changes.iter().flat_map(|c| &c.cells).any(|c| {
                    c.page == target.page()
                        && c.index == target.local_index()
                        && c.old.id() == rules::ids::WATER
                        && c.new == Cell::AIR
                })
                && batch.players.iter().any(|p| {
                    Inventory::decode(&p.inventory)
                        .is_ok_and(|i| i.hand() == ItemStack::new(I_BUCKET, 1).unwrap())
                })
        }));
    });
    assert_eq!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .filter(|r| r.stack.item() == I_WATER_BUCKET)
            .count(),
        1
    );
    assert_eq!(h.action(1, action).status, ActionStatus::Accepted);
    assert_eq!(h.inventory().hand().count(), 1);
}
#[test]
fn food_release_preempts_action_backlog_and_never_restarts_an_older_press() {
    let mut h = H::new();
    h.seed_hand(I_APPLE, 2);
    h.hurt();
    let start = Action::UseFood {
        held: true,
        inventory: h.guard(),
    };
    assert_eq!(h.action(1, start.clone()).status, ActionStatus::Accepted);
    for id in 2..34 {
        h.send(ClientMessage::Action {
            id: ActionId(id),
            action: Action::StopMining,
        });
    }
    h.send(ClientMessage::Action {
        id: ActionId(34),
        action: Action::UseFood {
            held: false,
            inventory: h.guard(),
        },
    });
    for _ in 0..35 {
        h.tick();
        assert_eq!(h.peer().health.hp(), 10.);
        assert_eq!(h.inventory().hand().count(), 2);
    }
    assert_eq!(h.action(1, start).status, ActionStatus::Accepted);
    assert!(h.peer().use_item.eating.is_none());
}
#[test]
fn bucket_visibility_protection_and_unknown_reject_without_item_or_fluid_mutation() {
    let target = point(6, 1, 11);
    {
        let mut h = H::variant(1);
        h.seed_hand(I_BUCKET, 1);
        for (id, expected) in [
            (1, Cell::from_parts(rules::ids::WATER, 0)),
            (2, Cell::from_parts(rules::ids::LAVA, 0)),
        ] {
            let action = Action::UseBucket {
                position: target,
                expected,
                inventory: h.guard(),
            };
            assert_eq!(
                h.aimed(id, action, [6.5, 1.5, 11.5]).status,
                ActionStatus::Rejected(RejectReason::Occluded)
            );
            assert_eq!(h.inventory().hand(), ItemStack::new(I_BUCKET, 1).unwrap());
        }
    }
    {
        let mut h = H::new();
        h.seed_hand(I_BUCKET, 1);
        let identity = h.peer().identity;
        h.server.identities.get_mut(&identity).unwrap().role = Role::Player;
        h.server.config.spawn_protection_radius = 16;
        let action = Action::UseBucket {
            position: target,
            expected: Cell::from_parts(rules::ids::WATER, 0),
            inventory: h.guard(),
        };
        assert_eq!(
            h.aimed(1, action, [6.5, 1.5, 11.5]).status,
            ActionStatus::Rejected(RejectReason::Protected)
        );
        assert_eq!(h.inventory().hand(), ItemStack::new(I_BUCKET, 1).unwrap());
    }
    {
        let mut h = H::variant(2);
        h.seed_hand(I_BUCKET, 1);
        let action = Action::UseBucket {
            position: target,
            expected: Cell::from_parts(rules::ids::WATER, 0),
            inventory: h.guard(),
        };
        assert_eq!(
            h.aimed(1, action, [6.5, 1.5, 11.5]).status,
            ActionStatus::Rejected(RejectReason::Unknown)
        );
        assert_eq!(h.inventory().hand(), ItemStack::new(I_BUCKET, 1).unwrap());
        assert_eq!(h.server.counters().generation_submitted, 0);
    }
}
#[test]
fn bucket_washes_plant_loot_and_creative_keeps_both_bucket_kinds() {
    let mut h = H::variant(3);
    h.seed_hand(I_WATER_BUCKET, 1);
    h.server
        .peers
        .get_mut(&1)
        .unwrap()
        .player
        .set_mode(GameMode::Creative);
    let target = point(9, 2, 11);
    let before = Cell::from_parts(rules::ids::TORCH, 0);
    let pour = Action::UseBucket {
        position: target,
        expected: before,
        inventory: h.guard(),
    };
    assert_eq!(
        h.aimed(1, pour.clone(), [9.05, 1.99, 11.05]).status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.inventory().hand(),
        ItemStack::new(I_WATER_BUCKET, 1).unwrap()
    );
    let torch = rules::items::block_item(rules::ids::TORCH);
    assert_eq!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .filter(|r| r.stack.item() == torch)
            .count(),
        1
    );
    assert_eq!(h.action(1, pour).status, ActionStatus::Accepted);
    assert_eq!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .filter(|r| r.stack.item() == torch)
            .count(),
        1
    );
    h.seed_hand(I_BUCKET, 2);
    while h.server.current_tick() < h.peer().place_ready_at {
        h.tick();
    }
    let target = point(6, 1, 11);
    let pickup = Action::UseBucket {
        position: target,
        expected: Cell::from_parts(rules::ids::WATER, 0),
        inventory: h.guard(),
    };
    assert_eq!(
        h.aimed(2, pickup, [6.5, 1.5, 11.5]).status,
        ActionStatus::Accepted
    );
    assert_eq!(h.inventory().hand(), ItemStack::new(I_BUCKET, 2).unwrap());
    assert_eq!(
        h.server
            .item_entities()
            .rows()
            .iter()
            .filter(|r| r.stack.item() == I_WATER_BUCKET)
            .count(),
        0
    );
}
#[test]
fn old_stop_cannot_cancel_newer_start_in_same_peer_or_reconnected_epoch() {
    let mut h = H::new();
    h.seed_hand(I_APPLE, 2);
    h.hurt();
    assert_eq!(
        h.action(
            1,
            Action::UseFood {
                held: true,
                inventory: h.guard()
            }
        )
        .status,
        ActionStatus::Accepted
    );
    let stop = Action::UseFood {
        held: false,
        inventory: h.guard(),
    };
    assert_eq!(h.action(2, stop.clone()).status, ActionStatus::Accepted);
    assert_eq!(
        h.action(
            3,
            Action::UseFood {
                held: true,
                inventory: h.guard()
            }
        )
        .status,
        ActionStatus::Accepted
    );
    assert_eq!(h.action(2, stop.clone()).status, ActionStatus::Accepted);
    assert!(h.peer().use_item.eating.is_some());
    assert_eq!(h.peer().use_item.start_action, ActionId(3));
    assert_eq!(h.peer().use_item.release_action, ActionId(2));
    h.reopen_inspect(|_| {});
    assert_eq!(
        h.action(
            4,
            Action::UseFood {
                held: true,
                inventory: h.guard()
            }
        )
        .status,
        ActionStatus::Accepted
    );
    let began = h.server.current_tick();
    assert_eq!(h.peer().use_item.eating.unwrap().ticks_left(), 31);
    assert_eq!(h.action(2, stop).status, ActionStatus::Accepted);
    assert!(h.peer().use_item.eating.is_some());
    while h.server.current_tick().0 < began.0 + 30 {
        h.tick();
        assert_eq!(h.peer().health.hp(), 10.);
        assert_eq!(h.inventory().hand().count(), 2);
    }
    h.tick();
    assert_eq!(h.peer().health.hp(), 14.);
    assert_eq!(h.inventory().hand().count(), 1);
}
#[test]
fn food_start_entitlement_and_active_use_are_authoritative() {
    let mut h = H::new();
    h.seed_hand(I_APPLE, 2);
    assert_eq!(
        h.action(
            1,
            Action::UseFood {
                held: true,
                inventory: h.guard()
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Unauthorized)
    );
    assert_eq!(h.inventory().hand().count(), 2);
    h.hurt();
    let stale = net::InventoryGuard {
        revision: 0,
        ..h.guard()
    };
    assert_eq!(
        h.action(
            2,
            Action::UseFood {
                held: true,
                inventory: stale
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Stale)
    );
    assert_eq!(
        h.action(
            3,
            Action::UseFood {
                held: true,
                inventory: h.guard()
            }
        )
        .status,
        ActionStatus::Accepted
    );
    assert_eq!(
        h.action(
            4,
            Action::AttackEntity {
                target: 99,
                inventory: h.guard()
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::RateLimited)
    );
    assert!(h.peer().use_item.eating.is_some());
    assert_eq!(h.peer().health.hp(), 10.);
    assert_eq!(h.inventory().hand().count(), 2);
    assert_eq!(
        h.action(5, Action::SelectSlot(1)).status,
        ActionStatus::Accepted
    );
    assert!(h.peer().use_item.eating.is_none());
    assert_eq!(h.inventory().get(0).unwrap().count(), 2);
}
