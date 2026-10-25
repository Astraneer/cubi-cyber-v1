#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    height_gpu::{HeightGpu, HeightGpuConfig, HeightGpuError, TILE_BYTES},
    height_tile::{HeightPlan, HeightTile, HeightTileKey},
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn terrain(gpu: &render::Gpu) -> TerrainGpu {
    TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        96,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 8,
            max_quads: 8192,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap()
}
fn camera() -> Camera {
    Camera::orthographic(
        [8., 40., 8.],
        [8., 0., 8.],
        [0., 0., -1.],
        8.,
        1.,
        0.1,
        100.,
    )
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
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    entities: Option<&mut HeightGpu>,
    camera: Camera,
) -> (Vec<u8>, [u32; 4]) {
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual instanced mob HDR composite"),
        size: wgpu::Extent3d {
            width: 96,
            height: 96,
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
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("entity scene image oracle"),
        size: 512 * 96,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let counts = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual entity indirect counts"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    if let Some(entities) = entities {
        entities
            .prepare_frame(&gpu.queue, &mut encoder, camera)
            .unwrap();
        let work = terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera,
                CullingMode::Frustum,
                |pass| entities.draw(pass),
            )
            .unwrap();
        assert!(work.draw_calls >= 1);
        encoder.copy_buffer_to_buffer(entities.indirect_buffer(), 0, &counts, 0, 16);
    } else {
        terrain
            .encode_frame(
                &gpu.queue,
                &mut encoder,
                &view,
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
                bytes_per_row: Some(512),
                rows_per_image: Some(96),
            },
        },
        wgpu::Extent3d {
            width: 96,
            height: 96,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let image = read(gpu, &pixels);
    let counts = read(gpu, &counts);
    (
        image
            .chunks_exact(512)
            .flat_map(|row| row[..384].iter().copied())
            .collect(),
        std::array::from_fn(|i| u32::from_le_bytes(counts[i * 4..i * 4 + 4].try_into().unwrap())),
    )
}
fn tile(key: HeightTileKey) -> HeightTile {
    let mut t = HeightTile {
        key,
        source: 1,
        heights: [0; 289],
        known: [255; 37],
        atlas: [3; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
    };
    t.known[36] = 1;
    t
}
#[test]
fn gpu_tiles_preserve_unknown_and_protected_air_and_reject_partial_publication() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut heights = HeightGpu::new(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig {
                max_tiles: 8,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        heights.take_work();
        let mut source = tile(HeightTileKey {
            space: 0,
            level: 0,
            x: 0,
            z: 0,
        });
        let h = heights
            .upload(
                &gpu.queue,
                None,
                &source,
                0,
                [0; 3],
                true,
                &mut UploadBudget::new(TILE_BYTES + 64),
            )
            .unwrap();
        assert_eq!(heights.take_work().upload_bytes, TILE_BYTES + 64);
        let sky = frame(&gpu, &mut terrain, None, camera()).0;
        let (full, draw) = frame(&gpu, &mut terrain, Some(&mut heights), camera());
        assert_eq!(draw, [1536, 1, 0, 0]);
        assert_eq!(
            full.chunks_exact(4)
                .zip(sky.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count(),
            9216
        );
        let protected = 7 + 16 * 7;
        source.protected[protected / 8] |= 1 << (protected % 8);
        let unknown = 2 + 17 * 2;
        source.known[unknown / 8] &= !(1 << (unknown % 8));
        source.source += 1;
        heights
            .upload(
                &gpu.queue,
                Some(h),
                &source,
                0,
                [0; 3],
                true,
                &mut UploadBudget::new(TILE_BYTES),
            )
            .unwrap();
        let holes = frame(&gpu, &mut terrain, Some(&mut heights), camera()).0;
        for y in 0..96 {
            for x in 0..96 {
                let cx = x / 6;
                let cz = y / 6;
                let should_draw = source.cell_drawable(cx, cz, 0);
                let i = (x + 96 * y) * 4;
                assert_eq!(
                    holes[i..i + 4] != sky[i..i + 4],
                    should_draw,
                    "independent pixel coverage x{x}y{y} cell{cx},{cz}"
                );
            }
        }
        let mut bad = source.clone();
        bad.source += 1;
        bad.heights[0] = 4;
        assert_eq!(
            heights.upload(
                &gpu.queue,
                Some(h),
                &bad,
                0,
                [0; 3],
                true,
                &mut UploadBudget::new(TILE_BYTES - 1)
            ),
            Err(HeightGpuError::UploadBudget)
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            holes
        );
        let mut other = tile(HeightTileKey {
            space: 0,
            level: 0,
            x: 1,
            z: 0,
        });
        other.heights.fill(3);
        let off = heights
            .upload(
                &gpu.queue,
                None,
                &other,
                0,
                [0; 3],
                false,
                &mut UploadBudget::new(TILE_BYTES + 64),
            )
            .unwrap();
        assert_eq!(
            heights.set_enabled_batch(
                &gpu.queue,
                &[(h, false), (off, true)],
                &mut UploadBudget::new(7)
            ),
            Err(HeightGpuError::UploadBudget)
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            holes
        );
        assert_eq!(
            heights
                .set_enabled_batch(
                    &gpu.queue,
                    &[(h, false), (off, true)],
                    &mut UploadBudget::new(8)
                )
                .unwrap(),
            8
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            sky
        );
        heights
            .release(&gpu.queue, h, &mut UploadBudget::new(4))
            .unwrap();
        assert_eq!(
            heights.upload(
                &gpu.queue,
                Some(h),
                &source,
                0,
                [0; 3],
                true,
                &mut UploadBudget::new(10000)
            ),
            Err(HeightGpuError::StaleHandle)
        );
        println!(
            "M5 height GPU96² independentcoverage9216; protected36px+unknown144px preserved; tile{}B+64B header; atomic8Bfrontswitch",
            TILE_BYTES
        );
    });
}
#[test]
fn actual_gpu_horizon_has_bounded_ring_work_and_idle_uploads_zero() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut heights = HeightGpu::new(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig::default(),
            None,
        )
        .unwrap();
        heights.take_work();
        let plan = HeightPlan::new(0, [0, 0], 9, 512).unwrap();
        let mut handles = Vec::new();
        let mut budget = UploadBudget::new(2000000);
        for selected in &plan.tiles {
            let source = tile(selected.key);
            handles.push((
                heights
                    .upload(
                        &gpu.queue,
                        None,
                        &source,
                        selected.seams,
                        [0; 3],
                        true,
                        &mut budget,
                    )
                    .unwrap(),
                source,
                selected.seams,
            ));
        }
        let upload = heights.take_work();
        assert_eq!(upload.tile_uploads, plan.tiles.len() as u32);
        assert_eq!(upload.cpu_vertices, 0);
        let c = Camera::orthographic(
            [0., 40000., 0.],
            [0., 0., 0.],
            [0., 0., -1.],
            32768.,
            1.,
            0.1,
            80000.,
        );
        let (_, draw) = frame(&gpu, &mut terrain, Some(&mut heights), c);
        assert_eq!(draw, [1536, plan.tiles.len() as u32, 0, 0]);
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..10 {
            for (handle, source, seams) in &handles {
                heights
                    .upload(
                        &gpu.queue,
                        Some(*handle),
                        source,
                        *seams,
                        [0; 3],
                        true,
                        &mut UploadBudget::new(0),
                    )
                    .unwrap();
            }
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
        assert_eq!(heights.take_work().upload_bytes, 0);
        let (handle, mut changed, seams) = handles[0].clone();
        changed.source += 1;
        changed.heights[8 + 17 * 8] = 4;
        heights
            .upload(
                &gpu.queue,
                Some(handle),
                &changed,
                seams,
                [0; 3],
                true,
                &mut UploadBudget::new(TILE_BYTES + 64),
            )
            .unwrap();
        let edited = heights.take_work();
        assert_eq!(edited.tile_uploads, 1);
        assert_eq!(edited.cpu_vertices, 0);
        println!(
            "M5 actualGPU horizon1024chunks: selected{} visits{} indirectTiles{} implicitQuads{} vertices{} startupUpload{}B oneEditedTile{}B idle10covers0B0alloc",
            plan.tiles.len(),
            plan.visited_nodes,
            draw[1],
            draw[1] * 256,
            draw[0] * draw[1],
            upload.upload_bytes,
            edited.upload_bytes
        );
    });
}
#[test]
fn stitched_gpu_edges_equal_flat_reference_and_large_origin_keeps_pixels() {
    pollster::block_on(async {
        use render::height_tile::{EAST, HeightError};
        use render::lod::Edge;
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut heights = HeightGpu::new(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig {
                max_tiles: 8,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        let coarse = tile(HeightTileKey {
            space: 0,
            level: 1,
            x: 1,
            z: 0,
        });
        let mut fine0 = tile(HeightTileKey {
            space: 0,
            level: 0,
            x: 1,
            z: 0,
        });
        let mut fine1 = tile(HeightTileKey {
            space: 0,
            level: 0,
            x: 1,
            z: 1,
        });
        let mut budget = UploadBudget::new(100000);
        let h0 = heights
            .upload(&gpu.queue, None, &fine0, 0, [0; 3], true, &mut budget)
            .unwrap();
        let h1 = heights
            .upload(&gpu.queue, None, &fine1, 0, [0; 3], true, &mut budget)
            .unwrap();
        heights
            .upload(&gpu.queue, None, &coarse, 0, [0; 3], true, &mut budget)
            .unwrap();
        let c = Camera::perspective(
            [47., 18., -14.],
            [32., 0., 16.],
            [0., 1., 0.],
            1.,
            1.,
            0.1,
            200.,
        );
        let reference = frame(&gpu, &mut terrain, Some(&mut heights), c).0;
        for z in (1..16).step_by(2) {
            fine0.heights[16 + 17 * z] = 10;
            fine1.heights[16 + 17 * z] = 10;
        }
        fine0.source += 1;
        fine1.source += 1;
        fine0.compatible_coarse(&coarse, Edge::East).unwrap();
        fine1.compatible_coarse(&coarse, Edge::East).unwrap();
        heights
            .upload(
                &gpu.queue,
                Some(h0),
                &fine0,
                EAST,
                [0; 3],
                true,
                &mut budget,
            )
            .unwrap();
        heights
            .upload(
                &gpu.queue,
                Some(h1),
                &fine1,
                EAST,
                [0; 3],
                true,
                &mut budget,
            )
            .unwrap();
        let stitched = frame(&gpu, &mut terrain, Some(&mut heights), c).0;
        assert_eq!(
            stitched, reference,
            "GPU fine edge did not match independent flat coarse reference"
        );
        heights
            .upload(&gpu.queue, Some(h0), &fine0, 0, [0; 3], true, &mut budget)
            .unwrap();
        assert_ne!(
            frame(&gpu, &mut terrain, Some(&mut heights), c).0,
            reference,
            "negative control did not expose unstitched teeth"
        );
        let mut stale = coarse.clone();
        stale.heights[0] = 1;
        assert_eq!(
            fine0.compatible_coarse(&stale, Edge::East),
            Err(HeightError::SourceMismatch)
        );
        let mut rebased = HeightGpu::new(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig {
                max_tiles: 1,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        let mut far = tile(HeightTileKey {
            space: 0,
            level: 0,
            x: 62500000,
            z: 62500000,
        });
        far.heights.fill(1000000000);
        let fh = rebased
            .upload(
                &gpu.queue,
                None,
                &far,
                0,
                [1000000000; 3],
                true,
                &mut budget,
            )
            .unwrap();
        let far_image = frame(&gpu, &mut terrain, Some(&mut rebased), camera()).0;
        let mut local = HeightGpu::new(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig {
                max_tiles: 1,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        local
            .upload(
                &gpu.queue,
                None,
                &tile(HeightTileKey {
                    space: 0,
                    level: 0,
                    x: 0,
                    z: 0,
                }),
                0,
                [0; 3],
                true,
                &mut budget,
            )
            .unwrap();
        assert_eq!(
            far_image,
            frame(&gpu, &mut terrain, Some(&mut local), camera()).0,
            "integer origin was converted after f32 precision loss"
        );
        let bytes = rebased
            .rebase(
                &gpu.queue,
                [1000000016, 1000000000, 1000000000],
                &mut UploadBudget::new(64),
            )
            .unwrap();
        assert_eq!(bytes, 64);
        let shifted = Camera::orthographic(
            [-8., 40., 8.],
            [-8., 0., 8.],
            [0., 0., -1.],
            8.,
            1.,
            0.1,
            100.,
        );
        assert_eq!(
            far_image,
            frame(&gpu, &mut terrain, Some(&mut rebased), shifted).0
        );
        assert_eq!(
            rebased
                .upload(
                    &gpu.queue,
                    Some(fh),
                    &far,
                    0,
                    [1000000016, 1000000000, 1000000000],
                    true,
                    &mut UploadBudget::new(0)
                )
                .unwrap(),
            fh
        );
        println!(
            "M5 GPU2:1seams BITWISE flatreference; unstitchednegativechangespixels; staleedge rejected; origin1e9 BITWISE local; rebase64Bheaders"
        );
    });
}
