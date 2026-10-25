use core_types::{
    BlockPos, Cell, CellProperties, ChannelMask, ComponentId, LocalIdx, PAGE_VOLUME, PageId,
    RegionId, SpaceId, SpaceTransform, Tick, Version,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use world::{
    CellRead, CommitError, ComponentData, CursorError, DerivedChanges, GameplayEvent,
    JournalLimits, LightSample, StorageMode, Transaction, World,
};

fn properties(cell: Cell) -> CellProperties {
    match cell.id() {
        0 => CellProperties::AIR,
        2 => CellProperties {
            shape: 2,
            emission: [(cell.state() & 15) as u8, 0, 0],
            ..CellProperties::SOLID
        },
        3 => CellProperties {
            shape: cell.raw(),
            occupied: true,
            fluid: true,
            light_opacity: 1,
            ..CellProperties::AIR
        },
        _ => CellProperties {
            shape: cell.raw(),
            ..CellProperties::SOLID
        },
    }
}
fn page(x: i32, y: i32, z: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn position(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn initialize(world: &mut World, id: PageId, fill: Cell) {
    let mut tx = Transaction::new(Tick(0), id.space);
    tx.create_page(id, vec![fill; PAGE_VOLUME]);
    world.commit(tx).unwrap();
}

#[test]
fn unknown_reads_are_pure_and_never_call_registry_or_generate() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let world = World::new(
        move |cell| {
            counted.fetch_add(1, Ordering::Relaxed);
            properties(cell)
        },
        16,
    );
    let before = world.canonical_hash();
    for n in 0..100_000 {
        assert_eq!(world.read(position(n, -n, n / 2)), CellRead::Unknown);
    }
    assert_eq!(world.canonical_hash(), before);
    assert_eq!(world.loaded_pages(), 0);
    assert_eq!(world.journal_len(), 0);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn atomic_failure_leaves_cells_components_versions_and_journal_unchanged() {
    let mut world = World::new(properties, 32);
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        ComponentData {
            anchor: position(0, 0, 0),
            kind: 10,
            payload: vec![1],
        },
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let before = world.canonical_hash();
    let sequence = world.journal_next();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(position(2, 2, 2), Cell::from_parts(1, 0));
    tx.set_component(
        ComponentId(2),
        ComponentData {
            anchor: position(0, 0, 0),
            kind: 11,
            payload: vec![2],
        },
        ChannelMask::CONTENTS,
    );
    assert!(matches!(
        world.commit(tx),
        Err(CommitError::ComponentAnchorOccupied(_))
    ));
    assert_eq!(world.canonical_hash(), before);
    assert_eq!(world.journal_next(), sequence);
    assert_eq!(world.read(position(2, 2, 2)), CellRead::Known(Cell::AIR));
    assert!(world.component(ComponentId(2)).is_none());
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(position(1, 1, 1), Cell::from_parts(1, 0));
    tx.set_cell(position(100, 100, 100), Cell::from_parts(1, 0));
    assert!(matches!(world.commit(tx), Err(CommitError::UnknownPage(_))));
    assert_eq!(world.canonical_hash(), before);
}

#[test]
fn snapshots_and_input_versions_cannot_publish_stale_results() {
    let mut world = World::new(properties, 32);
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    let snapshot = world.snapshot(page(0, 0, 0)).unwrap();
    let old = world.capture_input(page(0, 0, 0), ChannelMask::SHAPE);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(position(1, 1, 1), Cell::from_parts(2, 1));
    world.commit(tx).unwrap();
    assert!(!world.matches_inputs(&[old]));
    assert_eq!(snapshot.cell(position(1, 1, 1).local_index()), Cell::AIR);
    let shape = world.capture_input(page(0, 0, 0), ChannelMask::SHAPE);
    let emission = world.capture_input(page(0, 0, 0), ChannelMask::EMISSION);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(position(1, 1, 1), Cell::from_parts(2, 9));
    let change = world.commit(tx).unwrap();
    assert!(!change.channels.intersects(ChannelMask::SHAPE));
    assert!(change.channels.intersects(ChannelMask::EMISSION));
    assert!(world.matches_inputs(&[shape]));
    assert!(!world.matches_inputs(&[emission]));
    let mut mesh = DerivedChanges::new(ChannelMask::SHAPE, 4);
    mesh.observe(&change).unwrap();
    assert!(mesh.is_empty());
}

#[test]
fn inventory_and_light_changes_preserve_geometry_and_update_correct_channels() {
    let mut world = World::new(properties, 32);
    initialize(&mut world, page(0, 0, 0), Cell::from_parts(1, 0));
    let shape = world.capture_input(page(0, 0, 0), ChannelMask::SHAPE);
    let classified = world.page(page(0, 0, 0)).unwrap().classified_cells();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_component(
        ComponentId(7),
        ComponentData {
            anchor: position(0, 0, 0),
            kind: 2,
            payload: vec![3, 4],
        },
        ChannelMask::CONTENTS,
    );
    let change = world.commit(tx).unwrap();
    assert_eq!(change.channels, ChannelMask::CONTENTS);
    assert_eq!(
        world
            .page(page(0, 0, 0))
            .unwrap()
            .component(LocalIdx::new(0).unwrap()),
        Some(ComponentId(7))
    );
    assert_eq!(
        world.page(page(0, 0, 0)).unwrap().classified_cells(),
        classified
    );
    assert!(world.matches_inputs(&[shape]));
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_light(
        position(0, 0, 0),
        LightSample {
            sky: 15,
            block: 8,
            rgb: [8, 2, 1],
        },
    );
    let change = world.commit(tx).unwrap();
    assert_eq!(change.channels, ChannelMask::LIGHTING);
    assert!(world.matches_inputs(&[shape]));
    assert_eq!(
        world
            .page(page(0, 0, 0))
            .unwrap()
            .light(LocalIdx::new(0).unwrap())
            .block,
        8
    );
}

#[test]
fn all_six_masks_follow_one_cell_without_a_page_scan() {
    let mut world = World::new(properties, 32);
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(position(0, 0, 0), Cell::from_parts(1, 0));
    tx.set_cell(position(1, 0, 0), Cell::from_parts(2, 8));
    tx.set_cell(position(2, 0, 0), Cell::from_parts(3, 0));
    let change = world.commit(tx).unwrap();
    assert_eq!(change.work.pages_staged, 1);
    assert_eq!(change.work.mask_cells_classified, 3);
    assert_eq!(change.work.registry_queries, 9);
    let masks = world.page(page(0, 0, 0)).unwrap().masks();
    assert_eq!(masks.occupancy.count(), 3);
    assert_eq!(masks.opaque.count(), 2);
    assert_eq!(masks.collidable.count(), 2);
    assert_eq!(masks.blocks_projectile.count(), 2);
    assert_eq!(masks.fluid.count(), 1);
    assert_eq!(masks.emitter.count(), 1);
}

#[test]
fn one_edit_stages_one_page_independently_of_world_size() {
    let mut world = World::new(properties, 32);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in 0..128 {
        tx.create_page(page(x, 0, 0), vec![Cell::AIR; PAGE_VOLUME]);
    }
    world.commit(tx).unwrap();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(position(0, 0, 0), Cell::from_parts(1, 0));
    let change = world.commit(tx).unwrap();
    assert_eq!(change.work.pages_staged, 1);
    assert_eq!(change.work.component_records_staged, 0);
    assert_eq!(change.work.mask_cells_classified, 1);
    assert_eq!(
        world
            .region(RegionId {
                space: SpaceId(0),
                x: 0,
                z: 0
            })
            .unwrap()
            .loaded_pages,
        32
    );
}

#[test]
fn independent_cursors_require_explicit_ack_and_report_loss() {
    let mut world = World::new(properties, 3);
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    let slow = world.register_cursor();
    let fast = world.register_cursor();
    for tick in 1..=4 {
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.event(GameplayEvent {
            kind: tick as u16,
            subject: None,
            position: None,
            payload: vec![tick as u8],
        });
        world.commit(tx).unwrap();
        let first = world.read_cursor(fast, 10).unwrap();
        let repeated = world.read_cursor(fast, 10).unwrap();
        assert_eq!(first.first, repeated.first);
        assert_eq!(first.changes.len(), 1);
        world.acknowledge_cursor(fast, first.next).unwrap();
    }
    assert_eq!(world.journal_len(), 3);
    assert!(matches!(
        world.read_cursor(slow, 100),
        Err(CursorError::Lagged {
            expected: 1,
            oldest_available: 2
        })
    ));
    assert!(matches!(
        world.acknowledge_cursor(slow, world.journal_next()),
        Err(CursorError::Lagged { .. })
    ));
    world.recover_cursor(slow, world.journal_next()).unwrap();
    assert!(world.read_cursor(slow, 1).unwrap().changes.is_empty());
}

#[test]
fn a_hundred_thousand_visual_changes_coalesce_without_losing_game_events() {
    let mut world = World::new(properties, 8);
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    let cursor = world.register_cursor();
    let mut dirty = DerivedChanges::new(ChannelMask::SHAPE, 1);
    let mut received = 0;
    for tick in 1..=100_000_u64 {
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.set_cell(position(0, 0, 0), Cell::from_parts((tick % 2) as u16, 0));
        tx.event(GameplayEvent {
            kind: 1,
            subject: None,
            position: Some(position(0, 0, 0)),
            payload: tick.to_le_bytes().to_vec(),
        });
        dirty.observe(&world.commit(tx).unwrap()).unwrap();
        let batch = world.read_cursor(cursor, 1).unwrap();
        assert_eq!(batch.changes.len(), 1);
        assert_eq!(batch.changes[0].events[0].payload, tick.to_le_bytes());
        received += batch.changes[0].events.len();
        world.acknowledge_cursor(cursor, batch.next).unwrap();
    }
    assert_eq!(received, 100_000);
    assert_eq!(dirty.len(), 1);
    assert_eq!(
        dirty.pop_first(),
        Some((page(0, 0, 0), world.page_version(page(0, 0, 0))))
    );
    assert!(dirty.pop_first().is_none());
}

#[test]
fn stale_generation_and_aba_page_replacement_are_rejected() {
    let mut world = World::new(properties, 16);
    let requested = world.page_version(page(0, 0, 0));
    assert_eq!(requested, Version(0));
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    let first = world.page_version(page(0, 0, 0));
    let mut remove = Transaction::new(Tick(1), SpaceId(0));
    remove.remove_page(page(0, 0, 0));
    world.commit(remove).unwrap();
    assert_eq!(world.read(position(0, 0, 0)), CellRead::Unknown);
    assert!(world.page_version(page(0, 0, 0)) > first);
    let before = world.canonical_hash();
    let mut stale = Transaction::new(Tick(2), SpaceId(0));
    stale
        .expect_page(page(0, 0, 0), requested)
        .create_page(page(0, 0, 0), vec![Cell::from_parts(1, 0); PAGE_VOLUME]);
    assert!(matches!(
        world.commit(stale),
        Err(CommitError::ConflictingVersion { .. })
    ));
    assert_eq!(world.canonical_hash(), before);
}

#[test]
fn journal_byte_and_age_bounds_are_real_and_atomic() {
    let mut small = World::with_limits(
        properties,
        JournalLimits {
            max_records: 10,
            max_bytes: 512,
            max_age_ticks: 2,
        },
    );
    let before = small.canonical_hash();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page(0, 0, 0), vec![Cell::AIR; PAGE_VOLUME]);
    assert_eq!(small.commit(tx).unwrap_err(), CommitError::ChangeTooLarge);
    assert_eq!(small.canonical_hash(), before);
    let mut world = World::with_limits(
        properties,
        JournalLimits {
            max_records: 10,
            max_bytes: 1024 * 1024,
            max_age_ticks: 2,
        },
    );
    let cursor = world.register_cursor();
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    world.commit(Transaction::new(Tick(5), SpaceId(0))).unwrap();
    assert!(matches!(
        world.read_cursor(cursor, 10),
        Err(CursorError::Lagged {
            oldest_available: 1,
            ..
        })
    ));
    assert!(world.journal_bytes() <= 1024 * 1024);
}

#[test]
fn spaces_regions_and_y_pages_do_not_fix_world_height() {
    let mut world = World::new(properties, 16);
    let id = PageId {
        space: SpaceId(8),
        x: -33,
        y: 1_048_576,
        z: 64,
    };
    let transform = SpaceTransform {
        translation: [12.0, -4.0, 7.5],
        ..SpaceTransform::IDENTITY
    };
    let mut tx = Transaction::new(Tick(1), SpaceId(8));
    tx.create_space(transform)
        .create_page(id, vec![Cell::from_parts(1, 0); PAGE_VOLUME]);
    world.commit(tx).unwrap();
    assert_eq!(world.space(SpaceId(8)).unwrap().transform, transform);
    assert_eq!(
        RegionId::from_page(id),
        RegionId {
            space: SpaceId(8),
            x: -2,
            z: 2
        }
    );
    assert_eq!(
        world.region(RegionId::from_page(id)).unwrap().loaded_pages,
        1
    );
    assert_eq!(
        world.read(BlockPos {
            space: SpaceId(8),
            x: -528,
            y: 16_777_216,
            z: 1024
        }),
        CellRead::Known(Cell::from_parts(1, 0))
    );
    let original = world.canonical_hash();
    let mut tx = Transaction::new(Tick(2), SpaceId(8));
    tx.set_transform(SpaceTransform {
        translation: [f64::NAN, 0.0, 0.0],
        ..SpaceTransform::IDENTITY
    });
    assert_eq!(world.commit(tx).unwrap_err(), CommitError::InvalidTransform);
    assert_eq!(world.canonical_hash(), original);
}

#[test]
fn component_moves_are_atomic_and_independent_of_id_order() {
    let mut world = World::new(properties, 16);
    initialize(&mut world, page(0, 0, 0), Cell::AIR);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    for id in 1..=2 {
        tx.set_component(
            ComponentId(id),
            ComponentData {
                anchor: position(id as i32, 0, 0),
                kind: 1,
                payload: vec![id as u8],
            },
            ChannelMask::CONTENTS,
        );
    }
    world.commit(tx).unwrap();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    for id in 1..=2 {
        tx.set_component(
            ComponentId(id),
            ComponentData {
                anchor: position((3 - id) as i32, 0, 0),
                kind: 1,
                payload: vec![id as u8],
            },
            ChannelMask::CONTENTS,
        );
    }
    world.commit(tx).unwrap();
    assert_eq!(
        world
            .page(page(0, 0, 0))
            .unwrap()
            .component(position(1, 0, 0).local_index()),
        Some(ComponentId(2))
    );
    let before = world.canonical_hash();
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.remove_page(page(0, 0, 0));
    assert!(matches!(
        world.commit(tx),
        Err(CommitError::DanglingComponent(_))
    ));
    assert_eq!(world.canonical_hash(), before);
}

#[test]
fn deterministic_hash_ignores_storage_representation_and_cursor_state() {
    let final_cells: Vec<_> = (0..PAGE_VOLUME)
        .map(|n| Cell::from_parts((n % 129) as u16, 0))
        .collect();
    let mut a = World::new(properties, 16);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(page(0, 0, 0), final_cells.clone());
    a.commit(tx).unwrap();
    let mut b = World::new(properties, 16);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(
        page(0, 0, 0),
        (0..PAGE_VOLUME)
            .map(|n| Cell::from_parts((n % 300) as u16, 0))
            .collect::<Vec<_>>(),
    );
    for (index, cell) in final_cells.into_iter().enumerate() {
        let (x, y, z) = LocalIdx::new(index as u16).unwrap().xyz();
        tx.set_cell(position(i32::from(x), i32::from(y), i32::from(z)), cell);
    }
    b.commit(tx).unwrap();
    assert_eq!(
        a.page(page(0, 0, 0)).unwrap().storage_mode(),
        StorageMode::Paletted
    );
    assert_eq!(
        b.page(page(0, 0, 0)).unwrap().storage_mode(),
        StorageMode::Dense
    );
    a.register_cursor();
    assert_eq!(a.canonical_hash(), b.canonical_hash());
    for tick in 2..130 {
        for world in [&mut a, &mut b] {
            let mut tx = Transaction::new(Tick(tick), SpaceId(0));
            tx.set_cell(
                position((tick % 16) as i32, ((tick / 16) % 16) as i32, 3),
                Cell::from_parts((tick % 8) as u16, tick as u16),
            );
            world.commit(tx).unwrap();
        }
        assert_eq!(a.canonical_hash(), b.canonical_hash(), "tick {tick}");
    }
}
