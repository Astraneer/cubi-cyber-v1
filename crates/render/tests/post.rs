#![cfg(not(target_arch = "wasm32"))]
use render::OutputTransfer;
use render::post::{self, BloomConfig, Exposure, PostConfig, PostError, PostGpu};
use render::terrain_gpu::UploadBudget;

fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    data
}
fn input(gpu: &render::Gpu, width: u32, height: u32, data: &[[f32; 4]]) -> wgpu::Texture {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("independent linear HDR reference input"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(data),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 16),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}
fn output(gpu: &render::Gpu, width: u32, height: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("HDR post output"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
fn frame(
    gpu: &render::Gpu,
    post: &mut PostGpu,
    output: &wgpu::Texture,
    revision: u64,
) -> (Vec<u8>, post::PostWork) {
    frame_mode(gpu, post, output, revision, false)
}
fn frame_mode(
    gpu: &render::Gpu,
    post: &mut PostGpu,
    output: &wgpu::Texture,
    revision: u64,
    cached: bool,
) -> (Vec<u8>, post::PostWork) {
    let width = output.width();
    let height = output.height();
    let row = (width * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual post pixel readback"),
        size: u64::from(row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let view = output.create_view(&Default::default());
    let work = if cached {
        post.present_cached(&mut encoder, &view, &mut 1_000_000)
    } else {
        post.encode(&mut encoder, &view, revision, &mut 1_000_000)
    }
    .unwrap();
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    (
        read(gpu, &buffer)
            .chunks_exact(row as usize)
            .flat_map(|line| line[..width as usize * 4].iter().copied())
            .collect(),
        work,
    )
}
fn exposure(gpu: &render::Gpu, post: &PostGpu) -> [f32; 4] {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual persistent GPU exposure"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(post.state_buffer(), 0, &buffer, 0, 16);
    gpu.queue.submit([encoder.finish()]);
    let bytes = read(gpu, &buffer);
    std::array::from_fn(|index| {
        f32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
    })
}

#[test]
fn manual_post_matches_numeric_tone_and_reuses_pipelines_on_resize() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let colors = (0..64 * 48)
            .map(|index| [f32::from((index % 64) as u8) / 16.0, 0.18, 2.0, 1.0])
            .collect::<Vec<_>>();
        let hdr = input(&gpu, 64, 48, &colors);
        let view = hdr.create_view(&Default::default());
        let config = PostConfig {
            exposure: Exposure::Manual(0.5),
            bloom: None,
            histogram_stride: 2,
        };
        let mut denied = UploadBudget::new(0);
        assert!(matches!(
            PostGpu::new(
                &gpu.device,
                &gpu.queue,
                &view,
                64,
                48,
                wgpu::TextureFormat::Rgba8Unorm,
                OutputTransfer::ShaderSrgb,
                config,
                &mut denied
            ),
            Err(PostError::UploadBudget)
        ));
        let mut budget = UploadBudget::new(4096);
        let mut post = PostGpu::new(
            &gpu.device,
            &gpu.queue,
            &view,
            64,
            48,
            wgpu::TextureFormat::Rgba8Unorm,
            OutputTransfer::ShaderSrgb,
            config,
            &mut budget,
        )
        .unwrap();
        assert_eq!(post.pipeline_count(), 1);
        assert!(post.needs_redraw(1));
        let out = output(&gpu, 64, 48);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert!(matches!(
            post.encode(
                &mut encoder,
                &out.create_view(&Default::default()),
                1,
                &mut 0
            ),
            Err(PostError::WorkBudget)
        ));
        assert!(post.needs_redraw(1));
        let (actual, work) = frame(&gpu, &mut post, &out, 1);
        assert_eq!(work.passes, 1);
        assert_eq!(work.histogram_samples, 0);
        assert_eq!(work.bloom_texels, 0);
        assert_eq!(work.draws, 1);
        assert!(!post.needs_redraw(1));
        let mut max_error = 0u8;
        for (pixel, color) in actual.chunks_exact(4).zip(&colors) {
            for channel in 0..3 {
                let x = f64::from(color[channel]) * 0.5;
                let mapped =
                    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0);
                let srgb = if mapped <= 0.0031308 {
                    mapped * 12.92
                } else {
                    1.055 * mapped.powf(1.0 / 2.4) - 0.055
                };
                max_error = max_error.max(pixel[channel].abs_diff((srgb * 255.0).round() as u8));
            }
        }
        assert!(
            max_error <= 1,
            "tone or output transfer differs from f64 oracle: {max_error}"
        );
        for _ in 0..8 {
            assert_eq!(
                frame(&gpu, &mut post, &out, 1).0,
                actual,
                "static post flickered"
            );
        }
        assert_eq!(post.set_delta(&gpu.queue, 0.01, &mut budget).unwrap(), 0);
        assert_eq!(exposure(&gpu, &post)[0], 0.5);
        let prior_state = exposure(&gpu, &post);
        assert_eq!(
            post.set_compensation(&gpu.queue, 2., &mut UploadBudget::new(63)),
            Err(PostError::UploadBudget)
        );
        assert_eq!(post.compensation(), 1.);
        assert!(!post.needs_redraw(1));
        assert_eq!(
            post.set_compensation(&gpu.queue, f32::NAN, &mut budget),
            Err(PostError::InvalidConfig)
        );
        assert_eq!(
            post.set_compensation(&gpu.queue, 2., &mut UploadBudget::new(64))
                .unwrap(),
            64
        );
        let (brighter, changed) = frame(&gpu, &mut post, &out, 1);
        assert_ne!(brighter, actual);
        assert_eq!(changed.passes, 1);
        assert_eq!(changed.histogram_samples + changed.bloom_texels, 0);
        assert_eq!(exposure(&gpu, &post), prior_state);
        assert_eq!(post.pipeline_count(), 1);
        let mut error = 0u8;
        for (pixel, color) in brighter.chunks_exact(4).zip(&colors) {
            for channel in 0..3 {
                let x = f64::from(color[channel]);
                let mapped =
                    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0., 1.);
                let srgb = if mapped <= 0.0031308 {
                    mapped * 12.92
                } else {
                    1.055 * mapped.powf(1. / 2.4) - 0.055
                };
                error = error.max(pixel[channel].abs_diff((srgb * 255.).round() as u8));
            }
        }
        assert!(
            error <= 1,
            "exposure compensation differs from independent numeric tone: {error}"
        );
        post.set_compensation(&gpu.queue, 1., &mut budget).unwrap();
        assert_eq!(frame(&gpu, &mut post, &out, 1).0, actual);
        let smaller = input(&gpu, 33, 17, &vec![[0.18, 0.18, 0.18, 1.0]; 33 * 17]);
        post.resize_input(
            &gpu.device,
            &gpu.queue,
            &smaller.create_view(&Default::default()),
            33,
            17,
            &mut budget,
        )
        .unwrap();
        assert_eq!(post.pipeline_count(), 1);
        assert_eq!(
            frame(&gpu, &mut post, &output(&gpu, 33, 17), 2)
                .1
                .tone_pixels,
            33 * 17
        );
        post.invalidate();
        assert!(post.needs_redraw(2));
        let _default = PostConfig::default();
        println!(
            "POST_MANUAL_GPU_OK pixels=3072 max_f64_error={max_error} static_flicker=0 pipelines=1 resize_pipeline_compilations=0"
        );
    });
}

#[test]
fn histogram_exposure_adapts_slowly_and_reuses_static_measurement() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let hdr = input(&gpu, 64, 48, &vec![[1.0; 4]; 64 * 48]);
        let config = PostConfig {
            exposure: Exposure::Automatic {
                key: 0.18,
                min_ev: -4.0,
                max_ev: 4.0,
                brighten_seconds: 0.5,
                darken_seconds: 0.25,
            },
            bloom: None,
            histogram_stride: 2,
        };
        let mut budget = UploadBudget::new(4096);
        let mut post = PostGpu::new(
            &gpu.device,
            &gpu.queue,
            &hdr.create_view(&Default::default()),
            64,
            48,
            wgpu::TextureFormat::Rgba8Unorm,
            OutputTransfer::ShaderSrgb,
            config,
            &mut budget,
        )
        .unwrap();
        assert_eq!(post.pipeline_count(), 4);
        post.set_delta(&gpu.queue, 0.1, &mut budget).unwrap();
        let out = output(&gpu, 64, 48);
        let mut previous = 1.0f32;
        let mut measured = 0;
        for index in 0..42 {
            let (_, work) = frame(&gpu, &mut post, &out, 1);
            measured += work.histogram_samples;
            assert_eq!(work.histogram_samples, if index == 0 { 768 } else { 0 });
            let state = exposure(&gpu, &post);
            assert!((state[1] - 0.18).abs() < 0.01);
            assert!(
                state[0] <= previous + 1e-6 && state[0] >= state[1] - 1e-6,
                "exposure oscillated or overshot"
            );
            if post.needs_redraw(1) {
                let reference = previous + (state[1] - previous) * (1.0 - (-0.1f32 / 0.25).exp());
                assert!((state[0] - reference).abs() < 1e-5);
            }
            previous = state[0];
        }
        assert!(!post.needs_redraw(1));
        let state = exposure(&gpu, &post);
        assert!((state[0] - state[1]).abs() < 0.001);
        assert_eq!(measured, 768);
        let before = frame(&gpu, &mut post, &out, 1).0;
        let adapted = exposure(&gpu, &post);
        post.set_compensation(&gpu.queue, 0.5, &mut budget).unwrap();
        let (dimmed, work) = frame(&gpu, &mut post, &out, 1);
        assert_ne!(before, dimmed);
        assert_eq!(work.passes, 1, "cached exposure change is tone-only");
        assert_eq!(work.histogram_samples + work.bloom_texels, 0);
        assert_eq!(
            exposure(&gpu, &post),
            adapted,
            "display compensation cannot reset adaptation history"
        );
        assert_eq!(post.pipeline_count(), 4);
        post.set_compensation(&gpu.queue, 1., &mut budget).unwrap();
        let dark = input(&gpu, 64, 48, &vec![[0.01, 0.01, 0.01, 1.0]; 64 * 48]);
        post.resize_input(
            &gpu.device,
            &gpu.queue,
            &dark.create_view(&Default::default()),
            64,
            48,
            &mut budget,
        )
        .unwrap();
        let (_, work) = frame(&gpu, &mut post, &out, 2);
        assert_eq!(work.histogram_samples, 768);
        let next = exposure(&gpu, &post);
        assert_eq!(next[1], 16.0);
        assert!(next[0] > state[0] && next[0] < next[1]);
        println!(
            "POST_EXPOSURE_GPU_OK sampled_luminances={measured} cached_measurement_frames=41 target={} settled={} brightening={} histogram_bins=256",
            state[1], state[0], next[0]
        );
    });
}

#[test]
fn bloom_spreads_hdr_highlights_and_is_bitwise_stable() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let mut colors = vec![[0.0, 0.0, 0.0, 1.0]; 64 * 48];
        for y in 22..=24 {
            for x in 30..=32 {
                colors[x + 64 * y] = [16.0, 8.0, 2.0, 1.0];
            }
        }
        let hdr = input(&gpu, 64, 48, &colors);
        let view = hdr.create_view(&Default::default());
        let mut budget = UploadBudget::new(4096);
        let config = PostConfig {
            exposure: Exposure::Manual(1.0),
            bloom: Some(BloomConfig {
                threshold: 1.0,
                knee: 0.5,
                intensity: 0.1,
                levels: 4,
            }),
            histogram_stride: 4,
        };
        let mut bloom = PostGpu::new(
            &gpu.device,
            &gpu.queue,
            &view,
            64,
            48,
            wgpu::TextureFormat::Rgba8Unorm,
            OutputTransfer::ShaderSrgb,
            config,
            &mut budget,
        )
        .unwrap();
        let mut plain = PostGpu::new(
            &gpu.device,
            &gpu.queue,
            &view,
            64,
            48,
            wgpu::TextureFormat::Rgba8Unorm,
            OutputTransfer::ShaderSrgb,
            PostConfig {
                bloom: None,
                ..config
            },
            &mut budget,
        )
        .unwrap();
        assert_eq!(bloom.pipeline_count(), 2);
        let out = output(&gpu, 64, 48);
        let (actual, work) = frame(&gpu, &mut bloom, &out, 1);
        let reference = frame(&gpu, &mut plain, &out, 1).0;
        let halo = actual
            .chunks_exact(4)
            .zip(reference.chunks_exact(4))
            .filter(|(a, b)| b[..3] == [0, 0, 0] && a[0] > 0)
            .count();
        assert!(halo > 20, "HDR highlight did not produce bloom: {halo}");
        assert_eq!(work.bloom_texels, 32 * 24 + 16 * 12 + 8 * 6 + 4 * 3);
        assert_eq!(work.passes, 5);
        for _ in 0..8 {
            let (frame, work) = frame(&gpu, &mut bloom, &out, 1);
            assert_eq!(frame, actual);
            assert_eq!(work.bloom_texels, 0);
            assert_eq!(work.passes, 1);
        }
        println!(
            "POST_BLOOM_GPU_OK halo_pixels={halo} pyramid_texels={} static_bloom_work=0 static_flicker=0",
            work.bloom_texels
        );
    });
}

#[test]
fn cached_hdr_present_keeps_histogram_bloom_and_adaptation_history_while_only_toning_fresh_surfaces()
 {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let hdr = input(&gpu, 64, 32, &vec![[4., 1., 0.25, 1.]; 64 * 32]);
    let mut post = PostGpu::new(
        &gpu.device,
        &gpu.queue,
        &hdr.create_view(&Default::default()),
        64,
        32,
        wgpu::TextureFormat::Rgba8Unorm,
        OutputTransfer::ShaderSrgb,
        PostConfig {
            bloom: Some(BloomConfig {
                threshold: 1.,
                knee: 0.5,
                intensity: 0.2,
                levels: 2,
            }),
            ..Default::default()
        },
        &mut UploadBudget::new(10000),
    )
    .unwrap();
    let target = output(&gpu, 64, 32);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(matches!(
        post.present_cached(
            &mut encoder,
            &target.create_view(&Default::default()),
            &mut 1_000_000
        ),
        Err(PostError::InvalidConfig)
    ));
    let (original, work) = frame(&gpu, &mut post, &target, 7);
    assert!(work.histogram_samples > 0 && work.bloom_texels > 0 && post.has_cached_source());
    let state = exposure(&gpu, &post);
    let pending = post.pending_work(7);
    assert!(pending.passes > 1, "automatic adaptation remains active");
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(matches!(
        post.present_cached(
            &mut encoder,
            &target.create_view(&Default::default()),
            &mut 0
        ),
        Err(PostError::WorkBudget)
    ));
    for _ in 0..20 {
        let (pixels, work) = frame_mode(&gpu, &mut post, &target, 7, true);
        assert_eq!(pixels, original);
        assert_eq!(work.passes, 1);
        assert_eq!(work.draws, 1);
        assert_eq!(work.histogram_samples, 0);
        assert_eq!(work.bloom_texels, 0);
        assert_eq!(post.frame_graph().count, 1);
    }
    assert_eq!(
        exposure(&gpu, &post),
        state,
        "cached presentation must not advance adaptation"
    );
    assert_eq!(post.pending_work(7).passes, pending.passes);
    let (_, resumed) = frame(&gpu, &mut post, &target, 7);
    assert_eq!(resumed.histogram_samples, 0);
    assert_eq!(resumed.bloom_texels, 0);
    assert_ne!(
        exposure(&gpu, &post),
        state,
        "ordinary encode resumes the retained adaptation"
    );
    post.invalidate();
    assert!(!post.has_cached_source());
    eprintln!(
        "M10_CACHED_HDR_HOLD_OK frames20 BITWISE=true tone_passes1 histogram0 bloom0 exposure_history_retained=true budget_denial_atomic=true"
    );
}

#[test]
fn scaled_hdr_bilinear_matches_independent_f64_and_charges_the_native_output() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let (width, height) = (55u32, 38u32);
    let mut colors = Vec::new();
    for y in 0..height {
        for x in 0..width {
            colors.push([
                (x % 7) as f32 / 4.,
                (y % 5) as f32 / 3.,
                ((x + y) % 9) as f32 / 5.,
                1.,
            ]);
        }
    }
    let hdr = input(&gpu, width, height, &colors);
    let mut post = PostGpu::new(
        &gpu.device,
        &gpu.queue,
        &hdr.create_view(&Default::default()),
        width,
        height,
        wgpu::TextureFormat::Rgba8Unorm,
        OutputTransfer::Attachment,
        PostConfig {
            exposure: Exposure::Manual(1.),
            bloom: None,
            histogram_stride: 4,
        },
        &mut UploadBudget::new(4096),
    )
    .unwrap();
    assert_eq!(post.pipeline_count(), 1);
    assert_eq!(post.set_output_extent(0, 44), Err(PostError::InvalidConfig));
    assert_eq!(
        post.set_output_extent(u32::MAX, 44),
        Err(PostError::InvalidConfig)
    );
    assert_eq!(post.output_extent(), [width, height]);
    post.set_output_extent(64, 44).unwrap();
    assert_eq!(post.pending_work(1).tone_pixels, 64 * 44);
    let out = output(&gpu, 64, 44);
    let mut denied = gpu.device.create_command_encoder(&Default::default());
    let mut allowance = 64 * 44 - 1;
    assert_eq!(
        post.encode(
            &mut denied,
            &out.create_view(&Default::default()),
            1,
            &mut allowance
        )
        .unwrap_err(),
        PostError::WorkBudget
    );
    assert_eq!(allowance, 64 * 44 - 1);
    assert!(post.needs_redraw(1));
    let (actual, work) = frame(&gpu, &mut post, &out, 1);
    assert_eq!((work.passes, work.draws, work.tone_pixels), (1, 1, 64 * 44));
    assert_eq!((work.histogram_samples, work.bloom_texels), (0, 0));
    let mut maximum = 0u8;
    for y in 0..44 {
        for x in 0..64 {
            let coordinate = [
                (f64::from(x) + 0.5) * f64::from(width) / 64. - 0.5,
                (f64::from(y) + 0.5) * f64::from(height) / 44. - 0.5,
            ];
            let base = coordinate.map(|p| p.floor() as i32);
            let weights = coordinate.map(|p| p - p.floor());
            for c in 0..3 {
                let mut linear = 0.;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let sx = (base[0] + dx).clamp(0, width as i32 - 1) as usize;
                        let sy = (base[1] + dy).clamp(0, height as i32 - 1) as usize;
                        let weight = if dx == 0 { 1. - weights[0] } else { weights[0] }
                            * if dy == 0 { 1. - weights[1] } else { weights[1] };
                        linear += f64::from(colors[sx + sy * width as usize][c]) * weight;
                    }
                }
                let mapped = ((linear * (2.51 * linear + 0.03))
                    / (linear * (2.43 * linear + 0.59) + 0.14))
                    .clamp(0., 1.);
                maximum = maximum.max(
                    actual[((y * 64 + x) * 4 + c as u32) as usize]
                        .abs_diff((mapped * 255.).round() as u8),
                );
            }
            assert_eq!(actual[((y * 64 + x) * 4 + 3) as usize], 255);
        }
    }
    assert!(maximum <= 1, "independent scaled HDR error {maximum} bytes");
    for _ in 0..4 {
        let (cached, work) = frame_mode(&gpu, &mut post, &out, 1, true);
        assert_eq!(cached, actual);
        assert_eq!(work.tone_pixels, 64 * 44);
        assert_eq!(post.pipeline_count(), 1);
    }
}
