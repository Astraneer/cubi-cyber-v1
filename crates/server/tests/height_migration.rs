use core_types::{Cell, LocalIdx, PageId, SpaceId, Tick, TxId};
use r#gen::TerrainSeed;
use gen_authority::{PrivateSeed, ServerGenerator};
use server::{
    height_authority::{Config, HeightAuthority},
    height_index::{CellEdit, HeightEditIndex},
    height_migration::{CAPACITY, Error, LegacyWorker},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use world::{Transaction, World, lighting::SkyAccess};
fn page(space: u32, y: i32) -> PageId {
    PageId {
        space: SpaceId(space),
        x: 64,
        y,
        z: 0,
    }
}
fn position(page: PageId, index: LocalIdx) -> core_types::BlockPos {
    let (x, y, z) = index.xyz();
    core_types::BlockPos {
        space: page.space,
        x: page.x * 16 + i32::from(x),
        y: page.y * 16 + i32::from(y),
        z: page.z * 16 + i32::from(z),
    }
}
#[test]
fn worker_compares_real_private_baseline_with_two_retained_slots_and_late_live_edit_wins() {
    let generator = Arc::new(ServerGenerator::new(
        TerrainSeed(1337),
        PrivateSeed::new([37; 32]),
    ));
    let generated = generator.generate_page(page(0, 1), None).unwrap().page;
    let changed = generated
        .cells
        .iter()
        .position(|cell| *cell != Cell::AIR)
        .unwrap();
    let local = LocalIdx::new(changed as u16).unwrap();
    let mut actual = generated.cells;
    actual[changed] = Cell::AIR;
    let actual: Box<[Cell]> = actual;
    let mut world = World::new(rules::properties, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page(0, 1), actual);
    tx.create_page(page(0, 8), vec![Cell::AIR; 4096]);
    world.commit(tx).unwrap();
    let roof = position(page(0, 8), LocalIdx::new(0).unwrap());
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(roof, Cell::from_parts(rules::ids::WATER, 0));
    world.commit(tx).unwrap();
    let mut owner = HeightAuthority::new(Config::default()).unwrap();
    owner
        .begin_legacy(TxId(10), [page(0, 1), page(0, 8)])
        .unwrap();
    let mut worker = LegacyWorker::new(generator).unwrap();
    let main = std::thread::current().id();
    assert!(
        worker
            .try_submit(page(0, 1), world.snapshot(page(0, 1)).unwrap(), Tick(0))
            .unwrap()
    );
    assert!(
        !worker
            .try_submit(page(0, 1), world.snapshot(page(0, 1)).unwrap(), Tick(0))
            .unwrap()
    );
    assert!(
        worker
            .try_submit(page(0, 8), world.snapshot(page(0, 8)).unwrap(), Tick(0))
            .unwrap()
    );
    assert_eq!(worker.inflight(), CAPACITY);
    assert!(matches!(
        worker.try_submit(page(2, 0), world.snapshot(page(0, 8)).unwrap(), Tick(0)),
        Err(Error::Capacity)
    ));
    // A durable AIR change after the recovered snapshot supersedes late work.
    owner
        .apply_encoded_delta(
            &HeightEditIndex::encode_delta(
                TxId(11),
                &[CellEdit {
                    position: roof,
                    cell: Cell::AIR,
                }],
            )
            .unwrap(),
        )
        .unwrap();
    let started = Instant::now();
    let mut results = 0;
    while results < 2 {
        if let Some(result) = worker.try_recv().unwrap() {
            assert_ne!(result.worker_thread, main);
            assert_eq!(result.compared_cells, 4096);
            assert_eq!(result.differences.len(), 1);
            if result.page.y == 1 {
                assert_eq!(result.differences[0].position, position(page(0, 1), local));
                assert_eq!(result.differences[0].cell, Cell::AIR);
                assert_eq!(result.baseline_work.copied_cells, 4096);
            } else {
                assert_eq!(result.differences[0].position, roof);
                assert_eq!(result.differences[0].cell.id(), rules::ids::WATER);
                assert_eq!(result.baseline_work.copied_cells, 0);
            }
            owner
                .ingest_legacy_page(result.page, &result.differences)
                .unwrap();
            results += 1;
        } else {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(worker.inflight(), 0);
    assert_eq!(owner.index().pending_legacy_keys().count(), 0);
    owner.finish_legacy().unwrap();
    assert_eq!(owner.index().get(roof).unwrap().cell, Cell::AIR);
    assert_eq!(owner.blocker_columns(), 0);
    assert_eq!(
        owner
            .sky_proof(SpaceId(0), None)
            .unwrap()
            .incoming_from_unknown_above(core_types::BlockPos { y: 127, ..roof }),
        Some(15)
    );
    let recovered =
        HeightAuthority::restore(&owner.snapshot().unwrap(), Config::default()).unwrap();
    assert!(recovered.is_ready());
    assert_eq!(recovered.snapshot().unwrap(), owner.snapshot().unwrap());
}
#[test]
fn other_space_baseline_is_air_without_private_generation() {
    let generator = Arc::new(ServerGenerator::new(
        TerrainSeed(1337),
        PrivateSeed::new([37; 32]),
    ));
    let mut world = World::new(rules::properties, 16);
    let mut cells = vec![Cell::AIR; 4096];
    cells[4095] = Cell::from_parts(rules::ids::STONE, 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(2));
    tx.create_space(core_types::SpaceTransform::IDENTITY);
    tx.create_page(page(2, 0), cells);
    world.commit(tx).unwrap();
    let mut worker = LegacyWorker::new(generator).unwrap();
    worker
        .try_submit(page(2, 0), world.snapshot(page(2, 0)).unwrap(), Tick(0))
        .unwrap();
    let started = Instant::now();
    let result = loop {
        if let Some(result) = worker.try_recv().unwrap() {
            break result;
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(result.compared_cells, 4096);
    assert_eq!(result.baseline_work.copied_cells, 0);
    assert_eq!(result.differences.len(), 1);
    assert_eq!(
        result.differences[0].position,
        position(page(2, 0), LocalIdx::new(4095).unwrap())
    );
}

#[test]
fn the_same_worker_scan_returns_machine_and_cable_bootstrap_positions() {
    let generator = Arc::new(ServerGenerator::new(
        TerrainSeed(1337),
        PrivateSeed::new([37; 32]),
    ));
    let page = page(0, 8);
    let ids = [
        rules::ids::CHEST,
        rules::ids::FURNACE,
        rules::ids::UNIVERSAL_CABLE,
        rules::ids::LOGISTICAL_TRANSPORTER,
        rules::ids::MECHANICAL_PIPE,
        rules::ids::PRESSURIZED_TUBE,
    ];
    let mut cells = vec![Cell::AIR; 4096];
    for (i, id) in ids.into_iter().enumerate() {
        cells[i] = Cell::from_parts(id, 0);
    }
    cells[10] = Cell::from_parts(rules::ids::STONE, 0);
    let mut world = World::new(rules::properties, 16);
    let mut tx = Transaction::new(Tick(0), page.space);
    tx.create_page(page, cells);
    world.commit(tx).unwrap();
    let mut worker = LegacyWorker::new(generator).unwrap();
    worker
        .try_submit(page, world.snapshot(page).unwrap(), Tick(0))
        .unwrap();
    let started = Instant::now();
    let result = loop {
        if let Some(result) = worker.try_recv().unwrap() {
            break result;
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(result.compared_cells, 4096);
    assert_eq!(result.baseline_work.copied_cells, 0);
    assert_eq!(result.differences.len(), 7);
    assert_eq!(
        result.topology_positions,
        (0..6)
            .map(|i| { position(page, LocalIdx::new(i).unwrap()) })
            .collect::<Vec<_>>()
    );
    assert_ne!(result.worker_thread, std::thread::current().id());
}
