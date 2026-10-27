#![cfg(not(target_arch = "wasm32"))]
use client::{
    ClientScene, SceneConfig, native_pool,
    view::{ClientView, EnvironmentProfile},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    post::{Exposure, PostConfig},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
const EXTENT: [u32; 2] = [96, 64];
fn view(gpu: &render::Gpu) -> ClientView {
    let atlas = AtlasImage::donor().unwrap();
    let terrain = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        EXTENT[0],
        EXTENT[1],
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 8,
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
                y: 8,
                z: 8,
            },
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 27,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        EXTENT[0],
        EXTENT[1],
        OutputTransfer::Attachment,
        EnvironmentProfile {
            temporal: Some(render::taa::TaaConfig::default()),
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                post: PostConfig {
                    exposure: Exposure::Manual(0.),
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
    view.prepare_entities(&gpu.device, &gpu.queue).unwrap();
    view.prepare_props(
        &gpu.device,
        &gpu.queue,
        client::props_source::PropsLimits {
            max_pages: 4,
            max_instances: 16,
            max_running: 2,
            max_ready: 2,
            max_age_ticks: 40,
        },
    )
    .unwrap();
    view.prepare_temporal(&gpu.device, &gpu.queue).unwrap();
    assert!(view.temporal_prepared());
    assert!(view.temporal_allocation_bytes() < 256 * 1024 * 1024);
    let center = view.scene.config().center.page();
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if page == center {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * (z + 16 * 7)] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
            cells[9 + 16 * (9 + 16 * 8)] = Cell::from_parts(rules::ids::CHEST, 0);
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
                Tick(0),
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
                    values: Box::new([net::light::pack(15, 0, [0; 3]).unwrap(); 4096]),
                }),
                Tick(0),
            )
            .unwrap();
    }
    let workers = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "bootstrap {:?}/{:?}",
            view.stats(),
            view.props_stats()
        );
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        if view.stats().resident_pages == 1
            && view
                .props_stats()
                .is_some_and(|p| p.ready && p.instances == 1)
        {
            break;
        }
        std::thread::yield_now();
    }
    let row = rules::entities::mob::Instance {
        position: [7., 8., 7.],
        kind: 5,
        scale: 1.,
        id_low: 1,
        id_high: 2,
        ..Default::default()
    };
    let key = client::model_motion_identity::human(0, net::PlayerId([19; 16])).unwrap();
    view.set_remote_entities_with_identities(&[row], &[key], SpaceId(0), [0; 3])
        .unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    view
}
fn output(gpu: &render::Gpu) -> wgpu::TextureView {
    gpu.device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("whole View temporal output"),
            size: wgpu::Extent3d {
                width: EXTENT[0],
                height: EXTENT[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
fn encode(
    gpu: &render::Gpu,
    view: &mut ClientView,
    output: &wgpu::TextureView,
    frame: u64,
) -> wgpu::CommandEncoder {
    view.set_presentation_frame_id(frame).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, output, EXTENT[0], EXTENT[1])
        .unwrap();
    encoder
}
fn submit(gpu: &render::Gpu, view: &mut ClientView, encoder: wgpu::CommandEncoder, frame: u64) {
    gpu.queue.submit([encoder.finish()]);
    view.mark_presentation_submitted(frame).unwrap();
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
}
#[test]
fn whole_view_motion_resolve_and_post_commit_only_presented_and_settle() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut view = view(&gpu);
    let output = output(&gpu);
    let first = encode(&gpu, &mut view, &output, 1);
    assert_eq!(view.pending_presentation_frame(), Some(1));
    assert!(view.commit_presentation(1).is_err());
    assert!(view.commit_presentation(2).is_err());
    assert!(
        view.environment_mut()
            .unwrap()
            .gpu
            .temporal()
            .unwrap()
            .presented()
            .is_none()
    );
    drop(first);
    view.discard_presentation(1).unwrap();
    assert!(
        view.environment_mut()
            .unwrap()
            .gpu
            .temporal()
            .unwrap()
            .presented()
            .is_none()
    );
    let second = encode(&gpu, &mut view, &output, 2);
    submit(&gpu, &mut view, second, 2);
    view.discard_presentation(2).unwrap();
    assert!(
        view.environment_mut()
            .unwrap()
            .gpu
            .post()
            .temporal_restore_pending()
    );
    // No frame has ever been presented: restore exposure beside a Loading clear,
    // never read an unwritten or candidate HDR as the cached world image.
    let restore = encode(&gpu, &mut view, &output, 3);
    assert_eq!(view.pending_presentation_frame(), None);
    submit(&gpu, &mut view, restore, 3);
    assert!(
        !view
            .environment_mut()
            .unwrap()
            .gpu
            .post()
            .temporal_restore_pending()
    );
    for frame in 4..12 {
        let encoder = encode(&gpu, &mut view, &output, frame);
        submit(&gpu, &mut view, encoder, frame);
        view.commit_presentation(frame).unwrap();
    }
    let presented = view
        .environment_mut()
        .unwrap()
        .gpu
        .temporal()
        .unwrap()
        .presented()
        .unwrap();
    assert_eq!(presented.samples, 8);
    assert_eq!(presented.state.frame, 11);
    assert!(!view.needs_redraw(EXTENT[0], EXTENT[1]));
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        assert!(!view.needs_redraw(EXTENT[0], EXTENT[1]));
    }
    let allocations = scope.finish();
    assert_eq!(allocations.alloc_calls, 0);
    assert_eq!(allocations.dealloc_calls, 0);
    assert_eq!(
        view.environment_mut()
            .unwrap()
            .gpu
            .temporal()
            .unwrap()
            .presented()
            .unwrap()
            .state
            .frame,
        11
    );
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
}
