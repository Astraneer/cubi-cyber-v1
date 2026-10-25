use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick, Version};
use net::{ActionId, ActionReply, ActionStatus, PlayerId};
use save::*;
use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use world::{JournalLimits, Transaction, World};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/test-save");
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
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    }
}
fn position() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 1,
        y: 2,
        z: 3,
    }
}
fn world() -> World {
    let mut world = World::new(rules::properties, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        page(),
        vec![Cell::from_parts(rules::ids::STONE, 0); PAGE_VOLUME],
    );
    world.commit(tx).unwrap();
    world
}
fn snapshot(world: &World, sequence: u64) -> Snapshot {
    Snapshot {
        record_sequence: sequence,
        server_tick: world.tick(),
        public_seed: 1337,
        generator_version: 4,
        rules_version: 1,
        world: world.checkpoint().to_restore_state(),
        identities: Vec::new(),
        players: Vec::new(),
        dedup: Vec::new(),
        runtimes: Vec::new(),
    }
}
fn edit(world: &mut World, tick: u64) -> world::WorldChange {
    let mut tx = Transaction::new(Tick(tick), SpaceId(0));
    tx.set_cell(position(), Cell::from_parts(rules::ids::DIRT, tick as u16));
    world.commit(tx).unwrap()
}

#[test]
fn snapshot_and_durable_tail_restore_exact_state_and_action_results() {
    let directory = Directory::new();
    let (mut store, recovery) = Store::open(&directory.0).unwrap();
    assert!(recovery.snapshot.is_none());
    assert!(recovery.batches.is_empty());
    let mut world = world();
    store.checkpoint(&snapshot(&world, 0)).unwrap();
    let first = edit(&mut world, 1);
    let mut batch = Batch::empty(Tick(1));
    batch.changes.push(first);
    store.append(&batch).unwrap();
    store.checkpoint(&snapshot(&world, 1)).unwrap();
    let change = edit(&mut world, 2);
    let tx = change.tx;
    let mut batch = Batch::empty(Tick(2));
    batch.changes.push(change);
    let reply = ActionReply {
        id: ActionId(17),
        status: ActionStatus::Accepted,
        transaction: Some(tx),
        source_version: world.version(),
        position: Some(position()),
        cell: Some(Cell::from_parts(rules::ids::DIRT, 2)),
    };
    batch.actions.push((PlayerId([7; 16]), reply));
    assert_eq!(store.append(&batch).unwrap(), 2);
    let expected = world.canonical_hash();
    drop(store);
    let (_store, recovery) = Store::open(&directory.0).unwrap();
    assert_eq!(recovery.truncated_tail_bytes, 0);
    assert_eq!(recovery.batches.len(), 1);
    let mut restored = World::restore(
        rules::properties,
        JournalLimits::default(),
        recovery.snapshot.unwrap().world,
    )
    .unwrap();
    for (_, batch) in recovery.batches {
        for change in batch.changes {
            restored.apply_recovered_change(&change).unwrap();
        }
        assert_eq!(batch.actions, [(PlayerId([7; 16]), reply)]);
    }
    assert_eq!(restored.canonical_hash(), expected);
}

#[test]
fn incomplete_unacknowledged_tail_is_truncated_but_complete_corruption_is_fatal() {
    let directory = Directory::new();
    let (mut store, _) = Store::open(&directory.0).unwrap();
    let mut world = world();
    store.checkpoint(&snapshot(&world, 0)).unwrap();
    let mut batch = Batch::empty(Tick(1));
    batch.changes.push(edit(&mut world, 1));
    store.append(&batch).unwrap();
    drop(store);
    let path = directory.0.join("world.wal");
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"partial-tail")
        .unwrap();
    let (store, recovery) = Store::open(&directory.0).unwrap();
    assert_eq!(recovery.truncated_tail_bytes, 12);
    assert_eq!(recovery.batches.len(), 1);
    drop(store);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::Start(82 + 14)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(matches!(
        Store::open(&directory.0),
        Err(SaveError::Checksum)
    ));
}

#[test]
fn snapshot_checksum_and_exclusive_writer_are_enforced() {
    let directory = Directory::new();
    let (mut store, _) = Store::open(&directory.0).unwrap();
    assert!(matches!(
        Store::open(&directory.0),
        Err(SaveError::AlreadyOpen)
    ));
    store.checkpoint(&snapshot(&world(), 0)).unwrap();
    drop(store);
    let path = directory.0.join("world.snapshot");
    let mut bytes = std::fs::read(&path).unwrap();
    let index = bytes.len() - 1;
    bytes[index] ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(matches!(
        Store::open(&directory.0),
        Err(SaveError::Checksum)
    ));
}

#[test]
fn malformed_lengths_pending_results_and_rule_changes_do_not_silently_open() {
    for bytes in [vec![], vec![0; 4], vec![255; 128]] {
        assert!(decode_snapshot(&bytes).is_err());
        assert!(decode_batch(&bytes).is_err());
    }
    let mut batch = Batch::empty(Tick(0));
    batch.actions.push((
        PlayerId([1; 16]),
        ActionReply {
            id: ActionId(1),
            status: ActionStatus::Pending,
            transaction: None,
            source_version: Version(0),
            position: None,
            cell: None,
        },
    ));
    let directory = Directory::new();
    let (mut store, _) = Store::open(&directory.0).unwrap();
    assert!(store.append(&batch).is_err());
    assert_eq!(store.last_sequence(), 0);
    assert!(require_versions(snapshot(&world(), 0), 5, 1, None).is_err());
    struct ExplicitMigration;
    impl Migration for ExplicitMigration {
        fn migrate(
            &self,
            mut snapshot: Snapshot,
            generator_version: u32,
            rules_version: u32,
        ) -> Result<Snapshot, SaveError> {
            snapshot.generator_version = generator_version;
            snapshot.rules_version = rules_version;
            Ok(snapshot)
        }
    }
    assert_eq!(
        require_versions(snapshot(&world(), 0), 5, 2, Some(&ExplicitMigration))
            .unwrap()
            .generator_version,
        5
    );
}

#[test]
fn private_generation_entropy_is_persistent_bounded_and_not_snapshot_metadata() {
    let directory = Directory::new();
    let (store, _) = Store::open(&directory.0).unwrap();
    let key = store
        .load_or_create_private_entropy(|| Ok([37; 32]))
        .unwrap();
    assert_eq!(key, [37; 32]);
    assert_eq!(
        store
            .load_or_create_private_entropy(|| panic!("existing key must not be regenerated"))
            .unwrap(),
        key
    );
    let data = encode_snapshot(&snapshot(&world(), 0)).unwrap();
    assert!(!data.windows(32).any(|window| window == key));
    drop(store);
    std::fs::write(directory.0.join("generation.key"), [1, 2, 3]).unwrap();
    let (store, _) = Store::open(&directory.0).unwrap();
    assert!(
        store
            .load_or_create_private_entropy(|| Ok([9; 32]))
            .is_err()
    );
    drop(store);
    std::fs::remove_file(directory.0.join("generation.key")).unwrap();
    let (mut store, _) = Store::open(&directory.0).unwrap();
    store.checkpoint(&snapshot(&world(), 0)).unwrap();
    assert!(
        store
            .load_or_create_private_entropy(|| panic!(
                "existing world must restore its private seed"
            ))
            .is_err()
    );
}

#[test]
fn same_frontier_checkpoint_crash_recovers_matching_previous_snapshot_and_repairs_anchor() {
    let directory = Directory::new();
    let (mut store, _) = Store::open(&directory.0).unwrap();
    let world = world();
    store.checkpoint(&snapshot(&world, 0)).unwrap();
    let old_wal = std::fs::read(directory.0.join("world.wal")).unwrap();
    let mut newer = snapshot(&world, 0);
    newer.server_tick = Tick(200);
    store.checkpoint(&newer).unwrap();
    drop(store);
    // Installed snapshot + previous WAL models interruption between the two
    // atomic replacements, with no intervening transaction/record sequence.
    std::fs::write(directory.0.join("world.wal"), &old_wal).unwrap();
    let (mut store, recovery) = Store::open(&directory.0).unwrap();
    let recovered = recovery.snapshot.unwrap();
    assert_eq!(recovered.server_tick, Tick(0));
    assert_eq!(recovered.world.version, world.version());
    // Recovery repaired the primary, so repeating this crash window remains
    // recoverable instead of overwriting the only matching backup.
    let mut newer = snapshot(&world, 0);
    newer.server_tick = Tick(400);
    store.checkpoint(&newer).unwrap();
    drop(store);
    std::fs::write(directory.0.join("world.wal"), old_wal).unwrap();
    let (_store, recovery) = Store::open(&directory.0).unwrap();
    assert_eq!(recovery.snapshot.unwrap().server_tick, Tick(0));
}

#[test]
fn bounded_inventory_extension_preserves_m6_empty_accounts_and_roundtrips_owned_stacks() {
    let mut batch = Batch::empty(Tick(1));
    batch.players.push(SavedPlayer {
        state: net::PlayerState {
            identity: PlayerId([8; 16]),
            health: 20.0,
            ..Default::default()
        },
        simulation: vec![1, 2, 3],
        place_ready_at: Tick(5),
        break_ready_at: Tick(7),
        inventory: Vec::new(),
        window: Vec::new(),
    });
    let legacy = encode_batch(&batch).unwrap();
    assert!(
        decode_batch(&legacy).unwrap().players[0]
            .inventory
            .is_empty()
    );
    let mut inventory = rules::survival::Inventory::empty();
    inventory
        .set_trusted(
            0,
            rules::stack::ItemStack::new(rules::items::block_item(rules::ids::STONE), 3).unwrap(),
        )
        .unwrap();
    batch.players[0].inventory = inventory.encode().to_vec();
    let current = encode_batch(&batch).unwrap();
    assert!(current.starts_with(&legacy));
    let decoded = decode_batch(&current).unwrap();
    assert_eq!(
        rules::survival::Inventory::decode(&decoded.players[0].inventory).unwrap(),
        inventory
    );
    let mut state = snapshot(&world(), 0);
    state.players = batch.players;
    let encoded = encode_snapshot(&state).unwrap();
    assert_eq!(
        decode_snapshot(&encoded).unwrap().players[0].inventory,
        state.players[0].inventory
    );
    let mut corrupt = current.clone();
    corrupt[legacy.len()] = 0;
    assert!(decode_batch(&corrupt).is_err());
    let mut oversized = batch_empty_inventory();
    oversized.players[0].inventory = vec![0; 513];
    assert!(encode_batch(&oversized).is_err());
}
fn batch_empty_inventory() -> Batch {
    let mut batch = Batch::empty(Tick(1));
    batch.players.push(SavedPlayer {
        state: net::PlayerState::default(),
        simulation: Vec::new(),
        place_ready_at: Tick(0),
        break_ready_at: Tick(0),
        inventory: Vec::new(),
        window: Vec::new(),
    });
    batch
}

#[test]
fn background_writer_drains_queued_writes_and_returns_durability_tickets() {
    let directory = Directory::new();
    let (mut store, _) = Store::open(&directory.0).unwrap();
    let mut world = world();
    store.checkpoint(&snapshot(&world, 0)).unwrap();
    let writer = SaveWorker::spawn(store, 2).unwrap();
    let mut batch = Batch::empty(Tick(1));
    batch.changes.push(edit(&mut world, 1));
    writer
        .try_submit(WriteRequest::Batch { ticket: 99, batch })
        .unwrap_or_else(|_| panic!("queue has space"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match writer.try_complete() {
            Ok(completion) => {
                assert_eq!(completion.ticket, 99);
                assert!(matches!(
                    completion.result,
                    Ok(Durability::Batch { sequence: 1 })
                ));
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            Err(_) => panic!("writer disconnected"),
        }
    }
    drop(writer);
    let (_store, recovery) = Store::open(&directory.0).unwrap();
    assert_eq!(recovery.batches.len(), 1);
}
