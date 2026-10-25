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
