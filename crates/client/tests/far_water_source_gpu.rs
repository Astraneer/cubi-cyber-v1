use client::far_water_source::{Context, Error, FarWaterSource, Limits};
use core_types::{PageId, SpaceId, Version};
use net::{height::GeometryStamp, water::WaterHalo};
use std::sync::Arc;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn image(x: i32, view: u64, code: u8) -> Arc<WaterHalo> {
    let mut cells = Box::new([1; 5832]);
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                cells[(x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize)] =
                    if y == 0 {
                        code
                    } else if y < 0 {
                        2
                    } else {
                        1
                    };
            }
        }
    }
    Arc::new(WaterHalo {
        page: page(x),
        geometry_source: Version(19),
        geometry_view: 23,
        column_source: 37,
        water_view: view,
        cells,
        tints: [0xf800; 256],
    })
}
fn binding(x: i32) -> (GeometryStamp, u64) {
    (
        GeometryStamp {
            page: page(x),
            source: Version(19),
            view: 23,
        },
        37,
    )
}
fn source() -> FarWaterSource {
    FarWaterSource::new(Limits {
        max_pages: 4,
        ..Limits::default()
    })
    .unwrap()
}
fn finish(owner: &mut FarWaterSource) -> Context {
    let (ctx, task) = owner.take_task().unwrap();
    assert!(
        owner
            .complete(ctx, task.execute(|| false).unwrap())
            .unwrap()
    );
    ctx
}
struct GpuInputs {
    _color: wgpu::Texture,
    color: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth: wgpu::TextureView,
    _sky: wgpu::Texture,
    sky: wgpu::TextureView,
}
fn gpu_inputs(gpu: &render::Gpu) -> GpuInputs {
    let texture = |label, format| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 4,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    let color_texture = texture(
        "actual opaque source for live fluid owner",
        wgpu::TextureFormat::Rgba16Float,
    );
    let color = color_texture.create_view(&Default::default());
    let depth_texture = texture(
        "known opaque depth for live fluid owner",
        wgpu::TextureFormat::Depth32Float,
    );
    let depth = depth_texture.create_view(&Default::default());
    let sky_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unit environment for live fluid source oracle"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &sky_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0u8, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(16),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let sky = sky_texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("actual MSAA opaque HDR input clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.001),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    gpu.queue.submit([encoder.finish()]);
    GpuInputs {
        _color: color_texture,
        color,
        _depth: depth_texture,
        depth,
        _sky: sky_texture,
        sky,
    }
}
fn gpu_image(gpu: &render::Gpu, water: &render::water_gpu::WaterGpu) -> Vec<u8> {
    let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("real live fluid HDR image"),
        size: 64 * 64 * 8,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: water.resolved_texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &b,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(512),
                rows_per_image: Some(64),
            },
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = b.slice(..).get_mapped_range().to_vec();
    b.unmap();
    bytes
}
fn model(_: PageId) -> Option<[[f32; 4]; 4]> {
    Some(render::terrain_gpu::translation([0., 0., 0.]))
}
fn render_image(gpu: &render::Gpu, water: &mut render::water_gpu::WaterGpu) -> Vec<u8> {
    use render::{
        terrain_gpu::{Camera, UploadBudget},
        water_gpu::WaterState,
    };
    let state = WaterState {
        camera: Camera::perspective(
            [8.5, 6., 14.],
            [8.5, 0.5, 8.5],
            [0., 1., 0.],
            0.8,
            1.,
            0.1,
            100.,
        ),
        eye: [8.5, 6., 14.],
        time_seconds: 0.,
        sun: [0., 1., 0.],
        sun_intensity: 0.,
        underwater: false,
        scene_revision: 1,
    };
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    water
        .encode(
            &gpu.queue,
            &mut encoder,
            state,
            &mut UploadBudget::new(256),
            0,
        )
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu_image(gpu, water)
}
#[test]
fn actual_gpu_stages_inactive_retains_old_cover_and_atomically_swaps_guard_rebases_and_retires() {
    pollster::block_on(async {
        use render::{
            terrain_gpu::UploadBudget,
            water_gpu::{WaterConfig, WaterError, WaterGpu, WaterInputs},
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let inputs = gpu_inputs(&gpu);
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            WaterConfig {
                max_objects: 4,
                max_quads: 1024,
                reflection_steps: 0,
                wave_amplitude: 0.,
                ..WaterConfig::default()
            },
            64,
            64,
            WaterInputs {
                color_msaa: &inputs.color,
                depth_msaa: &inputs.depth,
                sky: &inputs.sky,
                transmittance: &inputs.sky,
            },
            None,
        )
        .unwrap();
        let baseline = render_image(&gpu, &mut water);
        let mut owner = source();
        owner.set_wanted(&[binding(0)]).unwrap();
        owner.admit(image(0, 1, 10)).unwrap();
        finish(&mut owner);
        let work = owner
            .flush_staging(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(100000),
                4,
                model,
            )
            .unwrap();
        assert_eq!(work.staged, 1);
        assert!(owner.all_ready());
        assert_eq!(
            render_image(&gpu, &mut water),
            baseline,
            "staged object must not draw before whole cover publishes"
        );
        assert_eq!(owner.publication_bytes(&mut water, |_| true).unwrap(), 4);
        owner
            .publish(&gpu.queue, &mut water, &mut UploadBudget::new(4), |_| true)
            .unwrap();
        let original = render_image(&gpu, &mut water);
        assert_ne!(original, baseline);
        owner.admit(image(0, 2, 3)).unwrap();
        finish(&mut owner);
        water.take_work();
        let work = owner
            .flush_staging(&gpu.queue, &mut water, &mut UploadBudget::new(80), 4, model)
            .unwrap();
        assert!(work.backpressure);
        assert_eq!(work.gpu_upload_bytes, 80);
        assert_eq!(water.take_work().object_uploads, 1);
        assert!(!owner.all_ready());
        assert_eq!(render_image(&gpu, &mut water), original);
        owner
            .flush_staging(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(100000),
                4,
                model,
            )
            .unwrap();
        assert!(owner.all_ready());
        assert_eq!(
            water.take_work().object_uploads,
            0,
            "budget retry retains same inactive handle"
        );
        assert_eq!(render_image(&gpu, &mut water), original);
        let shifted = |_: PageId| Some(render::terrain_gpu::translation([32., 0., 0.]));
        assert_eq!(owner.rebase_bytes(&mut water, shifted).unwrap(), 160);
        let mut short = UploadBudget::new(159);
        let work = owner
            .rebase(&gpu.queue, &mut water, &mut short, shifted)
            .unwrap();
        assert!(work.backpressure);
        assert_eq!(work.gpu_upload_bytes, 0);
        assert_eq!(short.remaining_bytes, 159);
        assert_eq!(render_image(&gpu, &mut water), original);
        assert_eq!(
            owner
                .rebase(&gpu.queue, &mut water, &mut UploadBudget::new(160), shifted)
                .unwrap()
                .gpu_upload_bytes,
            160
        );
        assert!(render_image(&gpu, &mut water) != original);
        assert_eq!(
            owner
                .rebase(&gpu.queue, &mut water, &mut UploadBudget::new(160), model)
                .unwrap()
                .gpu_upload_bytes,
            160
        );
        assert_eq!(render_image(&gpu, &mut water), original);
        assert_eq!(owner.publication_bytes(&mut water, |_| true).unwrap(), 8);
        let mut too_small = UploadBudget::new(4);
        assert_eq!(
            owner.publish(&gpu.queue, &mut water, &mut too_small, |_| true),
            Err(Error::Gpu(WaterError::UploadBudget))
        );
        assert_eq!(too_small.remaining_bytes, 4);
        assert_eq!(render_image(&gpu, &mut water), original);
        owner
            .publish(&gpu.queue, &mut water, &mut UploadBudget::new(8), |_| true)
            .unwrap();
        let lower = render_image(&gpu, &mut water);
        assert_ne!(lower, original);
        owner
            .flush_staging(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(100000),
                4,
                model,
            )
            .unwrap();
        assert_eq!(owner.stats().retiring, 0);
        assert_eq!(water.active_quad_count(), 256);
        water.take_work();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            assert_eq!(owner.publication_bytes(&mut water, |_| true).unwrap(), 0);
            let work = owner
                .publish(&gpu.queue, &mut water, &mut UploadBudget::new(0), |_| true)
                .unwrap();
            assert_eq!(work.gpu_upload_bytes, 0);
            assert_eq!(work.published, 0);
            assert_eq!(
                owner
                    .guard_near(&gpu.queue, &mut water, &mut UploadBudget::new(0), |_| true)
                    .unwrap(),
                0
            );
        }
        let idle = scope.finish();
        assert_eq!(idle.allocating_calls(), 0);
        assert_eq!(idle.dealloc_calls, 0);
        assert_eq!(water.take_work().upload_bytes, 0);
        assert_eq!(
            owner.guard_near(&gpu.queue, &mut water, &mut UploadBudget::new(0), |_| false),
            Err(Error::Gpu(WaterError::UploadBudget))
        );
        assert!(!owner.guard_ready());
        assert_eq!(render_image(&gpu, &mut water), lower);
        owner
            .guard_near(&gpu.queue, &mut water, &mut UploadBudget::new(4), |_| false)
            .unwrap();
        assert!(owner.guard_ready());
        assert_eq!(render_image(&gpu, &mut water), baseline);
        let shifted = |_: PageId| Some(render::terrain_gpu::translation([2., 0., 0.]));
        assert_eq!(owner.rebase_bytes(&mut water, shifted).unwrap(), 80);
        let mut small = UploadBudget::new(79);
        let work = owner
            .rebase(&gpu.queue, &mut water, &mut small, shifted)
            .unwrap();
        assert!(work.backpressure);
        assert_eq!(work.gpu_upload_bytes, 0);
        assert_eq!(small.remaining_bytes, 79);
        let work = owner
            .rebase(&gpu.queue, &mut water, &mut UploadBudget::new(80), shifted)
            .unwrap();
        assert_eq!(work.gpu_upload_bytes, 80);
        assert_eq!(
            render_image(&gpu, &mut water),
            baseline,
            "rebase must not reenable hidden water"
        );
        let invalid = |_: PageId| {
            let mut m = render::terrain_gpu::translation([0.; 3]);
            m[1][1] = f32::NAN;
            Some(m)
        };
        let mut budget = UploadBudget::new(1000);
        assert_eq!(
            owner.rebase(&gpu.queue, &mut water, &mut budget, invalid),
            Err(Error::Gpu(WaterError::InvalidModel))
        );
        assert_eq!(budget.remaining_bytes, 1000);
        let mut next = image(0, 3, 0);
        Arc::make_mut(&mut next).geometry_view = 24;
        let mut wanted = binding(0);
        wanted.0.view = 24;
        owner.set_wanted(&[wanted]).unwrap();
        owner.admit(next).unwrap();
        let (ctx, task) = owner.take_task().unwrap();
        assert!(
            !owner
                .complete(ctx, task.execute(|| false).unwrap())
                .unwrap()
        );
        assert!(!owner.all_ready());
        assert_eq!(
            owner.publication_bytes(&mut water, |_| true),
            Err(Error::NotReady)
        );
        owner
            .guard_near(&gpu.queue, &mut water, &mut UploadBudget::new(4), |_| true)
            .unwrap();
        assert_ne!(
            render_image(&gpu, &mut water),
            baseline,
            "guard independently controls old front while replacement remainsUnknown"
        );
        owner
            .teardown(&gpu.queue, &mut water, &mut UploadBudget::new(100000))
            .unwrap();
        assert_eq!(owner.stats().records, 0);
        assert_eq!(owner.stats().retiring, 0);
        assert_eq!(water.active_quad_count(), 0);
        assert_eq!(render_image(&gpu, &mut water), baseline);
    });
}
#[test]
fn actual_gpu_capacity_keeps_one_inactive_handle_and_logical_owner_quota_never_consumes_shared_capacity()
 {
    pollster::block_on(async {
        use render::{
            terrain_gpu::UploadBudget,
            water_gpu::{WaterConfig, WaterGpu, WaterInputs},
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let inputs = gpu_inputs(&gpu);
        let mut water = WaterGpu::new(
            &gpu.device,
            &gpu.queue,
            WaterConfig {
                max_objects: 2,
                max_quads: 256,
                reflection_steps: 0,
                wave_amplitude: 0.,
                ..WaterConfig::default()
            },
            64,
            64,
            WaterInputs {
                color_msaa: &inputs.color,
                depth_msaa: &inputs.depth,
                sky: &inputs.sky,
                transmittance: &inputs.sky,
            },
            None,
        )
        .unwrap();
        let mut owner = source();
        owner.set_wanted(&[binding(0)]).unwrap();
        owner.admit(image(0, 1, 10)).unwrap();
        finish(&mut owner);
        owner
            .flush_staging(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(100000),
                4,
                model,
            )
            .unwrap();
        owner.publication_bytes(&mut water, |_| true).unwrap();
        owner
            .publish(&gpu.queue, &mut water, &mut UploadBudget::new(4), |_| true)
            .unwrap();
        let old = render_image(&gpu, &mut water);
        owner.admit(image(0, 2, 3)).unwrap();
        finish(&mut owner);
        water.take_work();
        let work = owner
            .flush_staging(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(100000),
                4,
                model,
            )
            .unwrap();
        assert!(work.backpressure);
        assert_eq!(work.gpu_upload_bytes, 80);
        assert_eq!(water.take_work().object_uploads, 1);
        for _ in 0..10 {
            let work = owner
                .flush_staging(
                    &gpu.queue,
                    &mut water,
                    &mut UploadBudget::new(100000),
                    4,
                    model,
                )
                .unwrap();
            assert!(work.backpressure);
            assert_eq!(work.gpu_upload_bytes, 0);
        }
        assert_eq!(water.take_work().object_uploads, 0);
        assert_eq!(render_image(&gpu, &mut water), old);
        assert!(!owner.all_ready());
        owner
            .teardown(&gpu.queue, &mut water, &mut UploadBudget::new(100000))
            .unwrap();
        assert_eq!(water.active_quad_count(), 0);
        let mut owner = FarWaterSource::new(Limits {
            max_pages: 4,
            max_total_quads: 256,
            ..Limits::default()
        })
        .unwrap();
        owner.set_wanted(&[binding(0), binding(1)]).unwrap();
        for x in 0..2 {
            owner.admit(image(x, 1, 10)).unwrap();
            finish(&mut owner);
        }
        let work = owner
            .flush_staging(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(100000),
                4,
                model,
            )
            .unwrap();
        assert!(work.backpressure);
        assert_eq!(work.staged, 1);
        assert_eq!(owner.stats().staged_quads, 256);
        assert!(!owner.all_ready());
        owner.set_wanted(&[binding(0)]).unwrap();
        assert!(owner.all_ready());
        owner.publication_bytes(&mut water, |_| true).unwrap();
        owner
            .publish(&gpu.queue, &mut water, &mut UploadBudget::new(4), |_| true)
            .unwrap();
        assert_eq!(owner.stats().front_quads, 256);
        owner
            .teardown(&gpu.queue, &mut water, &mut UploadBudget::new(100000))
            .unwrap();
        assert_eq!(water.active_quad_count(), 0);
    });
}
