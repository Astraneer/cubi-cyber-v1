#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    atmosphere::{AtmosphereConfig, AtmosphereGpu, AtmosphereState},
    gi::{GiConfig, GiError, GiGpu, GiVoxel, ProbeSample, RAYS},
    gi_directional::{GiSurface, SurfaceUpdate},
    shadows::{ShadowConfig, ShadowGpu},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget, translation},
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
fn read(gpu: &render::Gpu, source: &wgpu::Buffer) -> Vec<u8> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent directional GI oracle"),
        size: source.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(source, 0, &buffer, 0, source.size());
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let result = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    result
}
fn trace(gpu: &render::Gpu, gi: &mut GiGpu, tick: u64) -> ProbeSample {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = gi.trace_tick(
        &gpu.queue,
        &mut encoder,
        tick,
        &mut UploadBudget::new(16),
        &mut 70,
    );
    assert_eq!(work.rays, 70);
    assert_eq!(work.max_dda_steps, 4480);
    assert_eq!(work.max_shadow_samples, 512);
    gpu.queue.submit([encoder.finish()]);
    bytemuck::pod_read_unaligned(&read(gpu, gi.probe_buffer())[..128])
}
#[test]
fn all_six_first_hit_faces_match_independent_ray_box_sh_after_portable_dda_normal_selection() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        32,
        32,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 1,
            max_quads: 16,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    let shadows = ShadowGpu::new(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        ShadowConfig {
            resolution: 32,
            ..Default::default()
        },
        [8.5; 3],
        [0., 1., 0.],
    )
    .unwrap();
    let mut budget = UploadBudget::new(2 * 1024 * 1024);
    let atmosphere = AtmosphereGpu::new(
        &gpu.device,
        &gpu.queue,
        AtmosphereConfig {
            transmittance_size: [16, 8],
            sky_size: [16, 8],
            transmittance_steps: 8,
            sky_steps: 8,
        },
        AtmosphereState {
            sun_direction: [0., 1., 0.],
            altitude_km: 0.,
        },
        &mut budget,
    )
    .unwrap();
    let mut gi = GiGpu::new(
        &gpu.device,
        &gpu.queue,
        GiConfig {
            world_origin_page: [0; 3],
            world_pages: [1; 3],
            probe_origin: [8.5; 3],
            probe_counts: [1; 3],
            rays_per_probe: 64,
            max_jobs: 1,
            max_steps: 64,
            max_distance: 16.,
            ..Default::default()
        },
        &mut budget,
    )
    .unwrap();
    gi.prepare_directional(&gpu.device, &atmosphere, &shadows)
        .unwrap();
    let colors = [
        [15, 0, 0],
        [0, 15, 0],
        [0, 0, 15],
        [15, 15, 0],
        [15, 0, 15],
        [0, 15, 15],
    ];
    let mut face_source = GiSurface::EMPTY;
    for (face, rgb) in colors.into_iter().enumerate() {
        face_source = face_source.with_face(face as u8, [1.; 3], 0, rgb).unwrap();
    }
    let mut voxels = Box::new([GiVoxel::EMPTY; 4096]);
    let mut surfaces = Box::new([GiSurface::EMPTY; 4096]);
    for y in 7..=9 {
        for z in 7..=9 {
            for x in 7..=9 {
                if [x, y, z] == [8; 3] {
                    continue;
                }
                let index = x + 16 * (z + 16 * y);
                voxels[index] = GiVoxel::opaque_reflected([0.; 3]).unwrap();
                surfaces[index] = face_source;
            }
        }
    }
    gi.upload_page_directional(&gpu.queue, [0; 3], 1, &voxels, &surfaces, 0, &mut budget)
        .unwrap();
    let actual = trace(&gpu, &mut gi, 1);
    let mut expected = [[0f64; 4]; 3];
    let mut wrong_face = [[0f64; 4]; 3];
    let mut hits = [0u32; 6];
    // Independent intersection with the six planes of an axis-aligned box.
    for ray in RAYS {
        let d = ray.map(f64::from);
        let mut axis = 0;
        for candidate in 1..3 {
            if 0.5 / d[candidate].abs() < 0.5 / d[axis].abs() {
                axis = candidate;
            }
        }
        let face = match (axis, d[axis] > 0.) {
            (0, true) => 4,
            (0, false) => 5,
            (1, true) => 0,
            (1, false) => 1,
            (2, true) => 2,
            _ => 3,
        };
        hits[face] += 1;
        let basis = [
            0.2820947918,
            0.4886025119 * d[1],
            0.4886025119 * d[2],
            0.4886025119 * d[0],
        ];
        for channel in 0..3 {
            for (coefficient, value) in basis.into_iter().enumerate() {
                let convolution = if coefficient == 0 {
                    std::f64::consts::PI
                } else {
                    2. * std::f64::consts::PI / 3.
                };
                let weight = value * convolution * 4. * std::f64::consts::PI / 64.;
                expected[channel][coefficient] += f64::from(colors[face][channel]) / 15. * weight;
                wrong_face[channel][coefficient] +=
                    f64::from(colors[(face + 1) % 6][channel]) / 15. * weight;
            }
        }
    }
    let coefficients = [actual.sh_r, actual.sh_g, actual.sh_b];
    let mut error = 0f64;
    let mut negative = 0f64;
    for channel in 0..3 {
        for coefficient in 0..4 {
            error = error.max(
                (f64::from(coefficients[channel][coefficient]) - expected[channel][coefficient])
                    .abs(),
            );
            negative = negative
                .max((wrong_face[channel][coefficient] - expected[channel][coefficient]).abs());
        }
    }
    assert!(hits.iter().all(|&count| count > 0));
    assert!(error < 0.00002, "six-face f64 SH error {error}");
    assert!(
        negative > 0.1,
        "fixture must detect a wrong normal/face permutation"
    );
    eprintln!(
        "GI_DDA_SIX_FACE_GPU_OK ray_counts={hits:?} max_f64_SH_error={error} wrong_face_negative={negative}"
    );
}
// Independent flat finite-plane intersections and f64 SH projection. No DDA.
fn expected(source: [f64; 3]) -> [[f64; 4]; 3] {
    let mut result = [[0.; 4]; 3];
    for d in RAYS {
        if d[1] >= 0. {
            continue;
        }
        let t = -3. / f64::from(d[1]);
        let x = 8. + t * f64::from(d[0]);
        let z = 8. + t * f64::from(d[2]);
        if t >= 24. || !(4.0..12.0).contains(&x) || !(4.0..12.0).contains(&z) {
            continue;
        }
        let basis = [
            0.2820947918,
            0.4886025119 * f64::from(d[1]),
            0.4886025119 * f64::from(d[2]),
            0.4886025119 * f64::from(d[0]),
        ];
        for channel in 0..3 {
            for (coefficient, basis_value) in basis.iter().enumerate() {
                let convolution = if coefficient == 0 {
                    std::f64::consts::PI
                } else {
                    2. * std::f64::consts::PI / 3.
                };
                result[channel][coefficient] +=
                    source[channel] * basis_value * convolution * std::f64::consts::TAU * 2. / 64.;
            }
        }
    }
    result
}
fn error(actual: ProbeSample, expected: [[f64; 4]; 3]) -> f64 {
    [actual.sh_r, actual.sh_g, actual.sh_b]
        .into_iter()
        .zip(expected)
        .flat_map(|(a, b)| a.into_iter().zip(b).map(|(a, b)| (f64::from(a) - b).abs()))
        .fold(0., f64::max)
}
#[test]
fn actual_atmosphere_shadowed_floor_and_night_match_f64_without_source_reuploads() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            64,
            64,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 4,
                max_quads: 128,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mut cells = [0; 4096];
        for z in 0..16 {
            for x in 0..16 {
                cells[x + 16 * (z + 16 * 2)] = 1;
            }
        }
        let mut mesh = PageMesh::default();
        mesh_page(
            &PageHalo::from_page(&cells, 0),
            |c| {
                if c == 0 {
                    CellMaterial::EMPTY
                } else {
                    CellMaterial::opaque(1)
                }
            },
            &mut MeshScratch::new(),
            &mut mesh,
        );
        let mut budget = UploadBudget::new(16 * 1024 * 1024);
        let roof = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0., 16., 0.]),
                &mesh,
                &mut budget,
            )
            .unwrap();
        let mut shadows = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution: 128,
                half_widths: [16., 32., 64.],
                depth_half_extent: 32.,
                ..Default::default()
            },
            [8., 4., 8.],
            [0., 1., 0.],
        )
        .unwrap();
        let state = AtmosphereState {
            sun_direction: [0., 1., 0.],
            altitude_km: 0.,
        };
        let mut atmosphere = AtmosphereGpu::new(
            &gpu.device,
            &gpu.queue,
            AtmosphereConfig {
                transmittance_size: [16, 8],
                sky_size: [16, 8],
                transmittance_steps: 32,
                sky_steps: 32,
            },
            state,
            &mut budget,
        )
        .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        atmosphere
            .encode_update(&mut encoder, &mut { u64::MAX })
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        for _ in 0..3 {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            shadows.encode(&gpu.queue, &mut encoder);
            gpu.queue.submit([encoder.finish()]);
        }
        let light: Vec<f32> = read(&gpu, atmosphere.lighting_buffer())
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
            .collect();
        let sky = std::array::from_fn::<_, 3, _>(|channel| {
            f64::from(light[channel * 4]) * 0.2820947918
                + f64::from(light[channel * 4 + 1]) * 0.4886025119
        });
        let solar = std::array::from_fn::<_, 3, _>(|channel| f64::from(light[12 + channel]));
        let mut gi = GiGpu::new(
            &gpu.device,
            &gpu.queue,
            GiConfig {
                world_origin_page: [0; 3],
                world_pages: [1; 3],
                probe_origin: [8., 4., 8.],
                probe_counts: [1; 3],
                probe_spacing: 1.,
                rays_per_probe: 64,
                max_steps: 64,
                max_distance: 24.,
                max_jobs: 1,
                ..Default::default()
            },
            &mut budget,
        )
        .unwrap();
        gi.prepare_directional(&gpu.device, &atmosphere, &shadows)
            .unwrap();
        assert_eq!(gi.directional_source_bytes(), 4096 * 48);
        let mut world = [GiVoxel::EMPTY; 4096];
        let mut surfaces = Box::new([GiSurface::EMPTY; 4096]);
        let face = GiSurface::EMPTY.with_face(1, [0.5; 3], 15, [0; 3]).unwrap();
        for z in 4..12 {
            for x in 4..12 {
                world[x + 16 * z] = GiVoxel::opaque_reflected([0.; 3]).unwrap();
                surfaces[x + 16 * z] = face;
            }
        }
        assert_eq!(
            gi.upload_page_directional(
                &gpu.queue,
                [0; 3],
                1,
                &world,
                &surfaces,
                0,
                &mut UploadBudget::new(16480)
            ),
            Err(GiError::UploadBudget)
        );
        assert_eq!(gi.source_page_version([0; 3]).unwrap(), None);
        assert_eq!(
            gi.upload_page_directional(&gpu.queue, [0; 3], 1, &world, &surfaces, 0, &mut budget)
                .unwrap(),
            4096 * 52 + 96
        );
        let albedo = 128. / 255.;
        let shaded = trace(&gpu, &mut gi, 1);
        let shaded_error = error(shaded, expected(sky.map(|v| albedo * v)));
        assert!(
            shaded_error < 0.0001,
            "shadow-certified source error {shaded_error}"
        );
        terrain.release_page(&gpu.queue, roof, &mut budget).unwrap();
        shadows.invalidate_all();
        for _ in 0..3 {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            shadows.encode(&gpu.queue, &mut encoder);
            gpu.queue.submit([encoder.finish()]);
        }
        gi.invalidate_lighting(&gpu.queue, 2, &mut budget).unwrap();
        let opened = trace(&gpu, &mut gi, 2);
        let open_error = error(
            opened,
            expected(std::array::from_fn(|c| {
                albedo * (sky[c] + solar[c] / std::f64::consts::PI)
            })),
        );
        assert!(open_error < 0.0001, "sunlit source error {open_error}");
        assert!(opened.sh_r[0] > shaded.sh_r[0] * 2.);
        assert_eq!(gi.source_page_version([0; 3]).unwrap(), Some(1));
        gi.invalidate(3);
        let mut obsolete = gpu.device.create_command_encoder(&Default::default());
        assert_eq!(
            gi.trace_tick(
                &gpu.queue,
                &mut obsolete,
                3,
                &mut UploadBudget::new(16),
                &mut 70
            )
            .updated_probes,
            1
        );
        let old_revision = gi.source_revision();
        assert_eq!(
            gi.invalidate_lighting(&gpu.queue, 3, &mut UploadBudget::new(95)),
            Err(GiError::UploadBudget)
        );
        assert_eq!(gi.source_revision(), old_revision);
        assert_eq!(gi.pending(), 0);
        atmosphere
            .set_state_and_solar(&gpu.queue, state, 0., &mut budget)
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        atmosphere
            .encode_update(&mut encoder, &mut { u64::MAX })
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gi.invalidate_lighting(&gpu.queue, 3, &mut budget).unwrap();
        assert_eq!(gi.source_revision(), old_revision + 1);
        gpu.queue.submit([obsolete.finish()]);
        let counters = read(&gpu, gi.statistics_buffer());
        assert_eq!(
            u32::from_le_bytes(counters[12..16].try_into().unwrap()),
            0,
            "a job captured before the lighting revision must not publish"
        );
        let retained: ProbeSample =
            bytemuck::pod_read_unaligned(&read(&gpu, gi.probe_buffer())[..128]);
        assert_eq!(
            (retained.sh_r, retained.sh_g, retained.sh_b),
            (opened.sh_r, opened.sh_g, opened.sh_b)
        );
        let night = trace(&gpu, &mut gi, 3);
        assert_eq!([night.sh_r, night.sh_g, night.sh_b], [[0.; 4]; 3]);
        let updates = [render::gi::CellUpdate {
            local: 4 + 16 * 4,
            value: GiVoxel::EMPTY,
        }];
        let changes = [SurfaceUpdate {
            local: updates[0].local,
            value: GiSurface::EMPTY,
        }];
        assert_eq!(
            gi.upload_cells_directional(
                &gpu.queue,
                [0; 3],
                2,
                &updates,
                &changes,
                4,
                &mut UploadBudget::new(147)
            ),
            Err(GiError::UploadBudget)
        );
        assert_eq!(gi.source_page_version([0; 3]).unwrap(), Some(1));
        assert_eq!(
            gi.upload_cells_directional(
                &gpu.queue,
                [0; 3],
                2,
                &updates,
                &changes,
                4,
                &mut UploadBudget::new(148)
            )
            .unwrap(),
            148
        );
        println!(
            "M10 directional GI finite-floor maxError shadow={shaded_error} sun={open_error}; night0; source213088B once; sun/shadow0sourceBytes; changedOwner148B"
        );
    });
}
