#![cfg(not(target_arch = "wasm32"))]
use client::{far_water_source::Limits, view::horizon::*};
use core_types::{Cell, PageId, SpaceId, Tick, Version};
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
use render::{
    terrain_gpu::{
        AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
    water_gpu::{WaterConfig, WaterGpu, WaterInputs, WaterState},
};
use std::sync::Arc;
const W: u32 = 96;
fn page(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1,
        y,
        z: 0,
    }
}
fn cell(x: i32, y: i32, z: i32) -> Cell {
    if y <= 3 || (z == 8 && (4..=11).contains(&x) && ((x == 4 || x == 11) && y <= 8 || y == 8)) {
        Cell::from_parts(rules::ids::STONE, 0)
    } else if y == 4 {
        Cell::from_parts(rules::ids::WATER, 0)
    } else {
        Cell::AIR
    }
}
fn geometry(y: i32) -> net::PublicGeometry {
    let mut world = world::World::new(rules::properties, 16);
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    let cells = (0..4096)
        .map(|i| cell(i % 16, y * 16 + i / 256, (i / 16) % 16))
        .collect::<Vec<_>>();
    tx.create_page(page(y), cells);
    world.commit(tx).unwrap();
    let data = server::lod::geometry_from_page(world.page(page(y)).unwrap(), Some(&[0x07e0; 256]));
    net::PublicGeometry {
        page: page(y),
        source_version: Version(42),
        view_version: 2,
        known: data.known,
        occupied: data.occupied,
        index_bits: data.index_bits,
        indices: data.indices,
        palette: data
            .palette
            .into_iter()
            .map(|s| net::PublicSurface {
                atlas_layer: s.atlas_layer,
                tint: s.tint,
                kind: match s.kind {
                    server::lod::PublicSurfaceKind::Opaque => net::PublicSurfaceKind::Opaque,
                    server::lod::PublicSurfaceKind::Cutout => net::PublicSurfaceKind::Cutout,
                    server::lod::PublicSurfaceKind::Water => net::PublicSurfaceKind::Water,
                },
            })
            .collect(),
    }
}
fn halo(y: i32, view: u64, unknown: bool) -> Arc<net::water::WaterHalo> {
    let cells = std::array::from_fn(|i| {
        let x = (i % 18) as i32 - 1;
        let z = ((i / 18) % 18) as i32 - 1;
        let local_y = (i / 324) as i32 - 1;
        if unknown && x == -1 && z == 0 && local_y == 4 {
            return 0;
        }
        match client::water::SurfaceCell::from_cell(cell(x, y * 16 + local_y, z)) {
            client::water::SurfaceCell::Unknown => 0,
            client::water::SurfaceCell::Air => 1,
            client::water::SurfaceCell::Solid => 2,
            client::water::SurfaceCell::Water { amount, falling } => {
                net::water::WaterCell::Water { amount, falling }
                    .encode()
                    .unwrap()
            }
        }
    });
    Arc::new(net::water::WaterHalo {
        page: page(y),
        geometry_source: Version(42),
        geometry_view: 2,
        water_view: view,
        column_source: 2,
        cells: Box::new(cells),
        tints: [0x07e0; 256],
    })
}
fn tile(key: HeightKey) -> Box<HeightTile> {
    let mut known = [255; 37];
    known[36] = 1;
    Box::new(HeightTile {
        key,
        source: 1,
        error: None,
        heights: [8; 289],
        known,
        atlas: [3; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: Replacement::None,
    })
}
fn finish_water(horizon: &mut ViewHorizon) {
    while let Some(task) = horizon.take_water_task() {
        let context = task.context();
        let request = encode_background_task(&task).unwrap();
        let result =
            decode_background_result(&execute_background_request(&request, || false).unwrap())
                .unwrap();
        horizon.complete_background(context, result).unwrap();
    }
}
fn texture(gpu: &render::Gpu) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("far water constant environment"),
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
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuckless_float_white(),
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
    let view = texture.create_view(&Default::default());
    (texture, view)
}
fn bytemuckless_float_white() -> &'static [u8] {
    &[0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63]
}
fn new_water(gpu: &render::Gpu, terrain: &TerrainGpu, sky: &wgpu::TextureView) -> WaterGpu {
    WaterGpu::new(
        &gpu.device,
        &gpu.queue,
        WaterConfig {
            max_objects: 32,
            max_quads: 8192,
            reflection_steps: 0,
            wave_amplitude: 0.,
            ..WaterConfig::default()
        },
        W,
        W,
        WaterInputs {
            color_msaa: terrain.scene_color_msaa_view().unwrap(),
            depth_msaa: terrain.scene_depth_msaa_view(),
            sky,
            transmittance: sky,
        },
        None,
    )
    .unwrap()
}
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    horizon: &mut ViewHorizon,
    water: &mut WaterGpu,
    mut direct: Option<&mut render::lod_gpu::LodProxyGpu>,
) -> Vec<u8> {
    let eye = [24.5, 15., 40.];
    let camera = Camera::perspective(eye, [24.5, 5., 8.], [0., 1., 0.], 1., 1., 0.1, 200.);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    horizon
        .prepare_camera_extent(&gpu.queue, &mut encoder, camera, eye, [W, W])
        .unwrap();
    if let Some(proxies) = &mut direct {
        proxies
            .prepare_frame_extent(
                &gpu.queue,
                &mut encoder,
                render::lod_gpu::ProxyCamera {
                    camera,
                    eye,
                    viewport_height: W,
                    orthographic_direction: None,
                },
                W,
            )
            .unwrap();
    }
    terrain.invalidate();
    terrain
        .encode_scene_with_extra(
            &gpu.queue,
            &mut encoder,
            &camera,
            CullingMode::Frustum,
            true,
            |pass| {
                if let Some(proxies) = &direct {
                    horizon.draw_heights(pass) + proxies.draw(pass)
                } else {
                    horizon.draw(pass)
                }
            },
        )
        .unwrap();
    water.invalidate();
    water
        .encode(
            &gpu.queue,
            &mut encoder,
            WaterState {
                camera,
                eye,
                time_seconds: 0.,
                sun: [0.35, 0.8, 0.2],
                sun_intensity: 1.,
                underwater: false,
                scene_revision: 1,
            },
            &mut UploadBudget::new(1_000_000),
            0,
        )
        .unwrap();
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("far-water exact HDR pixels"),
        size: u64::from(W * W * 8),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: water.resolved_texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 8),
                rows_per_image: Some(W),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: W,
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
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
fn assert_same(expected: &[u8], actual: &[u8], label: &str) {
    let differences = expected.iter().zip(actual).filter(|(a, b)| a != b).count();
    assert!(
        expected.len() == actual.len() && differences == 0,
        "{label}: expected {} bytes, actual {}, differing bytes {differences}, first {:?}",
        expected.len(),
        actual.len(),
        expected.iter().zip(actual).position(|(a, b)| a != b)
    );
}
#[test]
fn exact_column_publishes_height_opaque_and_certified_physical_water_as_one_gpu_cover() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            W,
            W,
            render::OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        terrain.prepare_scene_sampling(&gpu.device).unwrap();
        let (_sky, sky) = texture(&gpu);
        let mut water = new_water(&gpu, &terrain, &sky);
        let mut horizon = ViewHorizon::prepare(
            &gpu.device,
            &gpu.queue,
            &terrain,
            None,
            Config {
                max_tiles: 64,
                max_sources: 128,
                max_proxy_pages: 16,
                horizon_level: 0,
                ..Config::default()
            },
        )
        .unwrap();
        horizon
            .enable_separate_water(Limits {
                max_pages: 16,
                max_total_quads: 4096,
                ..Limits::default()
            })
            .unwrap();
        horizon.reserve_near_cutout(0).unwrap();
        horizon
            .guard_near(&gpu.queue, 0, [0, 0], &mut UploadBudget::new(1_000_000))
            .unwrap();
        assert!(!horizon.has_committed_front());
        let plan = horizon
            .plan_task(0, [0, 0])
            .unwrap()
            .unwrap()
            .execute()
            .unwrap();
        horizon.complete_plan(plan).unwrap();
        for key in horizon.wanted_height().collect::<Vec<_>>() {
            horizon.ingest_height(tile(key)).unwrap();
        }
        assert!(
            horizon
                .pump_with_water(
                    &gpu.queue,
                    &mut water,
                    &mut UploadBudget::new(1_000_000),
                    64,
                    16
                )
                .unwrap()
                .published
        );
        assert!(horizon.has_committed_front());
        let before = frame(&gpu, &mut terrain, &mut horizon, &mut water, None);
        let key = HeightKey {
            space: 0,
            level: 0,
            x: 1,
            z: 0,
        };
        let mut replacement = tile(key);
        replacement.source = 2;
        replacement.protected = [255; 32];
        replacement.replacement = Replacement::Column {
            min_y: 0,
            max_y: 7,
            geometry: (0..8)
                .map(|y| GeometryStamp {
                    page: page(y),
                    source: Version(42),
                    view: 2,
                })
                .collect(),
        };
        horizon.ingest_height(replacement).unwrap();
        for y in 0..8 {
            horizon.ingest_geometry(&geometry(y)).unwrap();
        }
        while let Some(task) = horizon.take_proxy_task() {
            let built = task.execute().unwrap();
            horizon.complete_proxy(built).map_err(|_| ()).unwrap();
        }
        for y in 0..8 {
            horizon.admit_water(halo(y, 1, y == 0)).unwrap();
        }
        finish_water(&mut horizon);
        let held = horizon
            .pump_with_water(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(1_000_000),
                64,
                16,
            )
            .unwrap();
        assert!(held.held_previous && !held.published);
        assert_eq!(horizon.water_stats().unwrap().unknown_held, 1);
        assert_same(
            &before,
            &frame(&gpu, &mut terrain, &mut horizon, &mut water, None),
            "held cover",
        );
        horizon.admit_water(halo(0, 2, false)).unwrap();
        finish_water(&mut horizon);
        let staged = horizon
            .stage_with_water(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(1_000_000),
                64,
                16,
            )
            .unwrap();
        assert!(staged.held_previous && !staged.published);
        assert!(
            horizon.has_committed_front(),
            "inactive replacement revoked the committed cover"
        );
        assert_same(
            &before,
            &frame(&gpu, &mut terrain, &mut horizon, &mut water, None),
            "held cover",
        );
        // Eight proxy slots reserve 32B, water 4B and Height two 4B entries.
        // The exact writes are smaller because empty proxy flags are unchanged.
        for bytes in [11, 43] {
            let denied = horizon
                .pump_with_water(
                    &gpu.queue,
                    &mut water,
                    &mut UploadBudget::new(bytes),
                    64,
                    16,
                )
                .unwrap();
            assert!(denied.held_previous && !denied.published);
            assert_eq!(denied.upload_bytes, 0);
            assert_same(
                &before,
                &frame(&gpu, &mut terrain, &mut horizon, &mut water, None),
                "held cover",
            );
        }
        let published = horizon
            .pump_with_water(&gpu.queue, &mut water, &mut UploadBudget::new(44), 64, 16)
            .unwrap();
        assert!(published.published && !published.held_previous);
        assert_eq!(published.upload_bytes, 12);
        let final_image = frame(&gpu, &mut terrain, &mut horizon, &mut water, None);
        assert!(
            before != final_image,
            "committed replacement must change the image"
        );
        let mut reference = new_water(&gpu, &terrain, &sky);
        let no_water = frame(&gpu, &mut terrain, &mut horizon, &mut reference, None);
        assert!(
            no_water != final_image,
            "opaque-only negative control hid missing water"
        );
        let halo = client::water::WaterHalo::from_fn(|x, y, z| {
            client::water::SurfaceCell::from_cell(cell(i32::from(x), i32::from(y), i32::from(z)))
        });
        let mesh = client::water::mesh_water(&halo, &[0xff00ff00; 256], 4096, || false).unwrap();
        assert_eq!(mesh.quads.len(), 254);
        let handle = reference
            .upload_object(
                &gpu.queue,
                None,
                translation([16., 0., 0.]),
                &mut UploadBudget::new(1_000_000),
            )
            .unwrap();
        reference
            .upload_page_quads(
                &gpu.queue,
                handle,
                &mesh.quads,
                &mut UploadBudget::new(1_000_000),
            )
            .unwrap();
        let mut direct = render::lod_gpu::LodProxyGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            render::lod_gpu::ProxyConfig {
                max_objects: 8,
                max_payload_words: 8192,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        for y in 0..8 {
            let source = render::lod::PublicPage::from_exposed(|[x, ly, z]| {
                if cell(i32::from(x), y * 16 + i32::from(ly), i32::from(z)).id()
                    == rules::ids::STONE
                {
                    render::lod::PublicVoxel::solid(
                        rules::blocks::layer_cell(rules::ids::STONE, 0, rules::blocks::mesh_face(1))
                            .0 as u16,
                        u32::MAX,
                    )
                } else {
                    render::lod::PublicVoxel::Air
                }
            });
            let proxy = render::lod::MaskedRayProxy::from_public_page(
                &source,
                render::lod::protection_groups(&source, 4),
            );
            let payload = render::lod_gpu::ProxyPayload::from_public(&proxy, 186).unwrap();
            direct
                .upload(
                    &gpu.queue,
                    None,
                    1,
                    translation([16., (y * 16) as f32, 0.]),
                    [0., 0., 0., 1.],
                    &payload,
                    &mut UploadBudget::new(1_000_000),
                )
                .unwrap();
        }
        assert_same(
            &final_image,
            &frame(
                &gpu,
                &mut terrain,
                &mut horizon,
                &mut reference,
                Some(&mut direct),
            ),
            "independent opaque and full water topology",
        );
        println!(
            "combined cover PASS: 11B/43B refuse without writes, 44B reservation publishes 12B, 8 opaque pages/254 water faces, independent HDR exact"
        );
    });
}

#[test]
fn horizon_settings_receipt_waits_for_complete_gpu_cover_and_keeps_previous_profile_during_staging()
{
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            W,
            W,
            render::OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        terrain.prepare_scene_sampling(&gpu.device).unwrap();
        let (_sky, sky) = texture(&gpu);
        let mut water = new_water(&gpu, &terrain, &sky);
        let mut horizon = ViewHorizon::prepare(
            &gpu.device,
            &gpu.queue,
            &terrain,
            None,
            Config {
                max_tiles: 64,
                max_sources: 128,
                max_proxy_pages: 16,
                horizon_level: 0,
                ..Config::default()
            },
        )
        .unwrap();
        horizon.reserve_near_cutout(0).unwrap();
        horizon
            .guard_near(&gpu.queue, 0, [0, 0], &mut UploadBudget::new(1_000_000))
            .unwrap();
        horizon
            .set_projection(Projection {
                eye_min: [24., 15., 40.],
                eye_max: [25., 16., 41.],
                focal_pixels: 100.,
                diagonal_tangent: 1.,
                target_pixels: 32.,
                hysteresis_pixels: 8.,
                view_direction: [0., 0., -1.],
                view_cosine: -1.,
            })
            .unwrap();
        for _ in 0..2 {
            if let Some(task) = horizon.plan_task(0, [0, 0]).unwrap() {
                horizon.complete_plan(task.execute().unwrap()).unwrap();
            }
            for key in horizon.wanted_height().collect::<Vec<_>>() {
                let mut image = tile(key);
                image.error = Some(net::height::HeightErrorBound {
                    max_error_q1024: 0,
                    min_height: 8,
                    max_height: 8,
                });
                horizon.ingest_height(image).unwrap();
            }
        }
        if let Some(task) = horizon.plan_task(0, [0, 0]).unwrap() {
            horizon.complete_plan(task.execute().unwrap()).unwrap();
        }
        horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap();
        assert!(horizon.horizon_selection_applied(2));
        assert!(horizon.error_selection_applied(32.));
        assert!(horizon.set_selection(3, 16.).unwrap());
        assert!(!horizon.horizon_selection_applied(3));
        assert!(!horizon.error_selection_applied(16.));
        assert!(horizon.horizon_selection_applied(2));
        for _ in 0..2 {
            if let Some(task) = horizon.plan_task(0, [0, 0]).unwrap() {
                horizon.complete_plan(task.execute().unwrap()).unwrap();
            }
            for key in horizon.wanted_height().collect::<Vec<_>>() {
                let mut image = tile(key);
                image.error = Some(net::height::HeightErrorBound {
                    max_error_q1024: 0,
                    min_height: 8,
                    max_height: 8,
                });
                horizon.ingest_height(image).unwrap();
            }
        }
        if let Some(task) = horizon.plan_task(0, [0, 0]).unwrap() {
            horizon.complete_plan(task.execute().unwrap()).unwrap();
        }
        let before = frame(&gpu, &mut terrain, &mut horizon, &mut water, None);
        horizon
            .stage_with_water(
                &gpu.queue,
                &mut water,
                &mut UploadBudget::new(1_000_000),
                64,
                16,
            )
            .unwrap();
        assert!(horizon.has_committed_front());
        assert!(horizon.horizon_selection_applied(2));
        assert!(!horizon.horizon_selection_applied(3));
        assert_same(
            &before,
            &frame(&gpu, &mut terrain, &mut horizon, &mut water, None),
            "settings inactive staging",
        );
        let held = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(0), 64, 16)
            .unwrap();
        assert!(held.held_previous && !held.published);
        assert!(horizon.horizon_selection_applied(2));
        let work = horizon
            .pump(&gpu.queue, &mut UploadBudget::new(1_000_000), 64, 16)
            .unwrap();
        assert!(work.published && !work.held_previous);
        assert!(horizon.horizon_selection_applied(3));
        assert!(horizon.error_selection_applied(16.));
        assert!(!horizon.horizon_selection_applied(2));
        assert!(!horizon.set_selection(3, 16.).unwrap());
        assert!(horizon.set_selection(0, 16.).is_err());
        assert!(horizon.set_selection(3, f64::NAN).is_err());
        assert!(horizon.horizon_selection_applied(3));
        println!(
            "settings receipt PASS: intermediate3-page target uses conservative dyadic cover; inactive+zero-budget holds preserve2-page receipt, atomic commit applies3/16px"
        );
    });
}
