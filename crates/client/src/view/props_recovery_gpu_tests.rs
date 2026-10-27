//! Actual GPU oracles for a lost removal while bounded source recovery is partial.
//! These tests deliberately retain the old non-HIDDEN GPU bytes with upload=0.
#![cfg(not(target_arch = "wasm32"))]
use super::ViewProps;
use crate::{ClientScene, SceneConfig, props_source::PropsLimits};
use crate::{
    native_pool,
    view::{ClientView, EnvironmentProfile},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use render::terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    post::{Exposure, PostConfig},
};
use std::time::{Duration, Instant};
const SLOTS: usize = 8192;
const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;
fn position() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 4,
        z: 8,
    }
}
fn import_chest(scene: &mut ClientScene, version: u64) {
    let mut cells = Box::new([Cell::AIR; 4096]);
    cells[position().local_index().get() as usize] = Cell::from_parts(rules::ids::CHEST, 0);
    scene
        .import_authoritative_page(
            &net::PageUpdate::Snapshot(net::PageSnapshot {
                page: position().page(),
                version,
                source_version: Version(version),
                applied_action: Default::default(),
                cells,
            }),
            Tick(0),
        )
        .unwrap();
    scene
        .import_authoritative_light(
            &net::light::Update::Snapshot(net::light::Snapshot {
                header: net::light::Header {
                    page: position().page(),
                    cell_view: version,
                    version: 1,
                    source: Version(version),
                },
                values: Box::new([net::light::pack(15, 0, [0; 3]).unwrap(); 4096]),
            }),
            Tick(0),
        )
        .unwrap();
}
fn lose_owner_and_journal(scene: &mut ClientScene, page: core_types::PageId) {
    assert!(
        scene
            .remove_authoritative_page(page, Tick(0))
            .unwrap()
            .is_some()
    );
    // The default Scene journal holds4096 records. No physical time advances,
    // no cell changes are synthesized and no other cursor is recovered here.
    for _ in 0..4097 {
        let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
        tx.event(world::GameplayEvent {
            kind: 91,
            subject: None,
            position: None,
            payload: Vec::new(),
        });
        scene.derived_world_mut().commit(tx).unwrap();
    }
    assert!(scene.authoritative_version(page).is_none());
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
fn record(gpu: &render::Gpu, props: &ViewProps) -> Vec<u8> {
    record_at(gpu, props, 0)
}
fn record_at(gpu: &render::Gpu, props: &ViewProps, slot: u64) -> Vec<u8> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("unchanged formerly owned GPU prop bytes"),
        size: 64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(props.gpu.instance_buffer(), slot * 64, &buffer, 0, 64);
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
}
fn frame(gpu: &render::Gpu, terrain: &mut TerrainGpu, props: Option<&mut ViewProps>) -> Vec<u8> {
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lost Raw owner actual raster oracle"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lost owner color readback"),
        size: u64::from(WIDTH * HEIGHT * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let camera = Camera::perspective(
        [11., 8., 17.],
        [8., 4.5, 8.],
        [0., 1., 0.],
        1.,
        WIDTH as f32 / HEIGHT as f32,
        0.1,
        200.,
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let output_view = output.create_view(&Default::default());
    if let Some(props) = props {
        props
            .prepare_camera(&gpu.queue, &mut encoder, camera)
            .unwrap();
        terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &output_view,
                &camera,
                CullingMode::Frustum,
                |pass| props.draw(pass),
            )
            .unwrap();
    } else {
        terrain
            .encode_frame(
                &gpu.queue,
                &mut encoder,
                &output_view,
                &camera,
                CullingMode::Frustum,
            )
            .unwrap();
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(WIDTH * 4),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &pixels)
}
fn settle(gpu: &render::Gpu, props: &mut ViewProps, scene: &mut ClientScene) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "model source failed to settle: {:?}",
            props.source.stats(scene.world())
        );
        props.advance_cpu(scene, Tick(0)).unwrap();
        props
            .upload_ready(
                scene,
                &gpu.queue,
                &mut UploadBudget::new(2 * 1024 * 1024),
                Tick(0),
            )
            .unwrap();
        if props.stats.ready {
            break;
        }
        std::thread::yield_now();
    }
}
#[test]
fn lost_removal_quarantines_actual_old_gpu_model_before_any_hidden_upload_aa1_and4() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    for samples in [1, 4] {
        let mut terrain = TerrainGpu::new_with_samples(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            WIDTH,
            HEIGHT,
            render::OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
            samples,
        )
        .unwrap();
        let mut scene = ClientScene::new(SceneConfig {
            center: position(),
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 27,
            authoritative_remote: true,
            ..SceneConfig::forest()
        })
        .unwrap();
        import_chest(&mut scene, 1);
        let mut props = ViewProps::prepare(
            &gpu.device,
            &gpu.queue,
            &terrain,
            &mut scene,
            None,
            None,
            PropsLimits {
                max_pages: 4,
                max_instances: SLOTS,
                max_running: 2,
                max_ready: 2,
                max_age_ticks: 40,
            },
        )
        .unwrap();
        settle(&gpu, &mut props, &mut scene);
        assert_eq!(props.source.stats(scene.world()).models, 1);
        let empty = frame(&gpu, &mut terrain, None);
        let visible = frame(&gpu, &mut terrain, Some(&mut props));
        assert!(
            visible
                .chunks_exact(4)
                .zip(empty.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count()
                >= 16,
            "positive Chest raster oracle missing atAA{samples}"
        );
        let bytes = record(&gpu, &props);
        let flags = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!(flags & render::block_models::HIDDEN, 0);
        let epoch = props.gpu.source_epoch();
        lose_owner_and_journal(&mut scene, position().page());
        let work = props.advance_cpu(&mut scene, Tick(0)).unwrap();
        assert_eq!(work.cursor_recoveries, 1);
        assert_eq!(work.recovery_slots, 2048);
        let source = props.source.stats(scene.world());
        assert!(source.recovering_cursor);
        assert_eq!(source.recovery_slots_remaining, SLOTS - 2048);
        assert_eq!(
            source.models, 1,
            "retirement metadata stays indexed until the actual hidden GPU publication"
        );
        assert!(scene.world().page(position().page()).is_none());
        assert!(
            scene.authoritative_version(position().page()).is_none(),
            "the retained index is not a current Raw owner"
        );
        assert!(!props.stats.ready);
        assert!(!props.gpu.source_valid());
        assert_ne!(props.gpu.source_epoch(), epoch);
        let work = props
            .upload_ready(&scene, &gpu.queue, &mut UploadBudget::new(0), Tick(0))
            .unwrap();
        assert_eq!(work.upload_bytes, 0);
        assert_eq!(
            record(&gpu, &props),
            bytes,
            "quarantine rewrote old GPU bytes with zero upload budget"
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut props)),
            empty,
            "retired non-HIDDEN GPU model leaked during partial recovery AA{samples}"
        );
        settle(&gpu, &mut props, &mut scene);
        assert!(props.gpu.source_valid());
        assert_eq!(props.source.stats(scene.world()).models, 0);
        assert_eq!(frame(&gpu, &mut terrain, Some(&mut props)), empty);
        import_chest(&mut scene, 2);
        settle(&gpu, &mut props, &mut scene);
        assert_eq!(props.source.stats(scene.world()).models, 1);
        assert_ne!(
            frame(&gpu, &mut terrain, Some(&mut props)),
            empty,
            "current replacement did not become visible"
        );
    }
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
}

const EXTENT: [u32; 2] = [96, 64];
// The visible last two slots lie beyond the first2048 recovery credits and
// beyond the first256 ordinary hidden uploads. TAA must honor the master gate.
const TEMPORAL_MODELS: usize = 2050;
fn temporal_view(gpu: &render::Gpu) -> ClientView {
    recovery_view(gpu, true)
}
fn recovery_view(gpu: &render::Gpu, temporal: bool) -> ClientView {
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
        if temporal { 1 } else { 4 },
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
            temporal: temporal.then(render::taa::TaaConfig::default),
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
        crate::props_source::PropsLimits {
            max_pages: 64,
            max_instances: SLOTS,
            max_running: 2,
            max_ready: 2,
            max_age_ticks: 40,
        },
    )
    .unwrap();
    if temporal {
        view.prepare_temporal(&gpu.device, &gpu.queue).unwrap();
        assert!(view.temporal_prepared());
        assert!(view.temporal_allocation_bytes() < 256 * 1024 * 1024);
    }
    let center = view.scene.config().center.page();
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if page == center {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * (z + 16 * 7)] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
            for cell in &mut cells[..TEMPORAL_MODELS] {
                *cell = Cell::from_parts(rules::ids::CHEST, 0);
            }
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
                .is_some_and(|p| p.ready && p.instances == TEMPORAL_MODELS)
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
    let key = crate::model_motion_identity::human(0, net::PlayerId([19; 16])).unwrap();
    view.set_remote_entities_with_identities(&[row], &[key], SpaceId(0), [0; 3])
        .unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    view.active = true;
    view.player.yaw = 0.;
    view.player.pitch = 14.;
    view.player.teleport([1.5, 8., -4.]).unwrap();
    view
}
fn temporal_output(gpu: &render::Gpu) -> wgpu::TextureView {
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
fn temporal_encode(
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
fn temporal_submit(
    gpu: &render::Gpu,
    view: &mut ClientView,
    encoder: wgpu::CommandEncoder,
    frame: u64,
) {
    gpu.queue.submit([encoder.finish()]);
    view.mark_presentation_submitted(frame).unwrap();
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
}

struct TemporalImage {
    current: Vec<[f64; 4]>,
    resolved: Vec<[f64; 4]>,
    ids: Vec<[u32; 4]>,
}
fn half(bits: u16) -> f64 {
    let sign = if bits & 0x8000 == 0 { 1. } else { -1. };
    let exponent = (bits >> 10) & 31;
    let fraction = bits & 1023;
    assert_ne!(exponent, 31, "fixture HDR must remain finite");
    sign * if exponent == 0 {
        f64::from(fraction) * 2_f64.powi(-24)
    } else {
        (1. + f64::from(fraction) / 1024.) * 2_f64.powi(i32::from(exponent) - 15)
    }
}
fn temporal_image(gpu: &render::Gpu, view: &mut ClientView) -> TemporalImage {
    const IMAGE_BYTES: u64 = EXTENT[0] as u64 * EXTENT[1] as u64 * 8;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("recovery actual current/resolved HDR and temporal identity oracle"),
        size: IMAGE_BYTES * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let hdr = view.renderer_mut().resolved_hdr_view().clone();
    let resolved = view
        .environment_mut()
        .unwrap()
        .gpu
        .temporal()
        .unwrap()
        .output_view()
        .clone();
    let ids = view.temporal_motion_views().unwrap().1.clone();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    for (texture, bytes, offset) in [
        (hdr.texture(), 8, 0),
        (resolved.texture(), 8, IMAGE_BYTES),
        (ids.texture(), 16, IMAGE_BYTES * 2),
    ] {
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(EXTENT[0] * bytes),
                    rows_per_image: Some(EXTENT[1]),
                },
            },
            wgpu::Extent3d {
                width: EXTENT[0],
                height: EXTENT[1],
                depth_or_array_layers: 1,
            },
        );
    }
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(gpu, &readback);
    let colors = |start| {
        bytes[start..start + IMAGE_BYTES as usize]
            .chunks_exact(8)
            .map(|pixel| {
                std::array::from_fn(|c| half(u16::from_le_bytes([pixel[2 * c], pixel[2 * c + 1]])))
            })
            .collect()
    };
    TemporalImage {
        current: colors(0),
        resolved: colors(IMAGE_BYTES as usize),
        ids: bytes[IMAGE_BYTES as usize * 2..]
            .chunks_exact(16)
            .map(|pixel| {
                std::array::from_fn(|c| {
                    u32::from_le_bytes(pixel[c * 4..c * 4 + 4].try_into().unwrap())
                })
            })
            .collect(),
    }
}
fn assert_current_reset_color(image: &TemporalImage, jitter: [f32; 2]) {
    let [jx, jy] = jitter.map(f64::from);
    let w = EXTENT[0] as i32;
    let h = EXTENT[1] as i32;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let at = (y * w + x) as usize;
            let sx = f64::from(x) + jx;
            let sy = f64::from(y) + jy;
            let ix = sx.floor() as i32;
            let iy = sy.floor() as i32;
            let fx = sx - f64::from(ix);
            let fy = sy - f64::from(iy);
            let samples = [(ix, iy), (ix + 1, iy), (ix, iy + 1), (ix + 1, iy + 1)]
                .map(|(u, v)| (v.clamp(0, h - 1) * w + u.clamp(0, w - 1)) as usize);
            for c in 0..3 {
                let [a, b, d, e] = samples.map(|p| image.current[p][c]);
                let expected = (a * (1. - fx) + b * fx) * (1. - fy) + (d * (1. - fx) + e * fx) * fy;
                let ulp = if expected.abs() < 2_f64.powi(-14) {
                    2_f64.powi(-24)
                } else {
                    2_f64.powi(expected.abs().log2().floor() as i32 - 10)
                };
                let error = (image.resolved[at][c] - expected).abs();
                assert!(
                    error <= ulp + 2e-7,
                    "revoked history leaked into current HDR ({x},{y}) channel{c}: error{error}, halfULP{ulp}"
                );
            }
        }
    }
}

#[test]
fn late_old_epoch_completion_cannot_reopen_cached_hdr_taa_or_initial_world_and_hud() {
    use crate::presentation_gate::{Event, PresentationGate};
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    for cached_hud in [false, true] {
        let mut view = temporal_view(&gpu);
        let output = temporal_output(&gpu);
        let first = temporal_encode(&gpu, &mut view, &output, 1);
        temporal_submit(&gpu, &mut view, first, 1);
        view.commit_presentation(1).unwrap();
        assert!(view.has_cached_present());
        let epoch = view.props_source_epoch();
        let initial_source_epoch = view
            .environment_mut()
            .unwrap()
            .gpu
            .temporal()
            .unwrap()
            .presented()
            .unwrap()
            .state
            .source_epoch;
        let initial_image = temporal_image(&gpu, &mut view);
        assert!(
            initial_image
                .ids
                .iter()
                .filter(|id| id[0] >> 24 == 13)
                .count()
                >= 16,
            "positive actual Chest identity raster is missing"
        );
        assert!(view.props_sources_valid());
        assert!(view.initial_scene_inputs_ready());
        assert_eq!(
            view.environment_mut()
                .unwrap()
                .gpu
                .temporal()
                .unwrap()
                .presented()
                .unwrap()
                .samples,
            1
        );
        // Hold a real submitted scene callback and, in the second iteration, a
        // real cached-HDR GPU submission used by the prepared initial HUD ticket.
        let old = if cached_hud {
            view.set_presentation_frame_id(2).unwrap();
            view.upload_cached_settings(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
                .unwrap();
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            assert!(view.encode_cached_present(&mut encoder, &output).unwrap());
            encoder
        } else {
            view.invalidate();
            let encoder = temporal_encode(&gpu, &mut view, &output, 2);
            assert_eq!(view.pending_presentation_frame(), Some(2));
            encoder
        };
        temporal_submit(&gpu, &mut view, old, 2);
        let mut world_pending = PresentationGate::default();
        world_pending.submitted_with_source(2, true, false, epoch);
        let mut hud_pending = PresentationGate::default();
        hud_pending.submitted_with_source(1, true, false, epoch);
        assert_eq!(
            hud_pending.completed_with_source(1, epoch),
            Some(Event::PrepareGameUi)
        );
        hud_pending.submitted_with_source(2, false, true, epoch);
        let old_bytes = record_at(
            &gpu,
            view.props.as_deref().unwrap(),
            (TEMPORAL_MODELS - 1) as u64,
        );
        assert_eq!(
            u32::from_le_bytes(old_bytes[20..24].try_into().unwrap())
                & render::block_models::HIDDEN,
            0
        );
        let page = view.scene.config().center.page();
        lose_owner_and_journal(&mut view.scene, page);
        // Run precisely the production Props CPU owner; unrelated Scene/LIGHT
        // cursor consumers are deliberately not advanced by this focused fixture.
        let work = view
            .props
            .as_deref_mut()
            .unwrap()
            .advance_cpu(&mut view.scene, Tick(0))
            .unwrap();
        assert_eq!((work.cursor_recoveries, work.recovery_slots), (1, 2048));
        view.record_props_work(work);
        view.observe_props_source_epoch(epoch);
        assert!(!view.props_sources_valid());
        assert_ne!(view.props_source_epoch(), epoch);
        assert!(
            !view.initial_scene_inputs_ready(),
            "quarantined no-model sources must not qualify first World"
        );
        assert_eq!(
            record_at(
                &gpu,
                view.props.as_deref().unwrap(),
                (TEMPORAL_MODELS - 1) as u64
            ),
            old_bytes,
            "CPU recovery must not rewrite GPU records"
        );
        assert!(!view.has_cached_present());
        let mut cached = gpu.device.create_command_encoder(&Default::default());
        assert!(!view.encode_cached_present(&mut cached, &output).unwrap());
        drop(cached);
        // The actual old GPU work has completed. Its late host callback may release
        // resources, but cannot commit an old HDR/history bank or initial ticket.
        view.commit_presentation(2).unwrap();
        assert_eq!(view.pending_presentation_frame(), None);
        assert!(!view.has_cached_present());
        assert_ne!(
            view.environment_mut()
                .unwrap()
                .gpu
                .temporal()
                .unwrap()
                .presented()
                .unwrap()
                .state
                .frame,
            2
        );
        let current = view.props_source_epoch();
        assert_eq!(world_pending.completed_with_source(2, current), None);
        assert_eq!(hud_pending.completed_with_source(2, current), None);
        assert!(!world_pending.input_admitted() && !hud_pending.input_admitted());
        let mut cached = gpu.device.create_command_encoder(&Default::default());
        assert!(!view.encode_cached_present(&mut cached, &output).unwrap());
        drop(cached);
        // A real quarantined scene can replace the image, while ordinary input
        // readiness remains false. SourceKey must reset the old temporal history.
        let restore_pending = view
            .environment_mut()
            .unwrap()
            .gpu
            .post()
            .temporal_restore_pending();
        let first = temporal_encode(&gpu, &mut view, &output, 3);
        let restore_only = view.pending_presentation_frame().is_none();
        temporal_submit(&gpu, &mut view, first, 3);
        view.commit_presentation(3).unwrap();
        let current_frame = if restore_only {
            assert!(
                restore_pending,
                "only the ordinary rollback restore may precede the fresh scene"
            );
            assert!(
                !view.has_cached_present(),
                "a Loading restore cannot qualify retired HDR"
            );
            assert!(!view.initial_scene_inputs_ready());
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
                1,
                "restore-only Loading must not commit a scene/history token"
            );
            world_pending.submitted_with_source(3, false, false, current);
            assert_eq!(world_pending.completed_with_source(3, current), None);
            let scene = temporal_encode(&gpu, &mut view, &output, 4);
            assert_eq!(view.pending_presentation_frame(), Some(4));
            temporal_submit(&gpu, &mut view, scene, 4);
            view.commit_presentation(4).unwrap();
            4
        } else {
            assert!(
                !restore_pending,
                "a pending rollback restore must be retired before a new scene"
            );
            3
        };
        let presented = view
            .environment_mut()
            .unwrap()
            .gpu
            .temporal()
            .unwrap()
            .presented()
            .unwrap();
        assert_eq!(
            (presented.state.frame, presented.samples),
            (current_frame, 1)
        );
        assert!(
            presented.state.source_epoch > initial_source_epoch,
            "source epoch must change independently of geometry retirement"
        );
        assert_eq!(
            record_at(
                &gpu,
                view.props.as_deref().unwrap(),
                (TEMPORAL_MODELS - 1) as u64
            ),
            old_bytes,
            "visible last GPU model must remain non-HIDDEN beyond the bounded hidden uploads"
        );
        let quarantined_image = temporal_image(&gpu, &mut view);
        assert!(
            quarantined_image.ids.iter().all(|id| id[0] >> 24 != 13),
            "quarantined block family wrote actual temporal IDs"
        );
        assert_current_reset_color(&quarantined_image, presented.state.jitter_pixels);
        assert!(!view.initial_scene_inputs_ready());
        assert!(!view.props_sources_valid());
        world_pending.submitted_with_source(
            current_frame,
            view.initial_scene_inputs_ready(),
            false,
            current,
        );
        assert_eq!(
            world_pending.completed_with_source(current_frame, current),
            None
        );
        assert!(!world_pending.input_admitted());
        // Drain the remaining bounded slot walk and actual hidden publication.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline);
            let before = view.props_source_epoch();
            let work = view
                .props
                .as_deref_mut()
                .unwrap()
                .advance_cpu(&mut view.scene, Tick(0))
                .unwrap();
            view.record_props_work(work);
            let work = view
                .props
                .as_deref_mut()
                .unwrap()
                .upload_ready(
                    &view.scene,
                    &gpu.queue,
                    &mut UploadBudget::new(2 * 1024 * 1024),
                    Tick(0),
                )
                .unwrap();
            view.record_props_work(work);
            view.observe_props_source_epoch(before);
            if view.props_stats().unwrap().ready {
                break;
            }
            std::thread::yield_now();
        }
        assert!(view.props_sources_valid());
        assert_ne!(view.props_source_epoch(), current);
        assert_eq!(
            view.props_source()
                .unwrap()
                .stats(view.scene.world())
                .models,
            0
        );
        let hidden = record_at(
            &gpu,
            view.props.as_deref().unwrap(),
            (TEMPORAL_MODELS - 1) as u64,
        );
        assert_ne!(
            u32::from_le_bytes(hidden[20..24].try_into().unwrap()) & render::block_models::HIDDEN,
            0
        );
        let reopened_frame = current_frame + 1;
        let reopened = temporal_encode(&gpu, &mut view, &output, reopened_frame);
        temporal_submit(&gpu, &mut view, reopened, reopened_frame);
        view.commit_presentation(reopened_frame).unwrap();
        let presented = view
            .environment_mut()
            .unwrap()
            .gpu
            .temporal()
            .unwrap()
            .presented()
            .unwrap();
        assert_eq!(
            (presented.state.frame, presented.samples),
            (reopened_frame, 1)
        );
    }
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
}

fn linear_hdr_bytes(gpu: &render::Gpu, view: &mut ClientView) -> Vec<u8> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("default AA actual retained HDR byte oracle"),
        size: u64::from(EXTENT[0] * EXTENT[1] * 8),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let hdr = view.renderer_mut().resolved_hdr_view().clone();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        hdr.texture().as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(EXTENT[0] * 8),
                rows_per_image: Some(EXTENT[1]),
            },
        },
        wgpu::Extent3d {
            width: EXTENT[0],
            height: EXTENT[1],
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
}
#[test]
fn default_msaa4_abandoned_quarantine_encode_cannot_relabel_actual_old_hdr_as_current_cache() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut view = recovery_view(&gpu, false);
    assert!(!view.temporal_prepared());
    let output = temporal_output(&gpu);
    let first = temporal_encode(&gpu, &mut view, &output, 1);
    temporal_submit(&gpu, &mut view, first, 1);
    view.commit_presentation(1).unwrap();
    assert!(view.has_cached_present());
    // Independently raster the same scene with only this model family gated.
    // Restore its valid family and raster again before the real lost removal.
    let epoch = view.props_source_epoch();
    view.props
        .as_deref_mut()
        .unwrap()
        .gpu
        .set_source_valid(false)
        .unwrap();
    view.observe_props_source_epoch(epoch);
    let hidden = temporal_encode(&gpu, &mut view, &output, 2);
    temporal_submit(&gpu, &mut view, hidden, 2);
    view.commit_presentation(2).unwrap();
    let no_props_reference = linear_hdr_bytes(&gpu, &mut view);
    let epoch = view.props_source_epoch();
    view.props
        .as_deref_mut()
        .unwrap()
        .gpu
        .set_source_valid(true)
        .unwrap();
    view.observe_props_source_epoch(epoch);
    let visible = temporal_encode(&gpu, &mut view, &output, 3);
    temporal_submit(&gpu, &mut view, visible, 3);
    view.commit_presentation(3).unwrap();
    let old_hdr = linear_hdr_bytes(&gpu, &mut view);
    let changed = old_hdr
        .chunks_exact(8)
        .zip(no_props_reference.chunks_exact(8))
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        changed >= 16,
        "default AA abandon negative requires actual visible Chest HDR witnesses"
    );
    let slot = (TEMPORAL_MODELS - 1) as u64;
    let old_record = record_at(&gpu, view.props.as_deref().unwrap(), slot);
    assert_eq!(
        u32::from_le_bytes(old_record[20..24].try_into().unwrap()) & render::block_models::HIDDEN,
        0
    );
    let epoch = view.props_source_epoch();
    let page = view.scene.config().center.page();
    lose_owner_and_journal(&mut view.scene, page);
    let work = view
        .props
        .as_deref_mut()
        .unwrap()
        .advance_cpu(&mut view.scene, Tick(0))
        .unwrap();
    assert_eq!((work.cursor_recoveries, work.recovery_slots), (1, 2048));
    view.record_props_work(work);
    view.observe_props_source_epoch(epoch);
    assert!(!view.props_sources_valid());
    assert!(!view.initial_scene_inputs_ready());
    // Normal CPU uploads can flush256 hidden slots; visible slot2049 remains
    // non-HIDDEN. The fresh no-Props HDR pass is ENCODED, then ABANDONED.
    let abandoned = temporal_encode(&gpu, &mut view, &output, 4);
    drop(abandoned);
    view.discard_presentation(4).unwrap();
    assert_eq!(
        linear_hdr_bytes(&gpu, &mut view),
        old_hdr,
        "unsubmitted encoder must leave actual old positive HDR bytes untouched"
    );
    assert_eq!(
        record_at(&gpu, view.props.as_deref().unwrap(), slot),
        old_record
    );
    let cache_ready = view.has_cached_present();
    let mut cache = gpu.device.create_command_encoder(&Default::default());
    let cache_encoded = view.encode_cached_present(&mut cache, &output).unwrap();
    drop(cache);
    println!(
        "PROPS_MSAA4_ABANDON_RETAINED_OLD_HDR_WITNESS positive_pixels={changed} old_hdr_unchanged=true old_record_nonhidden=true cache_ready={cache_ready} cache_encoded={cache_encoded}"
    );
    assert!(
        !cache_ready && !cache_encoded,
        "PROPS_MSAA4_ABANDONED_HDR_CACHE_REVOKED_REQUIRED cache_ready={cache_ready} cache_encoded={cache_encoded}"
    );
    // After the fixed discard, only a freshly submitted normal HDR pass may
    // publish a cache for the current quarantined epoch.
    let fresh = temporal_encode(&gpu, &mut view, &output, 5);
    temporal_submit(&gpu, &mut view, fresh, 5);
    view.commit_presentation(5).unwrap();
    assert!(view.has_cached_present());
    assert_ne!(linear_hdr_bytes(&gpu, &mut view), old_hdr);
    let mut cache = gpu.device.create_command_encoder(&Default::default());
    assert!(view.encode_cached_present(&mut cache, &output).unwrap());
    drop(cache);
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
}
