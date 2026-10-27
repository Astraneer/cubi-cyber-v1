//! Same host revision must not identify two different actual color uploads.
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
    material_pack::{MaterialPackGpu, PreparedMaterials, apply_albedo_in_place},
    post::{Exposure, PostConfig},
    taa::PresentedFrame,
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};
const EXTENT: [u32; 2] = [96, 64];
const PIXELS: usize = EXTENT[0] as usize * EXTENT[1] as usize;
const IMAGE_BYTES: u64 = PIXELS as u64 * 8;
const HOST_REVISION: u64 = 2;
const SURFACE_ROW_BYTES: u32 = (EXTENT[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
    * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
const SURFACE_BYTES: u64 = SURFACE_ROW_BYTES as u64 * EXTENT[1] as u64;

fn materials(second: bool) -> PreparedMaterials {
    let mut atlas = AtlasImage::donor().unwrap();
    // Both palettes contain the same red/blue range. A neighborhood clamp alone
    // cannot establish that history belongs to the newly published material.
    for mip in &mut atlas.mips {
        for (i, texel) in mip.rgba.chunks_exact_mut(4).enumerate() {
            let x = i % mip.size as usize;
            let y = i / mip.size as usize % mip.size as usize;
            texel.copy_from_slice(&if (x / 2 + y / 2).is_multiple_of(2) ^ second {
                [240, 24, 32, 255]
            } else {
                [24, 32, 240, 255]
            });
        }
    }
    PreparedMaterials::prepare(&atlas, &[], HOST_REVISION, Default::default()).unwrap()
}
fn setup(gpu: &render::Gpu) -> ClientView {
    setup_with_exposure(gpu, 0.)
}
fn setup_with_exposure(gpu: &render::Gpu, exposure: f32) -> ClientView {
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new_with_samples(
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
        renderer,
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
                    exposure: Exposure::Manual(exposure),
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
    let initial = PreparedMaterials::prepare(&atlas, &[], 0, Default::default()).unwrap();
    view.renderer_mut()
        .prepare_material_pack(
            &gpu.device,
            &gpu.queue,
            MaterialPackGpu::upload(&gpu.device, &gpu.queue, &initial).unwrap(),
            false,
        )
        .unwrap();
    view.prepare_temporal(&gpu.device, &gpu.queue).unwrap();
    let center = view.scene.config().center.page();
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if page == center {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * (z + 16 * 7)] = Cell::from_parts(rules::ids::STONE, 0);
                }
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
    while view.stats().resident_pages != 1 {
        assert!(Instant::now() < deadline, "bootstrap {:?}", view.stats());
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        std::thread::yield_now();
    }
    view
}
fn publish(gpu: &render::Gpu, view: &mut ClientView, prepared: &PreparedMaterials) -> u64 {
    let pack = MaterialPackGpu::upload(&gpu.device, &gpu.queue, prepared).unwrap();
    apply_albedo_in_place(&gpu.queue, view.renderer_mut().geometry(), prepared).unwrap();
    view.renderer_mut()
        .replace_material_pack(&gpu.device, &gpu.queue, pack, true)
        .unwrap();
    view.replace_material_albedo(prepared.atlas()).unwrap();
    view.invalidate();
    assert_eq!(
        view.renderer_mut().material_pack_revision(),
        Some(HOST_REVISION)
    );
    view.renderer_mut().geometry().shadow_alpha_state.revision()
}
struct Image {
    current: Vec<[f64; 4]>,
    resolved: Vec<[f64; 4]>,
    ids: Vec<[u32; 4]>,
    surface: Vec<u8>,
    presented: PresentedFrame,
    history: bool,
}
fn half(bits: u16) -> f64 {
    let sign = if bits & 0x8000 == 0 { 1. } else { -1. };
    let exponent = (bits >> 10) & 31;
    let fraction = bits & 1023;
    assert_ne!(exponent, 31, "all fixture HDR channels must remain finite");
    sign * if exponent == 0 {
        f64::from(fraction) * 2_f64.powi(-24)
    } else {
        (1. + f64::from(fraction) / 1024.) * 2_f64.powi(i32::from(exponent) - 15)
    }
}
fn frame(gpu: &render::Gpu, view: &mut ClientView, id: u64) -> Image {
    let output_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("material source actual View output"),
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
    });
    let output = output_texture.create_view(&Default::default());
    view.set_presentation_frame_id(id).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, &output, EXTENT[0], EXTENT[1])
        .unwrap();
    let history = view
        .environment_stats()
        .unwrap()
        .gpu
        .temporal
        .history_available;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material current HDR, resolved HDR and exact geometry IDs"),
        size: IMAGE_BYTES * 4 + SURFACE_BYTES,
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
    encoder.copy_texture_to_buffer(
        output_texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: IMAGE_BYTES * 4,
                bytes_per_row: Some(SURFACE_ROW_BYTES),
                rows_per_image: Some(EXTENT[1]),
            },
        },
        output_texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    view.mark_presentation_submitted(id).unwrap();
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    view.commit_presentation(id).unwrap();
    let presented = view
        .environment_mut()
        .unwrap()
        .gpu
        .temporal()
        .unwrap()
        .presented()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = readback.slice(..).get_mapped_range();
    let colors = |start| {
        bytes[start..start + IMAGE_BYTES as usize]
            .chunks_exact(8)
            .map(|pixel| {
                std::array::from_fn(|c| half(u16::from_le_bytes([pixel[2 * c], pixel[2 * c + 1]])))
            })
            .collect()
    };
    let current = colors(0);
    let resolved = colors(IMAGE_BYTES as usize);
    let ids = bytes[IMAGE_BYTES as usize * 2..IMAGE_BYTES as usize * 4]
        .chunks_exact(16)
        .map(|pixel| {
            std::array::from_fn(|c| u32::from_le_bytes(pixel[c * 4..c * 4 + 4].try_into().unwrap()))
        })
        .collect();
    let surface = bytes[IMAGE_BYTES as usize * 4..]
        .chunks_exact(SURFACE_ROW_BYTES as usize)
        .flat_map(|row| row[..EXTENT[0] as usize * 4].iter().copied())
        .collect();
    drop(bytes);
    readback.unmap();
    Image {
        current,
        resolved,
        ids,
        surface,
        presented,
        history,
    }
}
fn assert_current_color(image: &Image) -> usize {
    let w = EXTENT[0] as i32;
    let h = EXTENT[1] as i32;
    let [jx, jy] = image.presented.state.jitter_pixels.map(f64::from);
    let mut witnesses = 0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let at = (y * w + x) as usize;
            if image.ids[at][0] >> 24 != 1 {
                continue;
            }
            let sx = f64::from(x) + jx;
            let sy = f64::from(y) + jy;
            let ix = sx.floor() as i32;
            let iy = sy.floor() as i32;
            let fx = sx - f64::from(ix);
            let fy = sy - f64::from(iy);
            let samples = [(ix, iy), (ix + 1, iy), (ix, iy + 1), (ix + 1, iy + 1)]
                .map(|(u, v)| (v.clamp(0, h - 1) * w + u.clamp(0, w - 1)) as usize);
            if samples.iter().any(|&p| image.ids[p][0] >> 24 != 1) {
                continue;
            }
            witnesses += 1;
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
                    "current material color at ({x},{y}) channel={c}: error={error} halfULP={ulp}"
                );
            }
        }
    }
    assert!(
        witnesses > 100,
        "current-color proof needs visible terrain, got {witnesses}"
    );
    witnesses
}
#[test]
fn actual_material_upload_rejects_same_revision_history_without_resetting_steady_frames() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut view = setup(&gpu);
    let world = view.scene.world().canonical_hash();
    let resident = view.stats().resident_quads;
    let first_epoch = publish(&gpu, &mut view, &materials(false));
    assert_eq!(first_epoch, 1);
    let a = frame(&gpu, &mut view, 1);
    assert!(!a.history);
    let warm = frame(&gpu, &mut view, 2);
    assert!(warm.history);
    assert_eq!(
        warm.presented.state.source_epoch,
        a.presented.state.source_epoch
    );
    assert!(warm.presented.samples > 1);
    let next_epoch = publish(&gpu, &mut view, &materials(true));
    assert_eq!(next_epoch, first_epoch + 1);
    let b = frame(&gpu, &mut view, 3);
    // This is the causal regression: both hosts report revision2, but their real
    // queued color arrays differ. A new scene_revision alone permits history.
    assert!(
        !b.history,
        "same host revision must not reuse an older color bank"
    );
    assert!(b.presented.state.source_epoch > warm.presented.state.source_epoch);
    assert_eq!(
        b.presented.state.geometry_epoch,
        warm.presented.state.geometry_epoch
    );
    assert_eq!(b.presented.samples, 1);
    assert_eq!(view.scene.world().canonical_hash(), world);
    assert_eq!(view.stats().resident_quads, resident);
    assert!(
        a.current
            .iter()
            .zip(&b.current)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.1 && (a[2] - b[2]).abs() > 0.1),
        "two actual uploads must produce distinct HDR colors"
    );
    let witnesses = assert_current_color(&b);
    view.renderer_mut().invalidate();
    view.invalidate();
    let unchanged = frame(&gpu, &mut view, 4);
    assert!(
        unchanged.history,
        "ordinary redraw must retain compatible presented history"
    );
    assert_eq!(
        unchanged.presented.state.source_epoch,
        b.presented.state.source_epoch
    );
    assert_eq!(
        unchanged.presented.state.geometry_epoch,
        b.presented.state.geometry_epoch
    );
    assert_eq!(
        view.renderer_mut().geometry().shadow_alpha_state.revision(),
        next_epoch
    );
    // Explicit invalidation changes the refinement revision, so its sample
    // counter restarts even while the previous compatible color is reused.
    assert!(unchanged.presented.state.scene_revision > b.presented.state.scene_revision);
    assert_eq!(unchanged.presented.samples, 1);
    let steady = frame(&gpu, &mut view, 5);
    assert!(steady.history);
    assert_eq!(
        steady.presented.state,
        render::taa::TaaFrame {
            frame: 5,
            jitter_pixels: steady.presented.state.jitter_pixels,
            ..unchanged.presented.state
        }
    );
    assert!(steady.presented.samples > 1);
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
    eprintln!(
        "VIEW_TAA_MATERIAL_UPLOAD_OK host_revision=2 first_epoch={first_epoch} next_epoch={next_epoch} current_color_witnesses={witnesses} unchanged_geometry=true steady_history=true"
    );
}

fn cached_surface(gpu: &render::Gpu, view: &mut ClientView, id: u64) -> Vec<u8> {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cached View actual final RGBA8 output"),
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
    });
    let output = texture.create_view(&Default::default());
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cached final RGBA8 readback with padded rows"),
        size: SURFACE_BYTES,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let before = view.stats();
    let presented = view
        .environment_mut()
        .unwrap()
        .gpu
        .temporal()
        .unwrap()
        .presented()
        .unwrap();
    let output_revision = view.renderer_mut().hdr_revision();
    view.set_presentation_frame_id(id).unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(view.encode_cached_present(&mut encoder, &output).unwrap());
    assert!(
        view.pending_presentation_frame().is_none(),
        "cached tone must not encode a scene candidate"
    );
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SURFACE_ROW_BYTES),
                rows_per_image: Some(EXTENT[1]),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    view.mark_presentation_submitted(id).unwrap();
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    view.commit_presentation(id).unwrap();
    assert_eq!(
        view.renderer_mut().hdr_revision(),
        output_revision,
        "cached output must not redraw the current scene"
    );
    assert_eq!(view.stats().terrain_frames, before.terrain_frames);
    assert_eq!(view.stats().props_frames, before.props_frames);
    assert_eq!(view.stats().upload_bytes_total, before.upload_bytes_total);
    assert_eq!(
        view.environment_mut()
            .unwrap()
            .gpu
            .temporal()
            .unwrap()
            .presented(),
        Some(presented),
        "cached output must preserve the exact presented history, jitter and source epochs"
    );
    let (tx, rx) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = readback.slice(..).get_mapped_range();
    let surface = bytes
        .chunks_exact(SURFACE_ROW_BYTES as usize)
        .flat_map(|row| row[..EXTENT[0] as usize * 4].iter().copied())
        .collect();
    drop(bytes);
    readback.unmap();
    surface
}

#[test]
fn cached_final_rgba8_uses_last_presented_material_after_distinct_history_pings() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    // Manual is a linear multiplier; zero would make A and B final RGB equal.
    let mut view = setup_with_exposure(&gpu, 1.);
    publish(&gpu, &mut view, &materials(false));
    let a = frame(&gpu, &mut view, 1);
    let warm_a = frame(&gpu, &mut view, 2);
    assert_ne!(a.presented.history_index, warm_a.presented.history_index);
    publish(&gpu, &mut view, &materials(true));
    let b = frame(&gpu, &mut view, 3);
    assert!(
        !b.history,
        "the distinct B upload must start current-only color"
    );
    assert_ne!(b.presented.history_index, warm_a.presented.history_index);
    let old_a_witnesses = b
        .surface
        .chunks_exact(4)
        .zip(warm_a.surface.chunks_exact(4))
        .filter(|(b, a)| b[..3] != a[..3])
        .count();
    let cleared_witnesses = b
        .surface
        .chunks_exact(4)
        .filter(|pixel| pixel[..3] != [0, 0, 0])
        .count();
    assert!(
        old_a_witnesses > 100,
        "an accidentally bound old A ping must differ at visible pixels: {old_a_witnesses}"
    );
    assert!(
        cleared_witnesses > 100,
        "an uninitialized/cleared ping must differ at visible pixels: {cleared_witnesses}"
    );
    let cached = cached_surface(&gpu, &mut view, 4);
    assert_eq!(cached.len(), PIXELS * 4);
    let mismatches = cached
        .chunks_exact(4)
        .zip(b.surface.chunks_exact(4))
        .filter(|(cached, normal)| cached != normal)
        .count();
    assert_eq!(
        mismatches, 0,
        "UI-only cached final RGBA8 must be bitwise equal to the actual completed B normal output"
    );
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
    eprintln!(
        "VIEW_TAA_CACHED_FINAL_RGBA8_OK pixels={PIXELS} old_a_negative_witnesses={old_a_witnesses} cleared_negative_witnesses={cleared_witnesses} normal_b_mismatches={mismatches} cached_scene_encoded=false"
    );
}
