use client::{
    ClientScene, INPUT_PAGES, MESH_RETAINED_BYTES, SceneConfig, SceneError, SceneOutput, SceneTask,
};
use core_types::{
    BlockPos, Cell, ChannelMask, ComponentId, PAGE_VOLUME, PageId, SpaceId, Tick, Version,
};
use jobs::{CancellationToken, Completion, Dispatch, JobOutcome};
use std::{alloc::System, hint::black_box};
use telemetry::{AllocationScope, CountingAllocator};
use world::{CellRead, ComponentData, Transaction};

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);
fn config() -> SceneConfig {
    SceneConfig {
        center: BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 64,
            z: 8,
        },
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        ..SceneConfig::forest()
    }
}
fn point(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn center() -> PageId {
    point(8, 64, 8).page()
}
fn populated(cell: Cell) -> ClientScene {
    let mut scene = ClientScene::new(config()).unwrap();
    let pages: Vec<_> = scene.requested_pages().collect();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for page in pages {
        let mut cells = vec![Cell::AIR; PAGE_VOLUME];
        if page == center() {
            cells[8 + 8 * 16 + 8 * 256] = cell;
        }
        tx.create_page(page, cells);
    }
    scene.apply_transaction(tx).unwrap();
    scene
}
fn take(scene: &mut ClientScene, tick: u64) -> Dispatch<SceneTask> {
    scene
        .take_task(Tick(tick), &mut ClientScene::default_budget())
        .expect("queued task fits logical allowance")
}
fn execute(dispatch: Dispatch<SceneTask>) -> Completion<SceneOutput> {
    Completion {
        id: dispatch.id,
        outcome: JobOutcome::Success(dispatch.payload.execute(&dispatch.cancellation).unwrap()),
    }
}
fn ready_mesh(scene: &mut ClientScene) {
    scene.phase(Tick(1), 8).unwrap();
    let task = take(scene, 1);
    assert!(matches!(task.payload, SceneTask::Mesh(_)));
    scene.complete(execute(task)).unwrap();
    scene.phase(Tick(1), 8).unwrap();
}

#[test]
fn unknown_halo_blocks_meshing_and_never_generates_inside_reads() {
    let mut scene = ClientScene::new(config()).unwrap();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        center(),
        vec![Cell::from_parts(rules::ids::STONE, 0); PAGE_VOLUME],
    );
    scene.apply_transaction(tx).unwrap();
    let work = scene.phase(Tick(0), 8).unwrap();
    assert!(work.halo_waits > 0);
    assert_eq!(scene.stats().generation_published, 0);
    assert_eq!(scene.stats().meshes_published, 0);
    assert_eq!(scene.world().read(point(16, 64, 0)), CellRead::Unknown);
    assert!(matches!(
        take(&mut scene, 0).payload,
        SceneTask::Generation(_)
    ));
}

#[test]
fn all_27_inputs_are_captured_and_a_diagonal_ao_change_rejects_old_mesh() {
    let mut scene = populated(Cell::from_parts(rules::ids::STONE, 0));
    scene.phase(Tick(1), 8).unwrap();
    let dispatch = take(&mut scene, 1);
    if let SceneTask::Mesh(mesh) = &dispatch.payload {
        assert_eq!(mesh.inputs.len(), INPUT_PAGES);
        let unique: std::collections::BTreeSet<_> =
            mesh.inputs.iter().map(|input| input.page).collect();
        assert_eq!(unique.len(), INPUT_PAGES);
    } else {
        panic!("expected mesh");
    }
    let old = execute(dispatch);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(point(16, 80, 16), Cell::from_parts(rules::ids::STONE, 0));
    scene.apply_transaction(tx).unwrap();
    assert_eq!(scene.take_cancellation(), Some(old.id));
    assert!(scene.take_cancellation().is_none());
    scene.complete(old).unwrap();
    let work = scene.phase(Tick(2), 8).unwrap();
    assert_eq!(work.mesh_results, 0);
    assert_eq!(scene.stats().stale_results, 1);
    assert!(scene.take_mesh().is_none());
    let new = take(&mut scene, 2);
    scene.complete(execute(new)).unwrap();
    scene.phase(Tick(2), 8).unwrap();
    assert!(scene.take_mesh().is_some());
    assert_eq!(scene.stats().mesh_cells_scanned, 2 * 18 * 18 * 18);
}

#[test]
fn contents_and_emission_only_changes_do_not_cancel_mesh_or_dirty_geometry() {
    let mut scene = populated(Cell::from_parts(rules::ids::FURNACE, 2));
    scene.phase(Tick(1), 8).unwrap();
    let dispatch = take(&mut scene, 1);
    let before = execute(dispatch);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(
        point(8, 72, 8),
        Cell::from_parts(rules::ids::FURNACE, 2 | rules::blocks::ST_LIT),
    );
    tx.set_component(
        ComponentId(1),
        ComponentData {
            anchor: point(8, 72, 8),
            kind: 1,
            payload: vec![4, 5],
        },
        ChannelMask::CONTENTS,
    );
    let change = scene.apply_transaction(tx).unwrap();
    assert!(!change.channels.intersects(ChannelMask::SHAPE));
    assert_eq!(scene.pending_meshes(), 0);
    scene.complete(before).unwrap();
    scene.phase(Tick(2), 8).unwrap();
    assert!(scene.take_mesh().is_some());
    assert_eq!(scene.stats().meshes_published, 1);
    assert_eq!(scene.stats().stale_results, 0);
    assert!(scene.is_ready());
}

#[test]
fn repeated_invalidations_coalesce_before_worker_snapshot_building() {
    let mut scene = populated(Cell::from_parts(rules::ids::STONE, 0));
    for _ in 0..100_000 {
        scene.invalidate_page(center(), Tick(0));
    }
    assert_eq!(scene.pending_meshes(), 1);
    scene.phase(Tick(1), 8).unwrap();
    assert_eq!(scene.stats().queued_jobs, 1);
    let job = take(&mut scene, 1);
    scene.complete(execute(job)).unwrap();
    scene.phase(Tick(1), 8).unwrap();
    let mesh = scene.take_mesh().unwrap();
    assert_eq!(mesh.work.work.cells_scanned, 18 * 18 * 18);
    assert_eq!(mesh.copied_halo_cells, 18 * 18 * 18);
    assert_eq!(mesh.mesh.quad_count(), 6);
    assert_eq!(mesh.mesh.upload_bytes(), 96);
    assert!(
        scene
            .take_task(Tick(1), &mut ClientScene::default_budget())
            .is_none()
    );
}

#[test]
fn interior_edit_marks_one_mesh_and_corner_edit_marks_at_most_eight() {
    let mut cfg = config();
    cfg.horizontal_radius = 1;
    cfg.vertical_radius = 1;
    cfg.max_loaded_pages = 125;
    let mut scene = ClientScene::new(cfg).unwrap();
    let pages: Vec<_> = scene.requested_pages().collect();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for page in pages {
        tx.create_page(page, vec![Cell::AIR; PAGE_VOLUME]);
    }
    scene.apply_transaction(tx).unwrap();
    // Clear initial dirties by scheduling every visible page without executing;
    // later invalidation must replace/cancel only the geometrically dependent ones.
    for _ in 0..32 {
        scene.phase(Tick(0), 8).unwrap();
        while let Some(task) = scene.take_task(Tick(0), &mut ClientScene::default_budget()) {
            scene.complete(execute(task)).unwrap();
        }
        scene.phase(Tick(0), 8).unwrap();
        while scene.take_mesh().is_some() {}
        if scene.is_ready() {
            break;
        }
    }
    assert_eq!(scene.pending_meshes(), 0);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(point(8, 72, 8), Cell::from_parts(rules::ids::STONE, 0));
    scene.apply_transaction(tx).unwrap();
    assert_eq!(scene.pending_meshes(), 1);
    scene.phase(Tick(1), 0).unwrap();
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(point(15, 79, 15), Cell::from_parts(rules::ids::STONE, 0));
    scene.apply_transaction(tx).unwrap();
    assert_eq!(scene.pending_meshes(), 8);
}

#[test]
fn saturated_scheduler_keeps_all_current_generation_requests() {
    let mut cfg = config();
    cfg.scheduler.max_jobs = 1;
    cfg.scheduler.max_bytes = MESH_RETAINED_BYTES + 1024 * 1024;
    cfg.scheduler.reserved_interactive_jobs = 0;
    cfg.scheduler.reserved_interactive_bytes = 0;
    let mut scene = ClientScene::new(cfg).unwrap();
    for _ in 0..100 {
        scene.phase(Tick(0), 8).unwrap();
    }
    assert_eq!(scene.stats().queued_jobs, 1);
    assert_eq!(scene.pending_generation(), 27);
    let mut executions = 0;
    while !scene.is_ready() {
        let job = take(&mut scene, 0);
        scene.complete(execute(job)).unwrap();
        scene.phase(Tick(0), 8).unwrap();
        executions += 1;
        assert!(scene.stats().queued_jobs + scene.stats().running_jobs <= 1);
        assert!(executions <= 28);
    }
    assert_eq!(scene.stats().generation_published, 27);
    assert_eq!(scene.pending_generation(), 0);
    assert_eq!(scene.world().loaded_pages(), 27);
    assert_eq!(executions, 28);
    assert!(scene.stats().backpressure > 0);
}

#[test]
fn worker_backpressure_requeues_the_same_identity_without_losing_payload() {
    let mut scene = ClientScene::new(config()).unwrap();
    scene.phase(Tick(0), 8).unwrap();
    let first = take(&mut scene, 0);
    let id = first.id;
    let before = first.payload.encode();
    scene.requeue(first);
    let again = take(&mut scene, 0);
    assert_eq!(again.id, id);
    assert_eq!(again.payload.encode(), before);
}

#[test]
fn generation_arrivals_publish_in_deterministic_dispatch_order() {
    let mut cfg = config();
    cfg.scheduler.max_jobs = 2;
    cfg.scheduler.reserved_interactive_jobs = 0;
    cfg.scheduler.reserved_interactive_bytes = 0;
    let mut a = ClientScene::new(cfg).unwrap();
    let mut b = ClientScene::new(cfg).unwrap();
    for scene in [&mut a, &mut b] {
        scene.phase(Tick(0), 8).unwrap();
        let first = take(scene, 0);
        let second = take(scene, 0);
        let first_page = match &first.payload {
            SceneTask::Generation(task) => task.page,
            _ => unreachable!(),
        };
        let second_page = match &second.payload {
            SceneTask::Generation(task) => task.page,
            _ => unreachable!(),
        };
        scene.complete(execute(second)).unwrap();
        assert_eq!(scene.phase(Tick(0), 8).unwrap().applied, 0);
        assert_eq!(scene.world().loaded_pages(), 0);
        scene.complete(execute(first)).unwrap();
        scene.phase(Tick(0), 8).unwrap();
        assert_eq!(scene.world().page_version(first_page), Version(1));
        assert_eq!(scene.world().page_version(second_page), Version(2));
    }
    assert_eq!(a.world().canonical_hash(), b.world().canonical_hash());
}

#[test]
fn worker_failures_retry_boundedly_without_infinite_silent_loss() {
    let mut cfg = config();
    cfg.max_retries = 2;
    cfg.max_new_jobs_per_phase = 1;
    cfg.scheduler.max_jobs = 1;
    cfg.scheduler.reserved_interactive_jobs = 0;
    cfg.scheduler.reserved_interactive_bytes = 0;
    let mut scene = ClientScene::new(cfg).unwrap();
    scene.phase(Tick(0), 8).unwrap();
    for _ in 0..54 {
        let job = take(&mut scene, 0);
        scene
            .fail(job.id, "deliberate worker fault".into())
            .unwrap();
        scene.phase(Tick(0), 8).unwrap();
    }
    assert_eq!(scene.pending_generation(), 0);
    assert_eq!(scene.stats().failed_jobs, 54);
    assert!(matches!(scene.error(), Some(SceneError::WorkerFailed(_))));
    assert!(
        scene
            .take_task(Tick(0), &mut ClientScene::default_budget())
            .is_none()
    );
}

#[test]
fn resident_limit_is_checked_before_mutating_the_world() {
    let mut scene = populated(Cell::AIR);
    let before = scene.world().canonical_hash();
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 100,
            y: 4,
            z: 100,
        },
        vec![Cell::AIR; PAGE_VOLUME],
    );
    assert!(matches!(
        scene.apply_transaction(tx),
        Err(SceneError::PageLimit)
    ));
    assert_eq!(scene.world().canonical_hash(), before);
}

#[test]
fn idle_native_client_pump_performs_zero_allocations_after_warmup() {
    let mut scene = populated(Cell::from_parts(rules::ids::STONE, 0));
    ready_mesh(&mut scene);
    scene.take_mesh().unwrap();
    let pool = client::native_pool(1, 1).unwrap();
    for _ in 0..8 {
        scene
            .pump_native(&pool, Tick(1), &mut ClientScene::default_budget(), 8)
            .unwrap();
    }
    let scope = AllocationScope::begin().unwrap();
    let mut positive = Vec::with_capacity(128);
    positive.push(1_u8);
    black_box(&positive);
    drop(positive);
    assert!(scope.finish().alloc_calls > 0);
    let before = scene.world().canonical_hash();
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        let work = scene
            .pump_native(&pool, Tick(1), &mut ClientScene::default_budget(), 8)
            .unwrap();
        assert_eq!(work.enqueued, 0);
        assert_eq!(work.dispatched, 0);
        assert_eq!(work.applied, 0);
        black_box(scene.stats());
    }
    let counts = scope.finish();
    assert_eq!(counts, telemetry::AllocationCounts::ZERO);
    assert_eq!(scene.world().canonical_hash(), before);
}

#[test]
fn worker_wire_roundtrip_preserves_exact_mesh_and_rejects_corruption() {
    let mut scene = populated(Cell::from_parts(rules::ids::STONE, 0));
    scene.phase(Tick(1), 8).unwrap();
    let job = take(&mut scene, 1);
    let encoded = job.payload.encode();
    let decoded = SceneTask::decode(&encoded).unwrap();
    let result = decoded.execute(&CancellationToken::new()).unwrap();
    let bytes = result.encode();
    let roundtrip = SceneOutput::decode(&bytes).unwrap();
    match (&result, roundtrip) {
        (SceneOutput::Mesh(a), SceneOutput::Mesh(b)) => {
            assert_eq!(a.mesh.opaque, b.mesh.opaque);
            assert_eq!(a.inputs, b.inputs);
            assert_eq!(a.work, b.work);
        }
        _ => panic!("expected mesh outputs"),
    }
    for length in [0, 4, encoded.len() - 1] {
        assert!(SceneTask::decode(&encoded[..length]).is_err());
    }
    let mut wrong = encoded.clone();
    wrong[0] ^= 1;
    assert!(SceneTask::decode(&wrong).is_err());
    let mut wrong = bytes;
    let last = wrong.len() - 16 + 3;
    wrong[last] |= 0x80;
    assert!(SceneOutput::decode(&wrong).is_err());
}

#[test]
fn grass_only_tints_the_registry_faces_and_keeps_plain_material_ids() {
    let material = client::material(Cell::from_parts(rules::ids::GRASS, 0).raw(), 0x6c47);
    assert_eq!(material.face_materials[1], rules::ids::L_GRASS_TOP as u16);
    assert_eq!(material.face_materials[0], rules::ids::L_DIRT as u16);
    assert_ne!(material.tint_faces & (1 << 1), 0);
    assert_eq!(material.tint_faces & 1, 0);
    assert!(material.face_materials.iter().all(|value| *value < 0x8000));
}
