#![cfg(not(target_arch = "wasm32"))]
#[path = "reference/smaa_area.rs"]
mod reference;
use render::{
    OutputTransfer,
    smaa::{SmaaConfig, SmaaError, SmaaGpu, affine_areas},
    terrain_gpu::UploadBudget,
};
const SIZE: u32 = 128;
#[test]
fn analytic_table_agrees_with_independent_clipped_pixel_polygons() {
    for d0 in 0..=16 {
        for d1 in 0..=16 {
            for e0 in -1..=1 {
                for e1 in -1..=1 {
                    let a = affine_areas(d0, d1, e0, e1).unwrap();
                    let b = reference::area(d0, d1, e0, e1);
                    for i in 0..2 {
                        assert!(
                            (a[i] - b[i]).abs() < 2e-14,
                            "{d0}/{d1}/{e0}/{e1}: {a:?} vs {b:?}"
                        );
                    }
                }
            }
        }
    }
    assert_eq!(affine_areas(17, 0, 0, 0), None);
    assert_eq!(affine_areas(0, 0, 2, 0), None);
}
fn read(gpu: &render::Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let size = texture.size();
    let pitch = (size.width * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("SMAA actual completed pixels"),
        size: u64::from(pitch) * u64::from(size.height),
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
                bytes_per_row: Some(pitch),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    gpu.queue.submit([encoder.finish()]);
    let (send, receive) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |value| send.send(value).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receive.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let mut output = Vec::with_capacity((size.width * size.height * 4) as usize);
    for row in mapped.chunks(pitch as usize) {
        output.extend_from_slice(&row[..(size.width * 4) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    output
}
fn target(device: &wgpu::Device, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("SMAA independent image fixture"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
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
fn mapped(x: u32, y: u32, rotation: u32) -> (u32, u32) {
    match rotation {
        0 => (x, y),
        1 => (y, SIZE - 1 - x),
        2 => (SIZE - 1 - x, SIZE - 1 - y),
        _ => (SIZE - 1 - y, x),
    }
}
fn encode(gpu: &render::Gpu, stage: &SmaaGpu, output: &wgpu::TextureView, input: &[[u16; 4]]) {
    gpu.queue.write_texture(
        stage.input_texture().as_image_copy(),
        bytemuck::cast_slice(input),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stage.extent()[0] * 8),
            rows_per_image: Some(stage.extent()[1]),
        },
        stage.input_texture().size(),
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = stage.encode(&mut encoder, output, None);
    assert_eq!(work.passes, 3);
    assert_eq!(
        work.pixels,
        u64::from(stage.extent()[0]) * u64::from(stage.extent()[1]) * 3
    );
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
}
#[test]
fn actual_patterns_rotations_and_complements_match_polygon_area_and_preserve_detail() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut budget = UploadBudget::new(20824);
    let mut stage = SmaaGpu::new(
        &gpu.device,
        &gpu.queue,
        SIZE,
        SIZE,
        wgpu::TextureFormat::Rgba8Unorm,
        OutputTransfer::Attachment,
        SmaaConfig::default(),
        &mut budget,
    )
    .unwrap();
    assert_eq!(budget.remaining_bytes, 0);
    let output = target(&gpu.device, SIZE);
    let view = output.create_view(&Default::default());
    for run in [1u32, 2, 4, 8, 16] {
        for rotation in 0..4 {
            for invert in [false, true] {
                let offset = if run == 1 { 0 } else { 32 };
                let input: Vec<_> = (0..SIZE)
                    .flat_map(|y| {
                        (0..SIZE).map(move |x| {
                            let (x, y) = mapped(x, y, rotation);
                            let high = (y >= x / run + offset) ^ invert;
                            let c = if high { 0x3c00 } else { 0 };
                            [c, c, c, 0x3c00]
                        })
                    })
                    .collect();
                encode(&gpu, &stage, &view, &input);
                let actual = read(&gpu, &output);
                let mut improved = 0;
                for y in 20..SIZE - 20 {
                    for x in 20..SIZE - 20 {
                        let (bx, by) = mapped(x, y, rotation);
                        let boundary = bx / run + offset;
                        let d0 = bx % run;
                        let areas = reference::area(d0, run - 1 - d0, -1, 1);
                        let mut expected = if by >= boundary { 1. } else { 0. };
                        if by == boundary {
                            expected = 1. - areas[0];
                        } else if by + 1 == boundary {
                            expected = areas[1];
                        }
                        if invert {
                            expected = 1. - expected;
                        }
                        let expected = (expected * 255.).round() as i32;
                        let value = i32::from(actual[((y * SIZE + x) * 4) as usize]);
                        assert!(
                            (value - expected).abs() <= 1,
                            "run{run} rotation{rotation} invert{invert} pixel{x}/{y}: {value} != {expected}"
                        );
                        if value > 1 && value < 254 {
                            improved += 1;
                        }
                    }
                }
                assert!(
                    improved > 0,
                    "real staircase must receive fractional coverage"
                );
                let once = actual;
                encode(&gpu, &stage, &view, &input);
                assert_eq!(read(&gpu, &output), once, "static repeat bitwise");
            }
        }
    }
    for kind in 0..6 {
        let input: Vec<_> = (0..SIZE)
            .flat_map(|y| {
                (0..SIZE).map(move |x| {
                    let high = match kind {
                        0 => false,
                        1 => x >= 64,
                        2 => y >= 64,
                        3 => (20..100).contains(&x) && (20..40).contains(&y),
                        4 => x == 64,
                        _ => (x + y) % 2 == 0,
                    };
                    let c = if high { 0x3c00 } else { 0 };
                    [c, c, c, 0x3c00]
                })
            })
            .collect();
        encode(&gpu, &stage, &view, &input);
        let actual = read(&gpu, &output);
        for (pixel, source) in actual.chunks_exact(4).zip(&input) {
            let value = if source[0] == 0 { 0 } else { 255 };
            assert_eq!(
                pixel,
                [value, value, value, 255],
                "axis/corner/thin/checker detail{kind}"
            );
        }
    }
    let old = stage.reserved_bytes();
    assert!(stage.resize(&gpu.device, 64, 64).unwrap());
    assert!(stage.reserved_bytes() < old);
    assert!(!stage.resize(&gpu.device, 64, 64).unwrap());
    assert!(stage.resize(&gpu.device, 0, 64).is_err());
    assert_eq!(stage.extent(), [64, 64]);
    let mut denied = UploadBudget::new(20823);
    assert!(matches!(
        SmaaGpu::new(
            &gpu.device,
            &gpu.queue,
            64,
            64,
            wgpu::TextureFormat::Rgba8Unorm,
            OutputTransfer::Attachment,
            SmaaConfig::default(),
            &mut denied
        ),
        Err(SmaaError::UploadBudget)
    ));
    assert_eq!(denied.remaining_bytes, 20823);
}

#[test]
fn moving_subpixel_contour_has_independent_coverage_reference_and_no_reprojection_shimmer() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let stage = SmaaGpu::new(
        &gpu.device,
        &gpu.queue,
        SIZE,
        SIZE,
        wgpu::TextureFormat::Rgba8Unorm,
        OutputTransfer::Attachment,
        SmaaConfig::default(),
        &mut UploadBudget::new(20824),
    )
    .unwrap();
    let output = target(&gpu.device, SIZE);
    let view = output.create_view(&Default::default());
    let mut previous: Option<Vec<u8>> = None;
    let mut previous_raw: Option<Vec<bool>> = None;
    let (mut aa_error, mut raw_error, mut aa_jump, mut raw_jump) = (0f64, 0f64, 0f64, 0f64);
    for phase in 0..=32 {
        let offset = -0.5 + f64::from(phase) / 32.;
        let raw: Vec<bool> = (0..SIZE)
            .flat_map(|y| (0..SIZE).map(move |x| f64::from(y) >= f64::from(x) + offset))
            .collect();
        let input: Vec<[u16; 4]> = raw
            .iter()
            .map(|high| {
                let c = if *high { 0x3c00 } else { 0 };
                [c, c, c, 0x3c00]
            })
            .collect();
        encode(&gpu, &stage, &view, &input);
        let actual = read(&gpu, &output);
        for y in 20..SIZE - 20 {
            for x in 20..SIZE - 20 {
                let index = (y * SIZE + x) as usize;
                let measured = f64::from(actual[index * 4]) / 255.;
                let truth = reference::coverage(1., offset, f64::from(x), f64::from(y));
                let raw_value = f64::from(raw[index]);
                aa_error += (measured - truth).powi(2);
                raw_error += (raw_value - truth).powi(2);
                if let (Some(old), Some(old_raw)) = (&previous, &previous_raw) {
                    aa_jump = aa_jump.max((measured - f64::from(old[index * 4]) / 255.).abs());
                    raw_jump = raw_jump.max((raw_value - f64::from(old_raw[index])).abs());
                }
            }
        }
        previous = Some(actual);
        previous_raw = Some(raw);
    }
    assert!(
        aa_error < raw_error * 0.9,
        "independent moving polygon coverage: {aa_error}/{raw_error}"
    );
    assert!(
        aa_jump < 0.76 && raw_jump == 1.,
        "AA/raw maximum jump {aa_jump}/{raw_jump}"
    );
    let input: Vec<_> = (0..SIZE)
        .flat_map(|y| {
            (0..SIZE).map(move |x| {
                let c = if y >= x { 0x3c00 } else { 0 };
                [c, c, c, 0x3c00]
            })
        })
        .collect();
    encode(&gpu, &stage, &view, &input);
    let first = read(&gpu, &output);
    let shifted: Vec<_> = (0..SIZE)
        .flat_map(|y| {
            (0..SIZE).map(move |x| {
                let c = if y > x { 0x3c00 } else { 0 };
                [c, c, c, 0x3c00]
            })
        })
        .collect();
    encode(&gpu, &stage, &view, &shifted);
    let second = read(&gpu, &output);
    for y in 20..SIZE - 20 {
        for x in 20..SIZE - 20 {
            assert_eq!(
                &second[((y * SIZE + x) * 4) as usize..((y * SIZE + x) * 4 + 4) as usize],
                &first
                    [(((y - 1) * SIZE + x) * 4) as usize..(((y - 1) * SIZE + x) * 4 + 4) as usize]
            );
        }
    }
    println!(
        "SMAA_TEMPORAL aa_squared_error={aa_error:.9} raw_squared_error={raw_error:.9} maximum_aa_jump={aa_jump:.9} raw_jump={raw_jump:.1} integer_reprojection_bitwise=true"
    );
}
