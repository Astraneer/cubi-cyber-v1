#![cfg(not(target_arch = "wasm32"))]
use client::{
    ClientScene, SceneConfig, model_motion_identity,
    view::{ViewEntities, model_motion::ViewModelMotion},
};
use core_types::SpaceId;
use render::{
    OutputTransfer,
    entity_gpu::EntityError,
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
#[test]
fn exact_human_identity_bank_follows_only_successful_actual_view_uploads() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut terrain = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 4,
            max_quads: 1024,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
        1,
    )
    .unwrap();
    let scene = ClientScene::new(SceneConfig {
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        authoritative_remote: true,
        ..SceneConfig::forest()
    })
    .unwrap();
    let mut entities = ViewEntities::prepare(&gpu.device, &gpu.queue, &terrain, None).unwrap();
    assert!(entities.published_temporal_identities().is_none());
    terrain.prepare_scene_sampling(&gpu.device).unwrap();
    let config = render::model_motion::TargetConfig {
        extent: [64, 64],
        samples: 1,
        max_pixels: 4096,
        memory_budget_bytes: 2 * 1024 * 1024,
    };
    let required = ViewModelMotion::required_allocation_bytes(
        config,
        gpu.device.limits().max_texture_dimension_2d,
        gpu.device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY),
        Some(entities.gpu.instance_capacity()),
        None,
    )
    .unwrap();
    let mut motion =
        ViewModelMotion::prepare(&gpu.device, &gpu.queue, config, Some(&mut entities), None)
            .unwrap();
    assert_eq!(motion.startup_work().pipeline_compilations, 2);
    assert_eq!(motion.allocation_bytes(), required);
    let a = net::PlayerId([7; 16]);
    let mut b = a;
    b.0[15] = 8;
    let keys = [
        model_motion_identity::human(0, a).unwrap(),
        model_motion_identity::human(0, b).unwrap(),
    ];
    let rows = [-1., 1.].map(|x| rules::entities::mob::Instance {
        position: [x, 0., 0.],
        kind: 5,
        scale: 1.,
        id_low: 5,
        id_high: 6,
        ..Default::default()
    });
    entities
        .set_remote_frame_with_identities(&rows, &keys, &scene, SpaceId(0), [0; 3])
        .unwrap();
    assert_eq!(entities.published_temporal_identities().unwrap(), &[]);
    assert!(matches!(
        entities.upload_local(
            &gpu.queue,
            None,
            0.,
            sim::CameraMode::FirstPerson,
            &scene,
            SpaceId(0),
            [0; 3],
            &mut UploadBudget::new(0)
        ),
        Err(EntityError::UploadBudget)
    ));
    assert_eq!(entities.published_temporal_identities().unwrap(), &[]);
    entities
        .upload_local(
            &gpu.queue,
            None,
            0.,
            sim::CameraMode::FirstPerson,
            &scene,
            SpaceId(0),
            [0; 3],
            &mut UploadBudget::new(u64::MAX),
        )
        .unwrap();
    assert_eq!(
        entities.published_temporal_identities().unwrap(),
        &keys.map(Some)
    );
    let old = entities.gpu.instance_records().to_vec();
    let current = [rows[1], rows[0]];
    let reversed = [keys[1], keys[0]];
    entities
        .set_remote_frame_with_identities(&current, &reversed, &scene, SpaceId(0), [0; 3])
        .unwrap();
    assert_eq!(
        entities.published_temporal_identities().unwrap(),
        &keys.map(Some)
    );
    assert!(
        entities
            .upload_local(
                &gpu.queue,
                None,
                0.,
                sim::CameraMode::FirstPerson,
                &scene,
                SpaceId(0),
                [0; 3],
                &mut UploadBudget::new(0)
            )
            .is_err()
    );
    assert_eq!(
        entities.published_temporal_identities().unwrap(),
        &keys.map(Some)
    );
    assert_eq!(entities.gpu.instance_records(), &old);
    entities
        .upload_local(
            &gpu.queue,
            None,
            0.,
            sim::CameraMode::FirstPerson,
            &scene,
            SpaceId(0),
            [0; 3],
            &mut UploadBudget::new(u64::MAX),
        )
        .unwrap();
    assert_eq!(
        entities.published_temporal_identities().unwrap(),
        &reversed.map(Some)
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        entities
            .set_remote_frame_with_identities(&current, &reversed, &scene, SpaceId(0), [0; 3])
            .unwrap();
        let update = entities
            .upload_local(
                &gpu.queue,
                None,
                0.,
                sim::CameraMode::FirstPerson,
                &scene,
                SpaceId(0),
                [0; 3],
                &mut UploadBudget::new(0),
            )
            .unwrap();
        assert_eq!(update.work.upload_bytes, 0);
        assert_eq!(update.work.cpu_vertices, 0);
    }
    let allocations = scope.finish();
    assert_eq!(allocations.allocating_calls(), 0);
    assert_eq!(allocations.dealloc_calls, 0);
    let camera = render::terrain_gpu::Camera::perspective(
        [0., 2., 6.],
        [0., 1., 0.],
        [0., 1., 0.],
        0.7,
        1.,
        0.1,
        100.,
    );
    let mut history = render::taa::PresentedHistory::default();
    let frame = |frame| render::taa::TaaFrame {
        frame,
        space: 0,
        scene_revision: 1,
        geometry_epoch: 1,
        source_epoch: 1,
        jitter_pixels: [0.; 2],
        reset: render::taa::ResetReasons::NONE,
    };
    let state = render::presented_motion::Frame {
        space: 0,
        geometry_epoch: 1,
        source_epoch: 1,
        extent: [64, 64],
        origin: [0; 3],
        unjittered_view_projection: camera.view_projection,
        reset: false,
    };
    let token = history.begin(frame(1)).unwrap();
    let work = motion
        .stage(token, state, [0.; 2], Some(&entities), None)
        .unwrap();
    assert_eq!(work.records_visited, 2);
    let bytes = motion.required_upload_bytes().unwrap();
    assert!(bytes > 0);
    let mut denied = UploadBudget::new(bytes - 1);
    assert!(motion.upload(&gpu.queue, token, &mut denied).is_err());
    assert_eq!(denied.remaining_bytes, bytes - 1);
    assert!(motion.validate_commit(token).is_err());
    assert_eq!(
        motion
            .upload(&gpu.queue, token, &mut UploadBudget::new(bytes))
            .unwrap()
            .upload_bytes,
        bytes
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    entities
        .prepare_camera(&gpu.queue, &mut encoder, camera)
        .unwrap();
    terrain
        .encode_scene_with_extra(
            &gpu.queue,
            &mut encoder,
            &camera,
            render::terrain_gpu::CullingMode::Frustum,
            false,
            |pass| entities.draw(pass),
        )
        .unwrap();
    motion
        .targets()
        .encode(&mut encoder, terrain.scene_depth_msaa_view(), |pass| {
            render::model_motion::Work {
                draws: motion.draw(pass, token).unwrap(),
                ..Default::default()
            }
        });
    motion.mark_encoded(token).unwrap();
    history.mark_encoded(token).unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    motion.validate_commit(token).unwrap();
    history.validate_commit(token, 8).unwrap();
    motion.commit_presented(token).unwrap();
    history.commit_presented(token, 8).unwrap();
    let discarded = history.begin(frame(2)).unwrap();
    motion
        .stage(discarded, state, [0.; 2], Some(&entities), None)
        .unwrap();
    assert!(motion.validate_commit(token).is_err());
    motion.discard(discarded).unwrap();
    history.discard(discarded).unwrap();
    // Legacy/unkeyed Human records cannot inherit the old UUID's history.
    entities
        .set_remote_frame(&current, &scene, SpaceId(0), [0; 3])
        .unwrap();
    entities
        .upload_local(
            &gpu.queue,
            None,
            0.,
            sim::CameraMode::FirstPerson,
            &scene,
            SpaceId(0),
            [0; 3],
            &mut UploadBudget::new(0),
        )
        .unwrap();
    assert_eq!(
        entities.published_temporal_identities().unwrap(),
        &[None, None]
    );
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
}
