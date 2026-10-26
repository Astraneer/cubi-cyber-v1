#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    height_gpu::{HeightGpu, HeightGpuConfig, TILE_BYTES},
    height_tile::{HeightTile, HeightTileKey},
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn terrain(gpu: &render::Gpu) -> TerrainGpu {
    terrain_samples(gpu, 4)
}
fn terrain_samples(gpu: &render::Gpu, sample_count: u32) -> TerrainGpu {
    TerrainGpu::new_with_samples(
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
        sample_count,
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

fn assert_image(expected: &[u8], actual: &[u8], label: &str) {
    let first = expected.iter().zip(actual).position(|(a, b)| a != b);
    assert!(
        expected.len() == actual.len() && first.is_none(),
        "{label}: expected{}B actual{}B firstDiff{:?} diffBytes{}",
        expected.len(),
        actual.len(),
        first,
        expected.iter().zip(actual).filter(|(a, b)| a != b).count()
    );
}
#[test]
fn actual_height_near_mask_is_three_dimensional_atomic_and_retains_old_resident_pages() {
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
        let mut known = [255; 37];
        known[36] = 1;
        let source = HeightTile {
            key: HeightTileKey {
                space: 0,
                level: 0,
                x: 0,
                z: 0,
            },
            source: 1,
            heights: [8; 289],
            known,
            atlas: [3; 256],
            tint: [u32::MAX; 256],
            protected: [0; 32],
        };
        heights
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
        let sky = frame(&gpu, &mut terrain, None, camera()).0;
        let full = frame(&gpu, &mut terrain, Some(&mut heights), camera()).0;
        assert_eq!(
            full.chunks_exact(4)
                .zip(sky.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count(),
            9216,
            "full independent plane must cover every sample region"
        );
        let high = [[0, 4, 0], [1, 7, 1]];
        let low = [[0, 0, 0], [1, 1, 1]];
        assert_eq!(
            heights
                .update_near_cover(
                    &gpu.queue,
                    Some(high),
                    &[],
                    [0; 3],
                    &mut UploadBudget::new(render::near_cover::UPLOAD_BYTES)
                )
                .unwrap(),
            render::near_cover::UPLOAD_BYTES
        );
        assert_image(
            &full,
            &frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            "same X/Z at flight altitude removed lower ground",
        );
        let mut denied = UploadBudget::new(render::near_cover::UPLOAD_BYTES - 1);
        assert_eq!(
            heights.update_near_cover(&gpu.queue, Some(low), &[], [0; 3], &mut denied),
            Err(render::near_cover::Error::UploadBudget)
        );
        assert_eq!(denied.remaining_bytes, render::near_cover::UPLOAD_BYTES - 1);
        assert_image(
            &full,
            &frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            "held image",
        );
        heights
            .update_near_cover(
                &gpu.queue,
                Some(low),
                &[],
                [0; 3],
                &mut UploadBudget::new(render::near_cover::UPLOAD_BYTES),
            )
            .unwrap();
        assert_image(
            &sky,
            &frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            "intended Unknown near cells must block public fallback",
        );
        heights
            .update_near_cover(
                &gpu.queue,
                Some(high),
                &[[0, 0, 0]],
                [0; 3],
                &mut UploadBudget::new(render::near_cover::UPLOAD_BYTES),
            )
            .unwrap();
        assert_image(
            &sky,
            &frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            "budget-delayed old resident page lost its far exclusion",
        );
        let mut ample = UploadBudget::new(1_000_000);
        assert_eq!(
            heights.update_near_cover(
                &gpu.queue,
                Some(high),
                &[[0, 0, 0], [33, 0, 0]],
                [0; 3],
                &mut ample
            ),
            Err(render::near_cover::Error::Extent)
        );
        assert_eq!(ample.remaining_bytes, 1_000_000);
        assert_image(
            &sky,
            &frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            "held image",
        );
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            assert_eq!(
                heights
                    .update_near_cover(
                        &gpu.queue,
                        Some(high),
                        &[[0, 0, 0]],
                        [0; 3],
                        &mut UploadBudget::new(0)
                    )
                    .unwrap(),
                0
            );
        }
        let counts = scope.finish();
        assert_eq!(
            counts,
            telemetry::AllocationCounts::ZERO,
            "unchanged near mask allocated"
        );
        heights
            .update_near_cover(
                &gpu.queue,
                None,
                &[],
                [0; 3],
                &mut UploadBudget::new(render::near_cover::UPLOAD_BYTES),
            )
            .unwrap();
        assert_image(
            &full,
            &frame(&gpu, &mut terrain, Some(&mut heights), camera()).0,
            "retired near page did not restore the far surface",
        );
        println!(
            "3D Height coverage PASS: high-flight ground retained, intended Unknown blocks far, old resident retained, budget/extent atomic,1000idle0alloc/0upload"
        );
    });
}
