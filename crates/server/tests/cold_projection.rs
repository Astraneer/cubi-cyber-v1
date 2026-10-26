use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, TxId};
use gen_authority::{PrivateSeed, ServerGenerator};
use server::{
    cold_projection::{ColumnKey, ColumnSnapshot, Error, Limits},
    height_index::{CellEdit, HeightEditIndex, IndexLimits},
};
use world::{LightSample, Transaction, World};
fn generator() -> ServerGenerator {
    ServerGenerator::new(r#gen::TerrainSeed(1), PrivateSeed::new([37; 32]))
}
fn index() -> HeightEditIndex {
    HeightEditIndex::new(IndexLimits::default()).unwrap()
}
fn p(key: ColumnKey, x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: key.x * 16 + x,
        y,
        z: key.z * 16 + z,
    }
}
fn page(key: ColumnKey, y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: key.x,
        y,
        z: key.z,
    }
}
fn real_world(g: &ServerGenerator, key: ColumnKey) -> World {
    let mut world = World::new(rules::properties, 128);
    for dz in -1..=1 {
        for dx in -1..=1 {
            let col = g.generate_column(key.x + dx, key.z + dz).unwrap();
            let mut tx = Transaction::new(Tick(0), SpaceId(0));
            for y in 0..8 {
                let id = PageId {
                    x: key.x + dx,
                    z: key.z + dz,
                    ..page(key, y)
                };
                let cells: Box<[Cell]> = col.page(id, None).unwrap().cells;
                tx.create_page(id, cells);
            }
            world.commit(tx).unwrap();
        }
    }
    world
}
#[test]
fn cold_and_resident_private_authority_use_identical_sealed_projection_without_world_births() {
    let g = generator();
    let public = r#gen::Generator::new(r#gen::TerrainSeed(1));
    let mut chosen = None;
    'find: for z in -4..=4 {
        for x in -4..=4 {
            let a = g.generate_column(x, z).unwrap();
            let b = public.generate_column(x, z).unwrap();
            let mut air = 0;
            let mut lava = 0;
            for y in 1..48 {
                for lz in 1..17 {
                    for lx in 1..17 {
                        let raw = a.block_padded(lx, y, lz);
                        if raw == 0 && b.block_padded(lx, y, lz) != 0 {
                            air += 1
                        }
                        if raw == rules::ids::LAVA {
                            lava += 1
                        }
                    }
                }
            }
            if air > 0 && lava > 0 {
                chosen = Some(ColumnKey { x, z });
                break 'find;
            }
        }
    }
    let key = chosen.expect("private fixture must include actual AIR/lava features");
    let mut resident = real_world(&g, key);
    let mut idx = index();
    let edits = [
        CellEdit {
            position: p(key, 2, 10, 2),
            cell: Cell::AIR,
        },
        CellEdit {
            position: p(key, 8, 61, 8),
            cell: Cell::from_parts(rules::ids::WATER, 8),
        },
        CellEdit {
            position: p(key, -1, 49, 5),
            cell: Cell::from_parts(rules::ids::WATER, 5),
        },
    ];
    idx.apply_committed(TxId(1), &edits).unwrap();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    for e in edits {
        tx.set_cell(e.position, e.cell);
    }
    resident.commit(tx).unwrap();
    let cold = World::new(rules::properties, 16);
    let version = cold.version();
    let snapshot = ColumnSnapshot::capture(&cold, &idx, key, Limits::default()).unwrap();
    assert_eq!(snapshot.capture_work().snapshot_lookups, 90);
    assert_eq!(snapshot.capture_work().index_captures, 9);
    assert!(snapshot.certificate().matches(&cold, &idx));
    let built = snapshot.execute(&g, || false).unwrap();
    assert_eq!(built.work.generated_columns, 9);
    assert_eq!(built.work.tint_samples, 256);
    assert_eq!(built.work.projected_geometry_cells, 8 * 4096);
    assert_eq!(built.work.halo_samples, 8 * 5832);
    assert_eq!(built.work.resident_cell_reads, 0);
    assert_eq!(cold.pages().count(), 0);
    assert_eq!(cold.version(), version);
    assert!(built.certificate.matches(&cold, &idx));
    assert!(
        !built.certificate.matches(&resident, &idx),
        "materialization changes the private certificate even when the public body matches"
    );
    // The internal canonical generator remains private/exact. Both immutable
    // source forms must now apply the same exterior-only PUBLIC policy.
    let resident_projection = ColumnSnapshot::capture(&resident, &idx, key, Limits::default())
        .unwrap()
        .execute(&g, || false)
        .unwrap();
    assert_eq!(resident_projection.work.generated_columns, 0);
    assert_eq!(built.work.public_baseline_columns, 9);
    assert_eq!(built.work.exterior.authority_reads, 48 * 48 * 128);
    assert!(built.work.exterior.closed_transparent_cells > 0);
    for (cold, actual) in built.pages.iter().zip(&resident_projection.pages) {
        assert_eq!(cold.geometry, actual.geometry);
        assert_eq!(cold.water, actual.water);
        assert_eq!(cold.sealed_cells, actual.sealed_cells);
    }
}
#[test]
fn resident_dynamic_state_wins_and_tombstones_outside_profile_unknown_and_all_source_guards_are_exact()
 {
    let g = generator();
    let key = ColumnKey { x: 0, z: 0 };
    let mut w = World::new(rules::properties, 128);
    let mut idx = index();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for y in 3..8 {
        tx.create_page(page(key, y), vec![Cell::AIR; 4096]);
    }
    tx.create_page(
        page(key, -1),
        vec![Cell::from_parts(rules::ids::STONE, 0); 4096],
    );
    tx.create_page(page(key, 2), vec![Cell::AIR; 4096]);
    w.commit(tx).unwrap();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.remove_page(page(key, 2));
    tx.set_cell(p(key, 4, 49, 4), Cell::from_parts(rules::ids::WATER, 7));
    w.commit(tx).unwrap();
    // Deliberately stale persisted baseline values must never override the real
    // immutable resident page; actual caller normally supplies the durable pair.
    idx.apply_committed(
        TxId(1),
        &[CellEdit {
            position: p(key, 4, 49, 4),
            cell: Cell::from_parts(rules::ids::WATER, 0),
        }],
    )
    .unwrap();
    let snapshot = ColumnSnapshot::capture(&w, &idx, key, Limits::default()).unwrap();
    let cert = snapshot.certificate().clone();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_light(
        p(key, 4, 49, 4),
        LightSample {
            sky: 15,
            block: 7,
            rgb: [1, 2, 3],
        },
    );
    w.commit(tx).unwrap();
    assert!(cert.matches(&w, &idx));
    let built = snapshot.execute(&g, || false).unwrap();
    assert!(
        built.pages[2].geometry.is_none(),
        "removed page cannot be silently regenerated"
    );
    assert_eq!(
        built.pages[3].water[5 + 18 * (5 + 18 * 2)],
        3,
        "resident amount1 overrides saved source8"
    );
    assert_eq!(
        built.pages[0].water[19], // local[0,-1,0] in X,Z,Y halo order
        2,
        "residentY-1 source participates in halo"
    );
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.set_cell(p(key, 4, 49, 4), Cell::from_parts(rules::ids::STONE, 0));
    w.commit(tx).unwrap();
    assert!(!cert.matches(&w, &idx));
    let fresh = ColumnSnapshot::capture(&w, &idx, key, Limits::default()).unwrap();
    idx.apply_committed(
        TxId(2),
        &[CellEdit {
            position: p(key, 16, 5, 16),
            cell: Cell::AIR,
        }],
    )
    .unwrap();
    assert!(!fresh.certificate().matches(&w, &idx));
    let fresh = ColumnSnapshot::capture(&w, &idx, key, Limits::default()).unwrap();
    let mut tx = Transaction::new(Tick(4), SpaceId(0));
    tx.create_page(page(key, 0), vec![Cell::AIR; 4096]);
    w.commit(tx).unwrap();
    assert!(!fresh.certificate().matches(&w, &idx));
}
#[test]
fn cold_capture_respects_edit_output_limits_cancellation_and_unready_index() {
    let key = ColumnKey { x: 0, z: 0 };
    let w = World::new(rules::properties, 16);
    let mut idx = index();
    idx.apply_committed(
        TxId(1),
        &[CellEdit {
            position: p(key, 3, 3, 3),
            cell: Cell::AIR,
        }],
    )
    .unwrap();
    assert!(matches!(
        ColumnSnapshot::capture(
            &w,
            &idx,
            key,
            Limits {
                max_edit_cells: 0,
                ..Limits::default()
            }
        ),
        Err(Error::Capacity)
    ));
    let snapshot = ColumnSnapshot::capture(&w, &idx, key, Limits::default()).unwrap();
    assert!(matches!(
        snapshot.execute(&generator(), || true),
        Err(Error::Cancelled)
    ));
    let mut unready = index();
    unready.begin_legacy(TxId(1), [page(key, 0)]).unwrap();
    assert!(matches!(
        ColumnSnapshot::capture(&w, &unready, key, Limits::default()),
        Err(Error::IndexNotReady)
    ));
    assert!(matches!(
        ColumnSnapshot::capture(&w, &idx, ColumnKey { x: i32::MAX, z: 0 }, Limits::default()),
        Err(Error::Bounds)
    ));
    assert_eq!(w.pages().count(), 0);
}
#[test]
fn immutable_capture_and_actual_native_worker_ignore_unrelated_edits_and_reject_late_index_changes()
{
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };
    let w = World::new(rules::properties, 16);
    let mut idx = index();
    let unrelated: Vec<_> = (0..10000)
        .map(|i| CellEdit {
            position: BlockPos {
                space: SpaceId(0),
                x: 10000 + i,
                y: 2,
                z: 20000,
            },
            cell: Cell::AIR,
        })
        .collect();
    idx.apply_committed(TxId(1), &unrelated).unwrap();
    let key = ColumnKey { x: 0, z: 0 };
    let snapshot = ColumnSnapshot::capture(&w, &idx, key, Limits::default()).unwrap();
    assert_eq!(snapshot.capture_work().captured_edit_cells, 0);
    assert_eq!(snapshot.capture_work().index_captures, 9);
    let g = Arc::new(generator());
    let main = thread::current().id();
    let pool = jobs::NativePool::new(
        1,
        2,
        move |snapshot: Box<ColumnSnapshot>, cancel: &jobs::CancellationToken| {
            snapshot
                .execute(&g, || cancel.is_cancelled())
                .map(|built| (thread::current().id(), built))
                .map_err(|e| e.to_string())
        },
    )
    .unwrap();
    let spec = jobs::JobSpec::derived(
        jobs::JobKey {
            page: page(key, 0),
            kind: 97,
        },
        jobs::Priority::Normal,
        jobs::WorkCost {
            cells: 8 * (4096 + 5832),
            quads: 0,
            bytes: 16 * 1024 * 1024,
            rays: 0,
        },
        512 * 1024,
        Tick(1),
    );
    assert!(
        pool.try_submit(jobs::Dispatch {
            id: jobs::JobId(1),
            spec,
            cancellation: jobs::CancellationToken::new(),
            payload: Box::new(snapshot)
        })
        .is_ok()
    );
    idx.apply_committed(
        TxId(2),
        &[CellEdit {
            position: p(key, 2, 2, 2),
            cell: Cell::AIR,
        }],
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(done) = pool.try_recv() {
            let jobs::JobOutcome::Success((worker, built)) = done.outcome else {
                panic!("worker failed")
            };
            assert_ne!(worker, main);
            assert!(!built.certificate.matches(&w, &idx));
            assert_eq!(built.work.generated_columns, 9);
            assert_eq!(w.pages().count(), 0);
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(100));
    }
}
#[test]
fn dense_below_profile_and_resident_overrides_do_not_exhaust_cold_snapshot_quota() {
    let key = ColumnKey { x: 0, z: 0 };
    let mut idx = index();
    let mut edits = Vec::new();
    for z in -16..32 {
        for x in -16..32 {
            for y in -30..0 {
                edits.push(CellEdit {
                    position: BlockPos {
                        space: SpaceId(0),
                        x,
                        y,
                        z,
                    },
                    cell: Cell::from_parts(rules::ids::WATER, 0),
                });
            }
        }
    }
    assert_eq!(edits.len(), 69120);
    idx.apply_committed(TxId(1), &edits).unwrap();
    idx.apply_committed(
        TxId(2),
        &[CellEdit {
            position: p(key, 4, 61, 4),
            cell: Cell::from_parts(rules::ids::WATER, 8),
        }],
    )
    .unwrap();
    let mut world = World::new(rules::properties, 16);
    let cold = ColumnSnapshot::capture(&world, &idx, key, Limits::default()).unwrap();
    assert_eq!(cold.capture_work().captured_edit_cells, 1);
    assert!(cold.owned_input_bytes() < 16384);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for y in 3..8 {
        tx.create_page(page(key, y), vec![Cell::AIR; 4096]);
    }
    world.commit(tx).unwrap();
    let resident = ColumnSnapshot::capture(&world, &idx, key, Limits::default()).unwrap();
    assert_eq!(resident.capture_work().captured_edit_cells, 0);
    let built = resident.execute(&generator(), || false).unwrap();
    assert_eq!(built.pages[3].water[5 + 18 * (5 + 18 * 14)], 1);
}
#[test]
fn exterior_flood_certifies_whole_neighbour_interiors_and_resident_only_mode_never_fills_unknown() {
    let g = generator();
    let key = ColumnKey { x: 0, z: 0 };
    let index = index();
    let mut world = real_world(&g, key);
    let captured = ColumnSnapshot::capture(&world, &index, key, Limits::default()).unwrap();
    let neighbour = p(key, 24, 40, 8);
    let old = match world.read(neighbour) {
        world::CellRead::Known(c) => c,
        _ => unreachable!(),
    };
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(
        neighbour,
        if old == Cell::AIR {
            Cell::from_parts(rules::ids::STONE, 0)
        } else {
            Cell::AIR
        },
    );
    world.commit(tx).unwrap();
    assert!(
        !captured.certificate().matches(&world, &index),
        "an interior neighbour opening can change exterior connectivity without touching a halo border"
    );
    let empty = World::new(rules::properties, 16);
    let built = ColumnSnapshot::capture(
        &empty,
        &index,
        key,
        Limits {
            allow_cold: false,
            ..Limits::default()
        },
    )
    .unwrap()
    .execute(&g, || false)
    .unwrap();
    assert_eq!(built.work.generated_columns, 0);
    assert!(built.pages.iter().all(|p| p.geometry.is_none()
        && p.sealed_cells.is_none()
        && p.water.iter().all(|&c| c == 0)));
    assert_eq!(empty.pages().count(), 0);
}
