#![cfg(not(target_arch = "wasm32"))]
//! Current primary Terrain VS/depth versus actual previous presented matrices.
use render::{
    OutputTransfer,
    model_motion::{MotionTargets, TargetConfig},
    taa,
    terrain_gpu::{
        AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
    terrain_motion::{Error, TerrainMotionGpu},
};
use taa::{PresentedHistory, ResetReasons, TaaFrame};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
const W: u32 = 64;
const H: u32 = 32;
fn cube() -> PageMesh {
    let halo = PageHalo::from_page(&[1; 4096], 0);
    let mut mesh = PageMesh::default();
    mesh_page(
        &halo,
        |cell| {
            if cell == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial::opaque(1)
            }
        },
        &mut MeshScratch::new(),
        &mut mesh,
    );
    mesh
}
fn camera(rebase: f32) -> Camera {
    Camera::orthographic(
        [8. + rebase, 8., 40.],
        [8. + rebase, 8., 0.],
        [0., 1., 0.],
        10.,
        2.,
        1.,
        100.,
    )
}
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Pixel {
    motion: [f32; 4],
    surface: [u32; 4],
}
fn pixels(gpu: &render::Gpu, targets: &MotionTargets) -> Vec<Pixel> {
    let module=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("exact temporal source readback"),source:wgpu::ShaderSource::Wgsl("struct Pixel{motion:vec4<f32>,surface:vec4<u32>};@group(0)@binding(0)var m:texture_2d<f32>;@group(0)@binding(1)var s:texture_2d<u32>;@group(0)@binding(2)var<storage,read_write>o:array<Pixel>;@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){if p.x<64u&&p.y<32u{o[p.x+64u*p.y]=Pixel(textureLoad(m,vec2<i32>(p.xy),0),textureLoad(s,vec2<i32>(p.xy),0));}}".into())});
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
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(W * H) * 32,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: output.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
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
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(W / 8, H / 8, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &read, 0, output.size());
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let data = bytemuck::cast_slice::<u8, Pixel>(&read.slice(..).get_mapped_range()).to_vec();
    read.unmap();
    data
}
#[allow(clippy::too_many_arguments)] // All owners share the same candidate frame/token.
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    motion: &mut TerrainMotionGpu,
    targets: &MotionTargets,
    history: &mut PresentedHistory,
    n: u64,
    camera: Camera,
) -> (taa::FrameToken, Vec<Pixel>) {
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
    let mut denied = UploadBudget::new(95);
    assert_eq!(
        motion.prepare_frame(&gpu.queue, token, &camera, [W, H], [0.; 2], &mut denied),
        Err(Error::UploadBudget)
    );
    assert_eq!(denied.remaining_bytes, 95);
    assert!(motion.pending().is_none());
    assert_eq!(
        motion
            .prepare_frame(
                &gpu.queue,
                token,
                &camera,
                [W, H],
                [0.; 2],
                &mut UploadBudget::new(96)
            )
            .unwrap()
            .upload_bytes,
        96
    );
    assert_eq!(motion.commit_presented(token), Err(Error::NotEncoded));
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    terrain.invalidate();
    terrain
        .encode_scene_with_extra(
            &gpu.queue,
            &mut encoder,
            &camera,
            CullingMode::Frustum,
            false,
            |_| 0,
        )
        .unwrap();
    let work = targets.encode(&mut encoder, terrain.scene_depth_msaa_view(), |pass| {
        render::model_motion::Work {
            draws: motion.draw(pass).draws,
            ..Default::default()
        }
    });
    assert_eq!(work.draws, 6);
    let snapshot = motion.encode_snapshot(&mut encoder, token).unwrap();
    assert_eq!(
        motion.encode_snapshot(&mut encoder, token),
        Err(Error::Busy)
    );
    assert_eq!(snapshot.snapshot_records, 8);
    assert_eq!(snapshot.snapshot_read_bytes, 1024);
    assert_eq!(snapshot.snapshot_write_bytes, 1024);
    history.mark_encoded(token).unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    (token, pixels(gpu, targets))
}
fn half_ulp(value: f64) -> f64 {
    if value.abs() < 2f64.powi(-14) {
        2f64.powi(-24)
    } else {
        2f64.powi(value.abs().log2().floor() as i32 - 10)
    }
}
fn inspect(image: &[Pixel], current: (Camera, f64), previous: Option<(Camera, f64)>) -> usize {
    let mut count = 0;
    let mut owners = std::collections::BTreeSet::new();
    for (index, pixel) in image
        .iter()
        .enumerate()
        .filter(|(_, pixel)| pixel.surface[0] != 0)
    {
        count += 1;
        assert_eq!(pixel.surface[0] >> 24, 1);
        assert_eq!(pixel.surface[0] & 7, 3);
        owners.insert(pixel.surface[0] & 0x00ff_ffff);
        let uv = [
            (index % W as usize) as f64 / f64::from(W) + 0.5 / f64::from(W),
            (index / W as usize) as f64 / f64::from(H) + 0.5 / f64::from(H),
        ];
        let current_vp = current.0.view_projection;
        let world_x = (uv[0] * 2. - 1. - f64::from(current_vp[3][0])) / f64::from(current_vp[0][0]);
        let world_y = (1. - uv[1] * 2. - f64::from(current_vp[3][1])) / f64::from(current_vp[1][1]);
        let cell_x = (world_x - current.1).floor().clamp(0., 15.) as u32;
        let cell_y = world_y.floor().clamp(0., 15.) as u32;
        let expected_part = (cell_x + 16 * (15 + 16 * cell_y)) * 8 + 3;
        assert_eq!(pixel.surface, [0x01000000 | expected_part, 0, 0, 1]);
        if let Some((previous_camera, previous_translation)) = previous {
            // Independent f64 intersection with the frontal z16 cube face.
            // Use the exact represented camera coefficients, not the motion
            // shader's interpolants or previous-object bank.
            let previous_world = [world_x - current.1 + previous_translation, world_y, 16., 1.];
            let clip: [f64; 4] = std::array::from_fn(|axis| {
                (0..4)
                    .map(|column| {
                        f64::from(previous_camera.view_projection[column][axis])
                            * previous_world[column]
                    })
                    .sum()
            });
            let expected = [
                clip[0] / clip[3] * 0.5 + 0.5 - uv[0],
                0.5 - clip[1] / clip[3] * 0.5 - uv[1],
                clip[2] / clip[3],
            ];
            assert_eq!(pixel.motion[3], 1.);
            for (axis, expected) in expected.into_iter().enumerate() {
                // Float16 render-target conversion may round toward zero.
                // A full half ULP plus32 f32 eps bounds the recorded source;
                // an unconditional2e-5 incorrectly rejects legal motion-.075.
                let bound = half_ulp(expected) + 32. * f64::from(f32::EPSILON);
                assert!(
                    (f64::from(pixel.motion[axis]) - expected).abs() <= bound,
                    "{pixel:?} expected{expected} axis{axis} bound{bound}"
                );
            }
        } else {
            assert_eq!(pixel.motion, [0.; 4]);
        }
    }
    assert!(
        owners.len() > 100,
        "merged face needs exact cell identity: {}",
        owners.len()
    );
    assert!(count > 500);
    count
}
#[test]
fn terrain_motion_uses_presented_object_pose_and_cell_face_identity_through_discard_and_rebase() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let mut terrain = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 8,
            max_quads: 1024,
            occlusion: false,
        },
        &atlas,
        1,
    )
    .unwrap();
    terrain.prepare_scene_sampling(&gpu.device).unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    assert!(
        TerrainMotionGpu::new(&gpu.device, &gpu.queue, &terrain, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
    assert!(matches!(
        TerrainMotionGpu::new(&gpu.device, &gpu.queue, &terrain, Some(1)),
        Err(Error::MemoryBudget)
    ));
    let mut motion = TerrainMotionGpu::new(&gpu.device, &gpu.queue, &terrain, Some(1 << 20))
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
    let handle = terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([0.; 3]),
            &cube(),
            &mut UploadBudget::new(1 << 20),
        )
        .unwrap();
    let mut history = PresentedHistory::default();
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut motion,
        &targets,
        &mut history,
        1,
        camera(0.),
    );
    let first = inspect(&image, (camera(0.), 0.), None);
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    terrain
        .update_transform(
            &gpu.queue,
            handle,
            translation([1., 0., 0.]),
            &mut UploadBudget::new(128),
        )
        .unwrap();
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut motion,
        &targets,
        &mut history,
        2,
        camera(0.),
    );
    let moved = inspect(&image, (camera(0.), 1.), Some((camera(0.), 0.)));
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    terrain
        .update_transform(
            &gpu.queue,
            handle,
            translation([4., 0., 0.]),
            &mut UploadBudget::new(128),
        )
        .unwrap();
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut motion,
        &targets,
        &mut history,
        3,
        camera(0.),
    );
    inspect(&image, (camera(0.), 4.), Some((camera(0.), 1.)));
    motion.discard(t).unwrap();
    history.discard(t).unwrap();
    assert_eq!(motion.presented().unwrap().frame(), 2);
    assert_eq!(
        motion.prepare_frame(
            &gpu.queue,
            t,
            &camera(0.),
            [W, H],
            [0.; 2],
            &mut UploadBudget::new(96)
        ),
        Err(Error::WrongToken)
    );
    terrain
        .update_transform(
            &gpu.queue,
            handle,
            translation([2., 0., 0.]),
            &mut UploadBudget::new(128),
        )
        .unwrap();
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut motion,
        &targets,
        &mut history,
        4,
        camera(0.),
    );
    inspect(&image, (camera(0.), 2.), Some((camera(0.), 1.)));
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    terrain
        .update_transform(
            &gpu.queue,
            handle,
            translation([2. - 1024., 0., 0.]),
            &mut UploadBudget::new(128),
        )
        .unwrap();
    let (t, image) = frame(
        &gpu,
        &mut terrain,
        &mut motion,
        &targets,
        &mut history,
        5,
        camera(-1024.),
    );
    let rebased = inspect(&image, (camera(-1024.), 2. - 1024.), Some((camera(0.), 2.)));
    motion.commit_presented(t).unwrap();
    history.commit_presented(t, 8).unwrap();
    assert_eq!(motion.pipeline_compilations(), 2);
    motion.clear_timing();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        assert!(motion.pending().is_none());
        assert_eq!(motion.presented().unwrap(), t);
        assert_eq!(motion.required_upload_bytes(), 96);
    }
    assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
    println!(
        "TerrainMotion first{first}/moved{moved}/rebase{rebased}, six primary indirect buckets;2loadingpipelines;96B/frame+1024Bread+1024Bwrite;1000idle0alloc;candidateDiscard preserved2;buffers{}",
        motion.buffer_bytes()
    );
}
