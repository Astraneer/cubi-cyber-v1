//! Real Store boundaries: durable drop component -> runtime rows -> inventory.
//! Publication/ACK is represented only after append() has completed its fsync.
use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use rules::{entities::EntityId, ids::*, stack::ItemStack, survival::Inventory};
use save::{Batch, RUNTIME_ITEMS, RuntimeRecord, SavedPlayer, Store};
use server::{
    entities::items::{Budget, ItemDelta, ItemRuntime, PickupPlayer, Snapshot},
    survival,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use world::{Transaction, World};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/test-item-durability");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn saved(inventory: Inventory) -> SavedPlayer {
    SavedPlayer {
        state: net::PlayerState {
            identity: net::PlayerId([9; 16]),
            health: 20.0,
            position: [2.5, 1.0, 2.5],
            ..Default::default()
        },
        place_ready_at: Tick(0),
        break_ready_at: Tick(0),
        inventory: inventory.encode().to_vec(),
        window: Vec::new(),
        simulation: Vec::new(),
    }
}
fn total(world: &World, items: &ItemRuntime, inventory: &Inventory) -> u32 {
    let durable: u32 = world
        .components()
        .filter(|(_, c)| c.data.kind == survival::DROP_COMPONENT)
        .map(|(_, c)| {
            survival::DropBundle::decode(&c.data.payload)
                .unwrap()
                .entries
                .iter()
                .map(|e| u32::from(e.stack.count()))
                .sum::<u32>()
        })
        .sum();
    durable
        + items
            .entities()
            .rows()
            .iter()
            .map(|e| u32::from(e.stack.count()))
            .sum::<u32>()
        + inventory
            .slots()
            .iter()
            .map(|e| u32::from(e.count()))
            .sum::<u32>()
}

#[test]
fn actual_snapshot_wal_reopen_at_every_drop_import_pickup_boundary_conserves_items() {
    for boundary in 0..=2 {
        let directory = Directory::new();
        let (mut store, _) = Store::open(&directory.0).unwrap();
        let mut world = World::new(rules::properties, 100);
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        tx.create_page(
            PageId {
                space: SpaceId(0),
                x: 0,
                y: 0,
                z: 0,
            },
            (0..PAGE_VOLUME)
                .map(|index| Cell::from_parts(if index / 256 == 0 { STONE } else { AIR }, 0))
                .collect::<Vec<_>>(),
        );
        world.commit(tx).unwrap();
        let drop = survival::plan_drops(
            &world,
            BlockPos {
                space: SpaceId(0),
                x: 2,
                y: 1,
                z: 2,
            },
            [9; 16],
            1,
            Tick(0),
            &[ItemStack::new(COBBLE, 5).unwrap()],
            0,
            None,
        )
        .unwrap();
        let drop_id = drop.id;
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        drop.append_to(&mut tx);
        world.commit(tx).unwrap();
        let mut items = ItemRuntime::new(10, 1337).unwrap();
        let mut player = PickupPlayer {
            id: EntityId(9),
            space: SpaceId(0),
            position: [2.5, 1.0, 2.5],
            crouching: false,
            alive: true,
            inventory: Inventory::empty(),
        };
        store
            .checkpoint(&save::Snapshot {
                record_sequence: 0,
                server_tick: Tick(0),
                public_seed: 1337,
                generator_version: 4,
                rules_version: 1,
                world: world.checkpoint().to_restore_state(),
                identities: Vec::new(),
                players: vec![saved(player.inventory)],
                dedup: Vec::new(),
                runtimes: vec![RuntimeRecord {
                    kind: RUNTIME_ITEMS,
                    payload: items.snapshot().encode().unwrap(),
                }],
            })
            .unwrap();
        if boundary >= 1 {
            let plan = items
                .plan_import(&world, &[drop_id], Tick(1), true)
                .unwrap();
            items.validate_import(&world, &plan).unwrap();
            assert_eq!(total(&world, &items, &player.inventory), 5);
            let mut batch = Batch::empty(Tick(1));
            for tx in plan.transactions {
                batch.changes.push(world.commit(tx).unwrap());
            }
            batch.runtimes.push(RuntimeRecord {
                kind: RUNTIME_ITEMS,
                payload: plan.delta.encode().unwrap(),
            });
            store.append(&batch).unwrap();
            items.acknowledge(plan.delta).unwrap();
        }
        if boundary >= 2 {
            let plan = items
                .plan_tick(&world, Tick(2), &[player], Budget::default())
                .unwrap();
            items.validate(&world, &[player], &plan).unwrap();
            assert_eq!(plan.work.picked_items, 5);
            assert_eq!(player.inventory, Inventory::empty());
            let mut batch = Batch::empty(Tick(2));
            batch.runtimes.push(RuntimeRecord {
                kind: RUNTIME_ITEMS,
                payload: plan.delta.encode().unwrap(),
            });
            for patch in &plan.inventories {
                batch.players.push(saved(patch.after));
            }
            store.append(&batch).unwrap();
            for patch in plan.inventories {
                player.inventory = patch.after;
            }
            items.acknowledge(plan.delta).unwrap();
        }
        assert_eq!(total(&world, &items, &player.inventory), 5);
        std::mem::drop(store);
        let (_, recovery) = Store::open(&directory.0).unwrap();
        let snapshot = recovery.snapshot.unwrap();
        let mut restored_world = World::restore(
            rules::properties,
            world::JournalLimits::default(),
            snapshot.world,
        )
        .unwrap();
        let record = snapshot
            .runtimes
            .iter()
            .find(|r| r.kind == RUNTIME_ITEMS)
            .unwrap();
        let mut restored_items =
            ItemRuntime::restore(10, &Snapshot::decode(&record.payload, 10).unwrap()).unwrap();
        let mut restored_inventory = Inventory::decode(&snapshot.players[0].inventory).unwrap();
        for (_, batch) in recovery.batches {
            for change in &batch.changes {
                restored_world.apply_recovered_change(change).unwrap();
            }
            for record in batch.runtimes {
                restored_items
                    .acknowledge(ItemDelta::decode(&record.payload, 10).unwrap())
                    .unwrap();
            }
            for player in batch.players {
                restored_inventory = Inventory::decode(&player.inventory).unwrap();
            }
        }
        assert_eq!(restored_world.canonical_hash(), world.canonical_hash());
        assert_eq!(restored_items.snapshot(), items.snapshot());
        assert_eq!(restored_inventory, player.inventory);
        assert_eq!(
            total(&restored_world, &restored_items, &restored_inventory),
            5
        );
        assert_eq!(restored_world.component(drop_id).is_some(), boundary == 0);
        assert_eq!(restored_items.entities().len(), usize::from(boundary == 1));
        assert_eq!(
            restored_inventory.hand().count(),
            if boundary == 2 { 5 } else { 0 }
        );
    }
}
