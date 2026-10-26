#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::{LodProxyGpu, ProxyCamera, ProxyConfig, ProxyError, ProxyPayload},
    terrain_gpu::{
        AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
};
fn proxy(page: &PublicPage) -> MaskedRayProxy {
    MaskedRayProxy::from_protected(page, 4)
}
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
fn camera() -> ProxyCamera {
    ProxyCamera {
        camera: Camera::orthographic([8., 8., 40.], [8., 8., 8.], [0., 1., 0.], 8., 1., 0.1, 100.),
        eye: [8., 8., 40.],
        viewport_height: 96,
        orthographic_direction: Some([0., 0., -1.]),
    }
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    data
}
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    lod: Option<&mut LodProxyGpu>,
    camera: ProxyCamera,
) -> (Vec<u8>, [u32; 4]) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("masked LOD actual HDR composite"),
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
    let view = texture.create_view(&Default::default());
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("LOD coverage image"),
        size: 512 * 96,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let stats = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual DDA work"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    if let Some(lod) = lod {
        lod.prepare_frame(&gpu.queue, &mut encoder, camera).unwrap();
        terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera.camera,
                CullingMode::Frustum,
                |pass| lod.draw(pass),
            )
            .unwrap();
        if let Some(counters) = lod.counter_buffer() {
            encoder.copy_buffer_to_buffer(counters, 0, &stats, 0, 16);
        }
    } else {
        terrain
            .encode_frame(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera.camera,
                CullingMode::Frustum,
            )
            .unwrap();
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
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
    let image = read(gpu, &pixels)
        .chunks_exact(512)
        .flat_map(|row| row[..96 * 4].iter().copied())
        .collect();
    let stats = read(gpu, &stats);
    (
        image,
        std::array::from_fn(|i| u32::from_le_bytes(stats[i * 4..i * 4 + 4].try_into().unwrap())),
    )
}
#[test]
fn palette_bitpacking_roundtrips_known_public_materials() {
    for colors in [1, 2, 3, 5, 17, 37, 180] {
        let page = PublicPage::from_exposed(|p| {
            let i = usize::from(p[0]) + 16 * (usize::from(p[2]) + 16 * usize::from(p[1]));
            if i % 7 == 0 {
                PublicVoxel::Air
            } else {
                PublicVoxel::Solid {
                    material: (i % colors) as u16,
                    tint: 0xff000000 | ((i % colors) as u32 * 1001),
                    kind: if i % 11 == 0 {
                        SurfaceKind::Cutout
                    } else {
                        SurfaceKind::Opaque
                    },
                }
            }
        });
        let source =
            MaskedRayProxy::from_public_page(&page, render::lod::protection_groups(&page, 2));
        let packed = ProxyPayload::from_public(&source, 186).unwrap();
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(packed.voxel([x, y, z]), page.get([x, y, z]));
                }
            }
        }
        assert!(packed.payload_bytes() < 4096 * 16);
    }
}
#[test]
fn unknown_water_and_invalid_material_do_not_become_rendered_air() {
    for (voxel, error) in [
        (PublicVoxel::Unknown, ProxyError::UnknownData),
        (
            PublicVoxel::Solid {
                material: 3,
                tint: u32::MAX,
                kind: SurfaceKind::Water,
            },
            ProxyError::UnsupportedWater,
        ),
        (
            PublicVoxel::solid(65000, u32::MAX),
            ProxyError::InvalidMaterial,
        ),
    ] {
        let page = PublicPage::filled(voxel);
        assert_eq!(
            ProxyPayload::from_public(&proxy(&page), 186).unwrap_err(),
            error
        );
    }
}
#[test]
fn masked_proxy_preserves_actual_window_coverage_with_msaa_and_reverse_depth() {
    masked_proxies_sample_mode(4);
}
#[test]
fn one_sample_masked_proxies_keeps_actual_pixels_and_publication_guards() {
    masked_proxies_sample_mode(1);
}
fn masked_proxies_sample_mode(sample_count: u32) {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain_samples(&gpu, sample_count);
        let mut page = PublicPage::filled(PublicVoxel::Air);
        for y in 2..14 {
            for x in 2..14 {
                if !(7..=8).contains(&x) || !(3..=7).contains(&y) {
                    page.set([x, y, 8], PublicVoxel::solid(3, u32::MAX));
                }
            }
        }
        let guarded = render::lod::coarsen_page(&page, 4, true);
        let source = MaskedRayProxy::from_public_page(&guarded.page, guarded.protection);
        let payload = ProxyPayload::from_public(&source, 186).unwrap();
        let mut lod = LodProxyGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ProxyConfig {
                max_objects: 4,
                max_payload_words: 65536,
                atlas_layers: 186,
                instrument: true,
            },
            None,
        )
        .unwrap();
        let mut budget = UploadBudget::new(1000000);
        let handle = lod
            .upload(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                [0., 0., 0., 1.],
                &payload,
                &mut budget,
            )
            .unwrap();
        let (sky, _) = frame(&gpu, &mut terrain, None, camera());
        let (actual, stats) = frame(&gpu, &mut terrain, Some(&mut lod), camera());
        let mut solid = 0;
        let mut air = 0;
        for y in 0..96 {
            for x in 0..96 {
                let cell_x = (x / 6) as u8;
                let cell_y = (15 - y / 6) as u8;
                let expected = (0..16).any(|z| guarded.page.get([cell_x, cell_y, z]).occupied());
                let offset = (x + y * 96) * 4;
                let covered = actual[offset..offset + 3] != sky[offset..offset + 3];
                assert_eq!(
                    covered, expected,
                    "pixel {x},{y} disagrees with independent axial occupancy"
                );
                if expected {
                    solid += 1;
                } else {
                    air += 1;
                }
            }
        }
        assert!(
            solid > 1000 && air > 100,
            "constant sky/solid must fail window coverage: {solid}/{air}"
        );
        assert!(stats[0] > 0 && stats[1] >= stats[0]);
        assert_eq!(stats[3], 0, "DDA exhausted fixed46stepbudget");
        let before = actual;
        assert_eq!(
            lod.upload(
                &gpu.queue,
                Some(handle),
                2,
                translation([1., 0., 0.]),
                [0., 0., 0., 1.],
                &payload,
                &mut UploadBudget::new(0)
            ),
            Err(ProxyError::UploadBudget)
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut lod), camera()).0,
            before
        );
        assert_eq!(
            lod.set_enabled_batch(&gpu.queue, &[(handle, false)], &mut UploadBudget::new(3)),
            Err(ProxyError::UploadBudget)
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut lod), camera()).0,
            before
        );
        assert_eq!(
            lod.set_enabled_batch(&gpu.queue, &[(handle, false)], &mut UploadBudget::new(4))
                .unwrap(),
            4
        );
        assert_eq!(frame(&gpu, &mut terrain, Some(&mut lod), camera()).0, sky);
        lod.set_enabled_batch(&gpu.queue, &[(handle, true)], &mut UploadBudget::new(4))
            .unwrap();
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut lod), camera()).0,
            before
        );
        let staged = lod
            .upload_inactive(
                &gpu.queue,
                2,
                translation([1., 0., 0.]),
                [0., 0., 0., 1.],
                &payload,
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut lod), camera()).0,
            before,
            "staged replacement leaked into current covering front"
        );
        let changes = [(handle, false), (staged, true)];
        assert_eq!(
            lod.set_enabled_batch(&gpu.queue, &changes, &mut UploadBudget::new(7)),
            Err(ProxyError::UploadBudget)
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut lod), camera()).0,
            before
        );
        assert_eq!(
            lod.set_enabled_batch(&gpu.queue, &changes, &mut UploadBudget::new(8))
                .unwrap(),
            8
        );
        assert_ne!(
            frame(&gpu, &mut terrain, Some(&mut lod), camera()).0,
            before,
            "atomic complete front switch did not move geometry"
        );
        println!(
            "H4GPUwindow sixProxyQuads=6,coveredPixels={solid},airPixels={air},rays={},steps={},hits={},budgetExits={},payloadBytes={}",
            stats[0],
            stats[1],
            stats[2],
            stats[3],
            payload.payload_bytes()
        );
    });
}

#[path = "support/lod_fixtures.rs"]
mod fixtures;
fn axial_camera(axis: usize, sign: f32) -> (ProxyCamera, [f32; 3], [f32; 3]) {
    let mut eye = [8.; 3];
    eye[axis] += sign * 40.;
    let mut direction = [0.; 3];
    direction[axis] = -sign;
    let up = if axis == 1 {
        [0., 0., 1.]
    } else {
        [0., 1., 0.]
    };
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let right = cross(direction, up);
    let up = cross(right, direction);
    (
        ProxyCamera {
            camera: Camera::orthographic(eye, [8.; 3], up, 8., 1., 0.1, 100.),
            eye,
            viewport_height: 96,
            orthographic_direction: Some(direction),
        },
        right,
        up,
    )
}
#[test]
fn h4_fifty_buildings_three_transitions_six_sides_preserve_gpu_openings() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut lod = LodProxyGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ProxyConfig {
                max_objects: 4,
                max_payload_words: 65536,
                atlas_layers: 186,
                instrument: true,
            },
            None,
        )
        .unwrap();
        let (sky, _) = frame(&gpu, &mut terrain, None, camera());
        let mut handle = None;
        let mut version = 0;
        let mut plain_quads = 0;
        let mut payload_bytes = 0;
        let mut rays = 0u64;
        let mut steps = 0u64;
        let mut hits = 0u64;
        let mut compared_pixels = 0;
        let mut air_witnesses = 0;
        let mut solid_witnesses = 0;
        let mut scratch = render::terrain_mesh::MeshScratch::new();
        let mut mesh = render::terrain_mesh::PageMesh::default();
        for index in 0..50 {
            let fixture = fixtures::fixture(index);
            for edge in [2, 4, 8] {
                let guarded = render::lod::coarsen_page(&fixture.page, edge, true);
                let source = MaskedRayProxy::from_public_page(&guarded.page, guarded.protection);
                let payload = ProxyPayload::from_public(&source, 186).unwrap();
                for witness in &fixture.air {
                    assert_eq!(
                        payload.voxel(*witness),
                        PublicVoxel::Air,
                        "{}edge{edge}lostair",
                        fixture.name
                    );
                    air_witnesses += 1;
                }
                for witness in &fixture.solid {
                    assert!(
                        payload.voxel(*witness).occupied(),
                        "{}edge{edge}lostsolid",
                        fixture.name
                    );
                    solid_witnesses += 1;
                }
                let plain = render::lod::coarsen_page(&fixture.page, edge, false);
                render::lod::mesh_public_page(
                    &plain.page,
                    PublicVoxel::Air,
                    &mut scratch,
                    &mut mesh,
                );
                plain_quads += mesh.quad_count();
                payload_bytes += payload.payload_bytes();
                version += 1;
                handle = Some(
                    lod.upload(
                        &gpu.queue,
                        handle,
                        version,
                        translation([0.; 3]),
                        [0., 0., 0., 1.],
                        &payload,
                        &mut UploadBudget::new(1000000),
                    )
                    .unwrap(),
                );
                for axis in 0..3 {
                    for sign in [-1., 1.] {
                        let (camera, right, up) = axial_camera(axis, sign);
                        let (actual, work) = frame(&gpu, &mut terrain, Some(&mut lod), camera);
                        rays += u64::from(work[0]);
                        steps += u64::from(work[1]);
                        hits += u64::from(work[2]);
                        assert_eq!(work[3], 0, "{}edge{edge}DDAexhausted", fixture.name);
                        for y in 0..96 {
                            for x in 0..96 {
                                let horizontal = (x as f32 + 0.5) / 6. - 8.;
                                let vertical = 8. - (y as f32 + 0.5) / 6.;
                                let mut cell = std::array::from_fn::<_, 3, _>(|a| {
                                    (8. + right[a] * horizontal + up[a] * vertical).floor() as u8
                                });
                                let expected = (0..16).any(|depth| {
                                    cell[axis] = depth;
                                    guarded.page.get(cell).occupied()
                                });
                                let offset = (x + y * 96) * 4;
                                let covered = actual[offset..offset + 3] != sky[offset..offset + 3];
                                assert_eq!(
                                    covered, expected,
                                    "{}edge{edge}axis{axis}sign{sign}pixel{x},{y}",
                                    fixture.name
                                );
                                compared_pixels += 1;
                            }
                        }
                    }
                }
            }
        }
        let proxy_quads = 50 * 3 * 6;
        assert!(proxy_quads as f64 <= plain_quads as f64 * 1.25);
        assert_eq!(compared_pixels, 50 * 3 * 6 * 96 * 96);
        assert_eq!(air_witnesses, 1035);
        assert_eq!(solid_witnesses, 450);
        println!(
            "H4_GPUfixtures=50 transitions=150 views=900 proxy_quads={proxy_quads} plain_quads={plain_quads} payload_bytes_sum={payload_bytes} compared_pixels={compared_pixels} air_witnesses={air_witnesses} solid_witnesses={solid_witnesses} rays={rays} steps={steps} hits={hits} budget_exits=0 lost_air=0 lost_solid=0"
        );
    });
}

#[test]
fn optional_atmosphere_lighting_tracks_day_and_dark_night_without_recompilation() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut terrain = terrain(&gpu);
    let mut budget = UploadBudget::new(1_000_000);
    let mut atmosphere = render::atmosphere::AtmosphereGpu::new(
        &gpu.device,
        &gpu.queue,
        render::atmosphere::AtmosphereConfig {
            transmittance_size: [32, 8],
            sky_size: [16, 8],
            transmittance_steps: 32,
            sky_steps: 16,
        },
        render::atmosphere::AtmosphereState {
            sun_direction: [0., -1., 0.],
            altitude_km: 0.072,
        },
        &mut budget,
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmosphere
        .encode_update(&mut encoder, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let mut consumer = LodProxyGpu::new_with_atmosphere(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        ProxyConfig {
            max_objects: 4,
            max_payload_words: 65536,
            atlas_layers: 186,
            instrument: true,
        },
        None,
        &atmosphere,
    )
    .unwrap();
    let mut source = PublicPage::filled(PublicVoxel::Air);
    for y in 2..14 {
        for x in 2..14 {
            source.set([x, y, 8], PublicVoxel::solid(3, u32::MAX));
        }
    }
    let payload = ProxyPayload::from_public(&proxy(&source), 186).unwrap();
    consumer
        .upload(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            [0., 0., 0., 1.],
            &payload,
            &mut budget,
        )
        .unwrap();
    consumer.take_work();
    let dark = frame(&gpu, &mut terrain, Some(&mut consumer), camera()).0;
    let dark_pixels = dark
        .chunks_exact(4)
        .filter(|p| p[3] == 255 && p[..3] == [0, 0, 0])
        .count();
    assert!(
        dark_pixels > 50,
        "night receiver must be dark: {dark_pixels}"
    );
    atmosphere
        .set_state(
            &gpu.queue,
            render::atmosphere::AtmosphereState::default(),
            &mut budget,
        )
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmosphere
        .encode_update(&mut encoder, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let day = frame(&gpu, &mut terrain, Some(&mut consumer), camera()).0;
    let lit_pixels = day
        .chunks_exact(4)
        .zip(dark.chunks_exact(4))
        .filter(|(a, b)| b[3] == 255 && b[..3] == [0, 0, 0] && a[..3].iter().any(|v| *v > 3))
        .count();
    assert!(
        lit_pixels > 50,
        "day receivers must react to shared atmosphere: {lit_pixels}"
    );
    assert_eq!(
        consumer.take_work().pipeline_compilations,
        0,
        "day change must reuse pipelines"
    );
    println!(
        "M10_lod_gpu_ATMOSPHERE_GPU_OK consumer=lod_gpu night_black={dark_pixels} day_lit={lit_pixels} runtime_pipeline_compilations=0"
    );
}
