use client::{ClientScene, CompletedMesh, MESH_CHANNELS, SceneConfig, SceneOutput, SceneTask};
use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use jobs::{Completion, Dispatch, JobOutcome};
use std::collections::{BTreeMap, BTreeSet};
use world::{PageRegion, Transaction};

fn point(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn scene(radius: u8) -> ClientScene {
    let mut config = SceneConfig::forest();
    config.center = point(8, 72, 8);
    config.horizontal_radius = radius;
    config.vertical_radius = radius;
    config.max_loaded_pages = 125;
    let mut scene = ClientScene::new(config).unwrap();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for page in scene.requested_pages() {
        tx.create_page(page, vec![Cell::AIR; PAGE_VOLUME]);
    }
    scene.apply_transaction(tx).unwrap();
    scene
}
fn execute(dispatch: Dispatch<SceneTask>) -> Completion<SceneOutput> {
    Completion {
        id: dispatch.id,
        outcome: JobOutcome::Success(dispatch.payload.execute(&dispatch.cancellation).unwrap()),
    }
}
fn warm(scene: &mut ClientScene, residents: &mut BTreeMap<PageId, CompletedMesh>, tick: u64) {
    for _ in 0..64 {
        scene.phase(Tick(tick), 64).unwrap();
        while let Some(dispatch) = scene.take_task(Tick(tick), &mut ClientScene::default_budget()) {
            scene.complete(execute(dispatch)).unwrap();
        }
        scene.phase(Tick(tick), 64).unwrap();
        while let Some(mesh) = scene.take_mesh() {
            residents.insert(mesh.page, mesh);
        }
        if scene.is_ready() {
            return;
        }
    }
    panic!("bounded fully loaded scene did not settle");
}

#[test]
fn interior_face_edge_corner_edits_invalidate_exact_resident_dependencies() {
    let mut scene = scene(1);
    let mut residents = BTreeMap::new();
    warm(&mut scene, &mut residents, 0);
    assert_eq!(residents.len(), 27);
    for (tick, changed, expected) in [
        (1, point(8, 72, 8), 1),
        (2, point(15, 72, 8), 2),
        (3, point(15, 79, 8), 4),
        (4, point(15, 79, 15), 8),
    ] {
        let meshes_before = scene.stats().meshes_published;
        let cells_before = scene.stats().mesh_cells_scanned;
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.set_cell(changed, Cell::from_parts(rules::ids::STONE, 0));
        let change = scene.apply_transaction(tx).unwrap();
        assert_eq!(change.work.pages_staged, 1);
        assert_eq!(change.work.mask_cells_classified, 1);
        assert_eq!(change.work.registry_queries, 3);
        assert_eq!(change.work.rim_stamps_updated, expected - 1);
        assert_eq!(scene.pending_meshes(), expected);
        let invalid: BTreeSet<_> = residents
            .iter()
            .filter(|(_, mesh)| !scene.world().matches_inputs(&mesh.inputs))
            .map(|(page, _)| *page)
            .collect();
        assert_eq!(
            invalid.len(),
            expected,
            "unaffected residents remain certified"
        );
        assert!(!scene.is_ready());
        warm(&mut scene, &mut residents, tick);
        assert!(scene.is_ready());
        assert!(
            residents
                .values()
                .all(|mesh| scene.world().matches_inputs(&mesh.inputs))
        );
        assert_eq!(
            scene.stats().meshes_published - meshes_before,
            expected as u64
        );
        assert_eq!(
            scene.stats().mesh_cells_scanned - cells_before,
            expected as u64 * 18 * 18 * 18
        );
        assert_eq!(scene.stats().stale_results, 0);
    }
}

#[test]
fn in_flight_center_mesh_survives_an_interior_edit_in_a_halo_source() {
    let mut scene = scene(0);
    scene.phase(Tick(1), 8).unwrap();
    let dispatch = scene
        .take_task(Tick(1), &mut ClientScene::default_budget())
        .unwrap();
    let SceneTask::Mesh(task) = &dispatch.payload else {
        panic!("expected mesh");
    };
    let east = task
        .inputs
        .iter()
        .find(|input| input.page == point(24, 72, 8).page())
        .unwrap();
    assert_eq!(east.region, PageRegion::new([-1, 0, 0]).unwrap());
    assert_eq!(east.channels, MESH_CHANNELS);
    let inputs = task.inputs;
    let encoded = dispatch.payload.encode();
    assert_eq!(&encoded[..4], b"CST3");
    let decoded = SceneTask::decode(&encoded).unwrap();
    let SceneTask::Mesh(decoded) = decoded else {
        panic!("expected mesh");
    };
    assert_eq!(
        decoded.inputs, inputs,
        "wire order reconstructs every sampled region"
    );
    let mut old_magic = encoded;
    old_magic[..4].copy_from_slice(b"CST2");
    assert!(SceneTask::decode(&old_magic).is_err());
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(point(24, 72, 8), Cell::from_parts(rules::ids::STONE, 0));
    let change = scene.apply_transaction(tx).unwrap();
    assert_eq!(change.work.rim_stamps_updated, 0);
    assert!(scene.world().matches_inputs(&inputs));
    assert!(scene.take_cancellation().is_none());
    assert_eq!(scene.pending_meshes(), 0);
    scene.complete(execute(dispatch)).unwrap();
    scene.phase(Tick(2), 8).unwrap();
    let mesh = scene
        .take_mesh()
        .expect("unchanged sampled halo is accepted");
    assert_eq!(mesh.inputs, inputs);
    assert_eq!(scene.stats().stale_results, 0);
    assert_eq!(scene.stats().meshes_published, 1);
    assert!(scene.is_ready());
}
