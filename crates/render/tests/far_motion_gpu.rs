#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    far_motion::{Config, Error, FarMotionGpu},
    height_gpu::{HeightGpu, HeightGpuConfig},
    height_tile::{HeightTile, HeightTileKey},
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::{LodProxyGpu, ProxyCamera, ProxyConfig, ProxyPayload},
    model_motion::{MotionTargets, TargetConfig},
    taa::{self, PresentedHistory, ResetReasons, TaaFrame},
    terrain_gpu::{
        AtlasImage, AtlasMip, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
const W: u32 = 64;
const H: u32 = 64;
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Pixel {
    motion: [f32; 4],
    surface: [u32; 4],
}
fn capture(gpu: &render::Gpu, targets: &MotionTargets) -> Vec<Pixel> {
    let module=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:None,source:wgpu::ShaderSource::Wgsl("struct Pixel{motion:vec4<f32>,surface:vec4<u32>};@group(0)@binding(0)var m:texture_2d<f32>;@group(0)@binding(1)var s:texture_2d<u32>;@group(0)@binding(2)var<storage,read_write>o:array<Pixel>;@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){if p.x<64u&&p.y<64u{o[p.x+64u*p.y]=Pixel(textureLoad(m,vec2<i32>(p.xy),0),textureLoad(s,vec2<i32>(p.xy),0));}}".into())});
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &module,
            entry_point: Some("read"),
            compilation_options: Default::default(),
            cache: None,
        });
    let buffer = |usage| {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(W * H) * 32,
            usage,
            mapped_at_creation: false,
        })
    };
    let out = buffer(wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
    let read = buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(targets.motion_view()),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(targets.surface_view()),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: out.as_entire_binding(),
            },
        ],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(8, 8, 1);
    }
    encoder.copy_buffer_to_buffer(&out, 0, &read, 0, out.size());
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let values = bytemuck::cast_slice::<u8, Pixel>(&read.slice(..).get_mapped_range()).to_vec();
    read.unmap();
    values
}
fn camera(origin: f32) -> Camera {
    Camera::orthographic(
        [-origin, 40., 48.],
        [-origin, 8., 8.],
        [0., 1., 0.],
        24.,
        1.,
        1.,
        200.,
    )
}
fn tile() -> HeightTile {
    let mut known = [255; 37];
    known[36] = 1;
    HeightTile {
        key: HeightTileKey {
            space: 0,
            level: 0,
            x: -1,
            z: 0,
        },
        source: 1,
        heights: [8; 289],
        known,
        atlas: [0; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
    }
}
fn payload(kind: SurfaceKind, material: u16) -> ProxyPayload {
    let page = PublicPage::filled(PublicVoxel::Solid {
        material,
        tint: u32::MAX,
        kind,
    });
    ProxyPayload::from_public(&MaskedRayProxy::from_protected(&page, 4), 2).unwrap()
}
#[derive(Clone, Copy)]
struct Scene {
    origin: f32,
    object: f32,
    generation: u32,
    reactive: bool,
    cutout: bool,
}
#[allow(clippy::too_many_arguments)]
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    height: &mut HeightGpu,
    proxy: &mut LodProxyGpu,
    motion: &mut FarMotionGpu,
    targets: &MotionTargets,
    history: &mut PresentedHistory,
    n: u64,
    scene: Scene,
) -> (taa::FrameToken, Vec<Pixel>) {
    let camera = camera(scene.origin);
    let token = history
        .begin(TaaFrame {
            frame: n,
            space: 0,
            scene_revision: n,
            geometry_epoch: 1,
            source_epoch: 1,
            jitter_pixels: [0.; 2],
            reset: ResetReasons::NONE,
        })
        .unwrap();
    let required = motion.pending_upload_bytes(height, proxy).unwrap();
    let mut denied = UploadBudget::new(required - 1);
    assert_eq!(
        motion.prepare_frame(
            &gpu.queue,
            token,
            &camera,
            [W, H],
            [0.; 2],
            height,
            proxy,
            &mut denied
        ),
        Err(Error::UploadBudget)
    );
    assert_eq!(denied.remaining_bytes, required - 1);
    assert!(motion.pending().is_none());
    let work = motion
        .prepare_frame(
            &gpu.queue,
            token,
            &camera,
            [W, H],
            [0.; 2],
            height,
            proxy,
            &mut UploadBudget::new(required),
        )
        .unwrap();
    assert_eq!(work.upload_bytes, required);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    height
        .prepare_frame(&gpu.queue, &mut encoder, camera)
        .unwrap();
    proxy
        .prepare_frame_extent(
            &gpu.queue,
            &mut encoder,
            ProxyCamera {
                camera,
                eye: [-scene.origin, 40., 48.],
                viewport_height: H,
                orthographic_direction: Some([0., -32., -40.]),
            },
            W,
        )
        .unwrap();
    terrain.invalidate();
    terrain
        .encode_scene_with_extra(
            &gpu.queue,
            &mut encoder,
            &camera,
            CullingMode::Frustum,
            false,
            |pass| height.draw(pass) + proxy.draw(pass),
        )
        .unwrap();
    assert_eq!(
        targets
            .encode(&mut encoder, terrain.scene_depth_msaa_view(), |pass| {
                render::model_motion::Work {
                    draws: motion.draw(pass).draws,
                    ..Default::default()
                }
            })
            .draws,
        4
    );
    let snapshot = motion.encode_snapshot(&mut encoder, token).unwrap();
    assert_eq!(snapshot.snapshot_records, 8);
    assert_eq!(snapshot.snapshot_write_bytes, 640);
    motion.validate_commit(token).unwrap();
    history.mark_encoded(token).unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    (token, capture(gpu, targets))
}
fn inverse(camera: Camera) -> [[f64; 4]; 4] {
    let mut a: [[f64; 8]; 4] = std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            if column < 4 {
                f64::from(camera.view_projection[column][row])
            } else {
                f64::from(column - 4 == row)
            }
        })
    });
    for axis in 0..4 {
        let pivot = (axis..4)
            .max_by(|&left, &right| a[left][axis].abs().total_cmp(&a[right][axis].abs()))
            .unwrap();
        a.swap(axis, pivot);
        let divisor = a[axis][axis];
        for value in &mut a[axis] {
            *value /= divisor;
        }
        let row = a[axis];
        for (target, values) in a.iter_mut().enumerate() {
            if target == axis {
                continue;
            }
            let factor = values[axis];
            for (column, value) in values.iter_mut().enumerate() {
                *value -= factor * row[column];
            }
        }
    }
    std::array::from_fn(|row| std::array::from_fn(|column| a[row][column + 4]))
}
fn point(matrix: [[f64; 4]; 4], value: [f64; 4]) -> [f64; 3] {
    let p: [f64; 4] = std::array::from_fn(|row| {
        (0..4)
            .map(|column| matrix[row][column] * value[column])
            .sum()
    });
    std::array::from_fn(|axis| p[axis] / p[3])
}
fn ulp(value: f64) -> f64 {
    if value.abs() < 2f64.powi(-14) {
        2f64.powi(-24)
    } else {
        2f64.powi(value.abs().log2().floor() as i32 - 10)
    }
}
fn inspect(image: &[Pixel], current: Scene, previous: Option<Scene>) -> [usize; 2] {
    let inv = inverse(camera(current.origin));
    let mut counts = [0; 2];
    for y in 0..H {
        for x in 0..W {
            let uv = [
                (f64::from(x) + 0.5) / f64::from(W),
                (f64::from(y) + 0.5) / f64::from(H),
            ];
            let near = point(inv, [uv[0] * 2. - 1., 1. - uv[1] * 2., 1., 1.]);
            let far = point(inv, [uv[0] * 2. - 1., 1. - uv[1] * 2., 0., 1.]);
            let direction: [f64; 3] = std::array::from_fn(|axis| far[axis] - near[axis]);
            let origin = f64::from(current.origin);
            let object = f64::from(current.object) - origin;
            let mut hit: Option<(f64, usize, u32, [f64; 3])> = None;
            let t = (8. - near[1]) / direction[1];
            let p: [f64; 3] = std::array::from_fn(|axis| near[axis] + direction[axis] * t);
            if t >= 0. && (-16. - origin..-origin).contains(&p[0]) && (0.0..16.0).contains(&p[2]) {
                hit = Some((t, 0, 1, p));
            }
            let low = [object, 0., 0.];
            let high = [object + 16., 16., 16.];
            let mut enter = -1e30f64;
            let mut exit = 1e30f64;
            let mut face = 0;
            for axis in 0..3 {
                if direction[axis].abs() < 1e-12 {
                    if near[axis] < low[axis] || near[axis] > high[axis] {
                        exit = -1e30;
                    }
                } else {
                    let a = (low[axis] - near[axis]) / direction[axis];
                    let b = (high[axis] - near[axis]) / direction[axis];
                    if a.min(b) > enter {
                        enter = a.min(b);
                        face = match (axis, direction[axis] > 0.) {
                            (0, true) => 4,
                            (0, false) => 5,
                            (1, true) => 0,
                            (1, false) => 1,
                            (2, true) => 2,
                            _ => 3,
                        };
                    }
                    exit = exit.min(a.max(b));
                }
            }
            if enter >= 0. && exit >= enter && hit.is_none_or(|old| enter < old.0) {
                hit = Some((
                    enter,
                    1,
                    face,
                    std::array::from_fn(|axis| near[axis] + direction[axis] * enter),
                ));
            }
            let Some((_, family, face, p)) = hit else {
                continue;
            };
            let local = if family == 0 {
                [p[0] + 16. + origin, p[1], p[2]]
            } else {
                [p[0] - object, p[1], p[2]]
            };
            // Select independent strict interior witnesses, avoiding a grid/triangle
            // tie that has no unique f64 cell at the rasterization roundoff boundary.
            let axes = if family == 0 {
                [0, 2]
            } else {
                match face {
                    0 | 1 => [0, 2],
                    2 | 3 => [0, 1],
                    _ => [1, 2],
                }
            };
            if axes.into_iter().any(|axis| {
                let f = local[axis].fract();
                !(0.02..0.98).contains(&f)
            }) {
                continue;
            }
            let pixel = image[(x + W * y) as usize];
            counts[family] += 1;
            if family == 1 && current.reactive {
                assert_eq!(pixel.surface, [0; 4]);
                assert_eq!(pixel.motion, [0.; 4]);
                continue;
            }
            let part = if family == 0 {
                let fx = local[0].fract();
                let fz = local[2].fract();
                if (fx + fz - 1.).abs() < 0.02 {
                    counts[family] -= 1;
                    continue;
                }
                let cell = local[0].floor() as u32 + 16 * local[2].floor() as u32;
                cell * 2 + u32::from(fx + fz > 1.)
            } else {
                let normal = match face {
                    0 => [0., -1., 0.],
                    1 => [0., 1., 0.],
                    2 => [0., 0., -1.],
                    3 => [0., 0., 1.],
                    4 => [-1., 0., 0.],
                    _ => [1., 0., 0.],
                };
                let c: [u32; 3] = std::array::from_fn(|axis| {
                    (local[axis] - normal[axis] * 0.0001).floor().clamp(0., 15.) as u32
                });
                (c[0] + 16 * (c[2] + 16 * c[1])) * 8 + face
            };
            assert_eq!(
                pixel.surface,
                [
                    (if family == 0 {
                        16
                    } else if current.cutout {
                        18
                    } else {
                        17
                    }) << 24
                        | part,
                    0,
                    0,
                    if family == 0 { 1 } else { current.generation }
                ],
                "surface x{x}y{y} family{family}"
            );
            let Some(old) = previous else {
                assert_eq!(pixel.motion, [0.; 4]);
                continue;
            };
            if family == 1 && old.generation != current.generation {
                assert_eq!(pixel.motion, [0.; 4]);
                continue;
            }
            let previous_world = if family == 0 {
                [local[0] - 16. - f64::from(old.origin), 8., local[2], 1.]
            } else {
                [
                    local[0] + f64::from(old.object - old.origin),
                    local[1],
                    local[2],
                    1.,
                ]
            };
            let vp = camera(old.origin).view_projection;
            let clip: [f64; 4] = std::array::from_fn(|axis| {
                (0..4)
                    .map(|column| f64::from(vp[column][axis]) * previous_world[column])
                    .sum()
            });
            let expected = [
                clip[0] / clip[3] * 0.5 + 0.5 - uv[0],
                0.5 - clip[1] / clip[3] * 0.5 - uv[1],
                clip[2] / clip[3],
            ];
            assert_eq!(pixel.motion[3], 1.);
            for (axis, expected) in expected.into_iter().enumerate() {
                assert!(
                    (f64::from(pixel.motion[axis]) - expected).abs()
                        <= ulp(expected) + 32. * f64::from(f32::EPSILON),
                    "motion x{x}y{y} family{family} axis{axis}:{} vs{expected}",
                    pixel.motion[axis]
                );
            }
        }
    }
    assert!(
        counts.iter().all(|count| *count > 100),
        "insufficient finite witnesses{counts:?}"
    );
    counts
}
#[test]
fn exact_public_motion_preserves_presented_rebase_generations_and_reactive_cutout() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 2,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![255, 255, 255, 255, 255, 255, 255, 128],
            tint_mask: vec![255; 2],
        }],
    };
    let mut terrain = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 4,
            max_quads: 1024,
            occlusion: false,
        },
        &atlas,
        1,
    )
    .unwrap();
    terrain.prepare_scene_sampling(&gpu.device).unwrap();
    let mut height = HeightGpu::new(
        &gpu.device,
        terrain.geometry(),
        HeightGpuConfig {
            max_tiles: 4,
            atlas_layers: 2,
        },
        None,
    )
    .unwrap();
    let mut proxy = LodProxyGpu::new(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        ProxyConfig {
            max_objects: 4,
            max_payload_words: 65536,
            atlas_layers: 2,
            instrument: false,
        },
        None,
    )
    .unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    assert!(
        FarMotionGpu::new(&gpu.device, &gpu.queue, &height, &proxy, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
    assert_eq!(height.prepare_temporal_raster(&gpu.device).unwrap(), 2);
    assert_eq!(proxy.prepare_temporal_raster(&gpu.device).unwrap(), 2);
    assert_eq!(height.prepare_temporal_raster(&gpu.device).unwrap(), 0);
    assert_eq!(proxy.prepare_temporal_raster(&gpu.device).unwrap(), 0);
    let config = Config {
        memory_budget_bytes: 1 << 20,
        opaque_height_layers: [1, 0, 0, 0, 0, 0, 0, 0],
    };
    let mut motion = FarMotionGpu::new(&gpu.device, &gpu.queue, &height, &proxy, Some(config))
        .unwrap()
        .unwrap();
    let targets = MotionTargets::new(
        &gpu.device,
        &gpu.queue,
        Some(TargetConfig {
            extent: [W, H],
            samples: 1,
            max_pixels: u64::from(W * H),
            memory_budget_bytes: 1 << 20,
        }),
    )
    .unwrap()
    .unwrap();
    let mut budget = UploadBudget::new(1 << 20);
    height
        .upload(&gpu.queue, None, &tile(), 0, [0; 3], true, &mut budget)
        .unwrap();
    let mut handle = proxy
        .upload(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            [0.; 4],
            &payload(SurfaceKind::Opaque, 0),
            &mut budget,
        )
        .unwrap();
    let mut history = PresentedHistory::default();
    let a = Scene {
        origin: 0.,
        object: 0.,
        generation: handle.generation,
        reactive: false,
        cutout: false,
    };
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut height,
        &mut proxy,
        &mut motion,
        &targets,
        &mut history,
        1,
        a,
    );
    let first = inspect(&image, a, None);
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    height.rebase(&gpu.queue, [16, 0, 0], &mut budget).unwrap();
    proxy
        .set_models_batch(
            &gpu.queue,
            &[(handle, translation([-16., 0., 0.]))],
            &mut budget,
        )
        .unwrap();
    let b = Scene { origin: 16., ..a };
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut height,
        &mut proxy,
        &mut motion,
        &targets,
        &mut history,
        2,
        b,
    );
    inspect(&image, b, Some(a));
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    proxy
        .set_models_batch(
            &gpu.queue,
            &[(handle, translation([-13., 0., 0.]))],
            &mut budget,
        )
        .unwrap();
    let c = Scene { object: 3., ..b };
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut height,
        &mut proxy,
        &mut motion,
        &targets,
        &mut history,
        3,
        c,
    );
    inspect(&image, c, Some(b));
    motion.discard(t).unwrap();
    history.discard(t).unwrap();
    assert_eq!(motion.presented().unwrap().frame(), 2);
    proxy
        .set_models_batch(
            &gpu.queue,
            &[(handle, translation([-15., 0., 0.]))],
            &mut budget,
        )
        .unwrap();
    let d = Scene { object: 1., ..b };
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut height,
        &mut proxy,
        &mut motion,
        &targets,
        &mut history,
        4,
        d,
    );
    let moved = inspect(&image, d, Some(b));
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    proxy.release(&gpu.queue, handle, &mut budget).unwrap();
    handle = proxy
        .upload(
            &gpu.queue,
            None,
            2,
            translation([-15., 0., 0.]),
            [0.; 4],
            &payload(SurfaceKind::Cutout, 0),
            &mut budget,
        )
        .unwrap();
    let e = Scene {
        generation: handle.generation,
        cutout: true,
        ..d
    };
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut height,
        &mut proxy,
        &mut motion,
        &targets,
        &mut history,
        5,
        e,
    );
    inspect(&image, e, Some(d));
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    proxy
        .upload(
            &gpu.queue,
            Some(handle),
            3,
            translation([-15., 0., 0.]),
            [0.; 4],
            &payload(SurfaceKind::Cutout, 1),
            &mut budget,
        )
        .unwrap();
    let f = Scene {
        reactive: true,
        ..e
    };
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut height,
        &mut proxy,
        &mut motion,
        &targets,
        &mut history,
        6,
        f,
    );
    inspect(&image, f, Some(e));
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    motion.clear_timing();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        assert!(motion.pending().is_none());
        assert_eq!(motion.presented(), Some(t));
        assert_eq!(motion.pending_upload_bytes(&height, &proxy).unwrap(), 128);
    }
    assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
    assert_eq!(motion.pipeline_compilations(), 6);
    println!(
        "FarMotion first{first:?} moved{moved:?}; generationABA/reactiveCutout/currentCellFace/presentedDiscard/rebase f64 PASS;4draws;6loadingpipelines;buffers{};1000idle0alloc",
        motion.buffer_bytes()
    );
}
