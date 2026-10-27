#[path = "../src/height_index.rs"]
#[allow(dead_code)]
mod index;
use core_types::{BlockPos, Cell, SpaceId, TxId};
use index::*;
use std::collections::BTreeSet;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

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
fn coordinates(capture: &TileEdits) -> BTreeSet<(i32, i32)> {
    capture.columns.iter().map(|c| (c.x, c.z)).collect()
}
fn assert_indices(index: &HeightEditIndex) {
    assert_eq!(index.column_index_entries(), index.column_count() * 2);
    assert_eq!(
        index.column_index_bytes(),
        index.column_count() * 2 * std::mem::size_of::<(SpaceId, i32, i32)>()
    );
}

#[test]
fn four_edges_and_corners_capture_complete_body_halo_with_exact_existing_caps() {
    let halo = [
        (-1, 7),
        (16, 7),
        (7, -1),
        (7, 16),
        (-1, -1),
        (-1, 16),
        (16, -1),
        (16, 16),
        (7, 7),
    ];
    let mut edits: Vec<_> = halo.iter().map(|&(x, z)| edit(x, 3, z, 3)).collect();
    edits.extend([
        edit(-2, 3, 7, 3),
        edit(17, 3, 7, 3),
        edit(7, 3, -2, 3),
        edit(7, 3, 17, 3),
    ]);
    edits.push(edit(-1, 4, 7, 0));
    edits.push(CellEdit {
        position: BlockPos {
            space: SpaceId(1),
            ..edit(-1, 3, 7, 3).position
        },
        cell: Cell::AIR,
    });
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    index.apply_committed(TxId(1), &edits).unwrap();
    assert_indices(&index);
    let before = index.encode_snapshot().unwrap();
    let capture = index.capture(key(0, 0, 0), 9, 10).unwrap();
    assert_eq!(coordinates(&capture), BTreeSet::from(halo));
    assert_eq!((capture.visited_columns, capture.visited_cells), (9, 10));
    for (x, z) in halo {
        assert!(
            capture.column(x, z).is_some(),
            "Morton output ordering lost ({x},{z})"
        );
    }
    assert_eq!(
        index.capture(key(0, 0, 0), 8, 10).unwrap_err(),
        IndexError::Capacity
    );
    assert_eq!(
        index.capture(key(0, 0, 0), 9, 9).unwrap_err(),
        IndexError::Capacity
    );
    assert_eq!(index.encode_snapshot().unwrap(), before);
}

fn affected_oracle(position: BlockPos) -> BTreeSet<TileKey> {
    let mut expected = BTreeSet::new();
    for level in 0..=MAX_LEVEL {
        let span = 16_i64 << level;
        let cx = i64::from(position.x).div_euclid(span);
        let cz = i64::from(position.z).div_euclid(span);
        // An independent geometric containment oracle, rather than the new
        // affected() quotient formula or counts produced by the implementation.
        for x in cx - 1..=cx + 1 {
            for z in cz - 1..=cz + 1 {
                let tile = key(level, x as i32, z as i32);
                let Some(b) = tile.bounds() else {
                    continue;
                };
                if (b[0] - 1..=b[2]).contains(&i64::from(position.x))
                    && (b[1] - 1..=b[3]).contains(&i64::from(position.z))
                {
                    expected.insert(tile);
                }
            }
        }
    }
    expected
}

#[test]
fn every_level_invalidates_exact_halo_dependencies_including_negative_cross_corners() {
    let mut positions = BTreeSet::new();
    for level in 0..=MAX_LEVEL {
        let span = 16_i32 << level;
        for origin in [0, -2 * span] {
            for (dx, dz) in [
                (0, 0),
                (span - 1, 0),
                (0, span - 1),
                (span - 1, span - 1),
                (1, 1),
                (span - 2, span - 2),
            ] {
                positions.insert((origin + dx, origin + dz));
            }
        }
    }
    for x in [i32::MIN, i32::MIN + 1, i32::MAX - 1, i32::MAX] {
        for z in [i32::MIN, i32::MIN + 1, i32::MAX - 1, i32::MAX] {
            positions.insert((x, z));
        }
    }
    for (x, z) in positions {
        let e = edit(x, 3, z, 3);
        let expected = affected_oracle(e.position);
        let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
        let work = index.apply_committed(TxId(1), &[e]).unwrap();
        assert_eq!(work.ancestors_touched, expected.len(), "edit ({x},{z})");
        assert!(work.ancestors_touched <= 44);
        assert_eq!(index.stamp_count(), expected.len());
        for level in 0..=MAX_LEVEL {
            let span = 16_i64 << level;
            let cx = i64::from(x).div_euclid(span);
            let cz = i64::from(z).div_euclid(span);
            for tx in cx - 1..=cx + 1 {
                for tz in cz - 1..=cz + 1 {
                    let tile = key(level, tx as i32, tz as i32);
                    if tile.bounds().is_some() {
                        assert_eq!(
                            index.source(tile).unwrap(),
                            if expected.contains(&tile) { 2 } else { 1 },
                            "edit ({x},{z}) tile {tile:?}"
                        );
                    }
                }
            }
        }
        // Neighbor dependency invalidation must not broaden exact body stamps.
        assert_eq!(index.body_page_source(e.position.page()), Ok(1));
        assert_indices(&index);
    }
}

#[test]
fn world_edge_halos_clip_in_i64_without_wrapping_foreign_columns() {
    let mut rows = vec![
        edit(i32::MIN, 3, 7, 3),
        edit(i32::MIN + 15, 3, 7, 3),
        edit(i32::MAX - 15, 3, 7, 3),
        edit(i32::MAX, 3, 7, 3),
        edit(7, 3, i32::MIN, 3),
        edit(7, 3, i32::MAX, 3),
    ];
    for x in [i32::MIN, i32::MAX] {
        for z in [i32::MIN, i32::MAX] {
            rows.push(edit(x, 3, z, 3));
        }
    }
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    index.apply_committed(TxId(1), &rows).unwrap();
    let mut tiles = vec![
        key(0, i32::MIN.div_euclid(16), 0),
        key(0, i32::MAX.div_euclid(16), 0),
        key(0, 0, i32::MIN.div_euclid(16)),
        key(0, 0, i32::MAX.div_euclid(16)),
    ];
    for x in [i32::MIN.div_euclid(16), i32::MAX.div_euclid(16)] {
        for z in [i32::MIN.div_euclid(16), i32::MAX.div_euclid(16)] {
            tiles.push(key(0, x, z));
        }
    }
    for tile in tiles {
        let b = tile.bounds().unwrap();
        let expected: BTreeSet<_> = rows
            .iter()
            .filter(|e| {
                (b[0] - 1..=b[2]).contains(&i64::from(e.position.x))
                    && (b[1] - 1..=b[3]).contains(&i64::from(e.position.z))
            })
            .map(|e| (e.position.x, e.position.z))
            .collect();
        let capture = index.capture(tile, 20, 20).unwrap();
        assert_eq!(coordinates(&capture), expected);
        assert_eq!(capture.visited_columns, expected.len());
        assert_eq!(capture.visited_cells, expected.len());
    }
}

#[test]
fn level10_sparse_halo_returns_actual_rows_between_sample_points_and_ignores_unrelated_ranges() {
    let tile = key(10, -2, 3);
    let [xmin, zmin, xmax, zmax] = tile.bounds().unwrap().map(|v| v as i32);
    let selected = [
        (xmin - 1, zmin + 7),
        (xmax, zmin + 11),
        (xmin + 9, zmin - 1),
        (xmin + 13, zmax),
        (xmin - 1, zmin - 1),
        (xmin - 1, zmax),
        (xmax, zmin - 1),
        (xmax, zmax),
        (xmin + 5, zmin + 5),
    ];
    let mut rows: Vec<_> = selected.iter().map(|&(x, z)| edit(x, 3, z, 3)).collect();
    for i in 0..1000 {
        rows.push(edit(xmin - 1, 3, zmax + 1 + i, 3));
        rows.push(edit(xmax + 1 + i, 3, zmin - 1, 3));
    }
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    index.apply_committed(TxId(1), &rows).unwrap();
    let capture = index.capture(tile, selected.len(), selected.len()).unwrap();
    assert_eq!(coordinates(&capture), BTreeSet::from(selected));
    // These are copied-row counts. Four ordered range queries, rather than a
    // 16,384-coordinate edge loop, provide the output-sensitive traversal.
    assert_eq!((capture.visited_columns, capture.visited_cells), (9, 9));
    assert_eq!(index.column_index_entries(), rows.len() * 2);
    assert_indices(&index);
}

#[test]
fn live_delta_snapshot_and_pending_legacy_restore_rebuild_exactly_two_keys_per_column() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let e = edit(-1, 3, 7, 3);
    index.apply_committed(TxId(1), &[e]).unwrap();
    let (tx, delta) = HeightEditIndex::decode_delta(
        &HeightEditIndex::encode_delta(TxId(2), &[edit(-1, 4, 7, 0)]).unwrap(),
        1,
    )
    .unwrap();
    index.apply_committed(tx, &delta).unwrap();
    index
        .apply_committed(TxId(3), &[edit(-1, 3, 7, 0)])
        .unwrap();
    assert_eq!(
        (
            index.column_count(),
            index.cell_count(),
            index.column_index_entries()
        ),
        (1, 2, 2)
    );
    assert_eq!(index.body_page_source(e.position.page()), Ok(3));
    assert_eq!(
        index.body_page_source(edit(0, 3, 7, 0).position.page()),
        Ok(0)
    );
    let bytes = index.encode_snapshot().unwrap();
    let reopened = HeightEditIndex::decode_snapshot(&bytes, IndexLimits::default()).unwrap();
    assert_eq!(reopened.encode_snapshot().unwrap(), bytes);
    assert_eq!(
        reopened.capture(key(0, 0, 0), 1, 2).unwrap().visited_cells,
        2
    );
    assert_indices(&reopened);

    let mut legacy = HeightEditIndex::new(IndexLimits::default()).unwrap();
    legacy.begin_legacy(TxId(10), [e.position.page()]).unwrap();
    legacy
        .apply_committed(TxId(11), &[edit(-1, 3, 7, 0)])
        .unwrap();
    let checkpoint = legacy.encode_snapshot().unwrap();
    let mut legacy = HeightEditIndex::decode_snapshot(&checkpoint, IndexLimits::default()).unwrap();
    assert_eq!(legacy.source(key(0, 0, 0)), Err(IndexError::MissingIndex));
    assert_indices(&legacy);
    legacy
        .ingest_legacy_page(e.position.page(), &[e, edit(-1, 4, 7, 3)])
        .unwrap();
    legacy.finish_legacy().unwrap();
    assert_eq!(legacy.get(e.position).unwrap().cell, Cell::AIR);
    assert_eq!(legacy.get(e.position).unwrap().source_tx, TxId(11));
    assert_eq!(legacy.source(key(0, 0, 0)), Ok(12));
    assert_eq!(legacy.body_page_source(e.position.page()), Ok(11));
    assert_eq!(
        legacy.body_page_source(edit(0, 3, 7, 0).position.page()),
        Ok(0)
    );
    assert_eq!(legacy.capture(key(0, 0, 0), 1, 2).unwrap().visited_cells, 2);
    assert_indices(&legacy);
}

fn old_checkpoint(schema: u16, duplicate_stamp: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(if schema == 1 { b"HIX1" } else { b"HIX2" });
    bytes.extend_from_slice(&schema.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&9u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&(11u32 + u32::from(duplicate_stamp)).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    if schema == 1 {
        for value in [0u32, (-1i32) as u32, 3, 7, Cell::from_parts(3, 0).raw()] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&7u64.to_le_bytes());
    } else {
        // One column, one run: (Space0,x-1,z7), y3,len1,Cell3,Tx7.
        for value in [
            0u32,
            (-1i32) as u32,
            7,
            1,
            3,
            1,
            Cell::from_parts(3, 0).raw(),
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&7u64.to_le_bytes());
    }
    let mut saved: Vec<_> = (0..=MAX_LEVEL).map(|level| key(level, -1, 0)).collect();
    if duplicate_stamp {
        saved.push(key(0, -1, 0));
    }
    for tile in saved {
        bytes.extend_from_slice(&tile.space.0.to_le_bytes());
        bytes.push(tile.level);
        bytes.extend_from_slice(&tile.x.to_le_bytes());
        bytes.extend_from_slice(&tile.z.to_le_bytes());
        // Private saved stamps are preserved independently of retained row Tx7.
        bytes.extend_from_slice(&10u64.to_le_bytes());
    }
    bytes
}

#[test]
fn old_hix1_and_hix2_preserve_saved_sources_and_bytes_while_new_halo_captures_invalidate() {
    for schema in [1, 2] {
        let old = old_checkpoint(schema, false);
        let index = HeightEditIndex::decode_snapshot(&old, IndexLimits::default()).unwrap();
        for level in 0..=MAX_LEVEL {
            assert_eq!(index.source(key(level, -1, 0)), Ok(10));
            assert_eq!(index.source(key(level, 0, 0)), Ok(1));
        }
        assert_eq!(index.stamp_count(), 11);
        assert_eq!(
            index.body_page_source(edit(-1, 3, 7, 3).position.page()),
            Ok(7)
        );
        assert_indices(&index);
        let encoded = index.encode_snapshot().unwrap();
        // HIX1 retains the existing codec's HIX2 encoding; existing HIX2 bytes
        // are unchanged. Derived column keys add no persisted records or stamps.
        assert_eq!(encoded, old_checkpoint(2, false));
        assert_eq!(&encoded[..4], b"HIX2");
        assert_eq!(
            u16::from_le_bytes(encoded[4..6].try_into().unwrap()),
            SCHEMA
        );
        let mut reopened =
            HeightEditIndex::decode_snapshot(&encoded, IndexLimits::default()).unwrap();
        assert_eq!(reopened.encode_snapshot().unwrap(), encoded);
        let captured = reopened.capture(key(0, 0, 0), 1, 1).unwrap();
        assert_eq!(captured.source, 1);
        assert_eq!(captured.columns.len(), 1);
        assert_eq!(captured.columns[0].cells[0].source_tx, TxId(7));
        // A restored owner has no surviving old image/capture. The new capture
        // includes the halo under its opaque saved source, even when source1
        // is numerically lower than row Tx7. Later transactions must stale it.
        reopened
            .apply_committed(TxId(10), &[edit(-1, 3, 7, 0)])
            .unwrap();
        for level in 0..=MAX_LEVEL {
            assert_eq!(reopened.source(key(level, -1, 0)), Ok(11));
            assert_eq!(reopened.source(key(level, 0, 0)), Ok(11));
        }
        assert_ne!(reopened.source(captured.key).unwrap(), captured.source);
        assert_eq!(reopened.column_index_entries(), 2);
        assert_eq!(
            reopened.body_page_source(edit(-1, 3, 7, 0).position.page()),
            Ok(10)
        );
        assert_eq!(
            reopened.body_page_source(edit(0, 3, 7, 0).position.page()),
            Ok(0)
        );
        assert!(matches!(
            HeightEditIndex::decode_snapshot(&old_checkpoint(schema, true), IndexLimits::default()),
            Err(IndexError::Malformed)
        ));
        assert!(matches!(
            HeightEditIndex::decode_snapshot(
                &old,
                IndexLimits {
                    max_stamps: 10,
                    ..IndexLimits::default()
                }
            ),
            Err(IndexError::Malformed)
        ));
        assert!(matches!(
            HeightEditIndex::decode_snapshot(
                &old,
                IndexLimits {
                    max_snapshot_bytes: encoded.len() - 1,
                    ..IndexLimits::default()
                }
            ),
            Err(IndexError::SnapshotTooLarge)
        ));
        let mut exact = HeightEditIndex::decode_snapshot(
            &old,
            IndexLimits {
                max_stamps: 11,
                max_snapshot_bytes: encoded.len(),
                ..IndexLimits::default()
            },
        )
        .unwrap();
        assert_eq!(exact.encode_snapshot().unwrap(), encoded);
        assert_eq!(
            exact.apply_committed(TxId(10), &[edit(-1, 3, 7, 0)]),
            Err(IndexError::Capacity)
        );
        assert_eq!(exact.encode_snapshot().unwrap(), encoded);
        assert_eq!(exact.column_index_entries(), 2);
        assert_eq!(exact.source(captured.key), Ok(1));
    }
}

#[test]
fn refused_live_batches_do_not_mutate_columns_indices_stamps_or_snapshot_bytes() {
    let mut index = HeightEditIndex::new(IndexLimits {
        max_columns: 1,
        max_cells: 1,
        ..IndexLimits::default()
    })
    .unwrap();
    index.apply_committed(TxId(1), &[edit(1, 3, 1, 0)]).unwrap();
    let before = index.encode_snapshot().unwrap();
    for rows in [vec![edit(2, 3, 1, 3)], vec![edit(1, 4, 1, 3)]] {
        assert_eq!(
            index.apply_committed(TxId(2), &rows),
            Err(IndexError::Capacity)
        );
        assert_eq!(index.encode_snapshot().unwrap(), before);
        assert_eq!(index.column_index_entries(), 2);
        assert_indices(&index);
    }
    let mut stamps = HeightEditIndex::decode_snapshot(
        &before,
        IndexLimits {
            max_stamps: 11,
            ..IndexLimits::default()
        },
    )
    .unwrap();
    assert_eq!(
        stamps.apply_committed(TxId(2), &[edit(15, 3, 1, 3)]),
        Err(IndexError::Capacity)
    );
    assert_eq!(stamps.encode_snapshot().unwrap(), before);
    assert_indices(&stamps);
    let mut bytes = HeightEditIndex::decode_snapshot(
        &before,
        IndexLimits {
            max_snapshot_bytes: before.len() + 19,
            ..IndexLimits::default()
        },
    )
    .unwrap();
    assert_eq!(
        bytes.apply_committed(TxId(2), &[edit(1, 4, 1, 3)]),
        Err(IndexError::SnapshotTooLarge)
    );
    assert_eq!(bytes.encode_snapshot().unwrap(), before);
    assert_indices(&bytes);
}

#[test]
fn ten_thousand_unchanged_dependency_queries_allocate_nothing() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let edits = (0..64).map(|x| edit(x, 70, -40, 0)).collect::<Vec<_>>();
    index.apply_committed(TxId(1), &edits).unwrap();
    let k = key(0, 3, -3);
    let before = (
        index.cell_count(),
        index.column_count(),
        index.stamp_count(),
        index.column_index_entries(),
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert_eq!(index.source(k), Ok(2));
        assert_eq!(index.column_index_entries(), 128);
        assert_eq!(index.column_index_bytes(), 64 * 24);
        assert!(index.get(edit(47, 70, -40, 0).position).is_some());
        assert_eq!(
            index.body_page_source(edit(47, 70, -40, 0).position.page()),
            Ok(1)
        );
    }
    assert_eq!(scope.finish(), telemetry::AllocationCounts::ZERO);
    assert_eq!(
        before,
        (
            index.cell_count(),
            index.column_count(),
            index.stamp_count(),
            index.column_index_entries()
        )
    );
}
