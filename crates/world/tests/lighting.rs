use core_types::{
    BlockPos, Cell, CellProperties, ChannelMask, ComponentId, LocalIdx, PAGE_VOLUME, PageId,
    SpaceId, SpaceTransform, Tick,
};
use std::collections::{BTreeMap, VecDeque};
use world::lighting::{
    LightBudget, LightIngestBudget, LightPriority, LightingBackpressure, LightingEngine,
    LightingError, LightingLimits, SkyAccess,
};
use world::{ComponentData, DerivedChanges, JournalLimits, LightSample, Transaction, World};

#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
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
fn properties(cell: Cell) -> CellProperties {
    match cell.id() {
        0 => CellProperties::AIR,
        1 => CellProperties::SOLID,
        2 => CellProperties {
            shape: 2,
            occupied: true,
            emission: [15, 3, 0],
            ..CellProperties::AIR
        },
        3 => CellProperties {
            shape: 2,
            occupied: true,
            emission: [0, 4, 12],
            ..CellProperties::AIR
        },
        4 => CellProperties {
            shape: 4,
            occupied: true,
            fluid: true,
            light_opacity: 2,
            ..CellProperties::AIR
        },
        5 => CellProperties {
            shape: 5,
            occupied: true,
            emission: [3, 1, 0],
            ..CellProperties::SOLID
        },
        _ => CellProperties::SOLID,
    }
}
fn limits() -> LightingLimits {
    LightingLimits {
        max_frontier_cells: 32768,
        reserved_interactive_cells: 128,
        max_seed_pages: 16,
        max_queue_age_ticks: 40,
    }
}
fn budget() -> LightBudget {
    LightBudget {
        max_cells: 1024,
        max_neighbor_reads: 6144,
        max_seed_cells: 512,
        max_updates: 1024,
    }
}
fn world(pages: &[PageId]) -> World {
    let mut world = World::new(properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for page in pages {
        tx.create_page(*page, vec![Cell::AIR; PAGE_VOLUME]);
    }
    world.commit(tx).unwrap();
    world
}
fn set(world: &mut World, edits: &[(BlockPos, u16)]) {
    let mut tx = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
    for (position, id) in edits {
        tx.set_cell(*position, Cell::from_parts(*id, 0));
    }
    world.commit(tx).unwrap();
}
fn stored(world: &World, position: BlockPos) -> LightSample {
    world
        .page(position.page())
        .unwrap()
        .light(position.local_index())
}

fn settle(world: &mut World, engine: &mut LightingEngine, sky: &impl SkyAccess) -> u64 {
    let mut work = 0;
    for _ in 0..10000 {
        let tick = world.tick();
        let ingest = engine
            .ingest(world, tick, LightIngestBudget::default())
            .unwrap();
        let step = engine.step(world, tick, budget(), sky).unwrap();
        assert!(step.work.cells_examined <= budget().max_cells);
        assert!(step.work.neighbor_reads <= budget().max_neighbor_reads);
        assert!(step.work.light_updates <= budget().max_updates);
        assert!(step.work.seed_cells_examined <= budget().max_seed_cells);
        work += step.work.cells_examined;
        assert!(!step.backpressure, "test quota must hold this finite scene");
        if engine.is_idle()
            && ingest.work.journal_records == 0
            && world
                .read_cursor(engine.cursor(), 1)
                .unwrap()
                .changes
                .is_empty()
        {
            return work;
        }
    }
    panic!("lighting did not converge");
}

// Independent increase-only BFS from zero for a finite known snapshot. It reads
// no existing world light and is deliberately separate from incremental removal.
fn reference(world: &World, sky: &impl SkyAccess) -> BTreeMap<BlockPos, LightSample> {
    let mut cells = BTreeMap::new();
    for (page, data) in world.pages() {
        for index in 0..PAGE_VOLUME {
            let local = LocalIdx::new(index as u16).unwrap();
            let (x, y, z) = local.xyz();
            let position = BlockPos {
                space: page.space,
                x: page.x * 16 + i32::from(x),
                y: page.y * 16 + i32::from(y),
                z: page.z * 16 + i32::from(z),
            };
            cells.insert(position, properties(data.cell(local)));
        }
    }
    let mut light = BTreeMap::new();
    let mut queue = VecDeque::new();
    for (position, properties) in &cells {
        let rgb = properties.emission;
        let mut value = LightSample {
            sky: 0,
            block: *rgb.iter().max().unwrap(),
            rgb,
        };
        if !cells.contains_key(&BlockPos {
            space: position.space,
            ..pos(position.x, position.y + 1, position.z)
        }) && let Some(incoming) = sky.incoming_from_unknown_above(*position)
        {
            value.sky = if incoming == 15 && properties.light_opacity == 0 {
                15
            } else {
                incoming.saturating_sub(properties.light_opacity.max(1))
            };
        }
        light.insert(*position, value);
        if value != LightSample::default() {
            queue.push_back(*position);
        }
    }
    while let Some(source) = queue.pop_front() {
        let value = light[&source];
        for [x, y, z] in [
            [-1, 0, 0],
            [1, 0, 0],
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [0, 0, 1],
        ] {
            let destination = BlockPos {
                space: source.space,
                ..pos(source.x + x, source.y + y, source.z + z)
            };
            let Some(properties) = cells.get(&destination) else {
                continue;
            };
            let attenuation = properties.light_opacity.max(1);
            let current = light[&destination];
            let mut next = current;
            next.block = next.block.max(value.block.saturating_sub(attenuation));
            for channel in 0..3 {
                next.rgb[channel] =
                    next.rgb[channel].max(value.rgb[channel].saturating_sub(attenuation));
            }
            next.sky = next.sky.max(
                if y == -1 && value.sky == 15 && properties.light_opacity == 0 {
                    15
                } else {
                    value.sky.saturating_sub(attenuation)
                },
            );
            if next != current {
                light.insert(destination, next);
                queue.push_back(destination);
            }
        }
    }
    light
}
fn assert_reference(world: &World, sky: &impl SkyAccess) {
    for (position, expected) in reference(world, sky) {
        assert_eq!(stored(world, position), expected, "light at {position:?}");
    }
}

#[test]
fn colored_lamp_crosses_page_edges_and_removal_matches_independent_bfs() {
    let mut world = world(&[page(0, 0, 0), page(1, 0, 0)]);
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    set(&mut world, &[(pos(15, 8, 8), 2), (pos(21, 8, 8), 5)]);
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(stored(&world, pos(16, 8, 8)).rgb, [14, 2, 0]);
    assert_reference(&world, &|_| None);
    set(&mut world, &[(pos(15, 8, 8), 0)]);
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(stored(&world, pos(21, 8, 8)).block, 3);
    assert_reference(&world, &|_| None);
    set(&mut world, &[(pos(21, 8, 8), 0)]);
    settle(&mut world, &mut engine, &|_| None);
    assert_reference(&world, &|_| None);
    assert_eq!(stored(&world, pos(20, 8, 8)), LightSample::default());
}

#[test]
fn opening_and_closing_an_opaque_wall_updates_light_but_color_does_not_dirty_geometry() {
    let mut world = world(&[page(0, 0, 0)]);
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    let mut edits = vec![(pos(5, 8, 8), 2)];
    for y in 0..16 {
        for z in 0..16 {
            edits.push((pos(8, y, z), 1));
        }
    }
    set(&mut world, &edits);
    settle(&mut world, &mut engine, &|_| None);
    assert_reference(&world, &|_| None);
    assert_eq!(stored(&world, pos(10, 8, 8)).block, 0);
    set(&mut world, &[(pos(8, 8, 8), 0)]);
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(stored(&world, pos(10, 8, 8)).block, 10);
    assert_reference(&world, &|_| None);
    let before = world.input_version(page(0, 0, 0), ChannelMask::SHAPE);
    let cursor = world.register_cursor();
    set(&mut world, &[(pos(5, 8, 8), 3)]);
    settle(&mut world, &mut engine, &|_| None);
    let mut mesh = DerivedChanges::new(ChannelMask::SHAPE, 16);
    for change in world.read_cursor(cursor, 4096).unwrap().changes {
        mesh.observe(&change).unwrap();
    }
    assert!(mesh.is_empty());
    assert_eq!(
        before,
        world.input_version(page(0, 0, 0), ChannelMask::SHAPE)
    );
    assert_reference(&world, &|_| None);
    set(&mut world, &[(pos(8, 8, 8), 1)]);
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(stored(&world, pos(10, 8, 8)).block, 0);
    assert_reference(&world, &|_| None);
}

#[test]
fn sky_requires_a_certified_boundary_and_flows_through_loaded_vertical_pages() {
    let mut world = world(&[page(0, 0, 0), page(0, 1, 0)]);
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    engine
        .queue_page(page(0, 1, 0), Tick(0), LightPriority::Background)
        .unwrap();
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(
        stored(&world, pos(8, 31, 8)).sky,
        0,
        "Unknown above is not free sky"
    );
    let sky = |position: BlockPos| (position.y == 31).then_some(15);
    engine
        .queue_page(page(0, 1, 0), Tick(0), LightPriority::Background)
        .unwrap();
    settle(&mut world, &mut engine, &sky);
    assert_eq!(stored(&world, pos(8, 0, 8)).sky, 15);
    set(&mut world, &[(pos(8, 30, 8), 4), (pos(8, 8, 8), 3)]);
    settle(&mut world, &mut engine, &sky);
    assert_eq!(stored(&world, pos(8, 30, 8)).sky, 13);
    assert_reference(&world, &sky);
    // A complete roof removes the direct source; side boundaries are unknown/dark.
    let roof: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| (pos(x, 31, z), 1)))
        .collect();
    set(&mut world, &roof);
    settle(&mut world, &mut engine, &sky);
    assert_eq!(stored(&world, pos(8, 0, 8)).sky, 0);
    assert_reference(&world, &sky);
}

#[test]
fn static_world_and_contents_events_perform_zero_light_work() {
    let mut world = world(&[page(0, 0, 0)]);
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    set(&mut world, &[(pos(8, 8, 8), 2)]);
    settle(&mut world, &mut engine, &|_| None);
    let before = world.canonical_hash();
    let ingest = engine
        .ingest(&mut world, Tick(1), LightIngestBudget::default())
        .unwrap();
    let step = engine
        .step(&mut world, Tick(1), budget(), &|_| None)
        .unwrap();
    assert_eq!(ingest.work, Default::default());
    assert_eq!(step.work, Default::default());
    assert_eq!(before, world.canonical_hash());
    assert!(engine.is_idle());
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        ComponentData {
            anchor: pos(1, 1, 1),
            kind: 7,
            payload: vec![3, 4, 5],
        },
        ChannelMask::CONTENTS,
    );
    world.commit(tx).unwrap();
    let ingestion = engine
        .ingest(&mut world, Tick(2), LightIngestBudget::default())
        .unwrap();
    assert_eq!(ingestion.work.journal_records, 1);
    assert_eq!(ingestion.work.registry_queries, 0);
    let progress = engine
        .step(&mut world, Tick(2), budget(), &|_| None)
        .unwrap();
    assert_eq!(progress.work, Default::default());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn static_lighting_owner_loop_allocates_nothing_after_bootstrap() {
    use std::hint::black_box;
    use telemetry::{AllocationCounts, AllocationScope};
    let mut world = world(&[page(0, 0, 0)]);
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    set(&mut world, &[(pos(8, 8, 8), 2)]);
    settle(&mut world, &mut engine, &|_| None);
    let tick = world.tick();
    // A positive control ensures this binary actually installed the counter.
    let control = AllocationScope::begin().unwrap();
    let bytes = black_box(Vec::<u8>::with_capacity(black_box(77)));
    assert!(control.finish().allocating_calls() > 0);
    drop(bytes);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        let input = engine
            .ingest(&mut world, tick, LightIngestBudget::default())
            .unwrap();
        let light = engine.step(&mut world, tick, budget(), &|_| None).unwrap();
        black_box((input.work, light.work));
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}

#[test]
fn loading_and_unloading_pages_resume_blocked_boundaries_without_world_generation() {
    let mut world = world(&[page(0, 0, 0)]);
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    set(&mut world, &[(pos(15, 8, 8), 2)]);
    settle(&mut world, &mut engine, &|_| None);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.create_page(page(1, 0, 0), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(tx).unwrap();
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(stored(&world, pos(16, 8, 8)).block, 14);
    assert_reference(&world, &|_| None);
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.remove_page(page(0, 0, 0));
    world.commit(tx).unwrap();
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(stored(&world, pos(16, 8, 8)), LightSample::default());
    assert_reference(&world, &|_| None);
}

#[test]
fn portable_spaces_do_not_share_light_at_identical_local_coordinates() {
    let mut world = world(&[page(0, 0, 0)]);
    let mut create = Transaction::new(Tick(0), SpaceId(1));
    create.create_space(SpaceTransform::IDENTITY);
    create.create_page(
        PageId {
            space: SpaceId(1),
            ..page(0, 0, 0)
        },
        vec![Cell::AIR; PAGE_VOLUME],
    );
    world.commit(create).unwrap();
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    let source = BlockPos {
        space: SpaceId(1),
        ..pos(8, 8, 8)
    };
    let mut edit = Transaction::new(Tick(1), SpaceId(1));
    edit.set_cell(source, Cell::from_parts(2, 0));
    world.commit(edit).unwrap();
    settle(&mut world, &mut engine, &|_| None);
    assert_eq!(stored(&world, pos(8, 8, 8)).block, 0);
    assert_eq!(stored(&world, source).block, 15);
    assert_reference(&world, &|_| None);
}

#[test]
fn cursor_loss_requires_explicit_snapshot_reseed_instead_of_silently_skipping_edits() {
    let mut world = World::new(properties, 2);
    let mut create = Transaction::new(Tick(0), SpaceId(0));
    create.create_page(page(0, 0, 0), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(create).unwrap();
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    for id in [2, 3, 2, 3] {
        set(&mut world, &[(pos(8, 8, 8), id)]);
    }
    let tick = world.tick();
    assert!(matches!(
        engine.ingest(&mut world, tick, LightIngestBudget::default()),
        Err(LightingError::Cursor(world::CursorError::Lagged { .. }))
    ));
    assert_eq!(stored(&world, pos(8, 8, 8)), LightSample::default());
    // The owner retains a current snapshot before seeking past lost history.
    engine
        .queue_page(page(0, 0, 0), tick, LightPriority::Background)
        .unwrap();
    let boundary = world.journal_next();
    engine.recover_cursor(&mut world, boundary).unwrap();
    settle(&mut world, &mut engine, &|_| None);
    assert_reference(&world, &|_| None);
}

#[test]
fn bounded_frontier_reports_pressure_and_resumes_without_losing_light_edges() {
    let mut world = world(&[page(0, 0, 0)]);
    let tiny = LightingLimits {
        max_frontier_cells: 2,
        reserved_interactive_cells: 0,
        max_seed_pages: 1,
        max_queue_age_ticks: 2,
    };
    let mut engine = LightingEngine::attach(&mut world, tiny).unwrap();
    set(&mut world, &[(pos(8, 8, 8), 2)]);
    let ingest = engine
        .ingest(&mut world, Tick(1), LightIngestBudget::default())
        .unwrap();
    assert!(ingest.backpressure);
    let progress = engine
        .step(&mut world, Tick(5), budget(), &|_| None)
        .unwrap();
    assert!(progress.backpressure);
    assert!(progress.overdue);
    assert_eq!(
        stored(&world, pos(8, 8, 8)).block,
        0,
        "unretained fanout must not be published"
    );
    engine.set_limits(limits()).unwrap();
    settle(&mut world, &mut engine, &|_| None);
    assert_reference(&world, &|_| None);
}

#[test]
fn near_edits_have_reserved_slots_and_small_work_budgets_are_resumable() {
    let mut world = world(&[page(0, 0, 0)]);
    let policy = LightingLimits {
        max_frontier_cells: 16,
        reserved_interactive_cells: 7,
        max_seed_pages: 1,
        max_queue_age_ticks: 40,
    };
    let mut engine = LightingEngine::attach(&mut world, policy).unwrap();
    for x in 0..9 {
        engine
            .queue_cell(pos(x, 0, 0), Tick(0), LightPriority::Background)
            .unwrap();
    }
    assert_eq!(
        engine.queue_cell(pos(10, 0, 0), Tick(0), LightPriority::Background),
        Err(LightingBackpressure::FrontierFull)
    );
    engine
        .invalidate_cell(&world, pos(8, 8, 8), Tick(0), LightPriority::Interactive)
        .unwrap();
    let bounded = LightBudget {
        max_cells: 1,
        max_neighbor_reads: 6,
        max_seed_cells: 0,
        max_updates: 1,
    };
    let progress = engine
        .step(&mut world, Tick(0), bounded, &|_| None)
        .unwrap();
    assert_eq!(progress.work.cells_examined, 1);
    assert_eq!(progress.work.neighbor_reads, 6);
    assert_eq!(progress.pending_cells, 15);
}

#[test]
fn failing_light_transaction_restores_its_frontier_for_a_smaller_retry() {
    let mut world = World::with_limits(
        properties,
        JournalLimits {
            max_records: 4096,
            max_bytes: 20_000,
            max_age_ticks: 1200,
        },
    );
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page(0, 0, 0), vec![Cell::AIR; PAGE_VOLUME]);
    world.commit(tx).unwrap();
    let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
    set(&mut world, &[(pos(8, 8, 8), 2)]);
    engine
        .ingest(&mut world, Tick(1), LightIngestBudget::default())
        .unwrap();
    let before = world.canonical_hash();
    let huge = LightBudget {
        max_cells: 4096,
        max_neighbor_reads: 24576,
        max_seed_cells: 0,
        max_updates: 4096,
    };
    assert!(matches!(
        engine.step(&mut world, Tick(1), huge, &|_| None),
        Err(LightingError::Commit(_))
    ));
    assert_eq!(before, world.canonical_hash());
    assert!(engine.pending_cells() > 0);
    // Reference compares every cell after retry, catching lost or duplicated undo links.
    for _ in 0..2000 {
        engine
            .ingest(&mut world, Tick(1), LightIngestBudget::default())
            .unwrap();
        engine
            .step(
                &mut world,
                Tick(1),
                LightBudget {
                    max_cells: 128,
                    max_neighbor_reads: 768,
                    max_seed_cells: 0,
                    max_updates: 128,
                },
                &|_| None,
            )
            .unwrap();
        if engine.is_idle() {
            break;
        }
    }
    assert!(engine.is_idle());
    assert_reference(&world, &|_| None);
}

#[test]
fn identical_event_and_budget_sequences_produce_identical_world_hashes() {
    let scenario = || {
        let mut world = world(&[page(-1, 0, 0), page(0, 0, 0)]);
        let mut engine = LightingEngine::attach(&mut world, limits()).unwrap();
        let mut hashes = Vec::new();
        for edits in [
            vec![(pos(-1, 8, 8), 2)],
            vec![(pos(0, 8, 8), 4)],
            vec![(pos(-1, 8, 8), 3)],
            vec![(pos(-1, 8, 8), 0)],
        ] {
            set(&mut world, &edits);
            settle(&mut world, &mut engine, &|_| None);
            hashes.push(world.canonical_hash());
        }
        hashes
    };
    assert_eq!(scenario(), scenario());
}

#[cfg(not(target_arch = "wasm32"))]
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(8))]
    #[test]
    fn random_emitter_and_opacity_changes_match_full_relighting(edits in proptest::collection::vec((1i32..15,1i32..15,1i32..15,0u16..6),1..7)) {
        let mut world=world(&[page(0,0,0)]);
        let mut engine=LightingEngine::attach(&mut world,limits()).unwrap();
        for (x,y,z,id) in edits {
            set(&mut world,&[(pos(x,y,z),id)]);
            settle(&mut world,&mut engine,&|_| None);
            assert_reference(&world,&|_| None);
        }
    }
}
