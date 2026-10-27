#![cfg(not(target_arch = "wasm32"))]
//! Compare canceled temporal Post against an untouched ordinary Post owner.
use render::{
    OutputTransfer,
    post::{Exposure, PostConfig, PostError, PostFrameToken, PostGpu},
    terrain_gpu::UploadBudget,
};
const SIDE: u32 = 64;
fn texture(gpu: &render::Gpu, format: wgpu::TextureFormat) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("temporal Post reference image"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
fn source(gpu: &render::Gpu, image: &wgpu::Texture, bright: bool) {
    let pixels: Vec<[u16; 4]> = (0..SIDE * SIDE)
        .map(|i| {
            let value = if bright {
                0x4c40
            } else if i % SIDE < SIDE / 2 {
                0x3c00
            } else {
                0x3800
            };
            [value, value, value, 0x3c00]
        })
        .collect();
    gpu.queue.write_texture(
        image.as_image_copy(),
        bytemuck::cast_slice(&pixels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIDE * 8),
            rows_per_image: Some(SIDE),
        },
        image.size(),
    );
}
fn image(gpu: &render::Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("completed canceled Post comparison"),
        size: u64::from(SIDE * SIDE * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIDE * 4),
                rows_per_image: Some(SIDE),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
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
fn token(frame: u64) -> PostFrameToken {
    PostFrameToken {
        frame,
        serial: frame,
    }
}
fn run(manual: bool) {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let hdr = texture(&gpu, wgpu::TextureFormat::Rgba16Float);
    source(&gpu, &hdr, false);
    let hdr_view = hdr.create_view(&Default::default());
    let mut config = PostConfig::default();
    config.bloom.as_mut().unwrap().levels = 6;
    if manual {
        config.exposure = Exposure::Manual(1.);
    }
    let make = || {
        PostGpu::new(
            &gpu.device,
            &gpu.queue,
            &hdr_view,
            SIDE,
            SIDE,
            wgpu::TextureFormat::Rgba8Unorm,
            OutputTransfer::ShaderSrgb,
            config,
            &mut UploadBudget::new(4096),
        )
        .unwrap()
    };
    let mut ordinary = make();
    let mut temporal = make();
    let pipeline_count = ordinary.pipeline_count();
    assert_eq!(ordinary.temporal_rollback_bytes(), 0);
    temporal.prepare_temporal_rollback(&gpu.device).unwrap();
    assert_eq!(temporal.temporal_rollback_bytes(), 16);
    assert_eq!(temporal.pipeline_count(), pipeline_count);
    let target = texture(&gpu, wgpu::TextureFormat::Rgba8Unorm);
    let target_view = target.create_view(&Default::default());
    let reference = texture(&gpu, wgpu::TextureFormat::Rgba8Unorm);
    let reference_view = reference.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    ordinary
        .encode(&mut encoder, &reference_view, 1, &mut 1_000_000)
        .unwrap();
    let first = temporal
        .encode_temporal(&mut encoder, &target_view, 1, &mut 1_000_000, token(1))
        .unwrap();
    assert_eq!(first.state_copy_bytes, 16);
    gpu.queue.submit([encoder.finish()]);
    temporal.mark_temporal_submitted(token(1)).unwrap();
    assert_eq!(image(&gpu, &target), image(&gpu, &reference));
    temporal.commit_temporal_presented(token(1)).unwrap();
    let old = image(&gpu, &target);
    source(&gpu, &hdr, true);
    if manual {
        temporal
            .set_compensation(&gpu.queue, 0.5, &mut UploadBudget::new(64))
            .unwrap();
        ordinary
            .set_compensation(&gpu.queue, 0.5, &mut UploadBudget::new(64))
            .unwrap();
    }
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    temporal
        .encode_temporal(&mut encoder, &target_view, 2, &mut 1_000_000, token(2))
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    temporal.mark_temporal_submitted(token(2)).unwrap();
    let changed = image(&gpu, &target);
    assert_ne!(
        changed, old,
        "actual changed HDR/adaptation must be a positive control"
    );
    temporal.discard_temporal(token(2)).unwrap();
    assert!(temporal.temporal_restore_pending());
    // Stand in for resolver's stable-HDR restoration, retaining the exact old
    // source. Neither ordinary control owner nor its exposure was advanced.
    source(&gpu, &hdr, false);
    let plan = temporal.pending_cached_work();
    assert_eq!(plan.state_copy_bytes, 16);
    assert!(plan.bloom_texels > 0);
    assert_eq!(plan.histogram_samples > 0, !manual);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(matches!(
        temporal.present_cached(&mut encoder, &target_view, &mut (plan.pixels() - 1)),
        Err(PostError::WorkBudget)
    ));
    assert!(temporal.temporal_restore_pending());
    drop(encoder);
    // Abandon a complete restore encoder. Its unwritten commands cannot clear
    // the restore requirement or permit an un-restored tone on the next try.
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    temporal
        .present_cached(&mut encoder, &target_view, &mut 1_000_000)
        .unwrap();
    let abandoned_restore = temporal.temporal_restore_token().unwrap();
    drop(encoder);
    temporal
        .discard_temporal_restore(abandoned_restore)
        .unwrap();
    assert!(temporal.temporal_restore_pending());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let restored = temporal
        .present_cached(&mut encoder, &target_view, &mut 1_000_000)
        .unwrap();
    ordinary
        .present_cached(&mut encoder, &reference_view, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let restore = temporal.temporal_restore_token().unwrap();
    assert_ne!(restore, abandoned_restore);
    assert_eq!(
        temporal.mark_temporal_restore_submitted(abandoned_restore),
        Err(PostError::InvalidPresentation)
    );
    temporal.mark_temporal_restore_submitted(restore).unwrap();
    assert_eq!(
        image(&gpu, &target),
        image(&gpu, &reference),
        "old HDR, exposure and Bloom; hot compensation remains current"
    );
    assert_eq!(restored.state_copy_bytes, 16);
    assert!(!temporal.temporal_restore_pending());
    let cached = temporal.pending_cached_work();
    assert_eq!(
        (
            cached.passes,
            cached.state_copy_bytes,
            cached.histogram_samples,
            cached.bloom_texels
        ),
        (1, 0, 0, 0)
    );
    // Continue slow adaptation with source unchanged. Restored histogram/CPU
    // adaptation metadata must match the untouched control, not the candidate.
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    temporal
        .encode_temporal(&mut encoder, &target_view, 1, &mut 1_000_000, token(3))
        .unwrap();
    ordinary
        .encode(&mut encoder, &reference_view, 1, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    temporal.mark_temporal_submitted(token(3)).unwrap();
    assert_eq!(image(&gpu, &target), image(&gpu, &reference));
    temporal.commit_temporal_presented(token(3)).unwrap();
    // An unsubmitted candidate never wrote its checkpoint. Copying that stale
    // bank would incorrectly roll exposure back an additional adaptation step.
    source(&gpu, &hdr, true);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    temporal
        .encode_temporal(&mut encoder, &target_view, 2, &mut 1_000_000, token(4))
        .unwrap();
    drop(encoder);
    temporal.discard_temporal(token(4)).unwrap();
    assert!(!temporal.temporal_restore_pending());
    source(&gpu, &hdr, false);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    temporal
        .present_cached(&mut encoder, &target_view, &mut 1_000_000)
        .unwrap();
    ordinary
        .present_cached(&mut encoder, &reference_view, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    assert_eq!(image(&gpu, &target), image(&gpu, &reference));
    assert_eq!(temporal.pipeline_count(), pipeline_count);
}
#[test]
fn automatic_exposure_and_maximum_bloom_restore_only_submitted_candidates() {
    run(false);
}
#[test]
fn manual_hot_compensation_survives_submitted_and_unsubmitted_discard() {
    run(true);
}
