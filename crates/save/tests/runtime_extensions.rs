use core_types::Tick;
use save::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use world::World;
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/test-runtime-save");
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
fn player() -> SavedPlayer {
    SavedPlayer {
        state: net::PlayerState::default(),
        place_ready_at: Tick(0),
        break_ready_at: Tick(0),
        inventory: Vec::new(),
        window: Vec::new(),
        simulation: Vec::new(),
    }
}
fn snapshot(world: &World) -> Snapshot {
    Snapshot {
        record_sequence: 0,
        server_tick: Tick(0),
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
fn record(kind: u16) -> RuntimeRecord {
    RuntimeRecord {
        kind,
        payload: vec![kind as u8, 1, 2, 3],
    }
}

#[test]
fn absent_extensions_keep_exact_legacy_prefix_and_each_extension_is_optional() {
    let mut batch = Batch::empty(Tick(1));
    batch.players.push(player());
    let legacy = encode_batch(&batch).unwrap();
    let old = decode_batch(&legacy).unwrap();
    assert!(old.runtimes.is_empty());
    assert!(old.players[0].window.is_empty());
    assert!(old.players[0].inventory.is_empty());
    batch.runtimes = vec![record(RUNTIME_ITEMS), record(RUNTIME_SURVIVAL)];
    let current = encode_batch(&batch).unwrap();
    assert!(current.starts_with(&legacy));
    let decoded = decode_batch(&current).unwrap();
    assert_eq!(
        decoded.runtimes,
        [record(RUNTIME_SURVIVAL), record(RUNTIME_ITEMS)]
    );
    assert!(decoded.players[0].window.is_empty());
    assert!(!batch.is_empty());
    let world = World::new(rules::properties, 4);
    let mut state = snapshot(&world);
    let legacy = encode_snapshot(&state).unwrap();
    state.runtimes = vec![record(RUNTIME_MOBS)];
    let current = encode_snapshot(&state).unwrap();
    assert!(current.starts_with(&legacy));
    assert_eq!(decode_snapshot(&current).unwrap().runtimes, state.runtimes);
}
#[test]
fn inventory_window_and_runtime_roundtrip_preserve_cursor_context_bytes() {
    let mut batch = Batch::empty(Tick(1));
    batch.players.push(player());
    let legacy = encode_batch(&batch).unwrap();
    batch.players[0].inventory = rules::survival::Inventory::empty().encode().to_vec();
    let inventory_prefix = encode_batch(&batch).unwrap();
    batch.players[0].window = vec![0x5a; 407];
    batch.runtimes = vec![
        record(RUNTIME_SURVIVAL),
        record(RUNTIME_MOBS),
        record(RUNTIME_ITEMS),
        record(RUNTIME_MACHINES),
        record(RUNTIME_HEIGHT),
        record(RUNTIME_CHAT),
    ];
    let current = encode_batch(&batch).unwrap();
    assert!(current.starts_with(&legacy));
    assert!(current.starts_with(&inventory_prefix));
    let restored = decode_batch(&current).unwrap();
    assert_eq!(restored.players[0].inventory, batch.players[0].inventory);
    assert_eq!(restored.players[0].window, batch.players[0].window);
    assert_eq!(restored.runtimes, batch.runtimes);
    let world = World::new(rules::properties, 4);
    let mut state = snapshot(&world);
    state.players = batch.players;
    state.runtimes = batch.runtimes;
    let restored = decode_snapshot(&encode_snapshot(&state).unwrap()).unwrap();
    assert_eq!(restored.players[0].window, state.players[0].window);
    assert_eq!(restored.runtimes, state.runtimes);
}
#[test]
fn window_works_without_inventory_and_rejects_oversize_duplicate_or_wrong_owner() {
    let mut batch = Batch::empty(Tick(1));
    batch.players.push(player());
    let prefix = encode_batch(&batch).unwrap();
    batch.players[0].window = vec![7; 4096];
    let current = encode_batch(&batch).unwrap();
    assert!(current.starts_with(&prefix));
    assert_eq!(
        decode_batch(&current).unwrap().players[0].window.len(),
        4096
    );
    batch.players[0].window.push(1);
    assert!(matches!(encode_batch(&batch), Err(SaveError::Bounds(_))));
    for (owners, count) in [([0u32, 0], 2u32), ([1u32, 0], 1u32)] {
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(b"WND1");
        bytes.extend_from_slice(&count.to_le_bytes());
        for owner in owners.into_iter().take(count as usize) {
            bytes.extend_from_slice(&owner.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.push(7);
        }
        assert!(decode_batch(&bytes).is_err());
    }
}
#[test]
fn runtime_unknown_duplicate_empty_count_and_payload_bounds_rejected() {
    for kinds in [vec![0], vec![7], vec![1, 1], vec![1, 2, 3, 4, 4]] {
        let mut batch = Batch::empty(Tick(1));
        batch.runtimes = kinds.into_iter().map(record).collect();
        assert!(encode_batch(&batch).is_err());
    }
    let mut batch = Batch::empty(Tick(1));
    batch.runtimes = vec![RuntimeRecord {
        kind: 1,
        payload: Vec::new(),
    }];
    assert!(encode_batch(&batch).is_err());
    batch.runtimes[0].payload = vec![0; MAX_RUNTIME_BYTES + 1];
    assert!(matches!(encode_batch(&batch), Err(SaveError::Bounds(_))));
    let prefix = encode_batch(&Batch::empty(Tick(1))).unwrap();
    for (kind, length) in [(0, 1), (7, 1), (1, MAX_RUNTIME_BYTES + 1)] {
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(b"RUN1");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(kind as u16).to_le_bytes());
        bytes.extend_from_slice(&(length as u32).to_le_bytes());
        bytes.push(1);
        assert!(decode_batch(&bytes).is_err());
    }
}
#[test]
fn runtime_global_record_cap_and_four_snapshot_records_have_distinct_limits() {
    let records: Vec<_> = (1..=4)
        .map(|kind| RuntimeRecord {
            kind,
            payload: vec![kind as u8; MAX_RUNTIME_BYTES],
        })
        .collect();
    let mut batch = Batch::empty(Tick(1));
    batch.runtimes = records.clone();
    assert!(matches!(encode_batch(&batch), Err(SaveError::Bounds(_))));
    let world = World::new(rules::properties, 4);
    let mut state = snapshot(&world);
    state.runtimes = records;
    let encoded = encode_snapshot(&state).unwrap();
    let restored = decode_snapshot(&encoded).unwrap();
    assert_eq!(restored.runtimes.len(), 4);
    assert!(
        restored
            .runtimes
            .iter()
            .all(|record| record.payload.len() == MAX_RUNTIME_BYTES)
    );
}
#[test]
fn malformed_extension_order_and_unknown_tags_never_become_empty_state() {
    let mut batch = Batch::empty(Tick(1));
    batch.runtimes = vec![record(1)];
    let current = encode_batch(&batch).unwrap();
    for suffix in [
        b"WND1".as_slice(),
        b"INV1".as_slice(),
        b"RUN1".as_slice(),
        b"WHAT".as_slice(),
    ] {
        let mut corrupt = current.clone();
        corrupt.extend_from_slice(suffix);
        corrupt.extend_from_slice(&0u32.to_le_bytes());
        assert!(decode_batch(&corrupt).is_err());
    }
    let mut corrupt = encode_batch(&Batch::empty(Tick(1))).unwrap();
    corrupt.extend_from_slice(b"WH");
    assert!(decode_batch(&corrupt).is_err());
}
#[test]
fn window_and_runtime_share_real_durable_batch_and_reopen_exactly() {
    let directory = Directory::new();
    let (mut store, _) = Store::open(&directory.0).unwrap();
    let world = World::new(rules::properties, 4);
    store.checkpoint(&snapshot(&world)).unwrap();
    let mut batch = Batch::empty(Tick(1));
    batch.players.push(player());
    batch.players[0].window = vec![4; 407];
    batch.runtimes = vec![record(1), record(2), record(3), record(4)];
    assert_eq!(store.append(&batch).unwrap(), 1);
    drop(store);
    let (_, recovery) = Store::open(&directory.0).unwrap();
    assert_eq!(recovery.batches.len(), 1);
    assert_eq!(recovery.batches[0].1.players[0].window, vec![4; 407]);
    assert_eq!(recovery.batches[0].1.runtimes, batch.runtimes);
}
#[test]
fn actual_background_checkpoint_preserves_runtime_snapshot_and_window() {
    let directory = Directory::new();
    let (store, _) = Store::open(&directory.0).unwrap();
    let world = World::new(rules::properties, 4);
    let writer = SaveWorker::spawn(store, 2).unwrap();
    let mut saved = player();
    saved.window = vec![8; 407];
    writer
        .try_submit(WriteRequest::Checkpoint {
            ticket: 7,
            checkpoint: world.checkpoint(),
            metadata: SnapshotMetadata {
                server_tick: Tick(0),
                public_seed: 1337,
                generator_version: 4,
                rules_version: 1,
                identities: Vec::new(),
                players: vec![saved],
                dedup: Vec::new(),
                runtimes: vec![record(RUNTIME_MOBS), record(RUNTIME_ITEMS)],
            },
        })
        .unwrap();
    let start = Instant::now();
    loop {
        if let Ok(completion) = writer.try_complete() {
            assert_eq!(completion.ticket, 7);
            assert!(completion.result.is_ok());
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::yield_now();
    }
    drop(writer);
    let (_, recovery) = Store::open(&directory.0).unwrap();
    let snapshot = recovery.snapshot.unwrap();
    assert_eq!(snapshot.players[0].window, vec![8; 407]);
    assert_eq!(
        snapshot.runtimes,
        [record(RUNTIME_MOBS), record(RUNTIME_ITEMS)]
    );
}
