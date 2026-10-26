use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use server::lighting::*;
use world::{
    CellRead, Transaction, World,
    lighting::{LightBudget, LightIngestBudget, SkyAccess},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn position(y: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y,
        z: 8,
    }
}
fn page() -> PageId {
    position(126).page()
}
fn fixture() -> World {
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(page(), vec![Cell::AIR; 4096]);
    for z in 7..=9 {
        for x in 7..=9 {
            tx.set_cell(
                BlockPos {
                    space: SpaceId(0),
                    x,
                    y: 125,
                    z,
                },
                Cell::from_parts(rules::ids::FARMLAND, 7),
            );
        }
    }
    tx.set_cell(position(126), Cell::from_parts(rules::ids::WHEAT, 0));
    world.commit(tx).unwrap();
    world
}
fn config() -> ServerLightingConfig {
    ServerLightingConfig {
        step: LightBudget {
            max_cells: 4096,
            max_neighbor_reads: 24576,
            max_seed_cells: 4096,
            max_updates: 4096,
        },
        ingest: LightIngestBudget {
            max_records: 16,
            max_entries: 4096,
            max_seeds: 4096,
        },
        ..ServerLightingConfig::default()
    }
}
fn proof(ready: bool, roof: bool, source: u64) -> GeneratedSkyProof {
    let blockers = if roof {
        (0..16)
            .flat_map(|z| {
                (0..16).map(move |x| BlockPos {
                    space: SpaceId(0),
                    x,
                    y: 256,
                    z,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    GeneratedSkyProof::from_authority(SpaceId(0), 127, source, ready, blockers, 1024).unwrap()
}
fn pump(owner: &mut ServerLighting, world: &mut World, sky: &impl SkyAccess) -> usize {
    let first_tick = world.tick().0 + 1;
    for n in 0..250 {
        let phase = owner.step(world, Tick(first_tick + n as u64), sky).unwrap();
        assert!(phase.work.foreground_cell_reads == 0);
        assert!(phase.progress.work.cells_examined <= 4096);
        assert!(phase.progress.work.neighbor_reads <= 24576);
        assert!(phase.progress.work.seed_cells_examined <= 4096);
        let mut batch = save::Batch::empty(world.tick());
        batch
            .changes
            .extend(phase.changes.iter().map(|change| (**change).clone()));
        let encoded = save::encode_batch(&batch).unwrap();
        let decoded = save::decode_batch(&encoded).unwrap();
        assert_eq!(decoded.changes.len(), phase.changes.len());
        owner.acknowledge_after_durable(world, &phase).unwrap();
        if owner.is_idle() {
            return n + 1;
        }
    }
    panic!("light did not converge within explicit budget");
}
#[test]
fn actual_crops_observe_sky_and_persisted_unloaded_roof_stops_it() {
    let mut world = fixture();
    let mut owner = ServerLighting::attach(&mut world, config()).unwrap();
    let unknown = pump(&mut owner, &mut world, &proof(false, false, 1));
    assert_eq!(
        world
            .page(page())
            .unwrap()
            .light(position(126).local_index())
            .sky,
        0
    );
    assert!(owner.request_sky_page(page(), world.tick()));
    let daylight = pump(&mut owner, &mut world, &proof(true, false, 2));
    let growth = |world: &World| {
        let light = world
            .page(page())
            .unwrap()
            .light(position(126).local_index());
        rules::survival::farm::random_tick(
            |[x, y, z]| match world.read(BlockPos {
                space: SpaceId(0),
                x,
                y,
                z,
            }) {
                CellRead::Known(cell) => Some(cell),
                CellRead::Unknown => None,
            },
            [8, 126, 8],
            light.sky.max(light.block),
            0,
        )
        .unwrap()
    };
    assert_eq!(
        world
            .page(page())
            .unwrap()
            .light(position(126).local_index())
            .sky,
        15
    );
    assert!(growth(&world).change.is_some());
    // Commit and encode the real opaque roof, then evict its page. The proof
    // comes from the saved edits, not the currently loaded World or terrain height.
    let roof_page = position(256).page();
    let cursor = world.register_cursor();
    let mut tx = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
    tx.create_page(roof_page, vec![Cell::AIR; 4096]);
    world.commit(tx).unwrap();
    let mut tx = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
    for z in 0..16 {
        for x in 0..16 {
            tx.set_cell(
                BlockPos {
                    space: SpaceId(0),
                    x,
                    y: 256,
                    z,
                },
                Cell::from_parts(rules::ids::STONE, 0),
            );
        }
    }
    world.commit(tx).unwrap();
    let captured = world.read_cursor(cursor, 2).unwrap();
    let mut saved = save::Batch::empty(world.tick());
    saved.changes = captured
        .changes
        .iter()
        .map(|change| (**change).clone())
        .collect();
    let restored = save::decode_batch(&save::encode_batch(&saved).unwrap()).unwrap();
    let opaque = restored
        .changes
        .iter()
        .flat_map(|change| change.cells.iter())
        .filter(|cell| rules::properties(cell.new).opaque)
        .map(|cell| {
            let (x, y, z) = cell.index.xyz();
            BlockPos {
                space: cell.page.space,
                x: cell.page.x * 16 + i32::from(x),
                y: cell.page.y * 16 + i32::from(y),
                z: cell.page.z * 16 + i32::from(z),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(opaque.len(), 256);
    let roof = GeneratedSkyProof::from_authority(SpaceId(0), 127, 3, true, opaque, 1024).unwrap();
    let mut tx = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
    tx.remove_page(roof_page);
    world.commit(tx).unwrap();
    world.remove_cursor(cursor);
    assert!(owner.request_sky_page(page(), world.tick()));
    let darkness = pump(&mut owner, &mut world, &roof);
    assert_eq!(
        world
            .page(page())
            .unwrap()
            .light(position(126).local_index())
            .sky,
        0
    );
    assert!(growth(&world).change.is_none());
    assert_eq!(
        world.loaded_pages(),
        1,
        "unloaded roof was generated by light solver"
    );
    println!(
        "M8 actualcrop CPUsky15→0 persistedroofY256 unknownGap128cells, growth1→0; phases{unknown}/{daylight}/{darkness};noWorldgeneration"
    );
    owner.detach(&mut world).unwrap();
}
#[test]
fn derived_capture_excludes_external_changes_and_waits_for_same_wal_ack() {
    let mut world = fixture();
    let checkpoint = world.checkpoint().to_restore_state();
    let mut replay = World::restore(
        rules::properties,
        world::JournalLimits::default(),
        checkpoint,
    )
    .unwrap();
    let mut owner = ServerLighting::attach(&mut world, config()).unwrap();
    let external_boundary = world.register_cursor();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(position(124), Cell::from_parts(rules::ids::STONE, 0));
    world.commit(tx).unwrap();
    let external = world.read_cursor(external_boundary, 1).unwrap();
    let phase = owner
        .step(&mut world, Tick(2), &proof(true, false, 1))
        .unwrap();
    assert!(!phase.changes.is_empty());
    assert!(
        phase
            .changes
            .iter()
            .all(|change| change.cells.is_empty() && !change.lights.is_empty())
    );
    assert!(matches!(
        owner.step(&mut world, Tick(3), &proof(true, false, 1)),
        Err(ServerLightingError::AwaitingDurability)
    ));
    let mut batch = save::Batch::empty(Tick(2));
    batch.changes.extend(
        external
            .changes
            .iter()
            .chain(&phase.changes)
            .map(|change| (**change).clone()),
    );
    let encoded = save::encode_batch(&batch).unwrap();
    let decoded = save::decode_batch(&encoded).unwrap();
    for change in &decoded.changes {
        replay.apply_recovered_change(change).unwrap();
    }
    for index in 0..4096 {
        let index = core_types::LocalIdx::new(index).unwrap();
        assert_eq!(
            world.page(page()).unwrap().light(index),
            replay.page(page()).unwrap().light(index)
        );
    }
    owner.acknowledge_after_durable(&mut world, &phase).unwrap();
    assert!(matches!(
        owner.acknowledge_after_durable(&mut world, &phase),
        Err(ServerLightingError::InvalidDurableToken)
    ));
    assert!(
        owner
            .step(&mut world, Tick(3), &proof(true, false, 1))
            .is_ok()
    );
}
#[test]
fn complete_proof_is_immutable_bounded_and_never_inferred_from_unknown() {
    let sky = proof(true, true, 7);
    assert_eq!(sky.source(), 7);
    assert_eq!(sky.blocker_columns(), 256);
    assert!(sky.is_ready());
    assert_eq!(sky.incoming_from_unknown_above(position(127)), None);
    assert_eq!(sky.incoming_from_unknown_above(position(256)), Some(15));
    assert_eq!(
        sky.incoming_from_unknown_above(BlockPos {
            space: SpaceId(1),
            ..position(300)
        }),
        None
    );
    assert_eq!(
        proof(false, false, 8).incoming_from_unknown_above(position(300)),
        None
    );
    assert!(
        GeneratedSkyProof::from_authority(
            SpaceId(0),
            127,
            1,
            true,
            [
                position(128),
                BlockPos {
                    x: 9,
                    ..position(128)
                }
            ],
            1
        )
        .is_err()
    );
}

#[test]
fn idle_owner_has_no_allocations_cell_scans_or_derived_records() {
    let mut world = fixture();
    let mut owner = ServerLighting::attach(&mut world, config()).unwrap();
    let sky = proof(false, false, 1);
    pump(&mut owner, &mut world, &sky);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..100 {
        let phase = owner.step(&mut world, Tick(100), &sky).unwrap();
        assert!(phase.changes.is_empty());
        assert_eq!(phase.progress.work.cells_examined, 0);
        assert_eq!(phase.progress.work.neighbor_reads, 0);
        assert_eq!(phase.progress.work.seed_cells_examined, 0);
        assert_eq!(phase.work.foreground_cell_reads, 0);
    }
    assert_eq!(scope.finish().allocating_calls(), 0);
    owner.detach(&mut world).unwrap();
}

#[test]
fn durable_idle_owner_emits_no_wal_delta_and_allocates_nothing() {
    let mut world = fixture();
    let mut owner = ServerLighting::attach_durable(&mut world, config()).unwrap();
    let sky = proof(false, false, 1);
    for _ in 0..10_000 {
        let phase = owner.step(&mut world, Tick(100), &sky).unwrap();
        owner.take_delta().unwrap();
        if !phase.changes.is_empty() {
            owner.acknowledge_after_durable(&mut world, &phase).unwrap();
        }
        if owner.is_idle() {
            break;
        }
    }
    assert!(owner.is_idle());
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        owner.observe_changes(&world, &[]).unwrap();
        let phase = owner.step(&mut world, Tick(101), &sky).unwrap();
        assert!(phase.changes.is_empty());
        assert_eq!(phase.progress.work.cells_examined, 0);
        assert_eq!(phase.progress.work.neighbor_reads, 0);
        assert_eq!(phase.progress.work.seed_cells_examined, 0);
        assert_eq!(phase.work.foreground_cell_reads, 0);
        assert!(owner.take_delta().unwrap().is_none());
    }
    assert_eq!(scope.finish().allocating_calls(), 0);
    owner.detach(&mut world).unwrap();
}
