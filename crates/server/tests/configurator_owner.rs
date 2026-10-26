//! Mode changes use the real authority, immutable payload WAL and held-stack guard.
use core_types::{Cell, PageId, SpaceId, Tick};
use net::{
    Action, ActionId, ActionStatus, ClientMessage, InventoryGuard, RejectReason, ServerMessage,
};
use rules::{
    item_payload::{ConfiguratorMode, resolve_configurator},
    items::I_CONFIGURATOR,
    survival::Inventory,
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
impl Directory {
    fn config(&self) -> ServerConfig {
        let mut c = ServerConfig::new(&self.0);
        c.generation_enabled = false;
        c.spawn_override = Some([8.5, 1., 8.5]);
        c
    }
}
struct H {
    server: Server,
    now: u64,
    out: Vec<Outgoing>,
}
impl H {
    fn until(&mut self, mut accept: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(i) = self.out.iter().position(&mut accept) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < deadline, "owner response");
            self.now += 10;
            self.out.extend(self.server.tick(self.now));
            assert!(!self.server.is_failed());
            thread::sleep(Duration::from_micros(300));
        }
    }
    fn action(&mut self, id: u64, action: Action) -> ActionStatus {
        let immediate = self.server.handle(
            1,
            ClientMessage::Action {
                id: ActionId(id),
                action,
            },
            self.now,
        );
        self.out.extend(immediate);
        let result=self.until(|o|matches!(o.message,ServerMessage::Action(r)if r.id==ActionId(id)&&r.status!=ActionStatus::Pending));
        let ServerMessage::Action(reply) = result.message else {
            unreachable!()
        };
        reply.status
    }
    fn inventory(&mut self, after: u64) -> Inventory {
        let out=self.until(|o|matches!(&o.message,ServerMessage::Inventory{data}if Inventory::decode(data).is_ok_and(|i|i.revision()>after)));
        let ServerMessage::Inventory { data } = out.message else {
            unreachable!()
        };
        Inventory::decode(&data).unwrap()
    }
}
fn guard(inv: &Inventory) -> InventoryGuard {
    let held = inv.hand();
    InventoryGuard {
        revision: inv.revision(),
        selected: inv.selected() as u8,
        item: held.item(),
        count: held.count(),
        damage: held.damage(),
        payload: held.payload().map_or(0, |p| p.0),
    }
}
#[test]
fn owned_modes_cycle_and_reverse_once_with_durable_attributes_and_stale_guard_rejection() {
    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/tmp")
            .join(format!("configurator-owner-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    let mut cells = vec![Cell::AIR; 4096];
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
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
    let mut h = H {
        server: Server::open_with_world(directory.config(), world).unwrap(),
        now: 0,
        out: Vec::new(),
    };
    let ClientMessage::Hello(owner) =
        net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap()).unwrap()
    else {
        panic!("owner")
    };
    h.out
        .extend(h.server.handle(1, ClientMessage::Hello(owner.clone()), 0));
    h.until(|o| matches!(o.message, ServerMessage::Welcome(_)));
    assert_eq!(
        h.action(1, Action::Command(format!("/give {I_CONFIGURATOR} 1"))),
        ActionStatus::Accepted
    );
    let mut inv = h.inventory(0);
    let initial = guard(&inv);
    assert_eq!(
        h.action(
            2,
            Action::CycleToolMode {
                back: false,
                inventory: initial
            }
        ),
        ActionStatus::Accepted
    );
    inv = h.inventory(inv.revision());
    assert_eq!(
        resolve_configurator(inv.hand(), h.server.item_payloads()),
        Ok(ConfiguratorMode::Empty)
    );
    let unchanged = inv;
    assert_eq!(
        h.action(
            2,
            Action::CycleToolMode {
                back: false,
                inventory: initial
            }
        ),
        ActionStatus::Accepted,
        "duplicate action returns its original result"
    );
    assert_eq!(
        h.action(
            3,
            Action::CycleToolMode {
                back: false,
                inventory: initial
            }
        ),
        ActionStatus::Rejected(RejectReason::Stale)
    );
    for (id, expected) in [
        (4, ConfiguratorMode::Rotate),
        (5, ConfiguratorMode::Wrench),
        (6, ConfiguratorMode::Gas),
        (7, ConfiguratorMode::Items),
    ] {
        assert_eq!(
            h.action(
                id,
                Action::CycleToolMode {
                    back: false,
                    inventory: guard(&inv)
                }
            ),
            ActionStatus::Accepted
        );
        inv = h.inventory(inv.revision());
        assert_eq!(
            resolve_configurator(inv.hand(), h.server.item_payloads()),
            Ok(expected)
        );
        assert_eq!(inv.hand().damage(), 0, "mode never occupies durability");
    }
    assert_eq!(
        inv.revision(),
        unchanged.revision() + 4,
        "duplicate/stale requests consume no revision"
    );
    assert_eq!(
        h.action(
            8,
            Action::CycleToolMode {
                back: true,
                inventory: guard(&inv)
            }
        ),
        ActionStatus::Accepted
    );
    inv = h.inventory(inv.revision());
    assert_eq!(
        resolve_configurator(inv.hand(), h.server.item_payloads()),
        Ok(ConfiguratorMode::Gas)
    );
    let output=h.until(|o|matches!(&o.message,ServerMessage::ItemAttributes(a) if a.inventory_revision==inv.revision()));
    let ServerMessage::ItemAttributes(attributes) = output.message else {
        unreachable!()
    };
    assert_eq!(
        attributes.matching(
            0,
            inv.hand().item(),
            inv.hand().payload().unwrap().0,
            inv.revision()
        ),
        Some(net::item_attributes::Attribute::Configurator(4))
    );
    let wire = net::encode_server(&ServerMessage::ItemAttributes(attributes)).unwrap();
    assert!(
        wire.len() < 100,
        "owner facts contain no private machine/ledger blob"
    );
    assert_eq!(
        h.server.item_payloads().len(),
        5,
        "cycles reuse immutable content records"
    );
    drop(h);
    let mut requested = directory.config();
    requested.public_seed = 99;
    let recovered = Server::open(requested).unwrap();
    assert_eq!(
        recovered.public_seed(),
        1,
        "stored world identity wins a different reopen default"
    );
    assert_eq!(
        resolve_configurator(inv.hand(), recovered.item_payloads()),
        Ok(ConfiguratorMode::Gas)
    );
    drop(recovered);
    let (_store, recovery) = save::Store::open(&directory.0).unwrap();
    let saved = recovery
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
    assert_eq!(
        Inventory::decode(&saved.inventory).unwrap(),
        inv,
        "payload and owned inventory recover together"
    );
}
#[test]
fn guarded_cycle_wire_is_bounded_and_rejects_invalid_tagged_counts() {
    let mut inventory = InventoryGuard {
        revision: 7,
        selected: 8,
        item: I_CONFIGURATOR,
        count: 1,
        damage: 0,
        payload: 42,
    };
    let message = ClientMessage::Action {
        id: ActionId(1),
        action: Action::CycleToolMode {
            back: true,
            inventory,
        },
    };
    let bytes = net::encode_client(&message).unwrap();
    assert!(bytes.len() < 80);
    assert_eq!(net::decode_client(&bytes), Ok(message));
    inventory.count = 2;
    assert!(
        net::encode_client(&ClientMessage::Action {
            id: ActionId(1),
            action: Action::CycleToolMode {
                back: false,
                inventory
            }
        })
        .is_err()
    );
    inventory.count = 1;
    inventory.selected = 9;
    assert!(
        net::encode_client(&ClientMessage::Action {
            id: ActionId(1),
            action: Action::CycleToolMode {
                back: false,
                inventory
            }
        })
        .is_err()
    );
}
