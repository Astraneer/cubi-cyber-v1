use core_types::{BlockPos, Cell, CellProperties, ChannelMask, PAGE_VOLUME, PageId, SpaceId, Tick};
use world::{JournalLimits, PageRegion, Transaction, World};

const GEOMETRY: ChannelMask = ChannelMask(ChannelMask::SHAPE.0 | ChannelMask::OPACITY.0);
fn properties(cell: Cell) -> CellProperties {
    if cell == Cell::AIR {
        CellProperties::AIR
    } else {
        CellProperties {
            shape: cell.raw(),
            ..CellProperties::SOLID
        }
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
fn point([x, y, z]: [i32; 3]) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn regions() -> Vec<PageRegion> {
    (-1..=1)
        .flat_map(|y| {
            (-1..=1).flat_map(move |z| (-1..=1).map(move |x| PageRegion::new([x, y, z]).unwrap()))
        })
        .collect()
}
fn initialized() -> World {
    let mut world = World::new(properties, 32);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page(), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(tx).unwrap();
    world
}

#[test]
fn interior_edit_preserves_all_26_rim_inputs_and_snapshot_versions() {
    let mut world = initialized();
    let snapshot = world.snapshot(page()).unwrap();
    let inputs: Vec<_> = regions()
        .into_iter()
        .map(|region| world.capture_sampled_input(page(), GEOMETRY, region))
        .collect();
    let whole = world.capture_input(page(), GEOMETRY);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(point([8, 8, 8]), Cell::from_raw(1));
    let change = world.commit(tx).unwrap();
    assert_eq!(change.work.pages_staged, 1);
    assert_eq!(change.work.mask_cells_classified, 1);
    assert_eq!(change.work.registry_queries, 3);
    assert_eq!(change.work.rim_stamps_updated, 0);
    assert!(!world.matches_inputs(&[whole]));
    for input in inputs {
        assert_eq!(
            world.matches_inputs(&[input]),
            input.region != PageRegion::WHOLE
        );
        assert_eq!(
            snapshot.sampled_version(GEOMETRY, input.region),
            input.version
        );
    }
}

#[test]
fn each_boundary_cell_updates_only_its_planes_edges_and_corner() {
    for changed_region in regions()
        .into_iter()
        .filter(|region| *region != PageRegion::WHOLE)
    {
        let coordinates = changed_region.coordinates().map(|axis| match axis {
            -1 => 0,
            1 => 15,
            _ => 8,
        });
        let mut world = initialized();
        let inputs: Vec<_> = regions()
            .into_iter()
            .map(|region| world.capture_sampled_input(page(), GEOMETRY, region))
            .collect();
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.set_cell(point(coordinates), Cell::from_raw(1));
        let change = world.commit(tx).unwrap();
        let boundary_axes = changed_region
            .coordinates()
            .into_iter()
            .filter(|axis| *axis != 0)
            .count();
        assert_eq!(change.work.rim_stamps_updated, (1 << boundary_axes) - 1);
        assert_eq!(change.work.pages_staged, 1);
        assert_eq!(change.work.mask_cells_classified, 1);
        assert_eq!(change.work.registry_queries, 3);
        for input in inputs {
            let affected = input.region == PageRegion::WHOLE
                || input
                    .region
                    .coordinates()
                    .into_iter()
                    .zip(changed_region.coordinates())
                    .all(|(sample, changed)| sample == 0 || sample == changed);
            assert_eq!(
                !world.matches_inputs(&[input]),
                affected,
                "changed={changed_region:?}, sampled={:?}",
                input.region
            );
        }
    }
}

#[test]
fn failed_transaction_preserves_boundary_stamps_and_duplicate_writes_stamp_once() {
    let mut world = initialized();
    let before: Vec<_> = regions()
        .into_iter()
        .map(|region| world.capture_sampled_input(page(), GEOMETRY, region))
        .collect();
    let mut bad = Transaction::new(Tick(1), SpaceId(0));
    bad.set_cell(point([0, 0, 0]), Cell::from_raw(1));
    bad.set_cell(point([100, 0, 0]), Cell::from_raw(1));
    assert!(world.commit(bad).is_err());
    assert!(world.matches_inputs(&before));
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(point([0, 0, 0]), Cell::from_raw(1));
    tx.set_cell(point([0, 0, 0]), Cell::from_raw(2));
    tx.set_cell(point([0, 0, 0]), Cell::from_raw(1));
    let change = world.commit(tx).unwrap();
    assert_eq!(change.cells.len(), 1);
    assert_eq!(change.work.rim_stamps_updated, 7);
}

#[test]
fn create_remove_recreate_never_reuses_a_rim_version() {
    let region = PageRegion::new([-1, 1, -1]).unwrap();
    let mut world = World::new(properties, 32);
    let unknown = world.capture_sampled_input(page(), GEOMETRY, region);
    let mut create = Transaction::new(Tick(0), SpaceId(0));
    create.create_page(page(), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(create).unwrap();
    assert!(!world.matches_inputs(&[unknown]));
    let known = world.capture_sampled_input(page(), GEOMETRY, region);
    let mut remove = Transaction::new(Tick(1), SpaceId(0));
    remove.remove_page(page());
    world.commit(remove).unwrap();
    assert!(!world.matches_inputs(&[known]));
    let removed = world.capture_sampled_input(page(), GEOMETRY, region);
    let mut recreate = Transaction::new(Tick(2), SpaceId(0));
    recreate.create_page(page(), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(recreate).unwrap();
    assert!(!world.matches_inputs(&[removed]));
    assert!(!world.matches_inputs(&[known]));
}

#[test]
fn restore_reinitializes_derived_rims_without_changing_logical_hash_or_schema() {
    let mut world = initialized();
    let region = PageRegion::new([-1, 0, 0]).unwrap();
    let old_rim = world.capture_sampled_input(page(), GEOMETRY, region);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(point([8, 8, 8]), Cell::from_raw(1));
    world.commit(tx).unwrap();
    assert!(world.matches_inputs(&[old_rim]));
    let whole = world.capture_input(page(), GEOMETRY);
    let restored = World::restore(
        properties,
        JournalLimits::default(),
        world.checkpoint().to_restore_state(),
    )
    .unwrap();
    assert_eq!(restored.canonical_hash(), world.canonical_hash());
    assert!(restored.matches_inputs(&[whole]));
    assert!(
        !restored.matches_inputs(&[old_rim]),
        "restored caches start conservatively fresh"
    );
    assert_eq!(
        restored
            .capture_sampled_input(page(), GEOMETRY, region)
            .version,
        whole.version
    );
}
