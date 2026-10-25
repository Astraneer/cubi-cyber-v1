use core_types::{Cell, PageId, SpaceId, Tick, Version};
use r#gen::{GeneratedPage, GenerationTask, Generator, TerrainSeed, WireError};
use jobs::CancellationToken;

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
fn task() -> GenerationTask {
    GenerationTask {
        terrain_seed: TerrainSeed(1),
        page: page(0, 3, 0),
        expected: None,
    }
}
fn ore(id: u16) -> bool {
    matches!(id, 45 | 46 | 57..=61)
}

#[test]
fn public_columns_have_no_ore_or_underground_lava() {
    let generator = Generator::new(TerrainSeed(1337));
    for (x, z) in r#gen::PROBE_COLUMNS {
        let column = generator.generate_column(x, z).unwrap();
        assert!(
            column
                .raw_blocks()
                .iter()
                .all(|&id| !ore(id.into()) && id != 56)
        );
        assert!(column.work.scratch_reads > 0 && column.work.scratch_writes > 41472);
        assert_eq!(column.work.sampled_columns, 400);
    }
}

#[test]
fn signed_page_y_has_no_format_height_limit_and_uses_xzy_indexing() {
    let generator = Generator::new(TerrainSeed(1337));
    let column = generator.generate_column(-21, 12).unwrap();
    let generated = column.page(page(-21, 2, 12), Some(Version(9))).unwrap();
    for y in 0..16i32 {
        for z in 0..16i32 {
            for x in 0..16i32 {
                let expected = column.block_padded(x + 1, 32 + y, z + 1);
                assert_eq!(
                    generated.cells[(x + z * 16 + y * 256) as usize],
                    Cell::from_parts(expected, 0)
                );
            }
        }
    }
    for y in [-1, 8, i32::MIN, i32::MAX] {
        let outside = column.page(page(-21, y, 12), None).unwrap();
        assert!(outside.cells.iter().all(|&c| c == Cell::AIR));
    }
    assert!(column.page(page(-20, 2, 12), None).is_err());
    assert!(generator.generate_column(i32::MAX, 0).is_err());
}

#[test]
fn seed_and_coordinate_hashes_are_repeatable() {
    let a = Generator::new(TerrainSeed(1));
    let b = Generator::new(TerrainSeed(1));
    let different = Generator::new(TerrainSeed(7));
    let mut changes = 0;
    for id in [page(0, 3, 0), page(-7, 2, 5), page(12, 4, -2)] {
        let pa = a.generate_page(id, None).unwrap();
        let pb = b.generate_page(id, None).unwrap();
        assert_eq!(pa.canonical_hash(), pb.canonical_hash());
        changes += usize::from(
            pa.canonical_hash() != different.generate_page(id, None).unwrap().canonical_hash(),
        );
    }
    assert!(changes > 0);
}

#[test]
fn worker_codec_roundtrips_full_cell_width_and_metadata() {
    let request = task();
    assert_eq!(
        GenerationTask::decode(&request.encode()).unwrap().page,
        request.page
    );
    let mut generated = request.execute(&CancellationToken::new()).unwrap();
    generated.cells[77] = Cell::from_raw(u32::MAX);
    generated.expected = Some(Version(u64::MAX));
    let decoded = GeneratedPage::decode(&generated.encode()).unwrap();
    assert_eq!(decoded.canonical_hash(), generated.canonical_hash());
    assert_eq!(decoded.cells[77].raw(), u32::MAX);
    assert_eq!(decoded.work, generated.work);
}

#[test]
fn malformed_worker_messages_are_rejected_before_allocating_output() {
    let request = task().encode();
    let valid = task().execute(&CancellationToken::new()).unwrap().encode();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for length in 0..request.len() {
        assert!(GenerationTask::decode(&request[..length]).is_err());
    }
    for length in [0, 1, 64, 100, valid.len() - 1] {
        assert!(GeneratedPage::decode(&valid[..length]).is_err());
    }
    let counts = scope.finish();
    assert_eq!(counts.alloc_calls, 0);
    assert_eq!(counts.realloc_calls, 0);
    let mut wrong = request;
    wrong[24] = 3;
    assert_eq!(GenerationTask::decode(&wrong).unwrap_err(), WireError::Tag);
    wrong = request;
    wrong[25] = 1;
    assert_eq!(GenerationTask::decode(&wrong).unwrap_err(), WireError::Tag);
    let mut wrong = valid;
    wrong[29] = 99;
    assert_eq!(
        GeneratedPage::decode(&wrong).unwrap_err(),
        WireError::GeneratorVersion
    );
}

#[test]
fn cooperative_cancellation_stops_at_bounded_phase_boundaries() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(task().execute(&cancel).is_err());
    let checks = std::cell::Cell::new(0usize);
    let result = task().execute_with_cancellation(|| {
        let count = checks.get() + 1;
        checks.set(count);
        count >= 2
    });
    assert!(result.is_err());
    assert_eq!(checks.get(), 2);
}

#[cfg(not(target_arch = "wasm32"))]
fn result_from(
    pool: &jobs::NativePool<GenerationTask, GeneratedPage>,
) -> jobs::Completion<GeneratedPage> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(value) = pool.try_recv() {
            return value;
        }
        assert!(
            std::time::Instant::now() < until,
            "generation worker timeout"
        );
        std::thread::yield_now();
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_generation_is_offthread_unknown_until_atomic_publication() {
    let mut world = world::World::new(rules::properties, 8);
    let task = task();
    let mut queue = jobs::Scheduler::new(jobs::Limits::default()).unwrap();
    let pool = jobs::NativePool::new(
        1,
        2,
        |request: GenerationTask, token: &CancellationToken| {
            assert!(
                std::thread::current()
                    .name()
                    .unwrap_or_default()
                    .starts_with("cubi-worker-")
            );
            request.execute(token)
        },
    )
    .unwrap();
    queue
        .enqueue(task.job_spec(jobs::Priority::Normal, Tick(0)), task)
        .unwrap();
    let mut budget = jobs::WorkBudget::unrestricted(jobs::WorkCost {
        cells: 1_000_000,
        ..jobs::WorkCost::ZERO
    });
    let jobs::Take::Ready(dispatch) = queue.take_next(Tick(0), &mut budget, |_| None) else {
        panic!("job not ready")
    };
    pool.try_submit(dispatch).unwrap();
    let position = core_types::BlockPos {
        space: SpaceId(0),
        x: 0,
        y: 48,
        z: 0,
    };
    assert_eq!(world.read(position), world::CellRead::Unknown);
    let jobs::Finish::Published(generated) = queue.finish(result_from(&pool), |_| None) else {
        panic!("current job not published")
    };
    assert_eq!(world.read(position), world::CellRead::Unknown);
    let mut tx = world::Transaction::new(Tick(1), SpaceId(0));
    tx.expect_page(generated.page, Version(0));
    tx.create_page(generated.page, generated.cells.to_vec());
    world.commit(tx).unwrap();
    assert!(matches!(world.read(position), world::CellRead::Known(_)));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn generated_result_is_rejected_after_world_input_revision_changes() {
    let mut world = world::World::new(rules::properties, 8);
    let task = task();
    let mut queue = jobs::Scheduler::new(jobs::Limits::default()).unwrap();
    let pool = r#gen::native_pool(1, 2).unwrap();
    queue
        .enqueue(task.job_spec(jobs::Priority::Normal, Tick(0)), task)
        .unwrap();
    let mut budget = jobs::WorkBudget::unrestricted(jobs::WorkCost {
        cells: 1_000_000,
        ..jobs::WorkCost::ZERO
    });
    let jobs::Take::Ready(dispatch) = queue.take_next(Tick(0), &mut budget, |_| None) else {
        panic!("job not ready")
    };
    pool.try_submit(dispatch).unwrap();
    let mut tx = world::Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(task.page, vec![Cell::from_parts(3, 0); 4096]);
    world.commit(tx).unwrap();
    let finished = queue.finish(result_from(&pool), |page| {
        let version = world.page_version(page);
        (version.0 != 0).then_some(version)
    });
    assert!(matches!(finished, jobs::Finish::Stale));
    assert!(
        world
            .snapshot(task.page)
            .unwrap()
            .cells()
            .iter()
            .all(|cell| cell.id() == 3)
    );
}

#[test]
fn distant_columns_and_spawn_queries_do_not_overflow() {
    let generator = Generator::new(TerrainSeed(1337));
    let column = generator.generate_column(4000, -4000).unwrap();
    assert_eq!(column.raw_blocks().len(), 41472);
    for (x, z) in [(i32::MIN, i32::MIN), (i32::MAX, i32::MAX)] {
        assert!(!generator.protects_spawn(x, z));
        let _ = generator.column_info(x, z);
    }
}
