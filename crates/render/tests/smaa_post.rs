#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    post::{Exposure, PostConfig, PostError, PostGpu},
    smaa::SmaaConfig,
    terrain_gpu::UploadBudget,
};
fn texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height: width,
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
fn image(gpu: &render::Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let side = texture.width();
    let row = (side * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Post SMAA actual completed output"),
        size: u64::from(row) * u64::from(side),
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
                bytes_per_row: Some(row),
                rows_per_image: Some(side),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let mut pixels = Vec::new();
    for bytes in mapped.chunks(row as usize) {
        pixels.extend_from_slice(&bytes[..side as usize * 4]);
    }
    drop(mapped);
    buffer.unmap();
    pixels
}
fn hdr(gpu: &render::Gpu, side: u32) -> wgpu::Texture {
    let texture = texture(
        &gpu.device,
        "SMAA HDR independent staircase",
        side,
        wgpu::TextureFormat::Rgba16Float,
    );
    let pixels: Vec<[u16; 4]> = (0..side)
        .flat_map(|y| {
            (0..side).map(move |x| {
                let c = if y >= x { 0x3c00 } else { 0 };
                [c, c, c, 0x3c00]
            })
        })
        .collect();
    gpu.queue.write_texture(
        texture.as_image_copy(),
        bytemuck::cast_slice(&pixels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(side * 8),
            rows_per_image: Some(side),
        },
        texture.size(),
    );
    texture
}
#[test]
fn physical_aa_follows_tone_scale_transfer_cache_and_atomic_budget() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let source = hdr(&gpu, 64);
    let source_view = source.create_view(&Default::default());
    let output = texture(
        &gpu.device,
        "physical Post SMAA surface",
        64,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    let output_view = output.create_view(&Default::default());
    for transfer in [OutputTransfer::Attachment, OutputTransfer::ShaderSrgb] {
        let config = PostConfig {
            exposure: Exposure::Manual(1.),
            bloom: None,
            smaa: Some(SmaaConfig::default()),
            ..Default::default()
        };
        let mut post = PostGpu::new(
            &gpu.device,
            &gpu.queue,
            &source_view,
            64,
            64,
            wgpu::TextureFormat::Rgba8Unorm,
            transfer,
            config,
            &mut UploadBudget::new(20904),
        )
        .unwrap();
        assert_eq!(post.pipeline_count(), 4);
        assert!(post.smaa().is_some());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let mut denied = 16383;
        assert!(matches!(
            post.encode(&mut encoder, &output_view, 1, &mut denied),
            Err(PostError::WorkBudget)
        ));
        assert_eq!(denied, 16383);
        assert_eq!(post.frame_graph().count, 0);
        assert!(!post.has_cached_source());
        let mut pixels = 16384;
        let work = post
            .encode(&mut encoder, &output_view, 1, &mut pixels)
            .unwrap();
        assert_eq!(pixels, 0);
        assert_eq!(work.aa_pixels, 12288);
        assert_eq!(work.tone_pixels, 4096);
        assert_eq!(work.passes, 4);
        assert_eq!(post.frame_graph().kinds[..4], [5, 6, 7, 8]);
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        let baseline = image(&gpu, &output);
        let tone = (2.51 + 0.03) / (2.43 + 0.59 + 0.14);
        for x in 20..44 {
            for (y, weight) in [(x, 0.875), (x - 1, 0.125)] {
                let linear: f64 = tone * weight;
                let expected = if transfer == OutputTransfer::ShaderSrgb {
                    if linear <= 0.0031308 {
                        linear * 12.92
                    } else {
                        1.055 * linear.powf(1. / 2.4) - 0.055
                    }
                } else {
                    linear
                };
                let byte = (expected * 255.).round() as i32;
                assert!(
                    (i32::from(baseline[(y * 64 + x) * 4]) - byte).abs() <= 1,
                    "one linear blend and one output transfer"
                );
            }
        }
        for _ in 0..10 {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = post
                .present_cached(&mut encoder, &output_view, &mut 16384)
                .unwrap();
            assert_eq!(work.passes, 4);
            assert_eq!(work.histogram_samples, 0);
            assert_eq!(work.bloom_texels, 0);
            gpu.queue.submit([encoder.finish()]);
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            assert_eq!(image(&gpu, &output), baseline);
        }
        let smaller = hdr(&gpu, 55);
        let smaller_view = smaller.create_view(&Default::default());
        post.resize_input(
            &gpu.device,
            &gpu.queue,
            &smaller_view,
            55,
            55,
            &mut UploadBudget::new(64),
        )
        .unwrap();
        post.set_output_extent_with_device(&gpu.device, 64, 64)
            .unwrap();
        assert_eq!(post.smaa().unwrap().extent(), [64, 64]);
        assert_eq!(post.output_extent(), [64, 64]);
        assert_eq!(post.pipeline_count(), 4);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let work = post
            .encode(&mut encoder, &output_view, 2, &mut 16384)
            .unwrap();
        assert_eq!(work.aa_pixels, 12288);
        assert_eq!(work.tone_pixels, 4096);
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        assert!(image(&gpu, &output).iter().any(|v| *v > 0));
    }
    let plain = PostGpu::new(
        &gpu.device,
        &gpu.queue,
        &source_view,
        64,
        64,
        wgpu::TextureFormat::Rgba8Unorm,
        OutputTransfer::Attachment,
        PostConfig {
            exposure: Exposure::Manual(1.),
            bloom: None,
            ..Default::default()
        },
        &mut UploadBudget::new(80),
    )
    .unwrap();
    assert!(plain.smaa().is_none());
    assert_eq!(plain.pipeline_count(), 1);
    assert_eq!(plain.pending_work(1).aa_pixels, 0);
    assert_eq!(plain.pending_work(1).pixels(), 4096);
}

#[test]
fn maximum_bloom_and_automatic_aa_fit_prepared_timer_and_atomic_work_plan() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let input = hdr(&gpu, 64);
    let output = texture(
        &gpu.device,
        "all Post stages with SMAA",
        64,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    let mut post = PostGpu::new(
        &gpu.device,
        &gpu.queue,
        &input.create_view(&Default::default()),
        64,
        64,
        wgpu::TextureFormat::Rgba8Unorm,
        OutputTransfer::Attachment,
        PostConfig {
            smaa: Some(SmaaConfig::default()),
            bloom: Some(render::post::BloomConfig {
                threshold: 1.,
                knee: 0.5,
                intensity: 0.06,
                levels: 6,
            }),
            ..Default::default()
        },
        &mut UploadBudget::new(100_000),
    )
    .unwrap();
    let work = post.pending_work(1);
    assert_eq!(work.passes, 13);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let actual = post
        .encode(
            &mut encoder,
            &output.create_view(&Default::default()),
            1,
            &mut work.pixels(),
        )
        .unwrap();
    assert_eq!(actual.passes, 13);
    assert_eq!(post.frame_graph().count, 13);
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    assert!(
        image(&gpu, &output)
            .chunks_exact(4)
            .all(|pixel| pixel[3] == 255)
    );
}
