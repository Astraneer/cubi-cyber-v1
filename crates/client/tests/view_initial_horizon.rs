#![cfg(not(target_arch = "wasm32"))]
//! Initial Far publication uses ordinary uploads, never its first HDR encode.
use client::{
    ClientScene, SceneConfig, native_pool,
    view::{ClientView, EnvironmentProfile, horizon},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use render::{
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    post::{Exposure, PostConfig},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};

fn image(
    key: net::height::HeightKey,
    source: u64,
    certified: bool,
) -> Box<net::height::HeightTile> {
    let mut known = [255; 37];
    known[36] = 1;
    Box::new(net::height::HeightTile {
        key,
        source,
        error: certified.then_some(net::height::HeightErrorBound {
            max_error_q1024: 0,
            min_height: 8,
            max_height: 8,
        }),
        heights: [8; 289],
        known,
        atlas: [0; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: net::height::Replacement::None,
    })
}

#[test]
fn initial_far_quality_commits_during_loading_before_any_hdr_and_handles_explicit_empty_cover() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        render::OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 32,
            max_quads: 262144,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let center = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 8,
        z: 8,
    };
    let mut view = ClientView::new(
        renderer,
        SceneConfig {
            center,
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 32,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        64,
        64,
        render::OutputTransfer::Attachment,
        EnvironmentProfile {
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                post: PostConfig {
                    exposure: Exposure::Manual(1.),
                    bloom: None,
                    ..Default::default()
                },
                ..Default::default()
            },
            celestials: None,
            aerial: None,
            multiple_scattering: None,
            ..Default::default()
        },
        &atlas,
    )
    .unwrap();
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if page == center.page() {
            cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
        }
        view.scene
            .import_authoritative_page(
                &net::PageUpdate::Snapshot(net::PageSnapshot {
                    page,
                    version: 1,
                    source_version: Version(1),
                    applied_action: Default::default(),
                    cells,
                }),
                Tick(1),
            )
            .unwrap();
    }
    let pool = native_pool(2, 8).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let config = horizon::Config {
        horizon_level: 0,
        max_tiles: 64,
        max_sources: 128,
        max_proxy_pages: 8,
        ..Default::default()
    };
    view.prepare_horizon(&gpu.device, &gpu.queue, config)
        .unwrap();
    assert!(!view.initial_scene_inputs_ready());
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("initial Far cheap Loading frames"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = output.create_view(&Default::default());
    let hdr_before = view.renderer_mut().hdr_revision();
    let loading = |view: &mut ClientView| {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode_loading_clear(&mut encoder, &output);
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        assert_eq!(view.stats().terrain_frames, 0);
        assert_eq!(view.renderer_mut().hdr_revision(), hdr_before);
        assert!(!view.has_cached_present());
    };
    while !view.scene.is_ready()
        || view.stats().resident_pages != 1
        || view.horizon_wanted_height().next().is_none()
    {
        assert!(Instant::now() < deadline, "initial public selection");
        view.scene
            .pump_native(&pool, Tick(1), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        loading(&mut view);
    }
    let keys = view.horizon_wanted_height().collect::<Vec<_>>();
    for &key in &keys {
        view.ingest_remote_height(image(key, 1, false)).unwrap();
    }
    while !view.has_committed_far_front() {
        assert!(Instant::now() < deadline, "uncertified coarse cover");
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        loading(&mut view);
    }
    let coarse = view.horizon_stats().unwrap();
    assert!(coarse.committed_missing_error_certificates > 0);
    assert!(
        !view.initial_scene_inputs_ready(),
        "a coarse front is not the requested quality"
    );
    for &key in &keys {
        view.ingest_remote_height(image(key, 2, true)).unwrap();
    }
    loop {
        assert!(Instant::now() < deadline, "certified candidate preparation");
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(0))
            .unwrap();
        loading(&mut view);
        let stats = view.horizon_stats().unwrap();
        if stats.candidate_plan_id > coarse.candidate_plan_id
            && stats.missing_error_certificates == 0
        {
            assert_eq!(stats.committed_plan_id, coarse.committed_plan_id);
            assert!(
                !view.initial_scene_inputs_ready(),
                "CPU completion cannot replace GPU publication"
            );
            break;
        }
    }
    while !view.initial_scene_inputs_ready() {
        assert!(
            Instant::now() < deadline,
            "same-budget Far publication without HDR"
        );
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        loading(&mut view);
    }
    assert!(view.lod_error_setting_applied(32.));
    let prior_projection = view.horizon_stats().unwrap().committed_plan_id;
    // Actual loading resize changes the projection after a certified front but
    // before the first HDR. The old front stays drawable; its error receipt
    // cannot certify this new viewport while the replacement is only CPU-ready.
    view.resize(&gpu.device, 64, 96).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(0))
        .unwrap();
    assert!(view.has_committed_far_front());
    assert!(!view.lod_error_setting_applied(32.));
    assert!(!view.initial_scene_inputs_ready());
    assert_eq!(
        view.horizon_stats().unwrap().committed_plan_id,
        prior_projection
    );
    while !view.initial_scene_inputs_ready() {
        assert!(Instant::now() < deadline, "new projection publication");
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        loading(&mut view);
    }
    assert!(view.lod_error_setting_applied(32.));
    assert!(view.horizon_stats().unwrap().committed_plan_id > prior_projection);
    assert!(
        !view.is_ready(),
        "actual scene presentation is still required"
    );
    let resized_output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("initial Far actual resized HDR"),
        size: wgpu::Extent3d {
            width: 64,
            height: 96,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let resized_output = resized_output.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &resized_output, 64, 96)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(view.has_cached_present() && view.is_ready());
    let front = view.horizon_stats().unwrap().committed_plan_id;
    view.set_horizon_settings(3, 16.).unwrap();
    assert!(!view.initial_scene_inputs_ready());
    assert!(view.has_cached_present() && view.has_committed_far_front());
    assert_eq!(view.horizon_stats().unwrap().committed_plan_id, front);

    // A source-neutral component can be wholly covered by Near. Only its
    // completed empty selection, never an untouched cache, is an exemption.
    let mut empty =
        horizon::ViewHorizon::prepare(&gpu.device, &gpu.queue, view.renderer_mut(), None, config)
            .unwrap();
    assert!(!empty.initial_inputs_ready());
    empty.set_display_space(1);
    assert!(
        empty.initial_inputs_ready(),
        "Far is not drawn in another space"
    );
    empty.set_display_space(0);
    empty.reserve_near_cutout(8).unwrap();
    empty
        .guard_near(
            &gpu.queue,
            0,
            [0, 0],
            &mut UploadBudget::new(2 * 1024 * 1024),
        )
        .unwrap();
    empty
        .set_projection(horizon::Projection {
            eye_min: [8., 8., 8.],
            eye_max: [9., 9., 9.],
            focal_pixels: 64.,
            diagonal_tangent: 1.,
            target_pixels: 32.,
            hysteresis_pixels: 8.,
            view_direction: [0., 0., -1.],
            view_cosine: -1.,
        })
        .unwrap();
    let plan = empty
        .plan_task(0, [0, 0])
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();
    assert!(plan.plan.tiles.is_empty());
    empty.complete_plan(plan).unwrap();
    assert!(!empty.initial_inputs_ready());
    empty
        .pump(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024), 64, 16)
        .unwrap();
    assert!(!empty.has_committed_front());
    assert!(
        empty.initial_inputs_ready(),
        "explicit empty publication is complete"
    );
    // Shrinking Near makes the old empty selection insufficient, even after
    // the new mask is resident and before a replacement plan is captured.
    empty.reserve_near_cutout(0).unwrap();
    empty
        .guard_near(
            &gpu.queue,
            0,
            [0, 0],
            &mut UploadBudget::new(2 * 1024 * 1024),
        )
        .unwrap();
    assert!(!empty.initial_inputs_ready(), "old empty generation");
    let next = empty.plan_task(0, [0, 0]).unwrap().unwrap();
    assert!(
        !empty.initial_inputs_ready(),
        "replacement is only captured"
    );
    let next = next.execute().unwrap();
    assert!(!next.plan.tiles.is_empty());
    empty.complete_plan(next).unwrap();
    assert!(
        !empty.initial_inputs_ready(),
        "new Far cells still need public data and GPU publication"
    );
}
