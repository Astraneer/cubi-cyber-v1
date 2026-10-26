use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick};
use server::far_water::capture::{Error, Snapshot};
use world::{ComponentData, LightSample, Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn page(x: i32, y: i32, z: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z,
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
fn terrain() -> r#gen::Generator {
    r#gen::Generator::new(r#gen::TerrainSeed(1337))
}
fn idx(x: i32, y: i32, z: i32) -> usize {
    (x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize)
}
fn world27() -> World {
    let mut w = World::new(rules::properties, 256);
    for y in -1..=1 {
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        for z in -1..=1 {
            for x in -1..=1 {
                tx.create_page(page(x, y, z), vec![Cell::AIR; 4096]);
            }
        }
        w.commit(tx).unwrap();
    }
    w
}
#[test]
fn exact_halo_codes_amount_falling_diagonals_public_tints_and_no_private_payload() {
    let mut w = world27();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    for state in 0..=15 {
        tx.set_cell(
            pos(state as i32, 4, 5),
            Cell::from_parts(rules::ids::WATER, state),
        );
    }
    tx.set_cell(pos(-1, -1, -1), Cell::from_parts(rules::ids::IRON_ORE, 0));
    tx.set_cell(pos(16, 16, 16), Cell::from_parts(rules::ids::STONE, 0));
    tx.set_cell(pos(3, 3, 3), Cell::from_parts(rules::ids::LAVA, 0));
    tx.set_cell(pos(4, 3, 3), Cell::from_parts(rules::ids::GLASS, 0));
    tx.set_component(
        ComponentId(888),
        ComponentData {
            kind: 777,
            anchor: pos(1, 1, 1),
            payload: b"NEVER_PUBLISH_PRIVATE_COMPONENT".to_vec(),
        },
        ChannelMask::CONTENTS,
    );
    w.commit(tx).unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    let s = Snapshot::capture(&w, page(0, 0, 0)).unwrap();
    let measured = scope.finish();
    assert_eq!(measured.allocating_calls(), 0);
    assert_eq!(s.missing().count(), 0);
    assert!(s.certificate().matches(&w));
    let built = s.classify(&terrain(), || false).unwrap();
    for state in 0..=15 {
        let expected = if state < 8 { 10 - state as u8 } else { 18 };
        assert_eq!(built.cells[idx(state, 4, 5)], expected);
    }
    assert_eq!(built.cells[idx(-1, -1, -1)], 2);
    assert_eq!(built.cells[idx(16, 16, 16)], 2);
    assert_eq!(built.cells[idx(3, 3, 3)], 1);
    assert_eq!(built.cells[idx(4, 3, 3)], 1);
    assert!(built.cells.iter().all(|&v| v <= 18));
    assert_eq!(built.work.classified_cells, 5832);
    assert_eq!(built.work.tint_samples, 256);
    for z in 0..16 {
        for x in 0..16 {
            assert_eq!(
                built.tints[x + 16 * z],
                terrain().column_info(x as i32, z as i32).tint
            );
        }
    }
}
#[test]
fn sampled_input_certificate_ignores_light_and_unsampled_neighbor_interior_but_guards_corner_and_presence()
 {
    let mut w = world27();
    let s = Snapshot::capture(&w, page(0, 0, 0)).unwrap();
    let cert = s.certificate().clone();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_light(
        pos(16, 16, 16),
        LightSample {
            sky: 15,
            block: 4,
            rgb: [1, 2, 3],
        },
    );
    tx.set_cell(pos(20, 20, 20), Cell::from_parts(rules::ids::STONE, 0));
    w.commit(tx).unwrap();
    assert!(cert.matches(&w));
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(pos(16, 16, 16), Cell::from_parts(rules::ids::WATER, 5));
    w.commit(tx).unwrap();
    assert!(!cert.matches(&w));
    let fresh = Snapshot::capture(&w, page(0, 0, 0)).unwrap();
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.remove_page(page(-1, 1, -1));
    w.commit(tx).unwrap();
    assert!(!fresh.certificate().matches(&w));
    let missing = Snapshot::capture(&w, page(0, 0, 0)).unwrap();
    assert_eq!(missing.missing().collect::<Vec<_>>(), vec![page(-1, 1, -1)]);
    let frozen = missing.certificate().clone();
    let b = missing.classify(&terrain(), || false).unwrap();
    assert_eq!(b.cells[idx(-1, 16, -1)], 0);
    let mut tx = Transaction::new(Tick(4), SpaceId(0));
    tx.create_page(page(-1, 1, -1), vec![Cell::AIR; 4096]);
    w.commit(tx).unwrap();
    assert!(!frozen.matches(&w));
}
#[test]
fn absent_center_and_cancelled_jobs_do_not_generate_or_invent_air() {
    let mut w = World::new(rules::properties, 32);
    assert!(matches!(
        Snapshot::capture(&w, page(0, 0, 0)),
        Err(Error::MissingCenter)
    ));
    assert_eq!(w.pages().count(), 0);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        page(0, 0, 0),
        vec![Cell::from_parts(rules::ids::WATER, 0); 4096],
    );
    w.commit(tx).unwrap();
    let s = Snapshot::capture(&w, page(0, 0, 0)).unwrap();
    assert_eq!(s.missing().count(), 26);
    assert!(matches!(
        s.classify(&terrain(), || true),
        Err(Error::Cancelled)
    ));
    let b = Snapshot::capture(&w, page(0, 0, 0))
        .unwrap()
        .classify(&terrain(), || false)
        .unwrap();
    assert_eq!(b.cells[idx(-1, 1, 1)], 0);
    assert_eq!(b.cells[idx(0, 1, 1)], 10);
    assert_eq!(b.work.classified_cells, 4096);
    assert_eq!(w.pages().count(), 1);
}
