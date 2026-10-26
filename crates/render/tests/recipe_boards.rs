#![cfg(not(target_arch = "wasm32"))]
use render::{
    light_volume::PackedLight,
    recipe_boards::*,
    terrain_gpu::{AtlasImage, AtlasMip, Camera, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOC: telemetry::allocator::CountingAllocator =
    telemetry::allocator::CountingAllocator::new(std::alloc::System);
fn camera() -> Camera {
    Camera {
        view_projection: [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ],
    }
}
fn card(layer: u32, z: f32, size: f32) -> Instance {
    Instance {
        center: [0., 0., z],
        right: [1., 0., 0.],
        up: [0., 1., 0.],
        size: [size; 2],
        layer,
        light: PackedLight::new(0, 0, [15; 3]).unwrap(),
        enabled: true,
    }
}
fn capture(gpu: &render::Gpu, boards: &RecipeBoardGpu, samples: u32, mrt: bool) -> Vec<u16> {
    let make = |format, samples, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 32,
                height: 32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let output = make(
        wgpu::TextureFormat::Rgba16Float,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let msaa = make(
        wgpu::TextureFormat::Rgba16Float,
        samples,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let ambient = make(
        wgpu::TextureFormat::Rgba16Float,
        samples,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let depth = make(
        wgpu::TextureFormat::Depth32Float,
        samples,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let output_view = output.create_view(&Default::default());
    let msaa_view = msaa.create_view(&Default::default());
    let ambient_view = ambient.create_view(&Default::default());
    let depth_view = depth.create_view(&Default::default());
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 8192,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut attachments = vec![Some(wgpu::RenderPassColorAttachment {
            view: if samples == 1 {
                &output_view
            } else {
                &msaa_view
            },
            resolve_target: (samples > 1).then_some(&output_view),
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })];
        if mrt {
            attachments.push(Some(wgpu::RenderPassColorAttachment {
                view: &ambient_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            }));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        assert_eq!(
            boards.draw(&mut pass),
            u32::from(boards.active_boards() > 0)
        );
    }
    encoder.copy_texture_to_buffer(
        output.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &read,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(32),
            },
        },
        output.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |v| tx.send(v).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = read.slice(..).get_mapped_range();
    bytes
        .chunks_exact(2)
        .map(|v| u16::from_le_bytes([v[0], v[1]]))
        .collect()
}
#[test]
fn physical_cards_use_scene_depth_and_atomic_fixed_resources_without_idle_work() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 1,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![255; 4],
            tint_mask: vec![0],
        }],
    };
    let pixels = [255, 0, 0, 255, 0, 255, 0, 255];
    for samples in [1, 4] {
        for mrt in [false, true] {
            let mut renderer = TerrainGpu::new_with_samples(
                &gpu.device,
                &gpu.queue,
                wgpu::TextureFormat::Rgba8Unorm,
                32,
                32,
                render::OutputTransfer::Attachment,
                GpuConfig {
                    max_pages: 1,
                    max_quads: 16,
                    occlusion: false,
                },
                &atlas,
                samples,
            )
            .unwrap();
            if mrt {
                renderer.prepare_ambient_mrt(&gpu.device).unwrap();
            }
            let image = Images {
                width: 1,
                height: 1,
                layers: 2,
                rgba: &pixels,
            };
            let bytes = RecipeBoardGpu::required_allocation_bytes(1, 1, 2, 2).unwrap();
            assert!(matches!(
                RecipeBoardGpu::new(
                    &gpu.device,
                    &gpu.queue,
                    renderer.geometry(),
                    image,
                    Config {
                        max_boards: 2,
                        memory_budget_bytes: bytes - 1
                    },
                    Lighting::default()
                ),
                Err(Error::MemoryBudget)
            ));
            let mut boards = RecipeBoardGpu::new(
                &gpu.device,
                &gpu.queue,
                renderer.geometry(),
                image,
                Config {
                    max_boards: 2,
                    memory_budget_bytes: bytes,
                },
                Lighting::default(),
            )
            .unwrap();
            assert_eq!(boards.allocation_bytes(), 216);
            assert_eq!(boards.take_work().pipeline_compilations, 1);
            let mut budget = UploadBudget::new(208);
            boards
                .prepare_frame(&gpu.queue, camera(), &[card(0, 0.5, 1.5)], &mut budget)
                .unwrap();
            assert_eq!(budget.remaining_bytes, 0);
            let baseline = capture(&gpu, &boards, samples, mrt);
            assert_eq!(
                &baseline[(16 * 32 + 16) * 4..(16 * 32 + 16) * 4 + 4],
                &[0x3c00, 0, 0, 0x3c00]
            );
            // Front card is drawn FIRST: depth, rather than draw order, must preserve it.
            let pair = [card(1, 0.75, 0.5), card(0, 0.5, 1.5)];
            let mut denied = UploadBudget::new(127);
            assert_eq!(
                boards.prepare_frame(&gpu.queue, camera(), &pair, &mut denied),
                Err(Error::UploadBudget)
            );
            assert_eq!(denied.remaining_bytes, 127);
            assert_eq!(capture(&gpu, &boards, samples, mrt), baseline);
            boards
                .prepare_frame(&gpu.queue, camera(), &pair, &mut UploadBudget::new(128))
                .unwrap();
            let front = capture(&gpu, &boards, samples, mrt);
            assert_eq!(
                &front[(16 * 32 + 16) * 4..(16 * 32 + 16) * 4 + 4],
                &[0, 0x3c00, 0, 0x3c00]
            );
            assert_eq!(
                &front[(16 * 32 + 6) * 4..(16 * 32 + 6) * 4 + 4],
                &[0x3c00, 0, 0, 0x3c00]
            );
            boards.take_work();
            let scope = telemetry::allocator::AllocationScope::begin().unwrap();
            for _ in 0..1000 {
                let work = boards
                    .prepare_frame(&gpu.queue, camera(), &pair, &mut UploadBudget::new(0))
                    .unwrap();
                assert_eq!(work, Work::default());
            }
            let counts = scope.finish();
            assert_eq!(counts.alloc_calls, 0);
            assert_eq!(counts.dealloc_calls, 0);
            assert_eq!(boards.take_work(), Work::default());
            boards
                .prepare_frame(&gpu.queue, camera(), &[], &mut UploadBudget::new(128))
                .unwrap();
            assert!(capture(&gpu, &boards, samples, mrt).iter().all(|v| *v == 0));
            eprintln!(
                "RECIPE_BOARDS_GPU samples={samples} MRT={mrt} actual_bytes={bytes} occlusion=true denied_atomic=true idle_allocations=0"
            );
        }
    }
}
