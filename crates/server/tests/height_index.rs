#[path = "../src/height_index.rs"]
#[allow(dead_code)]
mod index;
use core_types::{BlockPos, Cell, SpaceId, TxId};
use index::*;
fn edit(x: i32, y: i32, z: i32, id: u16) -> CellEdit {
    CellEdit {
        position: BlockPos {
            space: SpaceId(0),
            x,
            y,
            z,
        },
        cell: Cell::from_parts(id, 0),
    }
}
fn key(level: u8, x: i32, z: i32) -> TileKey {
    TileKey {
        space: SpaceId(0),
        level,
        x,
        z,
    }
}
#[test]
fn air_overrides_survive_snapshot_and_deltas_touch_only_actual_ancestors() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let far = key(9, 1, 0);
    assert_eq!(index.source(far).unwrap(), 1);
    let work = index
        .apply_committed(TxId(1), &[edit(16384, 2, 0, 0), edit(-17, 4, -17, 3)])
        .unwrap();
    assert_eq!(work.world_reads, 0);
    assert_eq!(work.cells_written, 2);
    assert!(work.ancestors_touched <= 88);
    assert_eq!(
        index.get(edit(16384, 2, 0, 0).position).unwrap().cell,
        Cell::AIR
    );
    let source = index.source(far).unwrap();
    assert_eq!(source, 2);
    assert_eq!(index.source(key(9, -2, 2)).unwrap(), 1);
    index.apply_committed(TxId(2), &[]).unwrap();
    assert_eq!(index.source(far).unwrap(), source);
    let before = index.encode_snapshot().unwrap();
    let mut restored = HeightEditIndex::decode_snapshot(&before, IndexLimits::default()).unwrap();
    assert_eq!(restored.encode_snapshot().unwrap(), before);
    let delta = HeightEditIndex::encode_delta(TxId(3), &[edit(16384, 2, 0, 3)]).unwrap();
    let (tx, edits) = HeightEditIndex::decode_delta(&delta, 10).unwrap();
    restored.apply_committed(tx, &edits).unwrap();
    assert_eq!(restored.get(edits[0].position).unwrap().cell.id(), 3);
    assert_eq!(restored.source(far).unwrap(), 4);
    assert_eq!(
        restored.source(key(9, 2, 0)).unwrap(),
        4,
        "shared sample boundary neighbor must be dirtied"
    );
}
#[test]
fn morton_capture_never_scans_unrelated_columns_and_includes_shared_samples() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let mut edits = (0..1000)
        .map(|z| edit(1, 0, 10000 + z, 3))
        .collect::<Vec<_>>();
    edits.extend([
        edit(1, 0, 1, 0),
        edit(16, 0, 0, 3),
        edit(0, 0, 16, 3),
        edit(16, 0, 16, 3),
        edit(-17, 0, -17, 3),
    ]);
    index.apply_committed(TxId(1), &edits).unwrap();
    let c = index.capture(key(0, 0, 0), 4, 4).unwrap();
    assert_eq!(c.visited_columns, 4);
    assert_eq!(c.visited_cells, 4);
    assert_eq!(c.columns.len(), 4);
    assert!(c.column(16, 16).is_some());
    assert_eq!(
        index.capture(key(0, 0, 0), 3, 4).unwrap_err(),
        IndexError::Capacity
    );
    let negative = index.capture(key(0, -2, -2), 1, 1).unwrap();
    assert_eq!(negative.columns[0].x, -17);
    assert_eq!(negative.columns[0].z, -17);
}
#[test]
fn missing_legacy_index_stays_unknown_and_live_edits_win_old_background_results() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let old = edit(1, 2, 1, 3);
    let page = old.position.page();
    index.begin_legacy(TxId(10), [page]).unwrap();
    assert_eq!(index.source(key(0, 0, 0)), Err(IndexError::MissingIndex));
    assert_eq!(index.finish_legacy(), Err(IndexError::MissingIndex));
    index
        .apply_committed(TxId(11), &[edit(1, 2, 1, 0)])
        .unwrap();
    index
        .ingest_legacy_page(page, &[old, edit(2, 2, 2, 3)])
        .unwrap();
    assert_eq!(index.get(old.position).unwrap().cell, Cell::AIR);
    index.finish_legacy().unwrap();
    assert!(index.is_ready());
    assert_eq!(index.capture(key(0, 0, 0), 2, 2).unwrap().columns.len(), 2);
    let bytes = index.encode_snapshot().unwrap();
    assert!(
        HeightEditIndex::decode_snapshot(&bytes, IndexLimits::default())
            .unwrap()
            .is_ready()
    );
}
#[test]
fn capacity_and_malformed_inputs_preserve_previous_index() {
    let limits = IndexLimits {
        max_columns: 1,
        max_cells: 1,
        max_stamps: 64,
        max_snapshot_bytes: 10000,
    };
    let mut index = HeightEditIndex::new(limits).unwrap();
    index.apply_committed(TxId(1), &[edit(1, 1, 1, 0)]).unwrap();
    let before = index.encode_snapshot().unwrap();
    assert_eq!(
        index.apply_committed(TxId(2), &[edit(2, 1, 1, 3)]),
        Err(IndexError::Capacity)
    );
    assert_eq!(index.encode_snapshot().unwrap(), before);
    let delta = HeightEditIndex::encode_delta(TxId(2), &[edit(1, 1, 1, 3)]).unwrap();
    for n in 0..delta.len() {
        assert!(HeightEditIndex::decode_delta(&delta[..n], 10).is_err());
    }
    for n in 0..before.len() {
        assert!(HeightEditIndex::decode_snapshot(&before[..n], limits).is_err());
    }
    assert_eq!(
        index.apply_committed(TxId(2), &[edit(1, 1, 1, 3), edit(1, 1, 1, 0)]),
        Err(IndexError::DuplicateEdit)
    );
    assert_eq!(index.encode_snapshot().unwrap(), before);
}

#[test]
fn large_legacy_air_regions_fit_the_same_checkpoint_quota_without_losing_live_versions() {
    let limits = IndexLimits {
        max_columns: 65536,
        max_cells: 1048576,
        max_stamps: 262144,
        max_snapshot_bytes: 8 * 1024 * 1024,
    };
    let mut index = HeightEditIndex::new(limits).unwrap();
    let pages = (0..5)
        .flat_map(|z| {
            (0..5).flat_map(move |x| {
                (0..5).map(move |y| core_types::PageId {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                })
            })
        })
        .collect::<Vec<_>>();
    index.begin_legacy(TxId(10), pages.iter().copied()).unwrap();
    let live = edit(17, 31, 17, 3);
    index.apply_committed(TxId(11), &[live]).unwrap();
    for page in pages {
        let edits = (0..16)
            .flat_map(|z| {
                (0..16).flat_map(move |x| {
                    (0..16).map(move |y| edit(page.x * 16 + x, page.y * 16 + y, page.z * 16 + z, 0))
                })
            })
            .collect::<Vec<_>>();
        index.ingest_legacy_page(page, &edits).unwrap();
    }
    assert_eq!(index.cell_count(), 512000);
    assert_eq!(index.get(live.position).unwrap().cell, live.cell);
    assert_eq!(index.get(live.position).unwrap().source_tx, TxId(11));
    index.finish_legacy().unwrap();
    let source = index.source(key(0, 1, 1)).unwrap();
    let bytes = index.encode_snapshot().unwrap();
    assert_eq!(&bytes[..4], b"HIX2");
    assert!(bytes.len() < 512000, "flat HIX1 would need over 14 MiB");
    let restored = HeightEditIndex::decode_snapshot(&bytes, limits).unwrap();
    assert_eq!(restored.cell_count(), 512000);
    assert_eq!(restored.get(live.position), index.get(live.position));
    assert_eq!(restored.source(key(0, 1, 1)).unwrap(), source);
    for position in [(0, 0, 0), (79, 79, 79), (17, 32, 17)] {
        let p = edit(position.0, position.1, position.2, 0).position;
        assert_eq!(restored.get(p).unwrap().cell, Cell::AIR);
        assert_eq!(restored.get(p).unwrap().source_tx, TxId(10));
    }
    assert_eq!(restored.encode_snapshot().unwrap(), bytes);
}

#[test]
fn run_split_quota_is_preflighted_atomically_and_same_cell_different_transactions_stay_distinct() {
    let base = [edit(1, 0, 1, 0), edit(1, 1, 1, 0), edit(1, 2, 1, 0)];
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    index.apply_committed(TxId(1), &base).unwrap();
    let before = index.encode_snapshot().unwrap();
    let limits = IndexLimits {
        max_snapshot_bytes: before.len() + 39,
        ..IndexLimits::default()
    };
    let mut bounded = HeightEditIndex::decode_snapshot(&before, limits).unwrap();
    // Even AIR->AIR cannot merge across TxId: a late migration needs the exact
    // version to preserve a newer edit, including an explicit deletion.
    assert_eq!(
        bounded.apply_committed(TxId(2), &[edit(1, 1, 1, 0)]),
        Err(IndexError::SnapshotTooLarge)
    );
    assert_eq!(bounded.encode_snapshot().unwrap(), before);
    let limits = IndexLimits {
        max_snapshot_bytes: before.len() + 40,
        ..IndexLimits::default()
    };
    let mut exact = HeightEditIndex::decode_snapshot(&before, limits).unwrap();
    exact.apply_committed(TxId(2), &[edit(1, 1, 1, 0)]).unwrap();
    let split = exact.encode_snapshot().unwrap();
    assert_eq!(split.len(), before.len() + 40);
    let restored = HeightEditIndex::decode_snapshot(&split, limits).unwrap();
    assert_eq!(restored.get(base[1].position).unwrap().source_tx, TxId(2));
    assert_eq!(restored.get(base[0].position).unwrap().source_tx, TxId(1));
    // One batch can also merge both edges; no whole-column recount is needed.
    index
        .apply_committed(TxId(2), &[base[2], base[0], base[1]])
        .unwrap();
    assert_eq!(index.encode_snapshot().unwrap().len(), before.len());
}

#[test]
fn hix1_checkpoints_upgrade_without_changing_cell_or_tile_provenance() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"HIX1");
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&9u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for (y, tx) in [(i32::MIN, 3u64), (i32::MAX, 7)] {
        for word in [0, 1, y as u32, 1, 0] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&tx.to_le_bytes());
    }
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&0i32.to_le_bytes());
    bytes.extend_from_slice(&0i32.to_le_bytes());
    bytes.extend_from_slice(&8u64.to_le_bytes());
    let index = HeightEditIndex::decode_snapshot(&bytes, IndexLimits::default()).unwrap();
    assert_eq!(
        index
            .get(edit(1, i32::MIN, 1, 0).position)
            .unwrap()
            .source_tx,
        TxId(3)
    );
    assert_eq!(
        index
            .get(edit(1, i32::MAX, 1, 0).position)
            .unwrap()
            .source_tx,
        TxId(7)
    );
    assert_eq!(index.source(key(0, 0, 0)).unwrap(), 8);
    let upgraded = index.encode_snapshot().unwrap();
    assert_eq!(&upgraded[..4], b"HIX2");
    let restored = HeightEditIndex::decode_snapshot(&upgraded, IndexLimits::default()).unwrap();
    assert_eq!(
        restored.get(edit(1, i32::MIN, 1, 0).position),
        index.get(edit(1, i32::MIN, 1, 0).position)
    );
    assert_eq!(restored.source(key(0, 0, 0)).unwrap(), 8);
    for n in 0..upgraded.len() {
        assert!(HeightEditIndex::decode_snapshot(&upgraded[..n], IndexLimits::default()).is_err());
    }
    let mut malformed = upgraded.clone();
    // First column, first run: zero length, overflow, duplicate/overlap, or an
    // unknown future TxId must fail before a partially decoded index escapes.
    malformed[56..60].copy_from_slice(&0u32.to_le_bytes());
    assert!(HeightEditIndex::decode_snapshot(&malformed, IndexLimits::default()).is_err());
    malformed.clone_from(&upgraded);
    malformed[76..80].copy_from_slice(&2u32.to_le_bytes());
    assert!(HeightEditIndex::decode_snapshot(&malformed, IndexLimits::default()).is_err());
    malformed.clone_from(&upgraded);
    malformed[84..92].copy_from_slice(&10u64.to_le_bytes());
    assert!(HeightEditIndex::decode_snapshot(&malformed, IndexLimits::default()).is_err());
}
