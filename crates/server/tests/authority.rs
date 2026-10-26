use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use net::{
    Action, ActionId, ActionReply, ActionStatus, ClientMessage, Hello, InputFrame, PageUpdate,
    PlayerId, RejectReason, Replica, ReplicaRead, ServerMessage, Welcome,
};
use server::{Outgoing, Server, ServerConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};
use world::{CellRead, Transaction, World};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!(
                "authority-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> ServerConfig {
        let mut config = ServerConfig::new(&self.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 1.0, 8.5]);
        config
    }
    fn owner(&self) -> Hello {
        let bytes = std::fs::read(self.0.join("owner.identity")).unwrap();
        let ClientMessage::Hello(hello) = net::decode_client(&bytes).unwrap() else {
            panic!("owner file")
        };
        hello
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn cell(id: u16) -> Cell {
    Cell::from_parts(id, 0)
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 1024);
    let mut cells = vec![Cell::AIR; PAGE_VOLUME];
    for z in 0..16 {
        for x in 0..16 {
            cells[x + z * 16] = cell(rules::ids::STONE);
        }
    }
    cells[8 + 11 * 16 + 2 * 256] = cell(rules::ids::STONE);
    cells[8 + 12 * 16 + 2 * 256] = cell(rules::ids::IRON_ORE);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(pos(0, 0, 0).page(), cells);
    world.commit(tx).unwrap();
    world
}
struct Harness {
    server: Server,
    now: u64,
    outputs: Vec<Outgoing>,
}
impl Harness {
    fn new(directory: &Directory) -> Self {
        Self {
            server: Server::open_with_world(directory.config(), fixture()).unwrap(),
            now: 0,
            outputs: Vec::new(),
        }
    }
    fn send(&mut self, connection: u64, message: ClientMessage) {
        self.outputs
            .extend(self.server.handle(connection, message, self.now));
    }
    fn advance(&mut self) {
        self.now += 10;
        self.outputs.extend(self.server.tick(self.now));
        thread::sleep(Duration::from_micros(500));
    }
    fn until(&mut self, mut predicate: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        for _ in 0..3000 {
            if let Some(index) = self.outputs.iter().position(&mut predicate) {
                return self.outputs.remove(index);
            }
            self.advance();
        }
        let last = self.outputs.iter().rev().find_map(|o| {
            if let ServerMessage::State { player, .. } = o.message {
                Some(player)
            } else {
                None
            }
        });
        panic!(
            "authority response missing; tick={} failed={} counters={:?} last_state={last:?}",
            self.server.current_tick().0,
            self.server.is_failed(),
            self.server.counters()
        );
    }
    fn join(&mut self, connection: u64, hello: Hello) -> Welcome {
        self.send(connection, ClientMessage::Hello(hello));
        let outgoing = self.until(|o| {
            o.connection == connection && matches!(o.message, ServerMessage::Welcome(_))
        });
        let ServerMessage::Welcome(welcome) = outgoing.message else {
            unreachable!()
        };
        welcome
    }
    fn action(&mut self, connection: u64, id: u64, action: Action) -> ActionReply {
        self.send(
            connection,
            ClientMessage::Action {
                id: ActionId(id),
                action,
            },
        );
        self.final_reply(connection, id)
    }
    fn final_reply(&mut self, connection: u64, id: u64) -> ActionReply {
        let outgoing=self.until(|o|o.connection==connection&&matches!(o.message,ServerMessage::Action(reply) if reply.id==ActionId(id)&&reply.status!=ActionStatus::Pending));
        let ServerMessage::Action(reply) = outgoing.message else {
            unreachable!()
        };
        reply
    }
    fn page(&mut self, connection: u64, page: PageId, acknowledged: Option<u64>) -> PageUpdate {
        self.send(
            connection,
            ClientMessage::RequestPage { page, acknowledged },
        );
        let outgoing=self.until(|o|o.connection==connection&&matches!(&o.message,ServerMessage::Page(update) if match update {PageUpdate::Snapshot(s)=>s.page==page,PageUpdate::Delta(d)=>d.page==page}));
        let ServerMessage::Page(update) = outgoing.message else {
            unreachable!()
        };
        update
    }
}
fn guest(name: &str) -> Hello {
    Hello {
        identity: PlayerId::default(),
        name: name.into(),
        credential: None,
    }
}
fn accepted(reply: ActionReply) {
    assert_eq!(reply.status, ActionStatus::Accepted);
}

#[test]
fn accepted_raw_aba_and_external_overwrite_retire_by_durable_action_prefix_across_restart() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    h.join(1, owner.clone());
    accepted(h.action(1, 1, Action::Command("/gamemode creative".into())));
    // Default horizontal aim hits the fixture wall at(8,2,11); place on its
    // front face so the test exercises real authoritative reach/occlusion.
    let target = pos(8, 2, 10);
    let page = target.page();
    let first = h.page(1, page, None);
    let mut replica = Replica::new(1);
    replica.apply(first).unwrap();
    assert_eq!(replica.read(target), ReplicaRead::Known(Cell::AIR));
    let mut overlay = net::PredictionOverlay::new(8);
    overlay
        .predict(ActionId(2), target, cell(rules::ids::STONE))
        .unwrap();
    let placed = h.action(
        1,
        2,
        Action::Place {
            position: target,
            expected: Cell::AIR,
            cell: cell(rules::ids::STONE),
        },
    );
    accepted(placed);
    overlay.predict(ActionId(3), target, Cell::AIR).unwrap();
    h.send(
        1,
        ClientMessage::Inputs(vec![InputFrame {
            sequence: 1,
            target_tick: Tick(h.server.current_tick().0 + 1),
            keys: InputFrame::ATTACK,
            ..InputFrame::default()
        }]),
    );
    let removed = h.action(
        1,
        3,
        Action::Mine {
            position: target,
            expected: cell(rules::ids::STONE),
            tool: 0,
        },
    );
    accepted(removed);
    h.send(
        1,
        ClientMessage::Inputs(vec![InputFrame {
            sequence: 2,
            target_tick: Tick(h.server.current_tick().0 + 1),
            ..InputFrame::default()
        }]),
    );
    h.outputs
        .retain(|out| !matches!(out.message, ServerMessage::Page(_)));
    let truth = h.page(1, page, None);
    let image = match &truth {
        PageUpdate::Snapshot(p) => p,
        _ => panic!("explicit repair requires snapshot"),
    };
    assert_eq!(
        image.cells[usize::from(target.local_index().get())],
        Cell::AIR
    );
    assert!(image.applied_action >= ActionId(3));
    replica.apply(truth).unwrap();
    overlay.retire_applied(&replica);
    assert_eq!(
        overlay.len(),
        2,
        "Raw-before-reply cannot decide unconfirmed action outcomes"
    );
    overlay.reply(placed);
    overlay.reply(removed);
    overlay.retire_applied(&replica);
    assert!(
        overlay.is_empty(),
        "both accepted ABA edits retire even though the first cell was overwritten"
    );
    assert_eq!(
        overlay.read(target, &replica),
        ReplicaRead::Known(Cell::AIR)
    );

    for _ in 0..25 {
        h.advance();
    }
    overlay
        .predict(ActionId(4), target, cell(rules::ids::STONE))
        .unwrap();
    let placed_again = h.action(
        1,
        4,
        Action::Place {
            position: target,
            expected: Cell::AIR,
            cell: cell(rules::ids::STONE),
        },
    );
    accepted(placed_again);
    overlay.reply(placed_again);
    let mut external = Transaction::new(h.server.current_tick(), SpaceId(0));
    external.set_cell(target, Cell::AIR);
    assert!(h.server.queue_transaction(external).is_ok());
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while h.server.world().read(target) != CellRead::Known(Cell::AIR)
        || h.server.awaiting_durability()
    {
        assert!(std::time::Instant::now() < deadline);
        h.advance();
    }
    h.outputs
        .retain(|out| !matches!(out.message, ServerMessage::Page(_)));
    replica.apply(h.page(1, page, None)).unwrap();
    overlay.retire_applied(&replica);
    assert!(
        overlay.is_empty(),
        "an external overwrite is authoritative too"
    );
    assert_eq!(
        overlay.read(target, &replica),
        ReplicaRead::Known(Cell::AIR)
    );
    drop(h);

    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    let welcome = h.join(3, owner);
    assert_eq!(welcome.next_action, ActionId(5));
    let PageUpdate::Snapshot(reconnected) = h.page(3, page, None) else {
        panic!("new session snapshot")
    };
    assert_eq!(reconnected.version, 1);
    assert!(reconnected.applied_action >= ActionId(4));
    let mut replica = Replica::new(1);
    replica.apply(PageUpdate::Snapshot(reconnected)).unwrap();
    let mut overlay = net::PredictionOverlay::new(8);
    overlay
        .predict(ActionId(4), target, cell(rules::ids::STONE))
        .unwrap();
    let replayed = h.action(
        3,
        4,
        Action::Place {
            position: target,
            expected: Cell::AIR,
            cell: cell(rules::ids::STONE),
        },
    );
    assert_eq!(
        replayed, placed_again,
        "saved Action v1 records remain valid and replay exactly once"
    );
    assert_eq!(h.server.world().read(target), CellRead::Known(Cell::AIR));
    overlay.reply(replayed);
    overlay.retire_applied(&replica);
    assert!(
        overlay.is_empty(),
        "new public revision1 still carries the durable own prefix"
    );
}

#[test]
fn sealed_ore_reveal_confirmed_delta_duplicate_exact_once_and_restart() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    h.join(1, owner.clone());
    h.join(2, guest("observer"));
    accepted(h.action(1, 1, Action::Command("/gamemode creative".into())));
    let page = pos(8, 2, 12).page();
    let update = h.page(2, page, None);
    let mut replica = Replica::new(8);
    replica.apply(update).unwrap();
    assert_eq!(
        replica.read(pos(8, 2, 12)),
        ReplicaRead::Known(cell(rules::ids::STONE))
    );
    let version = replica.page(page).unwrap().version;
    h.send(2, ClientMessage::AckPage { page, version });
    // Mine is a held command, never one break per incoming packet.
    h.send(
        1,
        ClientMessage::Inputs(vec![InputFrame {
            sequence: 1,
            keys: InputFrame::ATTACK,
            ..InputFrame::default()
        }]),
    );
    h.send(
        1,
        ClientMessage::Action {
            id: ActionId(2),
            action: Action::Mine {
                position: pos(8, 2, 11),
                expected: cell(rules::ids::STONE),
                tool: 0,
            },
        },
    );
    for _ in 0..10 {
        h.send(
            1,
            ClientMessage::Action {
                id: ActionId(2),
                action: Action::Mine {
                    position: pos(8, 2, 11),
                    expected: cell(rules::ids::STONE),
                    tool: 0,
                },
            },
        );
    }
    let mined = h.final_reply(1, 2);
    accepted(mined);
    let tx = mined.transaction.unwrap();
    assert_eq!(
        h.server.world().read(pos(8, 2, 11)),
        CellRead::Known(Cell::AIR)
    );
    let update = h.until(|o| o.connection == 2 && matches!(o.message, ServerMessage::Page(_)));
    let ServerMessage::Page(update) = update.message else {
        unreachable!()
    };
    replica.apply(update).unwrap();
    assert_eq!(
        replica.read(pos(8, 2, 12)),
        ReplicaRead::Known(cell(rules::ids::IRON_ORE))
    );
    let again = h.action(
        1,
        2,
        Action::Mine {
            position: pos(8, 2, 11),
            expected: cell(rules::ids::STONE),
            tool: 0,
        },
    );
    assert_eq!(again, mined);
    assert_eq!(h.server.world().version(), mined.source_version);
    // Missing confirmed history must produce an independently usable image.
    assert!(matches!(
        h.page(2, page, Some(u64::MAX)),
        PageUpdate::Snapshot(_)
    ));
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    let welcome = h.join(3, owner);
    assert_eq!(welcome.next_action, ActionId(3));
    assert_eq!(
        h.server.world().read(pos(8, 2, 11)),
        CellRead::Known(Cell::AIR)
    );
    let replay = h.action(
        3,
        2,
        Action::Mine {
            position: pos(8, 2, 11),
            expected: cell(rules::ids::STONE),
            tool: 0,
        },
    );
    assert_eq!(replay.transaction, Some(tx));
    assert_eq!(replay, mined);
}

#[test]
fn authority_rejects_unknown_reach_roles_spawn_protection_and_unauthorized_h() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    h.join(1, directory.owner());
    let guest = h.join(2, guest("restricted"));
    assert!(guest.new_credential.is_some());
    assert_eq!(
        h.action(2, 1, Action::DebugNoclip(true)).status,
        ActionStatus::Rejected(RejectReason::DebugRequired)
    );
    assert_eq!(
        h.action(2, 2, Action::Command("/gamemode creative".into()))
            .status,
        ActionStatus::Rejected(RejectReason::Unauthorized)
    );
    assert_eq!(
        h.action(
            2,
            3,
            Action::Place {
                position: pos(8, 2, 10),
                expected: Cell::AIR,
                cell: cell(rules::ids::STONE)
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Unauthorized)
    );
    assert_eq!(
        h.action(
            2,
            4,
            Action::Mine {
                position: pos(8, 2, 11),
                expected: cell(rules::ids::STONE),
                tool: 0
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Protected)
    );
    assert_eq!(
        h.action(
            1,
            1,
            Action::Mine {
                position: pos(80, 2, 80),
                expected: Cell::AIR,
                tool: 0
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::OutOfReach)
    );
    assert_eq!(
        h.action(
            1,
            2,
            Action::Mine {
                position: pos(0, 2, 0),
                expected: Cell::AIR,
                tool: 0
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::OutOfReach)
    );
    assert_eq!(
        h.action(
            1,
            3,
            Action::Mine {
                position: pos(8, 2, 12),
                expected: cell(rules::ids::IRON_ORE),
                tool: 0
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Occluded)
    );
    accepted(h.action(1, 4, Action::Command("/ban restricted".into())));
    // A removed connection in the precollected tick list cannot panic the owner.
    for _ in 0..30 {
        h.advance();
    }
    h.send(
        3,
        ClientMessage::Hello(Hello {
            identity: guest.identity,
            name: "restricted".into(),
            credential: guest.new_credential,
        }),
    );
    h.until(|o| o.connection == 3 && o.message == ServerMessage::Error(RejectReason::Banned));
    h.send(
        4,
        ClientMessage::Hello(Hello {
            identity: guest.identity,
            name: "restricted".into(),
            credential: Some(net::SecretToken::new([0; 32])),
        }),
    );
    h.until(|o| o.connection == 4 && o.message == ServerMessage::Error(RejectReason::Unauthorized));
}

#[test]
fn sequence_reordering_duplicate_inputs_cannot_speed_up_owner_and_checkpoint_reopens() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    h.join(1, directory.owner());
    accepted(h.action(1, 1, Action::Flight(true)));
    let baseline = h.server.counters();
    let frames: Vec<_> = (1..=8)
        .map(|sequence| InputFrame {
            sequence,
            target_tick: Tick(0),
            keys: InputFrame::JUMP,
            yaw: 0.0,
            pitch: 0.0,
        })
        .collect();
    for _ in 0..10 {
        h.send(1, ClientMessage::Inputs(frames[4..].to_vec()));
        h.send(1, ClientMessage::Inputs(frames[..4].to_vec()));
    }
    let state = h
        .until(|o| matches!(o.message,ServerMessage::State{player,..} if player.input_sequence==8));
    let ServerMessage::State { player, .. } = state.message else {
        unreachable!()
    };
    assert!(player.position[1] > 1.0);
    assert!(player.position[1] < 10.0);
    assert_eq!(h.server.counters().input_steps - baseline.input_steps, 8);
    // Extra duplicate sequence histories never replay those eight movements.
    for _ in 0..50 {
        h.advance();
    }
    assert_eq!(h.server.counters().input_steps - baseline.input_steps, 8);
    // Wait until the owner is between durable batches, then take a cheap capture.
    for _ in 0..1000 {
        if h.server.request_checkpoint() {
            break;
        }
        h.advance();
    }
    drop(h);
    let server = Server::open(directory.config()).unwrap();
    assert!(!server.is_failed());
}

#[test]
fn reconnect_resumes_the_first_unfinished_action_instead_of_stranding_the_window() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    h.join(1, owner.clone());
    h.send(
        1,
        ClientMessage::Action {
            id: ActionId(1),
            action: Action::Mine {
                position: pos(8, 2, 11),
                expected: cell(rules::ids::STONE),
                tool: 0,
            },
        },
    );
    h.until(|o|matches!(o.message,ServerMessage::Action(reply) if reply.id==ActionId(1)&&reply.status==ActionStatus::Pending));
    let completed = h.action(1, 2, Action::Flight(true));
    accepted(completed);
    h.server.disconnect(1);
    let welcome = h.join(3, owner);
    assert_eq!(welcome.next_action, ActionId(1));
    accepted(h.action(3, 1, Action::Flight(false)));
    assert_eq!(h.action(3, 2, Action::Flight(true)), completed);
    accepted(h.action(3, 3, Action::Flight(true)));
}

#[test]
fn action_queue_pressure_never_returns_an_undurable_terminal_rejection() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    h.join(1, directory.owner());
    for id in 1..=65 {
        h.send(
            1,
            ClientMessage::Action {
                id: ActionId(id),
                action: Action::Flight(true),
            },
        );
    }
    h.until(|o| o.connection == 1 && o.message == ServerMessage::Error(RejectReason::RateLimited));
    assert!(!h.outputs.iter().any(|o|matches!(o.message,ServerMessage::Action(reply) if reply.id==ActionId(65)&&reply.status!=ActionStatus::Pending)));
    accepted(h.final_reply(1, 64));
    accepted(h.action(1, 65, Action::Flight(true)));
}

#[test]
fn movable_grid_geometry_and_transform_follow_world_aoi_and_durable_patches() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    h.join(1, directory.owner());
    let page = PageId {
        space: SpaceId(7),
        x: 0,
        y: 0,
        z: 0,
    };
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), page.space);
    let mut transform = core_types::SpaceTransform::IDENTITY;
    transform.translation = [10_000.0, 0.0, 10_000.0];
    tx.create_space(transform)
        .create_page(page, vec![cell(rules::ids::STONE); PAGE_VOLUME]);
    h.server
        .queue_transaction(tx)
        .unwrap_or_else(|_| panic!("owner queue"));
    h.until(|o| {
        matches!(
            o.message,
            ServerMessage::Transform {
                space: SpaceId(7),
                ..
            }
        )
    });
    h.send(
        1,
        ClientMessage::RequestPage {
            page,
            acknowledged: None,
        },
    );
    h.until(|o| o.connection == 1 && o.message == ServerMessage::PageUnavailable(page));
    accepted(h.action(1, 1, Action::Command("/space_move 7 8 0 8".into())));
    let update = h.page(1, page, None);
    let mut replica = Replica::new(1);
    replica.apply(update).unwrap();
    let version = replica.page(page).unwrap().version;
    h.send(1, ClientMessage::AckPage { page, version });
    let p = BlockPos {
        space: page.space,
        x: 3,
        y: 3,
        z: 3,
    };
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), page.space);
    tx.expect_page(page, h.server.world().page_version(page));
    tx.set_cell(p, Cell::AIR);
    h.server
        .queue_transaction(tx)
        .unwrap_or_else(|_| panic!("owner queue"));
    let outgoing = h.until(|o| o.connection == 1 && matches!(o.message, ServerMessage::Page(_)));
    let ServerMessage::Page(update) = outgoing.message else {
        unreachable!()
    };
    replica.apply(update).unwrap();
    assert_eq!(replica.read(p), ReplicaRead::Known(Cell::AIR));
    // Moving outside the outer radius evicts the geometry subscription.
    accepted(h.action(1, 2, Action::Command("/space_move 7 10000 0 10000".into())));
    h.send(
        1,
        ClientMessage::RequestPage {
            page,
            acknowledged: Some(version),
        },
    );
    h.until(|o| o.connection == 1 && o.message == ServerMessage::PageUnavailable(page));
}

#[test]
fn deop_revokes_debug_and_spectator_in_the_same_durable_batch() {
    let directory = Directory::new();
    let mut config = directory.config();
    config.server_debug = true;
    let mut h = Harness {
        server: Server::open_with_world(config, fixture()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(10, directory.owner());
    h.join(1, guest("debugger"));
    accepted(h.action(10, 1, Action::Command("/op debugger".into())));
    accepted(h.action(1, 1, Action::Command("/gamemode spectator".into())));
    accepted(h.action(1, 2, Action::DebugNoclip(true)));
    h.outputs.clear();
    let minimum_tick = h.server.current_tick();
    accepted(h.action(10, 2, Action::Command("/deop debugger".into())));
    h.until(|o|o.connection==1&&matches!(o.message,ServerMessage::State{tick,player} if tick>=minimum_tick&&player.mode==0&&!player.noclip));
    assert_eq!(
        h.action(1, 3, Action::DebugNoclip(true)).status,
        ActionStatus::Rejected(RejectReason::DebugRequired)
    );
}

fn inventory(h: &mut Harness, connection: u64, minimum: u64) -> rules::survival::Inventory {
    let outgoing = h.until(|o| {
        o.connection == connection
            && match &o.message {
                ServerMessage::Inventory { data } => {
                    rules::survival::Inventory::decode(data).is_ok_and(|i| i.revision() >= minimum)
                }
                _ => false,
            }
    });
    let ServerMessage::Inventory { data } = outgoing.message else {
        unreachable!()
    };
    rules::survival::Inventory::decode(&data).unwrap()
}
fn guard(inventory: &rules::survival::Inventory) -> net::InventoryGuard {
    let hand = inventory.hand();
    net::InventoryGuard {
        revision: inventory.revision(),
        selected: inventory.selected() as u8,
        item: hand.item(),
        count: hand.count(),
        damage: hand.damage(),
        payload: hand.payload().map_or(0, |id| id.0),
    }
}
#[test]
fn survival_placement_consumes_owned_stack_atomically_and_never_reconsumes_a_replayed_action() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    h.join(1, owner.clone());
    assert!(inventory(&mut h, 1, 0).hand().is_empty());
    accepted(h.action(
        1,
        1,
        Action::Command(format!(
            "/give {} 3",
            rules::items::block_item(rules::ids::STONE)
        )),
    ));
    let owned = inventory(&mut h, 1, 1);
    assert_eq!(owned.hand().count(), 3);
    let placement = Action::PlaceOwned {
        position: pos(8, 2, 10),
        expected: Cell::AIR,
        cell: cell(rules::ids::STONE),
        inventory: guard(&owned),
    };
    let result = h.action(1, 2, placement.clone());
    accepted(result);
    let after = inventory(&mut h, 1, 2);
    assert_eq!(after.hand().count(), 2);
    assert_eq!(
        h.server.world().read(pos(8, 2, 10)),
        CellRead::Known(cell(rules::ids::STONE))
    );
    assert_eq!(h.action(1, 2, placement.clone()), result);
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(2, owner.clone());
    assert_eq!(inventory(&mut h, 2, 2), after);
    assert_eq!(h.action(2, 2, placement), result);
    for _ in 0..40 {
        h.advance();
    }
    assert_eq!(
        h.action(
            2,
            3,
            Action::PlaceOwned {
                position: pos(8, 2, 9),
                expected: Cell::AIR,
                cell: cell(rules::ids::STONE),
                inventory: guard(&owned)
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Stale)
    );
    assert_eq!(
        h.action(
            2,
            4,
            Action::PlaceOwned {
                position: pos(80, 2, 80),
                expected: Cell::AIR,
                cell: cell(rules::ids::STONE),
                inventory: guard(&after)
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::OutOfReach)
    );
    let placement = Action::PlaceOwned {
        position: pos(8, 2, 9),
        expected: Cell::AIR,
        cell: cell(rules::ids::STONE),
        inventory: guard(&after),
    };
    let result = h.action(2, 5, placement.clone());
    accepted(result);
    let after = inventory(&mut h, 2, 3);
    assert_eq!(after.hand().count(), 1);
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(3, owner);
    assert_eq!(inventory(&mut h, 3, 3), after);
    assert_eq!(h.action(3, 5, placement), result);
}

#[test]
fn mining_import_pickup_and_restart_keep_one_owned_drop_and_exact_tool_wear() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    let welcome = h.join(1, owner.clone());
    let tool = rules::items::I_WOODEN_PICKAXE;
    accepted(h.action(1, 1, Action::Command(format!("/give {tool} 1"))));
    let before = inventory(&mut h, 1, 1);
    assert_eq!(before.hand().item(), tool);
    assert_eq!(before.hand().damage(), 0);
    let action = Action::Mine {
        position: pos(8, 2, 11),
        expected: cell(rules::ids::STONE),
        tool,
    };
    h.send(
        1,
        ClientMessage::Action {
            id: ActionId(2),
            action: action.clone(),
        },
    );
    for sequence in 1..=200 {
        h.send(
            1,
            ClientMessage::Inputs(vec![InputFrame {
                sequence,
                keys: InputFrame::ATTACK,
                ..Default::default()
            }]),
        );
        for _ in 0..5 {
            h.advance();
        }
        if h.outputs.iter().any(|o|matches!(o.message,ServerMessage::Action(reply) if reply.id==ActionId(2)&&reply.status!=ActionStatus::Pending)) {break;}
    }
    let mined = h.final_reply(1, 2);
    accepted(mined);
    let worn = inventory(&mut h, 1, 2);
    assert_eq!(worn.hand().damage(), 1);
    for _ in 0..200 {
        if h.server.item_entities().len() == 1 {
            break;
        }
        h.advance();
    }
    let entity = *h
        .server
        .item_entities()
        .rows()
        .first()
        .expect("durable imported item");
    assert_eq!(entity.stack.item(), rules::ids::COBBLE);
    assert_eq!(entity.stack.count(), 1);
    assert!(
        !h.server
            .world()
            .components()
            .any(|(_, c)| c.data.kind == server::survival::DROP_COMPONENT)
    );
    drop(h);
    // The accepted mining transaction still contains the independently durable
    // source bundle. Its later removal and the entity ledger share one WAL batch.
    let entry = {
        let (_store, recovery) = save::Store::open(&directory.0).unwrap();
        let source = recovery
            .batches
            .iter()
            .flat_map(|(_, b)| &b.changes)
            .find(|change| Some(change.tx) == mined.transaction)
            .unwrap();
        let component = source
            .components
            .iter()
            .find_map(|c| {
                c.after
                    .as_ref()
                    .filter(|r| r.data.kind == server::survival::DROP_COMPONENT)
            })
            .unwrap();
        let bundle = server::survival::DropBundle::decode(&component.data.payload).unwrap();
        assert_eq!(bundle.entries.len(), 1);
        bundle.entries[0].clone()
    };
    assert_eq!(entry.player, welcome.identity.0);
    assert_eq!(entry.action, 2);
    assert_eq!(entry.entity_id, entity.id.0);
    assert_eq!(entry.stack, entity.stack);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(2, owner.clone());
    assert_eq!(inventory(&mut h, 2, 2), worn);
    assert_eq!(h.action(2, 2, action.clone()), mined);
    let restored = h
        .server
        .item_entities()
        .get(entity.id)
        .expect("entity recovered from WAL");
    assert_eq!(restored.stack, entry.stack);
    assert_eq!(h.server.item_entities().len(), 1);
    assert_eq!(
        h.action(
            2,
            3,
            Action::Mine {
                position: pos(8, 2, 12),
                expected: cell(rules::ids::IRON_ORE),
                tool: rules::items::I_STEEL_PICKAXE
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Unauthorized)
    );
    accepted(h.action(2, 4, Action::SelectSlot(1)));
    let selected = inventory(&mut h, 2, 3);
    assert!(selected.hand().is_empty());
    assert_eq!(selected.get(0).unwrap().damage(), 1);
    assert_eq!(
        h.action(
            2,
            5,
            Action::Mine {
                position: pos(8, 2, 12),
                expected: cell(rules::ids::IRON_ORE),
                tool
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Unauthorized)
    );
    let position = h.server.item_entities().get(entity.id).unwrap().position;
    accepted(h.action(
        2,
        6,
        Action::Command(format!("/tp {} 1 {}", position[0], position[2])),
    ));
    for _ in 0..300 {
        if h.server.item_entities().is_empty() {
            break;
        }
        h.advance();
    }
    assert!(
        h.server.item_entities().is_empty(),
        "actual server physics/delay/pickup phase consumed item once"
    );
    let picked = inventory(&mut h, 2, 4);
    assert_eq!(picked.get(1).unwrap().item(), rules::ids::COBBLE);
    assert_eq!(picked.get(1).unwrap().count(), 1);
    assert_eq!(h.action(2, 2, action), mined);
    assert!(
        h.server.item_entities().is_empty(),
        "replayed mining cannot recreate an already picked drop"
    );
    for _ in 0..200 {
        if h.server.request_checkpoint() {
            break;
        }
        h.advance();
    }
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(3, owner);
    assert_eq!(inventory(&mut h, 3, 4), picked);
    assert!(
        h.server.item_entities().is_empty(),
        "snapshot/reopen conserves picked item"
    );
}

#[test]
fn hidden_ore_cannot_be_probed_through_expected_id_error_priority() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    h.join(1, directory.owner());
    let mut tx = Transaction::new(Tick(h.server.current_tick().0 + 1), SpaceId(0));
    tx.set_cell(pos(8, 2, 13), cell(rules::ids::STONE));
    h.server
        .queue_transaction(tx)
        .unwrap_or_else(|_| panic!("owner queue"));
    for _ in 0..20 {
        h.advance();
    }
    let ore = h.action(
        1,
        1,
        Action::Mine {
            position: pos(8, 2, 12),
            expected: cell(rules::ids::STONE),
            tool: 0,
        },
    );
    let stone = h.action(
        1,
        2,
        Action::Mine {
            position: pos(8, 2, 13),
            expected: cell(rules::ids::STONE),
            tool: 0,
        },
    );
    let guessed = h.action(
        1,
        3,
        Action::Mine {
            position: pos(8, 2, 12),
            expected: cell(rules::ids::IRON_ORE),
            tool: 0,
        },
    );
    assert_eq!(ore.status, ActionStatus::Rejected(RejectReason::Occluded));
    assert_eq!(stone.status, ore.status);
    assert_eq!(guessed.status, ore.status);
    accepted(h.action(1, 4, Action::Command("/tp 15.5 1 8.5".into())));
    h.send(
        1,
        ClientMessage::Inputs(vec![InputFrame {
            sequence: 1,
            yaw: 270.0,
            ..Default::default()
        }]),
    );
    h.until(|o| matches!(o.message,ServerMessage::State{player,..} if player.input_sequence==1));
    assert_eq!(
        h.action(
            1,
            5,
            Action::Mine {
                position: pos(16, 2, 8),
                expected: Cell::AIR,
                tool: 0
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Unknown)
    );
    accepted(h.action(
        1,
        6,
        Action::Command(format!(
            "/give {} 1",
            rules::items::block_item(rules::ids::STONE)
        )),
    ));
    let owned = inventory(&mut h, 1, 1);
    assert_eq!(
        h.action(
            1,
            7,
            Action::PlaceOwned {
                position: pos(16, 2, 8),
                expected: Cell::AIR,
                cell: cell(rules::ids::STONE),
                inventory: guard(&owned)
            }
        )
        .status,
        ActionStatus::Rejected(RejectReason::Unknown)
    );
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(2, directory.owner());
    assert_eq!(
        inventory(&mut h, 2, 1).hand().count(),
        1,
        "Unknown placement consumed nothing"
    );
}

#[test]
fn aimed_click_uses_its_validated_camera_and_keeps_action_id_idempotence() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    h.join(1, owner);
    accepted(h.action(1, 1, Action::Command("/gamemode creative".into())));
    // The last movement input faces away from the wall. A click carries only
    // its new camera orientation, never a client-authored position or velocity.
    h.send(
        1,
        ClientMessage::Inputs(vec![InputFrame {
            sequence: 1,
            target_tick: Tick(0),
            keys: 0,
            yaw: 180.0,
            pitch: 0.0,
        }]),
    );
    h.until(|o| matches!(o.message, ServerMessage::State { player, .. } if player.input_sequence == 1 && player.yaw == 180.0));
    let position = pos(8, 2, 10);
    let action = Action::Place {
        position,
        expected: Cell::AIR,
        cell: cell(rules::ids::DIRT),
    };
    h.send(
        1,
        ClientMessage::AimedAction {
            id: ActionId(2),
            action: action.clone(),
            yaw: 0.0,
            pitch: 0.0,
        },
    );
    let first = h.final_reply(1, 2);
    accepted(first);
    assert_eq!(
        h.server.world().read(position),
        CellRead::Known(cell(rules::ids::DIRT))
    );
    let version = h.server.world().version();
    // An altered retry of the same action cannot reapply it or change its aim.
    h.send(
        1,
        ClientMessage::AimedAction {
            id: ActionId(2),
            action,
            yaw: 180.0,
            pitch: 45.0,
        },
    );
    assert_eq!(h.final_reply(1, 2), first);
    assert_eq!(h.server.world().version(), version);
}

#[test]
fn ordinary_chat_is_durable_idempotent_and_recovers_before_unpublished_ack() {
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    let welcome = h.join(1, owner.clone());
    h.join(2, guest("chat-observer"));
    let first = h.action(1, 1, Action::Chat("Первое сообщение".into()));
    accepted(first);
    let outgoing=h.until(|o|o.connection==2 && matches!(&o.message,ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(line)) if line.text=="Первое сообщение"));
    let ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(line)) = outgoing.message
    else {
        unreachable!()
    };
    assert_eq!(line.identity, welcome.identity);
    assert!(line.sequence > 0);
    assert_eq!(
        h.action(1, 1, Action::Chat("изменённый повтор".into())),
        first
    );
    assert!(!h.outputs.iter().any(|o|o.connection==2 && matches!(&o.message,ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(next)) if next.sequence==line.sequence)),"retry is never rebroadcast to other peers");
    for _ in 0..100 {
        if h.server.request_checkpoint() {
            break;
        }
        h.advance();
    }
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(3, owner.clone());
    let recovered=h.until(|o|o.connection==3 && matches!(&o.message,ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(next)) if next.sequence==line.sequence));
    assert_eq!(
        recovered.message,
        ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(line.clone()))
    );
    assert_eq!(h.action(3, 1, Action::Chat("ignored retry".into())), first);
    // Process a new action and let its actual writer finish, without polling
    // the publication boundary. Reopening must recover the line and accepted
    // ActionId together, so the retry cannot acknowledge a missing message.
    // Poll the previous neutral/player WAL at the SAME wall time. A later
    // client action queued behind that barrier is not yet a submitted write.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while h.server.awaiting_durability() || !h.server.height_ready_for_publication() {
        h.outputs.extend(h.server.tick(h.now));
        assert!(std::time::Instant::now() < deadline);
        thread::sleep(Duration::from_micros(500));
    }
    h.now += 1000;
    h.send(
        3,
        ClientMessage::Action {
            id: ActionId(2),
            action: Action::Chat("Записано до ACK".into()),
        },
    );
    assert!(
        !h.outputs
            .iter()
            .any(|o| matches!(o.message,ServerMessage::Action(reply) if reply.id==ActionId(2)))
    );
    assert!(
        h.server.awaiting_durability(),
        "new chat action submitted to the actual WAL writer"
    );
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(4, owner);
    let outgoing=h.until(|o|o.connection==4 && matches!(&o.message,ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(next)) if next.text=="Записано до ACK"));
    let ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(next)) = outgoing.message
    else {
        unreachable!()
    };
    assert!(next.sequence > line.sequence);
    accepted(h.action(4, 2, Action::Chat("ignored after crash".into())));
    let third = h.action(4, 3, Action::Chat("После восстановления".into()));
    accepted(third);
    let final_line=h.until(|o|matches!(&o.message,ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(last)) if last.text=="После восстановления"));
    let ServerMessage::Presence(net::presence::PresenceServerEvent::Chat(last)) =
        final_line.message
    else {
        unreachable!()
    };
    assert!(last.sequence > next.sequence);
}

fn window_image(
    h: &mut Harness,
    connection: u64,
    id: u64,
    revision: u64,
) -> net::window::WindowSnapshot {
    let outgoing=h.until(|o|o.connection==connection && matches!(&o.message,ServerMessage::Window(state) if state.guard.id>=id && state.guard.revision>=revision));
    let ServerMessage::Window(state) = outgoing.message else {
        unreachable!()
    };
    *state
}
#[test]
fn actual_inventory_crafting_cursor_and_window_epoch_survive_restart_without_duplication() {
    use net::window::WindowIntent as W;
    let directory = Directory::new();
    let mut h = Harness::new(&directory);
    let owner = directory.owner();
    h.join(1, owner.clone());
    h.join(2, guest("window-observer"));
    accepted(h.action(
        1,
        1,
        Action::Command(format!("/give {} 1", rules::ids::LOG)),
    ));
    accepted(h.action(1, 2, Action::Window(W::OpenInventory)));
    let opened = window_image(&mut h, 1, 2, 0);
    assert!(!opened.closed);
    assert_eq!(opened.cursor, net::window::Stack::EMPTY);
    let click = W::Click {
        guard: opened.guard,
        slot: 0,
        right: false,
        shift: false,
    };
    accepted(h.action(1, 3, Action::Window(click)));
    let hand = window_image(&mut h, 1, 2, 1);
    assert_eq!(hand.cursor.item, rules::ids::LOG);
    assert_eq!(hand.cursor.count, 1);
    assert_eq!(
        h.action(1, 4, Action::Window(click)).status,
        ActionStatus::Rejected(RejectReason::Stale)
    );
    accepted(h.action(
        1,
        5,
        Action::Window(W::Click {
            guard: hand.guard,
            slot: 100,
            right: false,
            shift: false,
        }),
    ));
    let craft = window_image(&mut h, 1, 2, 2);
    assert_eq!(craft.craft[0].item, rules::ids::LOG);
    assert_eq!(craft.craft_result.item, rules::ids::PLANKS);
    assert_eq!(craft.craft_result.count, 4);
    let take = Action::Window(W::TakeCraft {
        guard: craft.guard,
        all: false,
    });
    let taken = h.action(1, 6, take.clone());
    accepted(taken);
    let result = window_image(&mut h, 1, 2, 3);
    assert_eq!(result.cursor.item, rules::ids::PLANKS);
    assert_eq!(result.cursor.count, 4);
    assert_eq!(result.craft[0], net::window::Stack::EMPTY);
    assert!(h.outputs.iter().filter(|o|o.connection==2).all(|o|!matches!(&o.message,ServerMessage::Window(w) if w.cursor.item==rules::ids::LOG || w.cursor.item==rules::ids::PLANKS)),"private cursor never broadcast");
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(3, owner.clone());
    let recovered = window_image(&mut h, 3, 2, 3);
    assert_eq!(recovered.cursor, result.cursor);
    assert_eq!(recovered.guard, result.guard);
    assert_eq!(h.action(3, 6, take), taken);
    accepted(h.action(
        3,
        7,
        Action::Window(W::Close {
            guard: recovered.guard,
        }),
    ));
    let closed = window_image(&mut h, 3, 2, 4);
    assert!(closed.closed);
    assert_eq!(closed.cursor, net::window::Stack::EMPTY);
    let after = inventory(&mut h, 3, 3);
    assert_eq!(after.get(0).unwrap().item(), rules::ids::PLANKS);
    assert_eq!(after.get(0).unwrap().count(), 4);
    accepted(h.action(3, 8, Action::Window(W::OpenInventory)));
    let reopened = window_image(&mut h, 3, 3, 0);
    assert!(
        reopened.guard.id > closed.guard.id,
        "new persistent epoch prevents staleguard ABA"
    );
    drop(h);
    let mut h = Harness {
        server: Server::open(directory.config()).unwrap(),
        now: 0,
        outputs: Vec::new(),
    };
    h.join(4, owner);
    assert_eq!(inventory(&mut h, 4, 3), after);
    let persisted = window_image(&mut h, 4, 3, 0);
    assert_eq!(persisted.guard.id, reopened.guard.id);
    assert_eq!(persisted.cursor, net::window::Stack::EMPTY);
}
