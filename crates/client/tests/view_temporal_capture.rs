//! The complete optional graph must fit the existing capture envelope. This is
//! a small source-known GPU fixture, not a product performance acceptance.
#![cfg(not(target_arch = "wasm32"))]
use client::{
    ClientScene, SceneConfig,
    capture::FrameGpuCapture,
    native_pool,
    view::{ClientView, EnvironmentProfile},
};
use core_types::{BlockPos, SpaceId, Tick, Version};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    gi::GiConfig,
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};
use telemetry::flight::{FlightRecorder, Sample};
const EXTENT: [u32; 2] = [96, 64];
#[path = "support/temporal_capture_far.rs"]
mod capture_far;

#[test]
fn complete_temporal_effects_and_motion_fit_the_existing_capture_envelope() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    assert!(
        gpu.device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
    );
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let atlas = AtlasImage::donor().unwrap();
    let terrain = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        EXTENT[0],
        EXTENT[1],
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 32,
            max_quads: 262144,
            occlusion: true,
        },
        &atlas,
        1,
    )
    .unwrap();
    let mut view = ClientView::new(
        terrain,
        SceneConfig {
            center: BlockPos {
                space: SpaceId(0),
                x: 8,
                y: 4,
                z: 8,
            },
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 32,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    let mut lighting =
        client::lighting::ClientLightingConfig::default().with_authoritative_source();
    lighting.shadows.resolution = 32;
    lighting.shadows.max_updates_per_frame = 3;
    lighting.shadows.dynamic_casters = true;
    lighting.shadows.sun_step_radians = 1e-6;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
    let mut profile = EnvironmentProfile::product()
        .with_quality(2, 2)
        .unwrap()
        .with_contact_ao(2)
        .unwrap()
        .with_reflections(2)
        .unwrap()
        .with_antialiasing(2)
        .unwrap();
    // Smaller spatial domains keep this a functional pass/query test; every
    // selected effect and the maximum six-level Bloom still execute.
    profile.graphics.atmosphere = AtmosphereConfig {
        transmittance_size: [16, 8],
        sky_size: [16, 8],
        transmittance_steps: 16,
        sky_steps: 16,
    };
    profile.graphics.gi = Some(GiConfig {
        world_pages: [3, 3, 3],
        probe_counts: [2, 1, 2],
        max_jobs: 4,
        rays_per_probe: 64,
        ..Default::default()
    });
    profile.graphics.post.bloom.as_mut().unwrap().levels = 6;
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        EXTENT[0],
        EXTENT[1],
        OutputTransfer::Attachment,
        profile,
        &atlas,
    )
    .unwrap();
    view.prepare_water(Default::default()).unwrap();
    view.prepare_entities(&gpu.device, &gpu.queue).unwrap();
    view.prepare_props(
        &gpu.device,
        &gpu.queue,
        client::props_source::PropsLimits {
            max_instances: 16,
            max_pages: 4,
            max_running: 2,
            max_ready: 2,
            max_age_ticks: 40,
        },
    )
    .unwrap();
    view.prepare_horizon(
        &gpu.device,
        &gpu.queue,
        client::view::horizon::Config {
            // This authored selection has25 Height tiles and8 exact pages;
            // both current/staged banks fit the existing capture/memory caps.
            max_tiles: 64,
            max_sources: 128,
            max_proxy_pages: 16,
            horizon_level: 0,
            ..Default::default()
        },
    )
    .unwrap();
    view.prepare_far_water(Default::default()).unwrap();
    let prepared =
        render::material_pack::PreparedMaterials::prepare(&atlas, &[], 0, Default::default())
            .unwrap();
    let pack =
        render::material_pack::MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap();
    view.renderer_mut()
        .prepare_material_pack(&gpu.device, &gpu.queue, pack, false)
        .unwrap();
    view.resize(&gpu.device, EXTENT[0], EXTENT[1]).unwrap();
    view.prepare_temporal(&gpu.device, &gpu.queue).unwrap();
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let cells = Box::new(std::array::from_fn(|index| {
            capture_far::cell(
                page.x * 16 + (index % 16) as i32,
                page.y * 16 + (index / 256) as i32,
                page.z * 16 + ((index / 16) % 16) as i32,
            )
        }));
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
        view.scene
            .import_authoritative_light(
                &net::light::Update::Snapshot(net::light::Snapshot {
                    header: net::light::Header {
                        page,
                        cell_view: 1,
                        version: 1,
                        source: Version(1),
                    },
                    values: Box::new([net::light::pack(15, 15, [1, 1, 15]).unwrap(); 4096]),
                }),
                Tick(1),
            )
            .unwrap();
    }
    let mut player = sim::Player::new([8., 4., 14.]);
    player.yaw = 180.;
    view.install_remote_player(&player, Tick(1)).unwrap();
    view.set_personal_time(Some(client::celestial_clock::PersonalTime::Day))
        .unwrap();
    view.set_public_clock(Tick(6000), 0.).unwrap();
    let mut far = capture_far::Fixture::install(&mut view);
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "input preparation: gates(initial,scene,near_far,far_front,lod32,far_water,props,near_water,environment)={:?}; horizon={:?}; far_water={:?}; horizon_jobs={:?}; lighting={:?}; admitted={far:?}; view={:?}; props={:?}; near_water={:?}; environment={:?}",
            (
                view.initial_scene_inputs_ready(),
                view.scene.is_ready(),
                view.near_far_join_ready(),
                view.has_committed_far_front(),
                view.lod_error_setting_applied(32.),
                view.far_water_guard_ready(),
                view.props_stats()
                    .is_some_and(|p| p.ready && p.instances == 1),
                view.water_stats().is_some_and(|w| w.ready),
                view.environment_stats().is_some_and(|e| e.source_ready),
            ),
            view.horizon_stats(),
            view.horizon_water_stats(),
            view.horizon_job_stats(),
            view.lighting_stats(),
            view.stats(),
            view.props_stats(),
            view.water_stats(),
            view.environment_stats()
        );
        view.scene
            .pump_native(&workers, Tick(1), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        far.admit_wanted(&mut view);
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        if view.initial_scene_inputs_ready()
            && view
                .props_stats()
                .is_some_and(|p| p.ready && p.instances == 1)
            && view.water_stats().is_some_and(|w| w.ready)
            && view.environment_stats().is_some_and(|e| e.source_ready)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    far.assert_committed(&mut view);
    let key = client::model_motion_identity::human(0, net::PlayerId([13; 16])).unwrap();
    view.set_remote_entities_with_identities(
        &[rules::entities::mob::Instance {
            position: [7., 2., 7.],
            kind: 5,
            scale: 1.,
            id_low: 13,
            id_high: 17,
            ..Default::default()
        }],
        &[key],
        SpaceId(0),
        [0; 3],
    )
    .unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let output = gpu
        .device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("complete optional temporal capture"),
            size: wgpu::Extent3d {
                width: EXTENT[0],
                height: EXTENT[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default());
    view.set_presentation_frame_id(1).unwrap();
    let mut capture = FrameGpuCapture::new_with_queue(&gpu.device, &gpu.queue);
    let mut recorder = FlightRecorder::new(16, 16).unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(capture.begin_frame(&mut encoder, gpu.queue.get_timestamp_period()));
    view.encode(&gpu.queue, &mut encoder, &output, EXTENT[0], EXTENT[1])
        .unwrap();
    let work = view.environment_stats().unwrap().gpu;
    assert!(work.gi.passes > 0 && work.contact_ao.passes > 0);
    assert!(work.water.passes > 0 && work.water.max_reflection_steps > 0);
    assert!(work.clouds.passes > 0 && work.aerial.passes > 0);
    assert!(work.post.histogram_samples > 0 && work.post.bloom_texels > 0);
    assert!(work.temporal.passes > 0 && work.temporal_optics.passes > 0 && work.motion_draws > 0);
    let (sources, queries) = view.gpu_timing_shape();
    assert!(
        sources >= 15 && queries >= 45,
        "all graph families must participate: sources={sources} queries={queries}"
    );
    assert!(
        sources <= 16 && queries <= 64,
        "sources={sources} queries={queries}"
    );
    let ticket = view
        .encode_gpu_capture(&mut capture, &mut encoder, 1)
        .expect("existing 16-source/64-query capture envelope");
    assert_eq!(capture.stats().failed, 0);
    gpu.queue.submit([encoder.finish()]);
    view.mark_presentation_submitted(1).unwrap();
    capture.submitted(ticket);
    recorder
        .record_frame(Sample {
            frame: 1,
            monotonic_ns: 1,
            interval_ns: 1,
            ..Default::default()
        })
        .unwrap();
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    view.commit_presentation(1).unwrap();
    for _ in 0..8 {
        capture.collect(&mut recorder);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        if capture.stats().completed == 1 {
            break;
        }
    }
    let sample = recorder.frames().next().unwrap();
    assert_eq!(capture.stats().completed, 1);
    assert!(sample.gpu_total_complete && sample.gpu_ns.is_some());
    assert!(sample.gpu_passes_ns[13].is_some());
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
    eprintln!(
        "WHOLE_VIEW_TEMPORAL_CAPTURE_OK sources={sources}/16 queries={queries}/64 full_gpu=true auto=true bloom_levels=6 models=true committed_height=true proxy_pages=8 far_water=true terrain_far_snapshots=true AO2=true Cloud2=true GI2=true SSR2=true"
    );
}
