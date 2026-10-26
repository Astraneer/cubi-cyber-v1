//! Default-profile shadow route through a voxel forest. Full redraw is the
//! independent publication control; receiver images remain in world space.
use super::signals;
use super::*;
use render::{
    entity_gpu::{EntityConfig, EntityGpu, EntityInstance, PRIMARY_HIDDEN},
    light_volume::LightVolumeGpu,
    shadows::ShadowWork,
    terrain_gpu::{Camera, CullingMode},
    terrain_mesh::MeshClass,
};
use signals::{LayerProbe, expected, footprint_edge, signal_error, temporal_residual};
fn rgba8(signal: &[[f32; 4]]) -> Vec<u8> {
    signal
        .iter()
        .flat_map(|p| {
            let v = (p[3].clamp(0., 1.) * 255.).round() as u8;
            [v, v, v, 255]
        })
        .collect()
}
fn forest(x: i32, y: i32, z: i32) -> u32 {
    if !(-16..32).contains(&x) || !(-16..32).contains(&z) || y < 0 {
        return 0;
    }
    if y == 0 {
        return 1;
    }
    // A four-block solid calibration column gives an independent deep-shadow
    // witness even at the coarsest0.5m shadow texel; foliage remains in the scene.
    if (10..14).contains(&x) && (10..14).contains(&z) && (1..5).contains(&y) {
        return 2;
    }
    for tz in [-10, -2, 6, 14, 22] {
        for tx in [-10, -2, 14, 22] {
            if x == tx && z == tz && (1..8).contains(&y) {
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
    }
    0
}
fn forest_mesh(px: i32, pz: i32) -> PageMesh {
    let mut halo = PageHalo::filled(0);
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                halo.set(
                    x,
                    y,
                    z,
                    forest(px * 16 + i32::from(x), i32::from(y), pz * 16 + i32::from(z)),
                );
            }
        }
    }
    let mut mesh = PageMesh::default();
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
            _ => CellMaterial {
                face_materials: [12; 6],
                tint: 0xff60_b27a,
                tint_faces: 0x3f,
                face_uv_rotations: [0; 6],
                class: MeshClass::Cutout,
                occludes: false,
                casts_ao: false,
                cull_group: 3,
            },
        },
        &mut MeshScratch::new(),
        &mut mesh,
    );
    mesh
}
fn actor(frame: u32) -> EntityInstance {
    let time = frame as f32 / 60.;
    EntityInstance {
        position: [
            time * 4.317 * 0.49613893,
            1.,
            -8. - time * 4.317 * 0.86824316,
        ],
        body_yaw: 90.,
        head_yaw: 90. + (time * 0.7).sin() * 15.,
        pitch: 0.,
        limb_phase: time * 20.,
        limb_amount: 0.55,
        kind: 5,
        flags: PRIMARY_HIDDEN,
        color: (time * 20.).to_bits(),
        id_low: 1,
        id_high: 0,
        death_roll: 0.,
        scale: 1.,
        reserved: 0.,
    }
}
fn encode_models(
    gpu: &render::Gpu,
    shadow: &mut ShadowGpu,
    models: &mut EntityGpu,
    timings: &wgpu::Buffer,
    offset: u64,
) -> ShadowWork {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = shadow
        .encode_dynamic(&gpu.queue, &mut encoder, models)
        .unwrap();
    if let Some(timing) = shadow.timing_source() {
        encoder.copy_buffer_to_buffer(timing.buffer, 0, timings, offset, 16);
    }
    gpu.queue.submit([encoder.finish()]);
    work
}
fn add(a: &mut ShadowWork, b: ShadowWork) {
    a.cascade_updates += b.cascade_updates;
    a.clear_pixels += b.clear_pixels;
    a.dynamic_restore_pixels += b.dynamic_restore_pixels;
    a.dynamic_copy_bytes += b.dynamic_copy_bytes;
    a.dynamic_draw_calls += b.dynamic_draw_calls;
    a.caster_dispatches += b.caster_dispatches;
    a.dynamic_cull_dispatches += b.dynamic_cull_dispatches;
    a.uniform_upload_bytes += b.uniform_upload_bytes;
}
fn cascade_for(shadow: &ShadowGpu, position: [f32; 3]) -> Option<usize> {
    let sampling = shadow.cache().sampling();
    (0..3).find(|&i| {
        if sampling.params[0] & (1 << i) == 0 {
            return false;
        }
        let p = std::array::from_fn::<_, 3, _>(|r| {
            sampling.desired_matrices[i][0][r] * position[0]
                + sampling.desired_matrices[i][1][r] * position[1]
                + sampling.desired_matrices[i][2][r] * position[2]
                + sampling.desired_matrices[i][3][r]
        });
        p[0].abs() <= 0.96 && p[1].abs() <= 0.96 && (0.0..=1.0).contains(&p[2])
    })
}
fn hits_box(p: [f32; 3], direction: [f32; 3], low: [f32; 3], high: [f32; 3]) -> bool {
    let mut near = 0f32;
    let mut far = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() < 0.000001 {
            if p[axis] < low[axis] || p[axis] > high[axis] {
                return false;
            }
        } else {
            let a = (low[axis] - p[axis]) / direction[axis];
            let b = (high[axis] - p[axis]) / direction[axis];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
    }
    far > near && far > 0.
}
fn independent_visibility(image: &[u8], sun: [f32; 3]) -> (usize, usize) {
    let mut dark = 0;
    let mut sky = 0;
    for y in 0..128 {
        for x in 0..128 {
            let p = [
                -4. + (x as f32 + 0.5) * 0.1875,
                1.01,
                -4. + (y as f32 + 0.5) * 0.1875,
            ];
            let witnesses = [[-0.8, -0.8], [-0.8, 0.8], [0.8, -0.8], [0.8, 0.8], [0., 0.]];
            let solid = witnesses.iter().all(|o| {
                hits_box(
                    [p[0] + o[0], p[1], p[2] + o[1]],
                    sun,
                    [10., 1., 10.],
                    [14., 5., 14.],
                )
            });
            let clear = witnesses.iter().all(|o| {
                let q = [p[0] + o[0], p[1], p[2] + o[1]];
                !hits_box(q, sun, [7., 1., 7.], [17., 5., 17.])
                    && [-10, -2, 6, 14, 22].iter().all(|&tz| {
                        [-10, -2, 14, 22].iter().all(|&tx| {
                            // Independent conservative tree geometry: a thin
                            // opaque trunk and a canopy hull only at its actual
                            // height. Filling the entire hull down to ground
                            // falsely removes every clear witness in this forest.
                            !hits_box(
                                q,
                                sun,
                                [(tx - 3) as f32, 1., (tz - 3) as f32],
                                [(tx + 4) as f32, 8., (tz + 4) as f32],
                            ) && !hits_box(
                                q,
                                sun,
                                [(tx - 5) as f32, 6., (tz - 5) as f32],
                                [(tx + 6) as f32, 10., (tz + 6) as f32],
                            )
                        })
                    })
            });
            let value = image[(x + y * 128) * 4];
            if solid {
                assert!(value <= 64, "solid ray witness{x}/{y} became sky{value}");
                dark += 1;
            }
            if clear {
                assert!(
                    value >= 191,
                    "clear ray witness{x}/{y} became shadow{value}"
                );
                sky += 1;
            }
        }
    }
    assert!(
        dark > 20 && sky > 100,
        "independent ray witnesses insufficient{dark}/{sky}"
    );
    (dark, sky)
}
fn difference(a: &[u8], b: &[u8]) -> f64 {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(a, b)| u64::from(a[0].abs_diff(b[0])))
        .sum::<u64>() as f64
        / (128. * 128. * 255.)
        * 1000.
}
#[test]
fn default_2048_forest_animated_route_reports_complete_shadow_cost_and_cascade_transitions() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            128,
            128,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 32,
                max_quads: 32768,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let config = ShadowConfig {
            resolution: 2048,
            dynamic_casters: true,
            ..ShadowConfig::default()
        };
        assert_eq!(config.half_widths, [32., 128., 512.]);
        assert_eq!(config.depth_half_extent, 1024.);
        let sun = [0.35, 0.8, 0.2];
        let initial = [0., 1., -8.];
        let mut cached = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            config,
            initial,
            sun,
        )
        .unwrap();
        let mut reference = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                max_updates_per_frame: 3,
                ..config
            },
            initial,
            sun,
        )
        .unwrap();
        let mut lights = LightVolumeGpu::new(&gpu.device, 32).unwrap();
        terrain
            .attach_lighting(&gpu.device, &cached, &lights)
            .unwrap();
        let mut geometry_budget = UploadBudget::new(8_000_000);
        let mut quads = 0;
        let mut cutout = 0;
        for pz in -1..=1 {
            for px in -1..=1 {
                let mesh = forest_mesh(px, pz);
                quads += mesh.quad_count();
                cutout += mesh.cutout.len();
                let handle = terrain
                    .upload_page(
                        &gpu.queue,
                        None,
                        1,
                        translation([px as f32 * 16., 0., pz as f32 * 16.]),
                        &mesh,
                        &mut geometry_budget,
                    )
                    .unwrap();
                lights
                    .upload_uniform(
                        &gpu.queue,
                        handle,
                        1,
                        PackedLight::new(15, 0, [0; 3]).unwrap(),
                        &mut geometry_budget,
                    )
                    .unwrap();
            }
        }
        assert!(cutout > 500, "fixture must contain real leaf cutout quads");
        let mut models = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 2,
                atlas_layers: 186,
            },
            Some(&cached),
        )
        .unwrap();
        let mut full_models = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 2,
                atlas_layers: 186,
            },
            Some(&reference),
        )
        .unwrap();
        let frames = std::env::var("CUBI_H5_ROUTE_FRAMES")
            .ok()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(2048);
        assert!((1..=4096).contains(&frames));
        let first_frame = std::env::var("CUBI_H5_ROUTE_DIAGNOSTIC_START")
            .ok()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(1);
        assert!((1..=frames).contains(&first_frame));
        if first_frame != 1 {
            eprintln!("DIAGNOSTIC SUBSET ONLY: frames{first_frame}..={frames}; not H5 acceptance");
        }
        let timings = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("full shadow route totals"),
            size: (u64::from(frames) + 8) * 32,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for shadow in [&mut cached, &mut reference] {
            let target = if shadow.cache().config().max_updates_per_frame == 1 {
                &mut models
            } else {
                &mut full_models
            };
            target
                .upload(&gpu.queue, &[actor(0)], &[], &mut UploadBudget::new(68))
                .unwrap();
            for _ in 0..3 {
                encode_models(&gpu, shadow, target, &timings, 0);
            }
        }
        // Exercise the real lit HDR/MSAA4/A2C color graph, not only a depth probe.
        let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("default forest A2C route"),
            size: wgpu::Extent3d {
                width: 128,
                height: 128,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let hdr = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual MSAA forest coverage"),
            size: 128 * 128 * 8,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = output.create_view(&Default::default());
        let camera = Camera::perspective(
            [18., 9., 30.],
            [8., 6., 8.],
            [0., 1., 0.],
            1.,
            1.,
            0.1,
            300.,
        );
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        models
            .prepare_frame(&gpu.queue, &mut encoder, camera)
            .unwrap();
        terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera,
                CullingMode::Frustum,
                |pass| models.draw(pass),
            )
            .unwrap();
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: terrain.resolved_hdr_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &hdr,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(128 * 8),
                    rows_per_image: Some(128),
                },
            },
            wgpu::Extent3d {
                width: 128,
                height: 128,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        let coverage = read_buffer(&gpu, &hdr)
            .chunks_exact(8)
            .filter(|p| {
                let alpha = u16::from_le_bytes([p[6], p[7]]);
                alpha > 0 && alpha < 0x3c00
            })
            .count();
        assert!(
            coverage > 20,
            "actual forest must exercise partial MSAA/A2C samples"
        );
        let probe = LayerProbe::new(&gpu, &cached);
        let full_probe = LayerProbe::new(&gpu, &reference);
        let base_layers = probe.capture(&gpu, 0);
        assert!(base_layers.iter().all(|p| p[..3].iter().all(|v| *v >= 0.)));
        let baseline = rgba8(&base_layers);
        let sample = cached.cache().sampling();
        let direction = [sample.sun[0], sample.sun[1], sample.sun[2]];
        let certified = independent_visibility(&baseline, direction);
        for layer in 0..3 {
            let layer_signal = base_layers
                .iter()
                .map(|p| [p[0], p[1], p[2], p[layer]])
                .collect::<Vec<_>>();
            independent_visibility(&rgba8(&layer_signal), direction);
        }
        assert!(signal_error(&base_layers, &full_probe.capture(&gpu, 0)) <= 0.02);
        let expected_initial = expected(&sample, &base_layers);
        let inverted = probe.capture(&gpu, 1);
        let alternate_error = temporal_residual(
            &inverted,
            &base_layers,
            &expected_initial,
            &expected_initial,
        );
        let normal_after = probe.capture(&gpu, 0);
        let reverse_error = temporal_residual(
            &normal_after,
            &inverted,
            &expected_initial,
            &expected_initial,
        );
        let hard_near = probe.capture(&gpu, 2);
        let hard_far = probe.capture(&gpu, 3);
        let abrupt_error =
            temporal_residual(&hard_far, &hard_near, &expected_initial, &expected_initial);
        assert!(
            alternate_error > 0.02 && reverse_error > 0.02 && abrupt_error > 0.02,
            "negative GPU signals escaped H5: alternating{alternate_error}/{reverse_error}, onejump{abrupt_error}"
        );
        let micro = probe.capture(&gpu, 4);
        let micro_error =
            temporal_residual(&micro, &base_layers, &expected_initial, &expected_initial);
        assert!(
            micro_error > 0.02,
            "0.1‰ one-frame float noise escaped H5: {micro_error}"
        );
        let shadowed = baseline.chunks_exact(4).filter(|p| p[0] < 64).count();
        let lit = baseline.chunks_exact(4).filter(|p| p[0] > 191).count();
        assert!(
            shadowed > 50 && lit > 500,
            "constant visibility cannot pass forest oracle {shadowed}/{lit}"
        );
        let mut work = ShadowWork::default();
        let mut full_work = ShadowWork::default();
        let mut max_temporal = 0f64;
        let mut cumulative_drift = 0f64;
        let mut sum_jump = 0f64;
        let mut max_full_jump = 0f64;
        let mut previous = baseline.clone();
        let mut previous_full = baseline.clone();
        let mut minima = baseline.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>();
        let mut maxima = minima.clone();
        let mut max_full_error = 0f64;
        let mut previous_signal = base_layers.clone();
        let mut previous_full_signal = base_layers.clone();
        let mut previous_expected = expected_initial;
        let mut max_expected_error = 0f64;
        let mut max_expected_residual = 0f64;
        let mut max_cache_temporal = 0f64;
        let mut max_single_jitter = 0f64;
        let mut max_sampled_layer_jitter = 0f64;
        let mut max_predicted_pixel_delta = 0f64;
        let mut previous_sampling = sample;
        let mut selected = 0u32;
        for frame in first_frame..=frames {
            let row = actor(frame);
            let center = row.position;
            models
                .upload(&gpu.queue, &[row], &[], &mut UploadBudget::new(68))
                .unwrap();
            full_models
                .upload(&gpu.queue, &[row], &[], &mut UploadBudget::new(68))
                .unwrap();
            cached.update_view(center, sun).unwrap();
            reference.update_view(center, sun).unwrap();
            reference.invalidate_all();
            add(
                &mut work,
                encode_models(
                    &gpu,
                    &mut cached,
                    &mut models,
                    &timings,
                    u64::from(frame) * 32,
                ),
            );
            add(
                &mut full_work,
                encode_models(
                    &gpu,
                    &mut reference,
                    &mut full_models,
                    &timings,
                    u64::from(frame) * 32 + 16,
                ),
            );
            if let Some(cascade) = cascade_for(&cached, [8., 1.01, 8.]) {
                selected |= 1 << cascade;
            }
            // Every completed frame, without temporal subsampling.
            let signal = probe.capture(&gpu, 0);
            let full_signal = full_probe.capture(&gpu, 0);
            let current_sampling = cached.cache().sampling();
            let expectation = expected(&current_sampling, &base_layers);
            for (now, old) in expectation.iter().zip(&previous_expected) {
                max_predicted_pixel_delta = max_predicted_pixel_delta.max((now - old).abs());
            }
            let expected_error = signal
                .iter()
                .zip(&expectation)
                .map(|(p, e)| (f64::from(p[3]) - e).abs())
                .sum::<f64>()
                / signal.len() as f64
                * 1000.;
            max_expected_error = max_expected_error.max(expected_error);
            max_expected_residual = max_expected_residual.max(temporal_residual(
                &signal,
                &previous_signal,
                &expectation,
                &previous_expected,
            ));
            let actual_full = full_signal
                .iter()
                .map(|p| f64::from(p[3]))
                .collect::<Vec<_>>();
            let old_full = previous_full_signal
                .iter()
                .map(|p| f64::from(p[3]))
                .collect::<Vec<_>>();
            max_cache_temporal = max_cache_temporal.max(temporal_residual(
                &signal,
                &previous_signal,
                &actual_full,
                &old_full,
            ));
            for layer in 0..3 {
                let mut delta = 0f64;
                let mut valid = 0;
                let mut largest = 0f32;
                let mut sampled_delta = 0f64;
                let mut sampled_valid = 0;
                for (pixel, (now, old)) in signal.iter().zip(&previous_signal).enumerate() {
                    if now[layer] >= 0. && old[layer] >= 0. {
                        largest = largest.max((now[layer] - old[layer]).abs());
                        delta += f64::from((now[layer] - old[layer]).abs());
                        valid += 1;
                        if footprint_edge(&current_sampling, layer, pixel) < 0.96
                            && footprint_edge(&previous_sampling, layer, pixel) < 0.96
                        {
                            sampled_delta += f64::from((now[layer] - old[layer]).abs());
                            sampled_valid += 1;
                        }
                    }
                }
                if sampled_valid > 0 {
                    max_sampled_layer_jitter =
                        max_sampled_layer_jitter.max(sampled_delta / sampled_valid as f64 * 1000.);
                }
                if valid > 0 {
                    let jitter = delta / valid as f64 * 1000.;
                    if jitter > max_single_jitter && jitter > 0.01 {
                        eprintln!(
                            "LAYER jitter frame{frame} layer{layer} valid{valid} mean{jitter:.9}‰ largest{largest:.9}"
                        );
                        for (pixel, (now, old)) in signal
                            .iter()
                            .zip(&previous_signal)
                            .enumerate()
                            .filter(|(_, (a, b))| {
                                a[layer] >= 0.
                                    && b[layer] >= 0.
                                    && (a[layer] - b[layer]).abs() > 0.01
                            })
                            .take(8)
                        {
                            let world = [
                                -4. + (pixel % 128) as f32 * 0.1875 + 0.09375,
                                1.01,
                                -4. + (pixel / 128) as f32 * 0.1875 + 0.09375,
                            ];
                            let m = cached.cache().sampling().desired_matrices[layer];
                            let p = std::array::from_fn::<_, 3, _>(|r| {
                                m[0][r] * world[0]
                                    + m[1][r] * world[1]
                                    + m[2][r] * world[2]
                                    + m[3][r]
                            });
                            eprintln!(
                                " jitter pixel{pixel} now{} old{} desiredEdge{}",
                                now[layer],
                                old[layer],
                                p[0].abs().max(p[1].abs())
                            );
                        }
                    }
                    max_single_jitter = max_single_jitter.max(jitter);
                }
            }
            max_full_error = max_full_error.max(signal_error(&signal, &full_signal));
            let image = rgba8(&signal);
            let oracle = rgba8(&full_signal);
            let jump = difference(&image, &previous);
            sum_jump += jump;
            max_temporal = max_temporal.max(jump);
            max_full_jump = max_full_jump.max(difference(&oracle, &previous_full));
            cumulative_drift = cumulative_drift.max(difference(&image, &baseline));
            for (i, pixel) in image.chunks_exact(4).enumerate() {
                minima[i] = minima[i].min(pixel[0]);
                maxima[i] = maxima[i].max(pixel[0]);
            }
            if frame % 64 == 0 || frame == frames {
                independent_visibility(&image, direction);
            }
            previous = image;
            previous_full = oracle;
            previous_signal = signal;
            previous_full_signal = full_signal;
            previous_expected = expectation;
            previous_sampling = current_sampling;
            if frame % 64 == 0 || frame == frames {
                eprintln!(
                    "H5route frame{frame}/{frames} cascades{selected} consecutive{max_temporal:.6}‰ INITIALdrift{cumulative_drift:.6}‰ fullError{max_full_error:.6}‰ layerJitter{max_single_jitter:.6} sampledLayer{max_sampled_layer_jitter:.6} expectedResidual{max_expected_residual:.6} cacheTemporal{max_cache_temporal:.6}"
                );
            }
        }
        let data = read_buffer(&gpu, &timings);
        let mut cached_ns = 0u64;
        let mut reference_ns = 0u64;
        let period = gpu.queue.get_timestamp_period() as f64;
        for frame in first_frame as usize..=frames as usize {
            let r = &data[frame * 32..frame * 32 + 32];
            let duration = |offset| {
                let a = u64::from_le_bytes(r[offset..offset + 8].try_into().unwrap());
                let b = u64::from_le_bytes(r[offset + 8..offset + 16].try_into().unwrap());
                assert!(b > a);
                ((b - a) as f64 * period).round() as u64
            };
            cached_ns += duration(0);
            reference_ns += duration(16);
        }
        let cached_texels = work.clear_pixels + work.dynamic_restore_pixels;
        let full_texels = full_work.clear_pixels + full_work.dynamic_restore_pixels;
        assert_eq!(work.dynamic_copy_bytes, 0);
        assert_eq!(full_work.dynamic_copy_bytes, 0);
        assert!(
            full_texels >= cached_texels * 2,
            "full work{full_texels} vs cached{cached_texels}"
        );
        if frames >= 2048 && first_frame == 1 {
            assert_eq!(selected, 7, "route did not exercise every cascade");
        }
        let mean_jump = sum_jump / f64::from(frames - first_frame + 1);
        let mean_amplitude = maxima
            .iter()
            .zip(&minima)
            .map(|(a, b)| u64::from(a - b))
            .sum::<u64>() as f64
            / (128. * 128. * 255.)
            * 1000.;
        let worst_amplitude = f64::from(
            maxima
                .iter()
                .zip(&minima)
                .map(|(a, b)| a - b)
                .max()
                .unwrap(),
        ) / 255.
            * 1000.;
        println!(
            "H5 defaultforest2048 frames{frames} quads{quads} cutout{cutout} fractionalCoverage{coverage} visibleSamples{shadowed}/{lit} independentRays{certified:?} cascadeMask{selected} workTexels{cached_texels}/{full_texels} totalGpuNs{cached_ns}/{reference_ns} gpuRatio{:.3} consecutiveWorst{max_temporal:.6} consecutiveMean{mean_jump:.6} fullConsecutiveWorst{max_full_jump:.6} INITIALdrift{cumulative_drift:.6} sequenceMeanAmplitude{mean_amplitude:.6} sequenceWorstAmplitude{worst_amplitude:.6} fullErrorPermille{max_full_error:.6} layerJitter{max_single_jitter:.6} expectedAbsError{max_expected_error:.6} expectedTemporalResidual{max_expected_residual:.6} cacheTemporalResidual{max_cache_temporal:.6} GPUnegativeAlternating{alternate_error:.3}/{reverse_error:.3} GPUnegativeSingleJump{abrupt_error:.3} GPUnegativeFloatNoise{micro_error:.6} sampledLayerJitter{max_sampled_layer_jitter:.6} maxPredictedPixelDelta{max_predicted_pixel_delta:.9} copies0; fullCostIncludesRestore+CasterCompute",
            reference_ns as f64 / cached_ns as f64
        );
        if first_frame != 1 {
            return;
        }
        assert!(
            reference_ns >= cached_ns * 2,
            "whole shadow GPU cost did not improve2x: {cached_ns}/{reference_ns}ns"
        );
        // Normative jitter excludes only the independently predicted smooth
        // LOD signal. Raw jumps/drift/amplitude remain reported above. Each layer
        // must be stable on its own; actual GPU one-frame/one-jump controls fail.
        assert!(
            max_full_error <= 0.02,
            "cache/full shadow route mismatch{max_full_error}‰"
        );
        assert!(
            max_expected_error <= 0.02,
            "independent smoothLOD signal mismatch{max_expected_error}‰"
        );
        assert!(
            max_expected_residual <= 0.02,
            "unpredicted temporal jitter{max_expected_residual}‰"
        );
        assert!(
            max_cache_temporal <= 0.02,
            "cache-added temporal jitter{max_cache_temporal}‰"
        );
        assert!(
            max_sampled_layer_jitter <= 0.02,
            "sampled single-cascade raw jitter{max_sampled_layer_jitter}‰ (unused-margin diagnostic{max_single_jitter}‰)"
        );
    });
}
