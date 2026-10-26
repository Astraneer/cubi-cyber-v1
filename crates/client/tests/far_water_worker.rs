use client::far_water_worker::{self as worker, BuiltFarWater, Error, FarWaterTask, MAX_QUADS};
use core_types::{PageId, SpaceId, Tick, Version};
use net::water::WaterHalo;
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn halo(mut cell: impl FnMut(i8, i8, i8) -> u8) -> Arc<WaterHalo> {
    let mut cells = Box::new([1; 5832]);
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                cells[(x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize)] =
                    cell(x, y, z);
            }
        }
    }
    Arc::new(WaterHalo {
        page: PageId {
            space: SpaceId(0),
            x: -3,
            y: 4,
            z: 7,
        },
        geometry_source: Version(19),
        geometry_view: 23,
        column_source: 37,
        water_view: 31,
        cells,
        tints: [0xf800; 256],
    })
}
fn plane(amount: u8) -> Arc<WaterHalo> {
    halo(|_, y, _| {
        if y == 0 {
            2 + amount
        } else if y < 0 {
            2
        } else {
            1
        }
    })
}
#[test]
fn foreground_retains_one_arc_then_worker_emits_independent_flat_height_and_rgb565_goldens() {
    let input = plane(8);
    let scope = telemetry::AllocationScope::begin().unwrap();
    let task = FarWaterTask::new(Arc::clone(&input), MAX_QUADS).unwrap();
    let c = scope.finish();
    assert_eq!(c.allocating_calls(), 0);
    assert_eq!(Arc::strong_count(&input), 2);
    let request = task.encode().unwrap();
    assert_eq!(request.len(), 6404);
    let built = task.execute(|| false).unwrap();
    assert_eq!(Arc::strong_count(&input), 1);
    assert!(built.complete());
    assert!(built.matches(&input));
    assert_eq!(built.mesh.quads.len(), 256);
    assert_eq!(built.classified_cells, 5832);
    assert_eq!(built.dependency_samples, 256 * 27);
    assert_eq!(built.mesh.work.cells_scanned, 4096);
    assert_eq!(built.mesh.work.cpu_frame_vertices, 0);
    for q in &built.mesh.quads {
        assert_eq!(q.words[0] >> 15 & 7, 1);
        assert_eq!(q.words[1], 0xe3e3e3e3);
        assert_eq!(q.words[2], 255);
        assert_eq!(q.words[3], 0);
    }
    let bytes = built.encode().unwrap();
    assert_eq!(bytes.len(), 128 + 256 * 16);
    let from_wire = BuiltFarWater::decode(&bytes).unwrap();
    assert_eq!(from_wire.mesh.quads, built.mesh.quads);
    assert_eq!(from_wire.stamp, built.stamp);
    assert_eq!(from_wire.mesh.work, built.mesh.work);
    assert_eq!(
        worker::execute_far_water_request(&request, || false).unwrap(),
        bytes
    );
    let shallow = FarWaterTask::new(plane(1), MAX_QUADS)
        .unwrap()
        .execute(|| false)
        .unwrap();
    assert!(shallow.mesh.quads.iter().all(|q| q.words[1] == 0x1c1c1c1c));
    let falling = FarWaterTask::new(
        halo(|_, y, _| {
            if y == 0 {
                18
            } else if y < 0 {
                2
            } else {
                1
            }
        }),
        MAX_QUADS,
    )
    .unwrap()
    .execute(|| false)
    .unwrap();
    assert_eq!(falling.mesh.quads, built.mesh.quads);
}
#[test]
fn unknown_never_certifies_an_empty_core_or_a_water_bottom_and_known_air_certifies_empty() {
    let unknown = FarWaterTask::new(halo(|_, _, _| 0), MAX_QUADS)
        .unwrap()
        .execute(|| false)
        .unwrap();
    assert!(unknown.mesh.quads.is_empty());
    assert_eq!(unknown.unknown_dependency_cells, 4096);
    assert!(!unknown.complete());
    let bottom = FarWaterTask::new(
        halo(|_, y, _| {
            if y == 0 {
                10
            } else if y < 0 {
                0
            } else {
                1
            }
        }),
        MAX_QUADS,
    )
    .unwrap()
    .execute(|| false)
    .unwrap();
    assert_eq!(
        bottom.mesh.work.unknown_surface_cells, 0,
        "near kernel omits the Unknown bottom"
    );
    assert_eq!(bottom.unknown_dependency_cells, 256);
    assert!(!bottom.complete());
    let boundary = FarWaterTask::new(
        halo(|x, y, _| {
            if x == 16 {
                0
            } else if y == 0 {
                10
            } else if y < 0 {
                2
            } else {
                1
            }
        }),
        MAX_QUADS,
    )
    .unwrap()
    .execute(|| false)
    .unwrap();
    assert_eq!(boundary.mesh.quads.len(), 240);
    assert_eq!(boundary.mesh.work.unknown_surface_cells, 16);
    assert!(!boundary.complete());
    let empty = FarWaterTask::new(halo(|x, _, _| if x == -1 { 0 } else { 1 }), MAX_QUADS)
        .unwrap()
        .execute(|| false)
        .unwrap();
    assert!(empty.mesh.quads.is_empty());
    assert!(empty.complete());
    assert!(
        BuiltFarWater::decode(&empty.encode().unwrap())
            .unwrap()
            .complete()
    );
}
#[test]
fn bounded_codecs_reject_malformed_counters_descriptors_and_payloads() {
    let input = plane(8);
    let task = FarWaterTask::new(Arc::clone(&input), MAX_QUADS).unwrap();
    let req = task.encode().unwrap();
    for end in [0, 4, 8, req.len() - 1] {
        assert!(FarWaterTask::decode(&req[..end]).is_err());
    }
    let mut bad = req.clone();
    bad[0] = 0;
    assert!(matches!(FarWaterTask::decode(&bad), Err(Error::Magic)));
    let mut bad = req.clone();
    bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(FarWaterTask::decode(&bad).is_err());
    let malformed = halo(|_, _, _| 19);
    let task = FarWaterTask::new(malformed, MAX_QUADS).unwrap();
    assert!(
        matches!(task.execute(|| false), Err(Error::Cell)),
        "code semantic decode stays in worker"
    );
    let built = FarWaterTask::new(input, MAX_QUADS)
        .unwrap()
        .execute(|| false)
        .unwrap();
    let bytes = built.encode().unwrap();
    for end in [0, 4, 52, 124, 127, bytes.len() - 1] {
        assert!(BuiltFarWater::decode(&bytes[..end]).is_err());
    }
    let mut bad = bytes.clone();
    bad[52..60].fill(0);
    assert!(matches!(BuiltFarWater::decode(&bad), Err(Error::Work)));
    let mut bad = bytes.clone();
    bad[124..128].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(BuiltFarWater::decode(&bad), Err(Error::Length)));
    let mut bad = bytes.clone();
    bad[128..132].copy_from_slice(&(7u32 << 15).to_le_bytes());
    assert!(matches!(
        BuiltFarWater::decode(&bad),
        Err(Error::Descriptor)
    ));
    let mut bad = bytes.clone();
    bad[140] = 1;
    assert!(matches!(
        BuiltFarWater::decode(&bad),
        Err(Error::Descriptor)
    ));
    let mut bad = bytes.clone();
    bad[142] = 128;
    assert!(matches!(
        BuiltFarWater::decode(&bad),
        Err(Error::Descriptor)
    ));
    let mut bad = bytes;
    bad.push(0);
    assert!(matches!(BuiltFarWater::decode(&bad), Err(Error::Length)));
    assert!(matches!(FarWaterTask::new(plane(8), 0), Err(Error::Bounds)));
}
#[test]
fn cancellation_and_budget_failure_do_not_publish_partial_meshes_and_large_mesh_is_bounded() {
    assert!(matches!(
        FarWaterTask::new(plane(8), MAX_QUADS)
            .unwrap()
            .execute(|| true),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        FarWaterTask::new(plane(8), 1).unwrap().execute(|| false),
        Err(Error::Capacity)
    ));
    let input = halo(|x, y, z| {
        if (0..16).contains(&x)
            && (0..16).contains(&y)
            && (0..16).contains(&z)
            && (x + y + z) % 2 == 0
        {
            10
        } else {
            1
        }
    });
    let built = FarWaterTask::new(input, MAX_QUADS)
        .unwrap()
        .execute(|| false)
        .unwrap();
    assert_eq!(built.mesh.quads.len(), 12288);
    assert_eq!(built.mesh.quads.capacity(), 12288);
    assert!(built.complete());
    let bytes = built.encode().unwrap();
    assert_eq!(bytes.len(), 196736);
    assert!(bytes.len() < worker::MAX_RESULT_BYTES);
    assert_eq!(
        BuiltFarWater::decode(&bytes).unwrap().mesh.quads,
        built.mesh.quads
    );
}
#[test]
fn real_native_completion_uses_external_job_identity_and_strict_public_stamp_guard() {
    let pool = worker::native_pool(1, 2).unwrap();
    let input = plane(8);
    let old = FarWaterTask::new(Arc::clone(&input), MAX_QUADS).unwrap();
    let spec = jobs::JobSpec::derived(
        jobs::JobKey {
            page: input.page,
            kind: 95,
        },
        jobs::Priority::Normal,
        jobs::WorkCost {
            cells: 120520,
            quads: MAX_QUADS as u64,
            bytes: worker::MAX_RESULT_BYTES as u64,
            rays: 0,
        },
        worker::MAX_RESULT_BYTES,
        Tick(1),
    );
    pool.try_submit(jobs::Dispatch {
        id: jobs::JobId(41),
        spec,
        cancellation: jobs::CancellationToken::new(),
        payload: old,
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let completion = loop {
        if let Some(done) = pool.try_recv() {
            break done;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(100));
    };
    assert_eq!(completion.id, jobs::JobId(41));
    let jobs::JobOutcome::Success(built) = completion.outcome else {
        panic!("real worker completion")
    };
    let mut current = (*input).clone();
    current.water_view += 1;
    assert!(!built.matches(&current));
    current.water_view = input.water_view;
    current.column_source += 1;
    assert!(!built.matches(&current));
    current.column_source = input.column_source;
    current.geometry_view += 1;
    assert!(!built.matches(&current));
    assert!(built.matches(&input));
    let token = jobs::CancellationToken::new();
    token.cancel();
    let spec = jobs::JobSpec::derived(
        jobs::JobKey {
            page: input.page,
            kind: 95,
        },
        jobs::Priority::Normal,
        jobs::WorkCost::default(),
        worker::MAX_RESULT_BYTES,
        Tick(2),
    );
    pool.try_submit(jobs::Dispatch {
        id: jobs::JobId(42),
        spec,
        cancellation: token,
        payload: FarWaterTask::new(input, MAX_QUADS).unwrap(),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(done) = pool.try_recv() {
            assert_eq!(done.id, jobs::JobId(42));
            assert!(matches!(done.outcome, jobs::JobOutcome::Cancelled));
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(100));
    }
}
