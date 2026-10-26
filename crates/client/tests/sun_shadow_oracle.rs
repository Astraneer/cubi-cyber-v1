#![cfg(not(target_arch = "wasm32"))]
//! Required continuous-Sun H5 acceptance, separate from fixed-Sun forest motion.
//! Historical coarse/lagging negatives remain reproducible through CUBI_SUN_* overrides.
use client::celestial_clock;
use core_types::Tick;
use render::{
    OutputTransfer,
    entity_gpu::{EntityConfig, EntityGpu, EntityInstance, PRIMARY_HIDDEN},
    shadows::{ShadowConfig, ShadowGpu, ShadowWork},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
#[path = "../../render/tests/common/h5_probe.rs"]
mod signals;
use signals::{LayerProbe, expected, signal_error, temporal_residual};
fn read_buffer(gpu: &render::Gpu, b: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = b.slice(..).get_mapped_range().to_vec();
    b.unmap();
    bytes
}
fn actor(frame: u32) -> EntityInstance {
    EntityInstance {
        position: [-8., 1., -8.],
        body_yaw: 90.,
        head_yaw: 90.,
        pitch: 0.,
        limb_phase: frame as f32 / 3.,
        limb_amount: 0.55,
        kind: 5,
        flags: PRIMARY_HIDDEN,
        color: 0,
        id_low: 1,
        id_high: 0,
        death_roll: 0.,
        scale: 1.,
        reserved: 0.,
    }
}
fn encode(
    gpu: &render::Gpu,
    shadow: &mut ShadowGpu,
    models: &mut EntityGpu,
    timing: &wgpu::Buffer,
    offset: u64,
    footprint: Option<(&wgpu::Buffer, u64)>,
) -> ShadowWork {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = shadow
        .encode_dynamic(&gpu.queue, &mut encoder, models)
        .unwrap();
    if let Some(t) = shadow.timing_source() {
        assert_eq!(t.count, 1);
        encoder.copy_buffer_to_buffer(t.buffer, 0, timing, offset, 16);
    }
    if let Some((target, offset)) = footprint {
        encoder.copy_buffer_to_buffer(
            shadow
                .footprint_metrics_buffer()
                .expect("sparse prototype enabled"),
            0,
            target,
            offset,
            768,
        );
    }
    gpu.queue.submit([encoder.finish()]);
    work
}
fn hits(p: [f64; 3], d: [f64; 3], lo: [f64; 3], hi: [f64; 3]) -> bool {
    let mut near = 0f64;
    let mut far = f64::INFINITY;
    for a in 0..3 {
        if d[a].abs() < 1e-12 {
            if p[a] < lo[a] || p[a] > hi[a] {
                return false;
            }
        } else {
            let x = (lo[a] - p[a]) / d[a];
            let y = (hi[a] - p[a]) / d[a];
            near = near.max(x.min(y));
            far = far.min(x.max(y));
        }
    }
    far > near && far > 0.
}
fn ray_oracle(image: &[[f32; 4]], sun: [f32; 3]) -> (usize, usize) {
    let d = sun.map(f64::from);
    let mut solid = 0;
    let mut sky = 0;
    for (y, row) in image.chunks_exact(128).enumerate() {
        for (x, pixel) in row.iter().enumerate() {
            let p = [
                -4. + (x as f64 + 0.5) * 0.1875,
                1.01,
                -4. + (y as f64 + 0.5) * 0.1875,
            ];
            let all_dark = [[-0.8, -0.8], [-0.8, 0.8], [0.8, -0.8], [0.8, 0.8], [0., 0.]]
                .into_iter()
                .all(|o| {
                    hits(
                        [p[0] + o[0], p[1], p[2] + o[1]],
                        d,
                        [10., 1., 10.],
                        [14., 5., 14.],
                    )
                });
            let clear = !hits(p, d, [8., 1., 8.], [16., 5., 16.]);
            if all_dark {
                assert!(
                    pixel[3] < 0.25,
                    "independent solid ray became sky{x}/{y}:{}",
                    pixel[3]
                );
                solid += 1;
            }
            if clear {
                assert!(
                    pixel[3] > 0.75,
                    "independent clear ray became dark{x}/{y}:{}",
                    pixel[3]
                );
                sky += 1;
            }
        }
    }
    assert!(solid > 20 && sky > 1000);
    (solid, sky)
}
#[test]
fn continuous_public_sun_cached_graph_matches_physical_reference_and_cost_gate() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let frames = std::env::var("CUBI_SUN_FRAMES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(192);
        assert!((3..=1024).contains(&frames));
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            32,
            32,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 4096,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mut halo = PageHalo::filled(0);
        for y in 1..5 {
            for z in 10..14 {
                for x in 10..14 {
                    halo.set(x, y, z, 1);
                }
            }
        }
        let mut mesh = PageMesh::default();
        mesh_page(
            &halo,
            |v| {
                if v == 0 {
                    CellMaterial::EMPTY
                } else {
                    CellMaterial::opaque(3)
                }
            },
            &mut MeshScratch::new(),
            &mut mesh,
        );
        terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                [
                    [1., 0., 0., 0.],
                    [0., 1., 0., 0.],
                    [0., 0., 1., 0.],
                    [0., 0., 0., 1.],
                ],
                &mesh,
                &mut UploadBudget::new(100000),
            )
            .unwrap();
        let mut failures = Vec::new();
        for start in [3000u64, 5190] {
            let first = celestial_clock::sample(Tick(start), 0., None).unwrap();
            let center = [8., 1., 8.];
            let config = ShadowConfig {
                resolution: 2048,
                sun_step_radians: std::env::var("CUBI_SUN_STEP")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.000001),
                max_updates_per_frame: std::env::var("CUBI_SUN_UPDATES")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(3),
                projected_bounds: !std::env::var("CUBI_SUN_SPARSE").is_ok_and(|v| v == "0"),
                dynamic_casters: true,
                ..ShadowConfig::default()
            };
            eprintln!(
                "SUN_PROFILE start{start} stepRadians{} updatesPerFrame{} sparse{}",
                config.sun_step_radians, config.max_updates_per_frame, config.projected_bounds
            );
            let mut cached = ShadowGpu::new(
                &gpu.device,
                &gpu.queue,
                terrain.geometry(),
                config,
                center,
                first.sun_direction,
            )
            .unwrap();
            let mut full = ShadowGpu::new(
                &gpu.device,
                &gpu.queue,
                terrain.geometry(),
                ShadowConfig {
                    max_updates_per_frame: 3,
                    projected_bounds: false,
                    ..config
                },
                center,
                first.sun_direction,
            )
            .unwrap();
            let mut physical = ShadowGpu::new(
                &gpu.device,
                &gpu.queue,
                terrain.geometry(),
                ShadowConfig {
                    max_updates_per_frame: 3,
                    sun_step_radians: 0.000001,
                    projected_bounds: false,
                    ..config
                },
                center,
                first.sun_direction,
            )
            .unwrap();
            let models = |s: &ShadowGpu| {
                EntityGpu::new(
                    &gpu.device,
                    &gpu.queue,
                    terrain.geometry(),
                    EntityConfig {
                        max_entities: 2,
                        atlas_layers: 186,
                    },
                    Some(s),
                )
                .unwrap()
            };
            let mut cm = models(&cached);
            let mut fm = models(&full);
            let mut pm = models(&physical);
            for m in [&mut cm, &mut fm, &mut pm] {
                m.upload(&gpu.queue, &[actor(0)], &[], &mut UploadBudget::new(68))
                    .unwrap();
            }
            let timings = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("continuousSun wholeShadowGraph"),
                size: (u64::from(frames) + 1) * 48,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let footprints = config.projected_bounds.then(|| {
                gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("completed continuousSun footprint counters"),
                    size: (u64::from(frames) + 1) * 768,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })
            });
            for _ in 0..3 {
                encode(&gpu, &mut cached, &mut cm, &timings, 0, None);
                encode(&gpu, &mut full, &mut fm, &timings, 16, None);
                encode(&gpu, &mut physical, &mut pm, &timings, 32, None);
            }
            let cp = LayerProbe::new(&gpu, &cached);
            let fp = LayerProbe::new(&gpu, &full);
            let pp = LayerProbe::new(&gpu, &physical);
            let mut previous = cp.capture(&gpu, 0);
            let mut previous_full = fp.capture(&gpu, 0);
            let mut previous_physical = pp.capture(&gpu, 0);
            let mut previous_expected = expected(&physical.cache().sampling(), &previous_physical);
            let mut cache_error = 0f64;
            let mut physical_error = 0f64;
            let mut cache_residual = 0f64;
            let mut physical_residual = 0f64;
            let mut physical_expected_error = 0f64;
            let mut raw_jump = 0f64;
            let mut static_updates = 0;
            let mut restore_pixels = 0;
            let mut copies = 0;
            let mut quantum_changes = 0;
            let mut prior_quantum = cached.cache().sampling().sun;
            for frame in 1..=frames {
                let clock = celestial_clock::sample(
                    Tick(start + u64::from(frame / 3)),
                    f64::from(frame % 3) / 3.,
                    None,
                )
                .unwrap();
                assert!(clock.public_tick.0 >= start && clock.sun_intensity == 1.);
                for m in [&mut cm, &mut fm, &mut pm] {
                    m.upload(&gpu.queue, &[actor(frame)], &[], &mut UploadBudget::new(68))
                        .unwrap();
                }
                cached.update_view(center, clock.sun_direction).unwrap();
                full.update_view(center, clock.sun_direction).unwrap();
                physical.update_view(center, clock.sun_direction).unwrap();
                full.invalidate_all();
                physical.invalidate_all();
                let work = encode(
                    &gpu,
                    &mut cached,
                    &mut cm,
                    &timings,
                    u64::from(frame) * 48,
                    footprints.as_ref().map(|b| (b, u64::from(frame) * 768)),
                );
                encode(
                    &gpu,
                    &mut full,
                    &mut fm,
                    &timings,
                    u64::from(frame) * 48 + 16,
                    None,
                );
                encode(
                    &gpu,
                    &mut physical,
                    &mut pm,
                    &timings,
                    u64::from(frame) * 48 + 32,
                    None,
                );
                static_updates += work.cascade_updates;
                restore_pixels += work.dynamic_restore_pixels;
                copies += work.dynamic_copy_bytes;
                let quantum = cached.cache().sampling().sun;
                if quantum != prior_quantum {
                    quantum_changes += 1;
                    prior_quantum = quantum;
                }
                let c = cp.capture(&gpu, 0);
                let f = fp.capture(&gpu, 0);
                let p = pp.capture(&gpu, 0);
                let e = expected(&physical.cache().sampling(), &p);
                ray_oracle(&p, clock.sun_direction);
                cache_error = cache_error.max(signal_error(&c, &f));
                physical_error = physical_error.max(signal_error(&c, &p));
                raw_jump = raw_jump.max(signal_error(&c, &previous));
                let fe = f.iter().map(|p| f64::from(p[3])).collect::<Vec<_>>();
                let old_fe = previous_full
                    .iter()
                    .map(|p| f64::from(p[3]))
                    .collect::<Vec<_>>();
                cache_residual = cache_residual.max(temporal_residual(&c, &previous, &fe, &old_fe));
                physical_residual =
                    physical_residual.max(temporal_residual(&c, &previous, &e, &previous_expected));
                physical_expected_error = physical_expected_error.max(
                    p.iter()
                        .zip(&e)
                        .map(|(p, e)| (f64::from(p[3]) - e).abs())
                        .sum::<f64>()
                        / p.len() as f64
                        * 1000.,
                );
                previous = c;
                previous_full = f;
                previous_physical = p;
                previous_expected = e;
            }
            assert!(
                celestial_clock::sample(Tick(start + u64::from(frames / 3)), 0., None)
                    .unwrap()
                    .sun_direction
                    != first.sun_direction
            );
            assert_eq!(copies, 0);
            assert!(
                quantum_changes > 0,
                "route did not change actual Sun quantum"
            );
            let _ = previous_physical;
            let mut clear_pixels = 0u64;
            if let Some(footprints) = &footprints {
                let bytes = read_buffer(&gpu, footprints);
                for frame in 1..=frames as usize {
                    for cascade in 0..3 {
                        let at = frame * 768 + cascade * 256 + 96;
                        clear_pixels +=
                            u64::from(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()));
                        restore_pixels += u64::from(u32::from_le_bytes(
                            bytes[at + 4..at + 8].try_into().unwrap(),
                        ));
                    }
                }
            }
            let bytes = read_buffer(&gpu, &timings);
            let mut sums = [0u64; 3];
            for frame in 1..=frames as usize {
                for (index, sum) in sums.iter_mut().enumerate() {
                    let at = frame * 48 + index * 16;
                    let a = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
                    let b = u64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap());
                    assert!(b > a);
                    *sum += ((b - a) as f64 * f64::from(gpu.queue.get_timestamp_period())).round()
                        as u64;
                }
            }
            println!(
                "H5_CANONICAL_SUN start{start} frames{frames} publicTicks{} interpolation3/displayTick sunQuantChanges{quantum_changes} staticUpdates{static_updates} GPUclearPixels{clear_pixels} restorePixels{restore_pixels} copies0 GPU_ns{}/{}/{} fullRatio{:0.3} rawConsecutive{raw_jump:0.6}‰ cacheFullAbs{cache_error:0.6}‰ cacheFullTemporal{cache_residual:0.6}‰ physicalAbs{physical_error:0.6}‰ physicalTemporal{physical_residual:0.6}‰ physicalF64BlendError{physical_expected_error:0.6}‰ descriptorMiB96perGraph",
                frames / 3,
                sums[0],
                sums[1],
                sums[2],
                sums[1] as f64 / sums[0] as f64
            );
            if cache_error > 0.02
                || cache_residual > 0.02
                || physical_error > 0.02
                || physical_residual > 0.02
                || physical_expected_error > 0.02
                || sums[1] < sums[0] * 2
            {
                failures.push((
                    start,
                    cache_error,
                    cache_residual,
                    physical_error,
                    physical_residual,
                    sums,
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "continuousSun H5 NOT ACCEPTED: {failures:?}"
        );
    });
}
