//! The optional source must own the same near pages as the actual View renderer.
use client::{
    ClientScene, SceneConfig, native_pool,
    view::{ClientView, EnvironmentProfile, water},
};
use core_types::{BlockPos, Cell, PageId, SpaceId, SpaceTransform, Tick, Version};
use net::{PageSnapshot, PageUpdate};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    post::{Exposure, PostConfig},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
    water_gpu::WaterConfig,
};
use std::time::{Duration, Instant};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn point(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn config() -> SceneConfig {
    SceneConfig {
        center: point(8, 4, 8),
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 64,
        authoritative_remote: true,
        ..SceneConfig::forest()
    }
}
fn image(page: PageId) -> PageUpdate {
    let mut cells = Box::new([Cell::AIR; 4096]);
    if page == point(8, 4, 8).page() {
        for z in 0..16 {
            for x in 0..16 {
                cells[x + 16 * z] = Cell::from_parts(rules::ids::STONE, 0);
            }
        }
        for y in 1..4 {
            for z in 4..12 {
                for x in 4..12 {
                    cells[x + 16 * (z + 16 * y)] = Cell::from_parts(rules::ids::WATER, 0);
                }
            }
        }
    } else if page == point(24, 4, 8).page() {
        cells[point(24, 4, 8).local_index().get() as usize] =
            Cell::from_parts(rules::ids::WATER, 0);
    }
    PageUpdate::Snapshot(PageSnapshot {
        page,
        version: 1,
        source_version: Version(1),
        cells,
    })
}
fn import_requested(view: &mut ClientView) {
    let pages: Vec<_> = view
        .scene
        .requested_pages()
        .filter(|p| view.scene.world().page(*p).is_none())
        .collect();
    for page in pages {
        view.scene
            .import_authoritative_page(&image(page), Tick(1))
            .unwrap();
    }
}
fn view(gpu: &render::Gpu, atlas: &AtlasImage, physical: bool) -> ClientView {
    let terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        64,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 32,
            max_quads: 262144,
            occlusion: true,
        },
        atlas,
    )
    .unwrap();
    let mut view = ClientView::new(terrain, config()).unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        96,
        64,
        OutputTransfer::ShaderSrgb,
        EnvironmentProfile {
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                water: physical.then_some(WaterConfig {
                    max_objects: 64,
                    max_quads: 8192,
                    reflection_steps: 0,
                    wave_amplitude: 0.,
                    ..Default::default()
                }),
                post: PostConfig {
                    exposure: Exposure::Manual(1.),
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
        atlas,
    )
    .unwrap();
    if physical {
        view.prepare_water(water::Config {
            source: client::water_source::WaterSourceLimits {
                max_pages: 32,
                max_total_quads: 8192,
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    }
    import_requested(&mut view);
    view
}
fn settle(gpu: &render::Gpu, view: &mut ClientView, water_quads: Option<usize>) {
    let pool = native_pool(2, 16).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        view.scene
            .pump_native(&pool, Tick(1), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        if view.scene.is_ready()
            && view.stats().resident_pages == 1
            && water_quads.is_none_or(|q| {
                view.water_stats()
                    .is_some_and(|s| s.ready && s.source.live_quads == q)
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "View water deadline: {:?} {:?}",
            view.stats(),
            view.water_stats()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn frame(gpu: &render::Gpu, view: &mut ClientView) -> Vec<u8> {
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual optional ViewWater image"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual optional ViewWater readback"),
        size: 512 * 64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.invalidate();
    view.encode(
        &gpu.queue,
        &mut encoder,
        &target.create_view(&Default::default()),
        96,
        64,
    )
    .unwrap();
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &output,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(512),
                rows_per_image: Some(64),
            },
        },
        wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    output
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let pixels = output
        .slice(..)
        .get_mapped_range()
        .chunks_exact(512)
        .flat_map(|row| row[..384].to_vec())
        .collect();
    output.unmap();
    pixels
}
#[test]
fn live_view_water_replaces_plain_water_and_never_owns_halo_or_old_window_pages() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let mut plain = view(&gpu, &atlas, false);
    let mut physical = view(&gpu, &atlas, true);
    settle(&gpu, &mut plain, None);
    settle(&gpu, &mut physical, Some(160));
    let baseline = frame(&gpu, &mut plain);
    let actual = frame(&gpu, &mut physical);
    let changed = baseline
        .chunks_exact(4)
        .zip(actual.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        changed > 100,
        "physical water was not present in the actual View image: {changed}"
    );
    assert!(physical.environment_stats().unwrap().gpu.water.draws > 0);
    assert_eq!(physical.stats().frame_pipeline_compilations, 0);
    assert_eq!(
        physical.water_stats().unwrap().source.live_quads,
        160,
        "halo contains six more water faces but owns no near representation"
    );
    assert_eq!(physical.scene.stats().generation_published, 0);
    physical.scene.set_center(point(24, 4, 8)).unwrap();
    import_requested(&mut physical);
    settle(&gpu, &mut physical, Some(6));
    frame(&gpu, &mut physical);
    assert_eq!(
        physical.water_stats().unwrap().source.live_quads,
        6,
        "old near pool must retire even while its World page remains a halo input"
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        physical.advance_input(0.).unwrap();
        physical
            .upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        assert!(physical.water_stats().unwrap().ready);
        assert_eq!(
            physical.water_stats().unwrap().work,
            client::water_source::WaterSourceWork::default()
        );
    }
    let allocations = scope.finish();
    assert_eq!(allocations.allocating_calls(), 0);
    assert_eq!(allocations.dealloc_calls, 0);
    println!(
        "VIEW_WATER_GPU_OK physical_changed_pixels={changed} near_quads160 halo_quads0 moved_near_quads6 stale_old_quads0 worker_only=true generation0 frame_pipeline0 static1000work0"
    );
}
#[test]
fn water_page_model_composes_quaternion_page_origin_and_render_rebase() {
    let mut world = world::World::new(rules::properties, 64);
    let space = SpaceId(7);
    let page = PageId {
        space,
        x: 1,
        y: 0,
        z: -1,
    };
    let half = std::f64::consts::FRAC_1_SQRT_2;
    let mut tx = world::Transaction::new(Tick(1), space);
    tx.create_space(SpaceTransform {
        translation: [100., 20., -30.],
        rotation: [0., half, 0., half],
    });
    tx.create_page(page, vec![Cell::AIR; 4096]);
    world.commit(tx).unwrap();
    let model = client::view::water_model::page_model(&world, page, [128., 16., -64.]).unwrap();
    let local = [1f64, 2., 3., 1.];
    let actual = std::array::from_fn::<_, 4, _>(|row| {
        (0..4)
            .map(|col| f64::from(model[col][row]) * local[col])
            .sum::<f64>()
    });
    for (a, b) in actual.into_iter().zip([-41., 6., 17., 1.]) {
        assert!(
            (a - b).abs() < 0.00001,
            "water quaternion/rebase {actual:?}"
        );
    }
}
