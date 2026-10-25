#![cfg(not(target_arch = "wasm32"))]

use render::{
    OutputTransfer,
    terrain_gpu::{
        AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn cube(material: u16) -> PageMesh {
    let halo = PageHalo::from_page(&[1; 4096], 0);
    let mut mesh = PageMesh::default();
    let mut scratch = MeshScratch::new();
    mesh_page(
        &halo,
        |cell| {
            if cell == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial::opaque(material)
            }
        },
        &mut scratch,
        &mut mesh,
    );
    mesh
}

fn frame(
    gpu: &render::Gpu,
    renderer: &mut TerrainGpu,
    camera: &Camera,
    mode: CullingMode,
    width: u32,
    height: u32,
) -> (Vec<u8>, [u32; 3]) {
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain test output"),
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
    });
    let view = output.create_view(&Default::default());
    let row = (width * 4).div_ceil(256) * 256;
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("terrain pixels"),
        size: u64::from(row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let counters = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual GPU indirect counts"),
        size: 96,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let timing = renderer.timestamp_buffer().map(|_| {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("real per-pass GPU timestamps"),
            size: 512,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        })
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = renderer
        .encode_frame(&gpu.queue, &mut encoder, &view, camera, mode)
        .unwrap();
    assert_eq!(work.passes, renderer.frame_graph().count);
    renderer.encode_final_depth_coverage(&mut encoder);
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
    encoder.copy_buffer_to_buffer(renderer.indirect_buffer(), 0, &counters, 0, 96);
    if let (Some((source, _)), Some(destination)) = (renderer.timestamp_buffer(), &timing) {
        encoder.copy_buffer_to_buffer(source, 0, destination, 0, 512);
    }
    gpu.queue.submit([encoder.finish()]);
    let read = |buffer: &wgpu::Buffer| {
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).unwrap();
            });
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        rx.recv().unwrap().unwrap();
        let result = buffer.slice(..).get_mapped_range().to_vec();
        buffer.unmap();
        result
    };
    let bytes = read(&pixels);
    let counts = read(&counters);
    if let Some(timing) = timing {
        let bytes = read(&timing);
        let (_, period) = renderer.timestamp_buffer().unwrap();
        assert!(period > 0.0);
        let mut nonzero = false;
        for pair in bytes
            .chunks_exact(16)
            .take(renderer.frame_graph().count as usize)
        {
            let begin = u64::from_le_bytes(pair[..8].try_into().unwrap());
            let end = u64::from_le_bytes(pair[8..].try_into().unwrap());
            assert!(end >= begin, "pass GPU timestamps reversed");
            nonzero |= end > 0;
        }
        assert!(nonzero, "GPU query readback contains only zero timestamps");
    }
    let tight = bytes
        .chunks_exact(row as usize)
        .flat_map(|row| row[..width as usize * 4].iter().copied())
        .collect();
    let instances = std::array::from_fn(|bucket| {
        [bucket, bucket + 3]
            .into_iter()
            .map(|index| {
                u32::from_le_bytes(counts[index * 16 + 4..index * 16 + 8].try_into().unwrap())
            })
            .sum()
    });
    (tight, instances)
}

fn conservative_depth(
    gpu: &render::Gpu,
    renderer: &TerrainGpu,
    width: u32,
    height: u32,
) -> Vec<f32> {
    let row = (width * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent opaque sample coverage oracle"),
        size: u64::from(row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: renderer
                .conservative_depth_texture()
                .expect("Hi-Z coverage oracle"),
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
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let result = mapped
        .chunks_exact(row as usize)
        .flat_map(|line| {
            line[..width as usize * 4]
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        })
        .collect();
    drop(mapped);
    buffer.unmap();
    result
}

fn colour_coverage(gpu: &render::Gpu, renderer: &TerrainGpu, width: u32, height: u32) -> Vec<u16> {
    let row = (width * 8).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent final color geometry coverage"),
        size: u64::from(row) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: renderer.resolved_hdr_texture(),
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
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    let result = mapped
        .chunks_exact(row as usize)
        .flat_map(|line| {
            line[..width as usize * 8]
                .chunks_exact(8)
                .map(|pixel| u16::from_le_bytes([pixel[6], pixel[7]]))
        })
        .collect();
    drop(mapped);
    buffer.unmap();
    result
}

fn flip_mean(width: u32, height: u32, reference: &[u8], actual: &[u8]) -> f32 {
    let rgb = |data: &[u8]| {
        data.chunks_exact(4)
            .flat_map(|pixel| pixel[..3].iter().copied())
            .collect::<Vec<_>>()
    };
    let metric = nv_flip::flip(
        nv_flip::FlipImageRgb8::with_data(width, height, &rgb(reference)),
        nv_flip::FlipImageRgb8::with_data(width, height, &rgb(actual)),
        nv_flip::DEFAULT_PIXELS_PER_DEGREE,
    );
    nv_flip::FlipPool::from_image(&metric).mean()
}

#[test]
fn descriptor_gpu_draws_and_culls_without_hiding_visible_pixels() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        assert_eq!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
        let atlas = AtlasImage::donor().unwrap();
        let mut renderer = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            129,
            97,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 32,
                max_quads: 8192,
                occlusion: true,
            },
            &atlas,
        )
        .unwrap();
        let mut budget = UploadBudget::new(1_000_000);
        let wall = cube(3);
        let mut center = None;
        for y in 0..3 {
            for x in -1..2 {
                let handle = renderer
                    .upload_page(
                        &gpu.queue,
                        None,
                        1,
                        translation([x as f32 * 16.0, y as f32 * 16.0, 0.0]),
                        &wall,
                        &mut budget,
                    )
                    .unwrap();
                if x == 0 && y == 1 {
                    center = Some(handle);
                }
            }
        }
        renderer
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.0, 16.0, -32.0]),
                &cube(4),
                &mut budget,
            )
            .unwrap();
        renderer
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([1000.0, 16.0, 0.0]),
                &cube(4),
                &mut budget,
            )
            .unwrap();
        let camera = Camera::orthographic(
            [8.0, 24.0, 90.0],
            [8.0, 24.0, 0.0],
            [0.0, 1.0, 0.0],
            28.0,
            129.0 / 97.0,
            0.1,
            200.0,
        );
        renderer.take_work();
        let (baseline, all) = frame(&gpu, &mut renderer, &camera, CullingMode::None, 129, 97);
        let (frustum, front) = frame(&gpu, &mut renderer, &camera, CullingMode::Frustum, 129, 97);
        let (occluded, visible) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        );
        assert_eq!(baseline, frustum, "frustum rejected visible terrain");
        assert_eq!(baseline, occluded, "Hi-Z rejected visible terrain samples");
        assert!(
            front[0] < all[0],
            "out-of-frustum page was not culled: {all:?}/{front:?}"
        );
        assert!(
            visible[0] < front[0],
            "hidden page was not occlusion-culled: {front:?}/{visible:?}"
        );
        assert!(baseline.chunks_exact(4).all(|p| p[3] == 255));
        assert!(
            baseline
                .chunks_exact(4)
                .any(|p| p[0] > 40 && p[0].abs_diff(p[2]) < 30),
            "stone wall missing"
        );
        let steady = renderer.take_work();
        assert_eq!(steady.pipeline_compilations, 0);
        frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        );
        assert_eq!(
            renderer.take_work().upload_bytes,
            0,
            "static frame reuploaded unchanged inputs"
        );
        let control = telemetry::AllocationScope::begin().unwrap();
        let memory = vec![5u8; std::hint::black_box(64)];
        std::hint::black_box(&memory);
        drop(memory);
        assert!(
            control.finish().allocating_calls() > 0,
            "allocation instrumentation must work"
        );
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..10_000 {
            assert!(!std::hint::black_box(
                renderer.needs_redraw(&camera, CullingMode::FrustumAndHiZ)
            ));
        }
        assert_eq!(
            scope.finish(),
            telemetry::AllocationCounts::ZERO,
            "retained-frame plan allocates"
        );
        assert!(!renderer.needs_redraw(&camera, CullingMode::FrustumAndHiZ));
        let mut empty_budget = UploadBudget::new(0);
        let center = center.unwrap();
        assert_eq!(
            renderer.release_page(&gpu.queue, center, &mut empty_budget),
            Err(render::terrain_gpu::TerrainError::UploadBudget)
        );
        let (unchanged, _) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        );
        assert_eq!(unchanged, baseline, "failed release mutated GPU state");
        renderer
            .release_page(&gpu.queue, center, &mut budget)
            .unwrap();
        assert!(
            renderer.needs_redraw(&camera, CullingMode::FrustumAndHiZ),
            "world edit failed to dirty retained image"
        );
        let (opened, _) = frame(&gpu, &mut renderer, &camera, CullingMode::None, 129, 97);
        let (opened_hiz, _) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        );
        assert_ne!(
            opened, baseline,
            "opening the wall did not reveal the far cube"
        );
        assert_eq!(
            opened, opened_hiz,
            "Hi-Z retained stale occluder depth after edit"
        );
        let replacement = renderer
            .upload_page(
                &gpu.queue,
                None,
                2,
                translation([0.0, 16.0, 0.0]),
                &wall,
                &mut budget,
            )
            .unwrap();
        assert_eq!(replacement.slot, center.slot);
        assert!(replacement.generation > center.generation);
        assert_eq!(
            renderer.upload_page(
                &gpu.queue,
                Some(center),
                3,
                translation([0.0, 16.0, 0.0]),
                &wall,
                &mut budget
            ),
            Err(render::terrain_gpu::TerrainError::StaleHandle)
        );
        assert_eq!(
            renderer.upload_page(
                &gpu.queue,
                Some(replacement),
                1,
                translation([0.0, 16.0, 0.0]),
                &wall,
                &mut budget
            ),
            Err(render::terrain_gpu::TerrainError::StaleVersion)
        );
        let (restored, _) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        );
        assert_eq!(
            restored, baseline,
            "arena slot reuse changed visible terrain"
        );
        println!(
            "TERRAIN_GPU_OK all={all:?} frustum={front:?} occlusion={visible:?} pixels={} msaa=4 reverse_z=true",
            129 * 97
        );
    });
}

fn forest_mesh(origin_x: i32, origin_z: i32) -> PageMesh {
    fn sample(x: i32, y: i32, z: i32) -> u32 {
        if !(-16..32).contains(&x) || !(-16..32).contains(&z) || y < 0 {
            return 0;
        }
        if y == 0 {
            return 5;
        }
        if y == 1 {
            return if x < -4 && z > 5 { 4 } else { 1 };
        }
        for (tx, tz) in [(-10, -9), (-3, 5), (8, -7), (10, 9), (2, 12)] {
            if x == tx && z == tz && (2..8).contains(&y) {
                return 2;
            }
            if (x - tx).abs() <= 2
                && (z - tz).abs() <= 2
                && (6..10).contains(&y)
                && ((x - tx).abs() + (z - tz).abs() < 4 || y == 7)
            {
                return 3;
            }
        }
        0
    }
    let mut halo = PageHalo::filled(0);
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                halo.set(
                    x,
                    y,
                    z,
                    sample(
                        origin_x + i32::from(x),
                        i32::from(y),
                        origin_z + i32::from(z),
                    ),
                );
            }
        }
    }
    let mut mesh = PageMesh::default();
    let mut scratch = MeshScratch::new();
    mesh_page(
        &halo,
        |cell| match cell {
            0 => CellMaterial::EMPTY,
            1 => CellMaterial {
                face_materials: [2, 0, 40, 40, 40, 40],
                tint: 0xff60_b27a,
                tint_faces: 0b11_1110,
                ..CellMaterial::opaque(0)
            },
            2 => CellMaterial {
                face_materials: [11, 11, 10, 10, 10, 10],
                tint_faces: 0,
                ..CellMaterial::opaque(10)
            },
            3 => CellMaterial {
                face_materials: [12; 6],
                tint: 0xff60_b27a,
                tint_faces: 0x3f,
                face_uv_rotations: [0; 6],
                class: render::terrain_mesh::MeshClass::Cutout,
                occludes: false,
                casts_ao: false,
                cull_group: 3,
            },
            4 => CellMaterial {
                face_materials: [19; 6],
                tint: u32::MAX,
                tint_faces: 0,
                face_uv_rotations: [0; 6],
                class: render::terrain_mesh::MeshClass::Water,
                occludes: false,
                casts_ao: false,
                cull_group: 4,
            },
            _ => CellMaterial::opaque(3),
        },
        &mut scratch,
        &mut mesh,
    );
    mesh
}

fn decode_rgba(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().unwrap();
    let mut output = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut output).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    output.truncate(info.buffer_size());
    (info.width, info.height, output)
}
fn save_rgba(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
    let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(rgba)
        .unwrap();
}

#[test]
fn forest_matches_recorded_reference_by_nvidia_flip() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let atlas = AtlasImage::donor().unwrap();
        let mut renderer = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            320,
            224,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 32,
                max_quads: 32768,
                occlusion: true,
            },
            &atlas,
        )
        .unwrap();
        let mut budget = UploadBudget::new(2_000_000);
        let mut quads = 0;
        for z in -1..2 {
            for x in -1..2 {
                let mesh = forest_mesh(x * 16, z * 16);
                quads += mesh.quad_count();
                renderer
                    .upload_page(
                        &gpu.queue,
                        None,
                        1,
                        translation([x as f32 * 16.0, 0.0, z as f32 * 16.0]),
                        &mesh,
                        &mut budget,
                    )
                    .unwrap();
            }
        }
        let camera = Camera::perspective(
            [36.0, 27.0, 43.0],
            [0.0, 4.0, 0.0],
            [0.0, 1.0, 0.0],
            60f32.to_radians(),
            320.0 / 224.0,
            0.1,
            200.0,
        );
        let (image, counts) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            320,
            224,
        );
        let (unculled, _) = frame(&gpu, &mut renderer, &camera, CullingMode::None, 320, 224);
        assert_eq!(
            image, unculled,
            "forest Hi-Z image differs from full rendering"
        );
        assert!(
            counts[0] > 0 && counts[1] > 0 && counts[2] > 0,
            "forest must exercise all material buckets: {counts:?}"
        );
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forest.png");
        if std::env::var_os("CUBI_UPDATE_FOREST_REFERENCE").is_some() {
            let reason = std::env::var("CUBI_FOREST_REFERENCE_REASON")
                .expect("an intentional reference update needs a reason");
            assert!(!reason.trim().is_empty());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            save_rgba(&path, 320, 224, &image);
            std::fs::write(path.with_extension("txt"),format!("Эталон M2 «лес»: {reason}\n320×224, MSAA4, HDR, reverse-Z, sRGB; атлас закреплён donor/IMPORT.json.\n")).unwrap();
            println!("FOREST_REFERENCE_UPDATED reason={reason}");
        }
        let (width, height, reference) =
            decode_rgba(&std::fs::read(&path).expect(
                "record forest reference intentionally; never generate it implicitly in CI",
            ));
        assert_eq!((width, height), (320, 224));
        let rgb = |data: &[u8]| {
            data.chunks_exact(4)
                .flat_map(|pixel| pixel[..3].iter().copied())
                .collect::<Vec<_>>()
        };
        let reference = nv_flip::FlipImageRgb8::with_data(320, 224, &rgb(&reference));
        let actual = nv_flip::FlipImageRgb8::with_data(320, 224, &rgb(&image));
        let black = nv_flip::FlipImageRgb8::with_data(320, 224, &vec![0; 320 * 224 * 3]);
        let negative = nv_flip::flip(reference.clone(), black, nv_flip::DEFAULT_PIXELS_PER_DEGREE);
        assert!(
            nv_flip::FlipPool::from_image(&negative).mean() > 0.05,
            "FLIP positive control did not detect a black frame"
        );
        let error = nv_flip::flip(reference, actual, nv_flip::DEFAULT_PIXELS_PER_DEGREE);
        let mean = nv_flip::FlipPool::from_image(&error).mean();
        assert!(mean <= 0.01, "forest NVIDIA FLIP mean {mean} exceeds 0.01");
        let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/artifacts/m2-forest.png");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        save_rgba(&artifact, 320, 224, &image);
        println!(
            "FOREST_GPU_OK quads={quads} visible={counts:?} FLIP_mean={mean:.8} threshold=0.01"
        );
    });
}

#[test]
fn edit_mask_overlay_has_no_coverage_holes_and_final_mesh_is_bitwise_exact() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let atlas = AtlasImage::donor().unwrap();
        let make_renderer = |max_quads| {
            TerrainGpu::new(
                &gpu.device,
                &gpu.queue,
                wgpu::TextureFormat::Rgba8Unorm,
                193,
                145,
                OutputTransfer::ShaderSrgb,
                GpuConfig {
                    max_pages: 8,
                    max_quads,
                    occlusion: true,
                },
                &atlas,
            )
            .unwrap()
        };
        let mut renderer = make_renderer(8192);
        let mut reference = make_renderer(8192);
        let classify = |cell| {
            if cell == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial::opaque(3)
            }
        };
        let mut halo = PageHalo::from_page(&[1; 4096], 0);
        let mut scratch = MeshScratch::new();
        let mut old = PageMesh::default();
        mesh_page(&halo, classify, &mut scratch, &mut old);
        let mut budget = UploadBudget::new(1_000_000);
        let base = renderer
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.0; 3]),
                &old,
                &mut budget,
            )
            .unwrap();
        let camera = Camera::perspective(
            [26.0, 31.0, 29.0],
            [8.0, 13.0, 8.0],
            [0.0, 1.0, 0.0],
            60f32.to_radians(),
            193.0 / 145.0,
            0.1,
            200.0,
        );
        let (before, _) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        halo.set(8, 15, 8, 0);
        let mut full = PageMesh::default();
        mesh_page(&halo, classify, &mut scratch, &mut full);
        reference
            .upload_page(
                &gpu.queue,
                None,
                2,
                translation([0.0; 3]),
                &full,
                &mut budget,
            )
            .unwrap();
        let (canonical, _) = frame(
            &gpu,
            &mut reference,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        let canonical_depth = conservative_depth(&gpu, &reference, 193, 145);
        let canonical_colour = colour_coverage(&gpu, &reference, 193, 145);
        let old_max = canonical
            .iter()
            .zip(&before)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(
            old_max > 4,
            "old-geometry negative control must fail the transient error oracle"
        );
        assert_ne!(
            canonical, before,
            "selected edit must visibly change the scene"
        );
        let mut owners = Vec::new();
        let mut mask = [0u32; 128];
        for y in 14..=15 {
            for z in 7..=9 {
                for x in 7..=9 {
                    owners.push([x, y, z]);
                    let index = usize::from(x) + 16 * (usize::from(z) + 16 * usize::from(y));
                    mask[index / 32] |= 1 << (index % 32);
                }
            }
        }
        let mut overlay = PageMesh::with_capacity([162; 3]);
        let work = render::terrain_mesh::mesh_cell_patch(
            &owners,
            |p| classify(halo.get(p[0], p[1], p[2])),
            &mut overlay,
        )
        .unwrap();
        assert_eq!(work.work.cells_scanned, owners.len() as u64 * 27);
        // The new base allocation fits, but leaves no contiguous range for the
        // shell. A second-allocation refusal must not publish the new base alone.
        assert!(overlay.quad_count() > old.quad_count());
        let mut tight = make_renderer((old.quad_count() + full.quad_count()) as u32);
        let mut tight_budget = UploadBudget::new(1_000_000);
        let tight_base = tight
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.0; 3]),
                &old,
                &mut tight_budget,
            )
            .unwrap();
        let tight_before = frame(
            &gpu,
            &mut tight,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        )
        .0;
        let tight_available = tight_budget.remaining_bytes;
        assert_eq!(
            tight.rebase_edit(
                &gpu.queue,
                tight_base,
                2,
                translation([0.0; 3]),
                &full,
                None,
                &mask,
                &overlay,
                &mut tight_budget
            ),
            Err(render::terrain_gpu::TerrainError::Capacity)
        );
        assert_eq!(
            tight_budget.remaining_bytes, tight_available,
            "failed second arena allocation consumed upload budget"
        );
        assert_eq!(
            frame(
                &gpu,
                &mut tight,
                &camera,
                CullingMode::FrustumAndHiZ,
                193,
                145
            )
            .0,
            tight_before,
            "failed second allocation published half of a coherent rebase"
        );
        assert_eq!(
            tight.publish_composite(
                &gpu.queue,
                None,
                2,
                translation([0.0; 3]),
                &full,
                None,
                &mask,
                &overlay,
                &mut tight_budget
            ),
            Err(render::terrain_gpu::TerrainError::Capacity)
        );
        assert_eq!(tight_budget.remaining_bytes, tight_available);
        assert_eq!(
            frame(
                &gpu,
                &mut tight,
                &camera,
                CullingMode::FrustumAndHiZ,
                193,
                145
            )
            .0,
            tight_before,
            "new-resident composite published only its base on second-allocation failure"
        );
        let mut streaming = make_renderer(8192);
        let (_, streaming_overlay) = streaming
            .publish_composite(
                &gpu.queue,
                None,
                2,
                translation([0.0; 3]),
                &old,
                None,
                &mask,
                &overlay,
                &mut budget,
            )
            .unwrap();
        assert!(streaming_overlay.is_some());
        let streaming_image = frame(
            &gpu,
            &mut streaming,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        )
        .0;
        assert!(
            flip_mean(193, 145, &canonical, &streaming_image) <= 0.001,
            "first-resident stale capture lost its current edit shell"
        );
        let mut owner_updates = Vec::new();
        let mut owner_mesh = PageMesh::with_capacity([6; 3]);
        for owner in &owners {
            render::terrain_mesh::mesh_cell_patch(
                &[*owner],
                |p| classify(halo.get(p[0], p[1], p[2])),
                &mut owner_mesh,
            )
            .unwrap();
            let index = u16::from(owner[0]) + 16 * u16::from(owner[2]) + 256 * u16::from(owner[1]);
            owner_updates
                .push(render::terrain_mesh::OwnerPatch::from_mesh(index, &owner_mesh).unwrap());
        }
        owner_updates.sort_by_key(|update| update.owner);
        let mut fixed = make_renderer(65536);
        let mut fixed_budget = UploadBudget::new(1_000_000);
        let fixed_base = fixed
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.0; 3]),
                &old,
                &mut fixed_budget,
            )
            .unwrap();
        let available = fixed_budget.remaining_bytes;
        let fixed_overlay = fixed
            .publish_owner_patches(
                &gpu.queue,
                fixed_base,
                None,
                2,
                translation([0.0; 3]),
                &mask,
                &owner_updates,
                &mut fixed_budget,
            )
            .unwrap();
        let initialized_bytes = available - fixed_budget.remaining_bytes;
        assert_eq!(
            initialized_bytes,
            16384 + 768 + owner_updates.len() as u64 * 100
        );
        let (fixed_image, fixed_counts) = frame(
            &gpu,
            &mut fixed,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        assert!(flip_mean(193, 145, &canonical, &fixed_image) <= 0.001);
        assert_eq!(
            fixed_counts[0] as usize,
            old.quad_count() + overlay.quad_count(),
            "inactive fixed slots reached the indirect draw"
        );
        let fixed_depth = conservative_depth(&gpu, &fixed, 193, 145);
        let fixed_colour = colour_coverage(&gpu, &fixed, 193, 145);
        assert_eq!(
            canonical_colour
                .iter()
                .zip(&fixed_colour)
                .filter(|(expected, actual)| **expected == 0x3c00 && **actual < 0x3c00)
                .count(),
            0,
            "final color pass exposed sky despite covered prepass depth"
        );
        assert_eq!(
            canonical_depth
                .iter()
                .zip(&fixed_depth)
                .filter(|(expected, actual)| **expected > 0.0 && **actual == 0.0)
                .count(),
            0
        );
        for eye in [
            [52.0, 62.0, 58.0],
            [130.0, 155.0, 145.0],
            [260.0, 310.0, 290.0],
        ] {
            let far_camera = Camera::perspective(
                eye,
                [8.0, 13.0, 8.0],
                [0.0, 1.0, 0.0],
                60f32.to_radians(),
                193.0 / 145.0,
                0.1,
                2000.0,
            );
            let (far_actual, _) = frame(
                &gpu,
                &mut fixed,
                &far_camera,
                CullingMode::FrustumAndHiZ,
                193,
                145,
            );
            let actual_depth = conservative_depth(&gpu, &fixed, 193, 145);
            let (far_reference, _) = frame(
                &gpu,
                &mut reference,
                &far_camera,
                CullingMode::FrustumAndHiZ,
                193,
                145,
            );
            let expected_depth = conservative_depth(&gpu, &reference, 193, 145);
            let actual_colour = colour_coverage(&gpu, &fixed, 193, 145);
            let expected_colour = colour_coverage(&gpu, &reference, 193, 145);
            assert_eq!(
                expected_colour
                    .iter()
                    .zip(&actual_colour)
                    .filter(|(expected, actual)| **expected == 0x3c00 && **actual < 0x3c00)
                    .count(),
                0,
                "distant color pass exposed sky at {eye:?}"
            );
            assert_eq!(
                expected_depth
                    .iter()
                    .zip(&actual_depth)
                    .filter(|(expected, actual)| **expected > 0.0 && **actual == 0.0)
                    .count(),
                0,
                "fixed-owner mask exposed sky at distant camera {eye:?}"
            );
            assert!(
                flip_mean(193, 145, &far_reference, &far_actual) <= 0.001,
                "distant edit patch diverged perceptually at {eye:?}"
            );
        }
        let changed = owner_updates
            .iter()
            .find(|update| update.owner == 8 + 16 * 7 + 256 * 15)
            .copied()
            .unwrap();
        let mut cleared = changed;
        cleared.classes = [render::terrain_mesh::MeshClass::Empty; 6];
        fixed.take_work();
        let available = fixed_budget.remaining_bytes;
        fixed
            .publish_owner_patches(
                &gpu.queue,
                fixed_base,
                Some(fixed_overlay),
                3,
                translation([0.0; 3]),
                &mask,
                &[cleared],
                &mut fixed_budget,
            )
            .unwrap();
        assert_eq!(
            available - fixed_budget.remaining_bytes,
            868,
            "one owner update rebuilt the accumulated shell"
        );
        let (cleared_image, cleared_counts) = frame(
            &gpu,
            &mut fixed,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        assert_ne!(
            cleared_image, fixed_image,
            "Empty owner classes did not clear prior faces"
        );
        let changed_faces = changed
            .classes
            .iter()
            .filter(|class| **class != render::terrain_mesh::MeshClass::Empty)
            .count();
        assert_eq!(
            cleared_counts[0] as usize,
            fixed_counts[0] as usize - changed_faces
        );
        assert_eq!(
            fixed.take_work().quads_emitted,
            6,
            "single-owner upload emitted more than six fixed descriptors"
        );
        fixed
            .publish_owner_patches(
                &gpu.queue,
                fixed_base,
                Some(fixed_overlay),
                4,
                translation([0.0; 3]),
                &mask,
                &[changed],
                &mut fixed_budget,
            )
            .unwrap();
        assert_eq!(
            frame(
                &gpu,
                &mut fixed,
                &camera,
                CullingMode::FrustumAndHiZ,
                193,
                145
            )
            .0,
            fixed_image
        );
        let mut retired_mask = mask;
        retired_mask[usize::from(changed.owner) / 32] &= !(1u32 << (changed.owner % 32));
        let rebased = fixed
            .publish_owner_composite(
                &gpu.queue,
                Some(fixed_base),
                5,
                translation([0.0; 3]),
                &full,
                Some(fixed_overlay),
                &retired_mask,
                &[],
                &mut fixed_budget,
            )
            .unwrap();
        assert_eq!(rebased, (fixed_base, Some(fixed_overlay)));
        let (_, retired_counts) = frame(
            &gpu,
            &mut fixed,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        assert_eq!(
            retired_counts[0] as usize,
            full.quad_count() + overlay.quad_count() - changed_faces,
            "mask retirement still drew stale owner state"
        );
        fixed
            .publish_owner_composite(
                &gpu.queue,
                Some(fixed_base),
                6,
                translation([0.0; 3]),
                &full,
                Some(fixed_overlay),
                &[0; 128],
                &[],
                &mut fixed_budget,
            )
            .unwrap();
        assert_eq!(
            frame(
                &gpu,
                &mut fixed,
                &camera,
                CullingMode::FrustumAndHiZ,
                193,
                145
            )
            .0,
            canonical,
            "fixed-owner retirement was not final bitwise exact"
        );
        println!(
            "H1_FIXED_GPU_OK owners={} initialized_upload_bytes={initialized_bytes} one_owner_update_bytes=868 active_quads={} reserved_quads=24576 retired_state_draws=0",
            owner_updates.len(),
            overlay.quad_count()
        );
        let mut aba = make_renderer(65536);
        let mut aba_budget = UploadBudget::new(1_000_000);
        let parent = aba
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.0; 3]),
                &old,
                &mut aba_budget,
            )
            .unwrap();
        let child = aba
            .publish_owner_patches(
                &gpu.queue,
                parent,
                None,
                2,
                translation([0.0; 3]),
                &mask,
                &owner_updates,
                &mut aba_budget,
            )
            .unwrap();
        aba.release_page(&gpu.queue, parent, &mut aba_budget)
            .unwrap();
        assert_eq!(
            frame(&gpu, &mut aba, &camera, CullingMode::None, 193, 145).1,
            [0, 0, 0],
            "fixed overlay survived its released parent"
        );
        let reused = aba
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.0; 3]),
                &old,
                &mut aba_budget,
            )
            .unwrap();
        assert_eq!(reused.slot, parent.slot);
        assert_ne!(reused.generation, parent.generation);
        aba.set_edit_mask(&gpu.queue, reused, &mask, &mut aba_budget)
            .unwrap();
        assert_eq!(
            frame(&gpu, &mut aba, &camera, CullingMode::None, 193, 145).1[0] as usize,
            old.quad_count(),
            "stale fixed overlay attached to a reused parent slot"
        );
        let available = aba_budget.remaining_bytes;
        assert_eq!(
            aba.publish_owner_patches(
                &gpu.queue,
                reused,
                Some(child),
                3,
                translation([0.0; 3]),
                &mask,
                &owner_updates,
                &mut aba_budget
            ),
            Err(render::terrain_gpu::TerrainError::StaleHandle)
        );
        assert_eq!(aba_budget.remaining_bytes, available);
        aba.release_page(&gpu.queue, child, &mut aba_budget)
            .unwrap();
        let mut denied = UploadBudget::new(0);
        assert_eq!(
            renderer.publish_edit_overlay(
                &gpu.queue,
                base,
                &mask,
                None,
                2,
                translation([0.0; 3]),
                &overlay,
                &mut denied
            ),
            Err(render::terrain_gpu::TerrainError::UploadBudget)
        );
        let (unpublished, _) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        assert_eq!(
            unpublished, before,
            "failed atomic overlay changed visible state"
        );
        let available = budget.remaining_bytes;
        let patch = renderer
            .publish_edit_overlay(
                &gpu.queue,
                base,
                &mask,
                None,
                2,
                translation([0.0; 3]),
                &overlay,
                &mut budget,
            )
            .unwrap();
        let bytes = available - budget.remaining_bytes;
        // Twelve withheld 60-Hz rebuild slots represent a deterministic 200-ms
        // final-mesher delay. The edit must already be correct on the first frame.
        for frame_index in 0..12 {
            let (actual, _) = frame(
                &gpu,
                &mut renderer,
                &camera,
                CullingMode::FrustumAndHiZ,
                193,
                145,
            );
            let differing = actual
                .chunks_exact(4)
                .zip(canonical.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            let maximum_delta = actual
                .iter()
                .zip(&canonical)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            let mean = flip_mean(193, 145, &canonical, &actual);
            let actual_depth = conservative_depth(&gpu, &renderer, 193, 145);
            let actual_colour = colour_coverage(&gpu, &renderer, 193, 145);
            assert_eq!(
                canonical_colour
                    .iter()
                    .zip(&actual_colour)
                    .filter(|(expected, actual)| **expected == 0x3c00 && **actual < 0x3c00)
                    .count(),
                0,
                "legacy color pass exposed sky at frame{frame_index}"
            );
            let holes = canonical_depth
                .iter()
                .zip(&actual_depth)
                .filter(|(expected, actual)| **expected > 0.0 && **actual == 0.0)
                .count();
            assert_eq!(
                holes, 0,
                "unexpected uncovered MSAA samples at delayed frame {frame_index}"
            );
            assert!(
                maximum_delta <= 4,
                "transient maximum delta {maximum_delta} exceeds the strict synthetic oracle"
            );
            assert!(
                mean <= 0.001,
                "transient FLIP {mean} exceeds SPEC-scoped 0.001 threshold"
            );
            if frame_index == 0 {
                let (without_occlusion, _) =
                    frame(&gpu, &mut renderer, &camera, CullingMode::None, 193, 145);
                assert_eq!(
                    actual, without_occlusion,
                    "Hi-Z changed visible masked-base or edit-overlay pixels"
                );
                println!(
                    "H1_TRANSIENT differing_pixels={differing} maximum_delta={maximum_delta} FLIP_mean={mean:.10} unexpected_coverage_holes={holes} old_negative_max={old_max}"
                );
                let directory =
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/artifacts");
                std::fs::create_dir_all(&directory).unwrap();
                save_rgba(&directory.join("h1-actual.png"), 193, 145, &actual);
                save_rgba(&directory.join("h1-canonical.png"), 193, 145, &canonical);
            }
        }
        let rebased = renderer
            .rebase_edit(
                &gpu.queue,
                base,
                3,
                translation([0.0; 3]),
                &old,
                Some(patch),
                &mask,
                &overlay,
                &mut budget,
            )
            .unwrap();
        assert_eq!(rebased, Some(patch));
        let rebased_image = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        )
        .0;
        assert!(
            flip_mean(193, 145, &canonical, &rebased_image) <= 0.001,
            "coherent captured-base rebase lost its current shell"
        );
        let before_toggle = budget.remaining_bytes;
        renderer
            .set_enabled(&gpu.queue, patch, false, &mut budget)
            .unwrap();
        assert_eq!(before_toggle - budget.remaining_bytes, 128);
        frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        let missing_depth = conservative_depth(&gpu, &renderer, 193, 145);
        let missing_holes = canonical_depth
            .iter()
            .zip(&missing_depth)
            .filter(|(expected, actual)| **expected > 0.0 && **actual == 0.0)
            .count();
        assert!(
            missing_holes > 0,
            "missing-overlay negative control escaped the independent sample-coverage oracle"
        );
        renderer
            .set_enabled(&gpu.queue, patch, true, &mut budget)
            .unwrap();
        let before_transform = budget.remaining_bytes;
        renderer
            .update_transform(
                &gpu.queue,
                patch,
                translation([-1.0, 0.0, 0.0]),
                &mut budget,
            )
            .unwrap();
        assert_eq!(
            before_transform - budget.remaining_bytes,
            128,
            "pose update must upload only the object table entry"
        );
        renderer
            .update_transform(&gpu.queue, patch, translation([0.0; 3]), &mut budget)
            .unwrap();
        renderer
            .finish_edit(
                &gpu.queue,
                base,
                4,
                translation([0.0; 3]),
                &full,
                &[patch],
                &mut budget,
            )
            .unwrap();
        let (final_image, _) = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            193,
            145,
        );
        assert_eq!(
            final_image, canonical,
            "atomic final mesh replacement changed the edit"
        );
        assert_eq!(
            renderer.set_enabled(&gpu.queue, patch, true, &mut budget),
            Err(render::terrain_gpu::TerrainError::StaleHandle)
        );
        println!(
            "H1_GPU_OK owners={} reads={} overlay_quads={} upload_bytes={bytes} visible_frame=1 final_delay_frames=12",
            owners.len(),
            work.work.cells_scanned,
            overlay.quad_count()
        );
    });
}

#[test]
fn lit_terrain_samples_independent_rgb_sky_and_cached_shadow_resources() {
    pollster::block_on(async {
        use render::light_volume::{LIGHT_PAGE_BYTES, LIGHT_SAMPLES, LightVolumeGpu, PackedLight};
        use render::shadows::{ShadowConfig, ShadowGpu};
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let atlas = AtlasImage::donor().unwrap();
        let mut renderer = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            129,
            97,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: true,
            },
            &atlas,
        )
        .unwrap();
        let mut budget = UploadBudget::new(1_000_000);
        let base = renderer
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.0; 3]),
                &cube(3),
                &mut budget,
            )
            .unwrap();
        let mut lights = LightVolumeGpu::new(&gpu.device, 8).unwrap();
        let sky = [PackedLight::new(15, 0, [0; 3]).unwrap(); LIGHT_SAMPLES];
        lights
            .upload_page(&gpu.queue, base, 1, &sky, &mut budget)
            .unwrap();
        let mut shadows = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            renderer.geometry(),
            ShadowConfig {
                resolution: 64,
                half_widths: [32.0, 64.0, 128.0],
                max_updates_per_frame: 3,
                ..ShadowConfig::default()
            },
            [8.0; 3],
            [0.35, 0.8, 0.2],
        )
        .unwrap();
        renderer
            .attach_lighting(&gpu.device, &shadows, &lights)
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert_eq!(shadows.encode(&gpu.queue, &mut encoder).cascade_updates, 3);
        gpu.queue.submit([encoder.finish()]);
        let camera = Camera::perspective(
            [26.0, 31.0, 29.0],
            [8.0; 3],
            [0.0, 1.0, 0.0],
            60f32.to_radians(),
            129.0 / 97.0,
            0.1,
            200.0,
        );
        let sky_image = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        )
        .0;
        renderer.take_work();
        lights.take_upload_bytes();
        let red = [PackedLight::new(0, 0, [15, 0, 0]).unwrap(); LIGHT_SAMPLES];
        lights
            .upload_page(&gpu.queue, base, 2, &red, &mut budget)
            .unwrap();
        renderer.invalidate();
        let red_image = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        )
        .0;
        let red_pixels = red_image
            .chunks_exact(4)
            .filter(|p| p[0] > 20 && p[1] < 3 && p[2] < 3)
            .count();
        assert!(
            red_pixels > 100,
            "visible RGB volume was not sampled by terrain: {red_pixels}"
        );
        assert_ne!(sky_image, red_image);
        let geometry_work = renderer.take_work();
        assert_eq!(
            geometry_work.quads_emitted, 0,
            "light edit rebuilt descriptor geometry"
        );
        assert_eq!(
            geometry_work.upload_bytes, 0,
            "light edit uploaded geometry or objects"
        );
        assert_eq!(lights.take_upload_bytes(), LIGHT_PAGE_BYTES);
        let block_only = [PackedLight::new(0, 15, [0; 3]).unwrap(); LIGHT_SAMPLES];
        lights
            .upload_page(&gpu.queue, base, 3, &block_only, &mut budget)
            .unwrap();
        renderer.invalidate();
        let gameplay = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        )
        .0;
        let dark = [PackedLight::new(0, 0, [0; 3]).unwrap(); LIGHT_SAMPLES];
        lights
            .upload_page(&gpu.queue, base, 4, &dark, &mut budget)
            .unwrap();
        renderer.invalidate();
        let dark_image = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            129,
            97,
        )
        .0;
        assert_eq!(
            gameplay, dark_image,
            "scalar gameplay light was counted again as visible emission"
        );
        assert_eq!(
            renderer.attach_lighting(&gpu.device, &shadows, &lights),
            Err(render::terrain_gpu::TerrainError::LatePreparation)
        );
        println!(
            "LIT_TERRAIN_GPU_OK red_pixels={red_pixels} independent_light_upload_bytes={LIGHT_PAGE_BYTES} geometry_upload_bytes=0 scalar_block_double_count=false"
        );
    });
}

#[test]
fn physical_atmosphere_uses_shared_terrain_hdr_and_camera_direction() {
    pollster::block_on(async {
        use render::atmosphere::{AtmosphereConfig, AtmosphereGpu, AtmosphereState};
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let atlas = AtlasImage::donor().unwrap();
        let mut budget = UploadBudget::new(1024);
        let mut atmosphere = AtmosphereGpu::new(
            &gpu.device,
            &gpu.queue,
            AtmosphereConfig {
                transmittance_size: [64, 32],
                sky_size: [48, 32],
                transmittance_steps: 64,
                sky_steps: 48,
            },
            AtmosphereState::default(),
            &mut budget,
        )
        .unwrap();
        let mut renderer = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            129,
            97,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 1,
                max_quads: 1,
                occlusion: false,
            },
            &atlas,
        )
        .unwrap();
        renderer
            .attach_atmosphere(&gpu.device, &atmosphere)
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        atmosphere
            .encode_update(&mut encoder, &mut 1_000_000)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        let camera = Camera::perspective(
            [0.0, 72.0, 0.0],
            [0.0, 73.0, -1.0],
            [0.0, 1.0, 0.0],
            60f32.to_radians(),
            129.0 / 97.0,
            0.1,
            1000.0,
        );
        let (day, _) = frame(&gpu, &mut renderer, &camera, CullingMode::Frustum, 129, 97);
        assert!(
            day.chunks_exact(4)
                .filter(|p| p[2] > p[0] && p[2] > 20)
                .count()
                > 100,
            "physical blue sky LUT did not reach the HDR color graph"
        );
        let down = Camera::perspective(
            [0.0, 72.0, 0.0],
            [0.0, 71.0, 0.0],
            [0.0, 0.0, -1.0],
            60f32.to_radians(),
            129.0 / 97.0,
            0.1,
            1000.0,
        );
        let (ground, _) = frame(&gpu, &mut renderer, &down, CullingMode::Frustum, 129, 97);
        let brightness = |image: &[u8]| {
            image
                .chunks_exact(4)
                .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
                .sum::<u64>()
        };
        assert!(
            brightness(&day) > brightness(&ground) * 3,
            "sky direction ignored the actual inverse camera projection"
        );
        renderer.take_work();
        atmosphere
            .set_state(
                &gpu.queue,
                AtmosphereState {
                    sun_direction: [0.0, -1.0, 0.0],
                    ..AtmosphereState::default()
                },
                &mut budget,
            )
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        atmosphere
            .encode_update(&mut encoder, &mut 1_000_000)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        renderer.invalidate();
        let (night, _) = frame(&gpu, &mut renderer, &down, CullingMode::Frustum, 129, 97);
        assert!(
            night.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]),
            "night-side sky leaked sunlight through the planet"
        );
        assert_eq!(
            renderer.take_work().upload_bytes,
            0,
            "LUT-only sky change reuploaded camera or geometry"
        );
        assert_eq!(
            renderer.attach_atmosphere(&gpu.device, &atmosphere),
            Err(render::terrain_gpu::TerrainError::LatePreparation)
        );
        println!(
            "ATMOSPHERE_TERRAIN_GPU_OK pixels=12513 day_brightness={} ground_brightness={} night_sunlight_leaks=0 msaa=4 HDR=true",
            brightness(&day),
            brightness(&ground)
        );
    });
}

#[test]
fn sixty_four_pages_complete_one_thousand_incremental_edits_with_bounded_uploads() {
    pollster::block_on(async {
        use render::terrain_mesh::{MeshClass, OwnerPatch};
        struct Loaded {
            halo: PageHalo,
            base: render::terrain_gpu::PageHandle,
            overlay: Option<render::terrain_gpu::PageHandle>,
            model: [[f32; 4]; 4],
            mask: [u32; 128],
            changed: [u16; 4096],
            face_counts: [u8; 4096],
            base_quads: usize,
        }
        let classify = |raw| {
            if raw == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial::opaque(3)
            }
        };
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let atlas = AtlasImage::donor().unwrap();
        // 64 worst-case base pages plus 64 fixed patch pages require 48 MiB of
        // descriptors; 64 MiB reserves bounded growth/fragmentation headroom.
        let mut renderer = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            256,
            192,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 128,
                max_quads: 4_194_304,
                occlusion: true,
            },
            &atlas,
        )
        .unwrap();
        let old = cube(3);
        let mut startup = UploadBudget::new(2 * 1024 * 1024);
        let mut pages = Vec::with_capacity(64);
        for z in 0..8 {
            for x in 0..8 {
                let model = translation([x as f32 * 20.0, 0.0, z as f32 * 20.0]);
                let base = renderer
                    .upload_page(&gpu.queue, None, 1, model, &old, &mut startup)
                    .unwrap();
                pages.push(Loaded {
                    halo: PageHalo::from_page(&[1; 4096], 0),
                    base,
                    overlay: None,
                    model,
                    mask: [0; 128],
                    changed: [0; 4096],
                    face_counts: [0; 4096],
                    base_quads: old.quad_count(),
                });
            }
        }
        let camera = Camera::perspective(
            [78.0, 170.0, 205.0],
            [78.0, 7.0, 78.0],
            [0.0, 1.0, 0.0],
            70f32.to_radians(),
            256.0 / 192.0,
            0.1,
            1000.0,
        );
        let before = frame(&gpu, &mut renderer, &camera, CullingMode::None, 256, 192).0;
        let mut owner_mesh = PageMesh::with_capacity([6; 3]);
        let mut updates = Vec::with_capacity(27);
        let mut scratch = MeshScratch::new();
        let mut edits = 0usize;
        let mut revision = 2u64;
        let mut upload_total = 0u64;
        let mut maximum_upload = 0u64;
        let mut peak_overlays = 0;
        let mut completed_frames = 0;
        let mut rebases = 0;
        let mut ever_changed = false;
        let frames = std::env::var("H1_GPU_LOAD_FRAMES")
            .ok()
            .map(|value| value.parse::<u16>().expect("integer H1_GPU_LOAD_FRAMES"))
            .unwrap_or(60);
        assert!((60..=600).contains(&frames) && frames.is_multiple_of(60));
        let mut jobs: [Option<(usize, u16, PageMesh)>; 2] = std::array::from_fn(|_| None);
        let mut next_capture = 0usize;
        for tick in 1..=frames {
            let mut budget = UploadBudget::new(2 * 1024 * 1024);
            let target = usize::from(tick) * 1000 / 60;
            while edits < target {
                let page = &mut pages[(edits * 17) % 64];
                let sequence = edits / 64;
                let mut hash = (sequence as u32)
                    .wrapping_mul(747796405)
                    .wrapping_add(((edits * 17) % 64) as u32)
                    .wrapping_mul(2891336453u32);
                hash = (hash ^ (hash >> 16)).wrapping_mul(2246822519);
                hash ^= hash >> 13;
                let x = (hash % 14 + 1) as i8;
                let z = ((hash >> 8) % 14 + 1) as i8;
                let edit_y = if edits.is_multiple_of(4) {
                    15
                } else {
                    ((hash >> 16) % 14 + 1) as i8
                };
                let raw = 1 - page.halo.get(x, edit_y, z);
                page.halo.set(x, edit_y, z, raw);
                updates.clear();
                for y in edit_y - 1..=(edit_y + 1).min(15) {
                    for owner_z in z - 1..=z + 1 {
                        for owner_x in x - 1..=x + 1 {
                            let owner = [owner_x as u8, y as u8, owner_z as u8];
                            let index = u16::from(owner[0])
                                + 16 * u16::from(owner[2])
                                + 256 * u16::from(owner[1]);
                            page.mask[usize::from(index) / 32] |= 1u32 << (index % 32);
                            page.changed[usize::from(index)] = tick;
                            render::terrain_mesh::mesh_cell_patch(
                                &[owner],
                                |p| classify(page.halo.get(p[0], p[1], p[2])),
                                &mut owner_mesh,
                            )
                            .unwrap();
                            let patch = OwnerPatch::from_mesh(index, &owner_mesh).unwrap();
                            page.face_counts[usize::from(index)] = patch
                                .classes
                                .iter()
                                .filter(|class| **class != MeshClass::Empty)
                                .count()
                                as u8;
                            updates.push(patch);
                        }
                    }
                }
                page.overlay = Some(
                    renderer
                        .publish_owner_patches(
                            &gpu.queue,
                            page.base,
                            page.overlay,
                            revision,
                            page.model,
                            &page.mask,
                            &updates,
                            &mut budget,
                        )
                        .expect(
                            "64-page fixed-patch capacity and 2-MiB budget must admit every edit",
                        ),
                );
                revision += 1;
                edits += 1;
            }
            for job in &mut jobs {
                if job
                    .as_ref()
                    .is_some_and(|(_, capture, _)| tick - *capture >= 12)
                {
                    let (index, capture, mesh) = job.take().unwrap();
                    let page = &mut pages[index];
                    assert_eq!(
                        tick - capture,
                        12,
                        "full publication delay must remain 200 ms at 60 Hz"
                    );
                    let mut mask = [0u32; 128];
                    for (owner, changed) in page.changed.iter().enumerate() {
                        if *changed > capture {
                            mask[owner / 32] |= 1u32 << (owner % 32);
                        }
                    }
                    let (base, overlay) = renderer
                        .publish_owner_composite(
                            &gpu.queue,
                            Some(page.base),
                            revision,
                            page.model,
                            &mesh,
                            page.overlay,
                            &mask,
                            &[],
                            &mut budget,
                        )
                        .unwrap();
                    page.base = base;
                    page.overlay = overlay;
                    page.mask = mask;
                    page.base_quads = mesh.quad_count();
                    revision += 1;
                    rebases += 1;
                }
                if job.is_none() {
                    let index = next_capture % 64;
                    next_capture += 1;
                    let page = &pages[index];
                    let mut capture = PageMesh::default();
                    mesh_page(&page.halo, classify, &mut scratch, &mut capture);
                    *job = Some((index, tick, capture));
                }
            }
            let spent = 2 * 1024 * 1024 - budget.remaining_bytes;
            maximum_upload = maximum_upload.max(spent);
            upload_total += spent;
            let active = pages
                .iter()
                .map(|page| {
                    page.base_quads
                        + page
                            .face_counts
                            .iter()
                            .enumerate()
                            .filter(|(owner, _)| {
                                page.mask[owner / 32] & (1u32 << (owner % 32)) != 0
                            })
                            .map(|(_, faces)| usize::from(*faces))
                            .sum::<usize>()
                })
                .sum::<usize>();
            let (image, counts) = frame(&gpu, &mut renderer, &camera, CullingMode::None, 256, 192);
            completed_frames += 1;
            assert_eq!(
                counts[0] as usize, active,
                "completed frame {tick} missed or retained an owner update"
            );
            assert_eq!(counts[1..], [0, 0]);
            ever_changed |= image != before;
            peak_overlays =
                peak_overlays.max(pages.iter().filter(|page| page.overlay.is_some()).count());
        }
        assert_eq!(edits, usize::from(frames) * 1000 / 60);
        assert_eq!(completed_frames, frames);
        assert_eq!(peak_overlays, 64);
        assert!(
            ever_changed,
            "the GPU edit workload never changed visible pixels"
        );
        let mut reference = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            256,
            192,
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 64,
                max_quads: 262_144,
                occlusion: true,
            },
            &atlas,
        )
        .unwrap();
        let mut final_budget = UploadBudget::new(2 * 1024 * 1024);
        for page in &mut pages {
            let mut mesh = PageMesh::default();
            mesh_page(&page.halo, classify, &mut scratch, &mut mesh);
            renderer
                .publish_owner_composite(
                    &gpu.queue,
                    Some(page.base),
                    revision,
                    page.model,
                    &mesh,
                    page.overlay,
                    &[0; 128],
                    &[],
                    &mut final_budget,
                )
                .unwrap();
            revision += 1;
            reference
                .upload_page(&gpu.queue, None, 1, page.model, &mesh, &mut startup)
                .unwrap();
        }
        let actual = frame(
            &gpu,
            &mut renderer,
            &camera,
            CullingMode::FrustumAndHiZ,
            256,
            192,
        )
        .0;
        let canonical = frame(
            &gpu,
            &mut reference,
            &camera,
            CullingMode::FrustumAndHiZ,
            256,
            192,
        )
        .0;
        assert_eq!(
            actual, canonical,
            "final full publication differs from independent 64-page mesh reference"
        );
        let actual_depth = conservative_depth(&gpu, &renderer, 256, 192);
        let expected_depth = conservative_depth(&gpu, &reference, 256, 192);
        assert_eq!(
            actual_depth, expected_depth,
            "final 64-page sample coverage differs from canonical"
        );
        println!(
            "H1_GPU_LOAD_OK resident_pages=64 edits={edits} logical_seconds={} capture_workers=2 submitted_edit_frames={frames} completed_edit_frames={completed_frames} owner_response_completed_frames=1 delayed_full_frames=12 rebases={rebases} peak_patch_pages={peak_overlays} reserved_patch_bytes={} arena_bytes=67108864 visibility_bytes=100663296 upload_total_bytes={upload_total} max_frame_upload_bytes={maximum_upload} frame_upload_limit_bytes=2097152 final_bitwise=true final_coverage_bitwise=true",
            f32::from(frames) / 60.0,
            64 * 24576 * 16
        );
    });
}

#[test]
fn textured_floor_edit_at_runtime_camera_has_no_color_coverage_cracks() {
    pollster::block_on(async {
        use render::terrain_mesh::{MeshClass, OwnerPatch};
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let atlas = AtlasImage::donor().unwrap();
        let make = || {
            TerrainGpu::new(
                &gpu.device,
                &gpu.queue,
                wgpu::TextureFormat::Rgba8Unorm,
                780,
                437,
                OutputTransfer::ShaderSrgb,
                GpuConfig {
                    max_pages: 32,
                    max_quads: 262_144,
                    occlusion: true,
                },
                &atlas,
            )
            .unwrap()
        };
        let mut actual = make();
        let mut reference = make();
        let mut budget = UploadBudget::new(4_000_000);
        let classify = |raw| match raw {
            0 => CellMaterial::EMPTY,
            1 => CellMaterial {
                face_materials: [2, 0, 1, 1, 1, 1],
                tint: 0xff70_c080,
                tint_faces: 1 << 1,
                ..CellMaterial::opaque(0)
            },
            _ => CellMaterial::opaque(2),
        };
        let mut halo = PageHalo::filled(0);
        for y in -1..=16 {
            for z in -1..=16 {
                for x in -1..=16 {
                    halo.set(
                        x,
                        y,
                        z,
                        if y < 8 {
                            2
                        } else if y == 8 {
                            1
                        } else {
                            0
                        },
                    );
                }
            }
        }
        let mut scratch = MeshScratch::new();
        let mut old = PageMesh::default();
        mesh_page(&halo, classify, &mut scratch, &mut old);
        assert_eq!(old.quad_count(), 1);
        let mut base = None;
        let mut resident = Vec::new();
        for z in 5..=7 {
            for x in 5..=7 {
                let model = translation([x as f32 * 16.0, 64.0, z as f32 * 16.0]);
                let handle = actual
                    .upload_page(&gpu.queue, None, 1, model, &old, &mut budget)
                    .unwrap();
                resident.push((x, z, handle));
                if x == 5 && z == 5 {
                    base = Some(handle);
                } else {
                    reference
                        .upload_page(&gpu.queue, None, 1, model, &old, &mut budget)
                        .unwrap();
                }
            }
        }
        let base_handle = base.unwrap();
        let east = resident
            .iter()
            .find(|(x, z, _)| *x == 6 && *z == 5)
            .unwrap()
            .2;
        let south = resident
            .iter()
            .find(|(x, z, _)| *x == 5 && *z == 6)
            .unwrap()
            .2;
        let neighbors = [None, None, None, Some(south), None, Some(east)];
        assert_eq!(
            actual
                .set_page_neighbors(&gpu.queue, base_handle, neighbors, &mut budget)
                .unwrap(),
            56
        );
        assert_eq!(
            actual
                .set_page_neighbors(
                    &gpu.queue,
                    base_handle,
                    neighbors,
                    &mut UploadBudget::new(0)
                )
                .unwrap(),
            0
        );
        let mut denied = neighbors;
        denied[3] = None;
        assert_eq!(
            actual.set_page_neighbors(&gpu.queue, base_handle, denied, &mut UploadBudget::new(0)),
            Err(render::terrain_gpu::TerrainError::UploadBudget)
        );
        let mut stale = neighbors;
        stale[5] = Some(render::terrain_gpu::PageHandle {
            slot: east.slot,
            generation: east.generation + 1,
        });
        assert_eq!(
            actual.set_page_neighbors(&gpu.queue, base_handle, stale, &mut budget),
            Err(render::terrain_gpu::TerrainError::StaleHandle)
        );
        let yaw = f64::from(-135.0f32 * (std::f32::consts::PI / 180.0));
        let pitch = f64::from(33.0f32 * (std::f32::consts::PI / 180.0));
        let eye64 = [96.5, 73.0 + f64::from(1.62f32), 96.5];
        let look = [
            yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        ];
        let camera = Camera::perspective(
            eye64.map(|v| v as f32),
            std::array::from_fn(|axis| (eye64[axis] + look[axis]) as f32),
            [0.0, 1.0, 0.0],
            70f32.to_radians(),
            780.0 / 437.0,
            0.05,
            2048.0,
        );
        let (before, _) = frame(
            &gpu,
            &mut actual,
            &camera,
            CullingMode::FrustumAndHiZ,
            780,
            437,
        );
        halo.set(14, 8, 14, 0);
        let mut full = PageMesh::default();
        mesh_page(&halo, classify, &mut scratch, &mut full);
        reference
            .upload_page(
                &gpu.queue,
                None,
                2,
                translation([80.0, 64.0, 80.0]),
                &full,
                &mut budget,
            )
            .unwrap();
        let (canonical, _) = frame(
            &gpu,
            &mut reference,
            &camera,
            CullingMode::FrustumAndHiZ,
            780,
            437,
        );
        let expected_coverage = colour_coverage(&gpu, &reference, 780, 437);
        let mut mask = [0u32; 128];
        let mut updates = Vec::with_capacity(27);
        let mut owner_mesh = PageMesh::with_capacity([6; 3]);
        for y in 7..=9u8 {
            for z in 13..=15u8 {
                for x in 13..=15u8 {
                    let owner = u16::from(x) + 16 * u16::from(z) + 256 * u16::from(y);
                    mask[usize::from(owner) / 32] |= 1u32 << (owner % 32);
                    render::terrain_mesh::mesh_cell_patch(
                        &[[x, y, z]],
                        |p| classify(halo.get(p[0], p[1], p[2])),
                        &mut owner_mesh,
                    )
                    .unwrap();
                    updates.push(OwnerPatch::from_mesh(owner, &owner_mesh).unwrap());
                }
            }
        }
        assert!(
            updates
                .iter()
                .any(|patch| patch.classes.iter().any(|class| *class != MeshClass::Empty))
        );
        actual
            .publish_owner_patches(
                &gpu.queue,
                base.unwrap(),
                None,
                2,
                translation([80.0, 64.0, 80.0]),
                &mask,
                &updates,
                &mut budget,
            )
            .unwrap();
        let (fast, _) = frame(
            &gpu,
            &mut actual,
            &camera,
            CullingMode::FrustumAndHiZ,
            780,
            437,
        );
        let fast_coverage = colour_coverage(&gpu, &actual, 780, 437);
        let holes = expected_coverage
            .iter()
            .zip(&fast_coverage)
            .filter(|(expected, actual)| **expected == 0x3c00 && **actual < 0x3c00)
            .count();
        let mean = flip_mean(780, 437, &canonical, &fast);
        let negative = flip_mean(780, 437, &canonical, &before);
        let differing = fast
            .chunks_exact(4)
            .zip(canonical.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        let maximum = fast
            .iter()
            .zip(&canonical)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        let artifacts =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        save_rgba(&artifacts.join("h1-textured-fast.png"), 780, 437, &fast);
        save_rgba(
            &artifacts.join("h1-textured-canonical.png"),
            780,
            437,
            &canonical,
        );
        save_rgba(&artifacts.join("h1-textured-before.png"), 780, 437, &before);
        println!(
            "H1_TEXTURED_RUNTIME_GPU holes={holes} differing_pixels={differing} max_delta={maximum} FLIP_mean={mean:.9} old_negative_FLIP={negative:.9}"
        );
        assert_eq!(
            holes, 0,
            "runtime-perspective textured floor lost covered color samples"
        );
        assert!(
            mean <= 0.001,
            "runtime-perspective FLIP {mean} exceeds .001"
        );
        assert!(
            negative > mean + 0.001,
            "old-floor negative control escaped perceptual rejection"
        );
    });
}
