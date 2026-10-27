#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    block_models::{self, BlockCatalog, BlockConfig, BlockInstance, BlockModelsGpu, BlockVertex},
    entity_gpu::{EntityConfig, EntityGpu, EntityInstance},
    entity_models::{ModelCatalog, ModelVertex},
    model_motion::{self, ModelMotionGpu, MotionTargets, Parameters, TargetConfig},
    presented_motion::{Frame, ObjectKey, ObjectPose, PresentedMotion, Token},
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
const W: u32 = 128;
const H: u32 = 128;
fn camera() -> Camera {
    Camera::perspective(
        [8., 7., 13.],
        [0., 3., 0.],
        [0., 1., 0.],
        0.65,
        1.,
        0.1,
        100.,
    )
}
fn atlas() -> AtlasImage {
    let mut a = AtlasImage::donor().unwrap();
    for mip in &mut a.mips {
        for pixel in mip.rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[180, 120, 60, 255]);
        }
    }
    a
}
fn terrain(gpu: &render::Gpu, samples: u32, a: &AtlasImage) -> TerrainGpu {
    let mut t = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 4,
            max_quads: 1024,
            occlusion: false,
        },
        a,
        samples,
    )
    .unwrap();
    t.prepare_scene_sampling(&gpu.device).unwrap();
    t
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Pixel {
    hdr: [f32; 4],
    motion: [f32; 4],
    surface: [u32; 4],
    depth: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Witness {
    pixel: u32,
    surface: [u32; 4],
    reserved: u32,
    expected: [f64; 3],
}
#[allow(clippy::too_many_arguments)]
fn dump_dawn(
    entity: bool,
    camera: Camera,
    vertices: &[u8],
    ranges: &[u8],
    current: &[u8],
    previous: &[u8],
    matches: &[render::presented_motion::GpuMatch],
    parameters: Parameters,
    witnesses: &[Witness],
) {
    let Some(root) = std::env::var_os("MODEL_MOTION_DUMP") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let root = if root.is_absolute() {
        root
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(root)
    };
    let root = root.join(if entity { "entity" } else { "block" });
    std::fs::create_dir_all(&root).unwrap();
    let source = if entity {
        render::entity_gpu::shader_source(false, false, false).unwrap()
    } else {
        render::block_models::shader_source(false, false, false).unwrap()
    };
    let color = if entity {
        render::model_motion_shader::entity_color(&source)
    } else {
        render::model_motion_shader::block_color(&source)
    }
    .unwrap();
    let motion = if entity {
        render::model_motion_shader::entity_motion(&source)
    } else {
        render::model_motion_shader::block_motion(&source)
    }
    .unwrap();
    let cull = if entity {
        render::entity_gpu::cull_shader_source()
    } else {
        render::block_models::cull_shader_source()
    };
    for (name, data) in [
        ("color.wgsl", color.as_bytes()),
        ("motion.wgsl", motion.as_bytes()),
        ("cull.wgsl", cull.as_bytes()),
        ("vertices.bin", vertices),
        ("ranges.bin", ranges),
        ("current.bin", current),
        ("previous.bin", previous),
        ("matches.bin", bytemuck::cast_slice(matches)),
        ("parameters.bin", bytemuck::bytes_of(&parameters)),
        ("witnesses.bin", bytemuck::cast_slice(witnesses)),
    ] {
        std::fs::write(root.join(name), data).unwrap();
    }
    let mut camera_bytes = Vec::from(bytemuck::bytes_of(&camera.view_projection));
    camera_bytes.extend_from_slice(bytemuck::cast_slice(&[2_u32, 4, 0, 0, 0, 0, 0, 0]));
    std::fs::write(root.join("camera.bin"), camera_bytes).unwrap();
    let atlas = if entity {
        render::entity_models::model_atlas().unwrap()
    } else {
        atlas()
    };
    std::fs::write(
        root.join("atlas.bin"),
        bytemuck::cast_slice(&[atlas.tile_size, atlas.layers, atlas.mips.len() as u32]),
    )
    .unwrap();
    for (index, mip) in atlas.mips.iter().enumerate() {
        std::fs::write(root.join(format!("atlas{index}.bin")), &mip.rgba).unwrap();
    }
}
struct Probe {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    out: wgpu::Buffer,
    read: wgpu::Buffer,
}
impl Probe {
    fn color(gpu: &render::Gpu, t: &TerrainGpu, samples: u32) -> Self {
        let depth = if samples == 1 {
            "vec4(textureLoad(depth,at,0),0.,0.,0.)"
        } else {
            "vec4(textureLoad(depth,at,0),textureLoad(depth,at,1),textureLoad(depth,at,2),textureLoad(depth,at,3))"
        };
        let source = format!(
            r#"
struct Pixel{{hdr:vec4<f32>,motion:vec4<f32>,surface:vec4<u32>,depth:vec4<f32>}};
@group(0)@binding(0)var hdr:texture_2d<f32>;
@group(0)@binding(1)var depth:{};
@group(0)@binding(2)var<storage,read_write>output:array<Pixel>;
@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){{if p.x<128u&&p.y<128u{{let at=vec2<i32>(p.xy);output[p.x+128u*p.y]=Pixel(textureLoad(hdr,at,0),vec4(0.),vec4(0u),{depth});}}}}
"#,
            if samples == 1 {
                "texture_depth_2d"
            } else {
                "texture_depth_multisampled_2d"
            }
        );
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("all actual color/depth samples bitwise baseline"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
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
        let size = u64::from(W) * u64::from(H) * 64;
        let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(t.resolved_hdr_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(t.scene_depth_msaa_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        Self {
            pipeline,
            bind,
            out,
            read,
        }
    }
    fn new(gpu: &render::Gpu, t: &TerrainGpu, m: &MotionTargets) -> Self {
        let module=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("actual HDR depth motion and exactID readback"),source:wgpu::ShaderSource::Wgsl(r#"
struct Pixel{hdr:vec4<f32>,motion:vec4<f32>,surface:vec4<u32>,depth:vec4<f32>};
@group(0)@binding(0)var hdr:texture_2d<f32>;@group(0)@binding(1)var motion:texture_2d<f32>;@group(0)@binding(2)var surface:texture_2d<u32>;@group(0)@binding(3)var depth:texture_depth_2d;@group(0)@binding(4)var<storage,read_write>output:array<Pixel>;
@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){if p.x<128u&&p.y<128u{let at=vec2<i32>(p.xy);output[p.x+128u*p.y]=Pixel(textureLoad(hdr,at,0),textureLoad(motion,at,0),textureLoad(surface,at,0),vec4(textureLoad(depth,at,0),0.,0.,0.));}}
"#.into())});
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
        let size = u64::from(W) * u64::from(H) * 64;
        let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(t.resolved_hdr_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(m.motion_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(m.surface_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(t.scene_depth_msaa_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        Self {
            pipeline,
            bind,
            out,
            read,
        }
    }
    fn encode(&self, e: &mut wgpu::CommandEncoder) {
        {
            let mut p = e.begin_compute_pass(&Default::default());
            p.set_pipeline(&self.pipeline);
            p.set_bind_group(0, &self.bind, &[]);
            p.dispatch_workgroups(W.div_ceil(8), H.div_ceil(8), 1);
        }
        e.copy_buffer_to_buffer(&self.out, 0, &self.read, 0, self.out.size());
    }
    fn pixels(&self, gpu: &render::Gpu) -> Vec<Pixel> {
        let bytes = read(gpu, &self.read);
        bytes
            .chunks_exact(64)
            .map(bytemuck::pod_read_unaligned)
            .collect()
    }
}
fn token(i: u64) -> Token {
    Token {
        frame: i,
        serial: i,
    }
}
fn state() -> Frame {
    Frame {
        space: 0,
        geometry_epoch: 1,
        source_epoch: 1,
        extent: [W, H],
        origin: [0; 3],
        unjittered_view_projection: camera().view_projection,
        reset: false,
    }
}
fn rows(moved: bool) -> [ObjectPose<BlockInstance>; 2] {
    let chest = ObjectPose {
        key: ObjectKey::native(1, 0, 11).unwrap(),
        topology: 1,
        pose: BlockInstance {
            position: [-2. + if moved { 0.3 } else { 0. }, 0., 0.],
            phase: if moved { 0.65 } else { 0.2 },
            kind: block_models::kind::CHEST,
            ..Default::default()
        },
        visible: true,
        discontinuity: false,
    };
    let wind = ObjectPose {
        key: ObjectKey::native(1, 0, 22).unwrap(),
        topology: 1,
        pose: BlockInstance {
            position: [2. + if moved { -0.3 } else { 0. }, 0., 0.],
            phase: if moved { 1.1 } else { 0.3 },
            kind: block_models::kind::WIND_GENERATOR,
            ..Default::default()
        },
        visible: true,
        discontinuity: false,
    };
    if moved { [wind, chest] } else { [chest, wind] }
}
fn render(
    gpu: &render::Gpu,
    t: &mut TerrainGpu,
    models: &mut BlockModelsGpu,
    motion: &ModelMotionGpu,
    targets: &MotionTargets,
    probe: &Probe,
    frame: Token,
) -> Vec<Pixel> {
    render_camera(gpu, t, models, motion, targets, probe, frame, camera())
}
#[allow(clippy::too_many_arguments)]
fn render_camera(
    gpu: &render::Gpu,
    t: &mut TerrainGpu,
    models: &mut BlockModelsGpu,
    motion: &ModelMotionGpu,
    targets: &MotionTargets,
    probe: &Probe,
    frame: Token,
    camera: Camera,
) -> Vec<Pixel> {
    let mut e = gpu.device.create_command_encoder(&Default::default());
    models.prepare_frame(&gpu.queue, &mut e, camera).unwrap();
    t.invalidate();
    t.encode_scene_with_extra(
        &gpu.queue,
        &mut e,
        &camera,
        CullingMode::Frustum,
        false,
        |p| models.draw(p),
    )
    .unwrap();
    targets.encode(&mut e, t.scene_depth_msaa_view(), |p| {
        motion.draw(p, frame).unwrap()
    });
    probe.encode(&mut e);
    gpu.queue.submit([e.finish()]);
    probe.pixels(gpu)
}

#[test]
fn actual_jittered_deformed_model_vectors_warp_to_the_previous_rendered_surface_once() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut t = terrain(&gpu, 1, &atlas());
    let mut models = BlockModelsGpu::new(
        &gpu.device,
        &gpu.queue,
        t.geometry(),
        BlockConfig {
            max_instances: 4,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    models.prepare_temporal_raster(&gpu.device).unwrap();
    let mut motion = ModelMotionGpu::blocks(&gpu.device, &models).unwrap();
    let targets = MotionTargets::new(
        &gpu.device,
        &gpu.queue,
        Some(TargetConfig {
            extent: [W, H],
            samples: 1,
            max_pixels: u64::from(W) * u64::from(H),
            memory_budget_bytes: 1_000_000,
        }),
    )
    .unwrap()
    .unwrap();
    let probe = Probe::new(&gpu, &t, &targets);
    let mut cache = PresentedMotion::new(4, 32.).unwrap();
    let previous_jitter = [-0.25, 0.125];
    let current_jitter = [0.375, -0.25];
    let previous_camera = render::taa::jitter_camera(camera(), [W, H], previous_jitter).unwrap();
    let current_camera = render::taa::jitter_camera(camera(), [W, H], current_jitter).unwrap();
    let initial = rows(false);
    let moved = rows(true);
    let mut old_image = Vec::new();
    let mut new_image = Vec::new();
    for (id, rows, jitter, camera) in [
        (21, initial, previous_jitter, previous_camera),
        (22, moved, current_jitter, current_camera),
    ] {
        cache.stage(token(id), state(), &rows).unwrap();
        models
            .upload(
                &gpu.queue,
                &rows.map(|r| r.pose),
                &mut UploadBudget::new(u64::MAX),
            )
            .unwrap();
        motion.synchronize_blocks(&models).unwrap();
        motion
            .upload_blocks(
                &gpu.queue,
                token(id),
                cache.presented_poses(),
                cache.matches(token(id)).unwrap(),
                Parameters {
                    previous_unjittered_view_projection: self::camera().view_projection,
                    inverse_extent_jitter: [1. / W as f32, 1. / H as f32, jitter[0], jitter[1]],
                },
                &mut UploadBudget::new(u64::MAX),
            )
            .unwrap();
        let image = render_camera(
            &gpu,
            &mut t,
            &mut models,
            &motion,
            &targets,
            &probe,
            token(id),
            camera,
        );
        if id == 21 {
            old_image = image;
            cache.commit_presented(token(id)).unwrap();
        } else {
            new_image = image;
        }
    }
    let catalog = BlockCatalog::new();
    let mut checked = 0;
    let mut warped = 0;
    let mut zero_camera = 0;
    let sample = |uv: [f32; 2]| -> Option<&Pixel> {
        if uv.iter().any(|v| !(0. ..1.).contains(v)) {
            return None;
        }
        old_image.get(
            (uv[0] * W as f32).floor() as usize + (uv[1] * H as f32).floor() as usize * W as usize,
        )
    };
    for (at, pixel) in new_image.iter().enumerate() {
        if pixel.depth[0] <= 0. {
            continue;
        }
        assert_eq!(pixel.motion[3], 1.);
        let key = u64::from(pixel.surface[1]) | (u64::from(pixel.surface[2]) << 32);
        let now = moved
            .iter()
            .find(|p| {
                if key == 11 {
                    p.key == initial[0].key
                } else {
                    p.key == initial[1].key
                }
            })
            .unwrap()
            .pose;
        let old = initial
            .iter()
            .find(|p| p.pose.kind == now.kind)
            .unwrap()
            .pose;
        let tri = (pixel.surface[0] & 0xffffff) as usize;
        let vertices = &catalog.vertices[tri * 3..tri * 3 + 3];
        let current = std::array::from_fn(|i| clip_with(current_camera, point(&vertices[i], now)));
        let previous = std::array::from_fn(|i| clip(point(&vertices[i], old)));
        let x = (at % W as usize) as u32;
        let y = (at / W as usize) as u32;
        let Some(mut want) = expected_projected(current, previous, x, y) else {
            continue;
        };
        want[0] += f64::from(current_jitter[0]) / f64::from(W);
        want[1] += f64::from(current_jitter[1]) / f64::from(H);
        for (c, want) in want.into_iter().enumerate() {
            assert!(
                (f64::from(pixel.motion[c]) - want).abs() <= want.abs() / 1024. + 2e-6,
                "jitter removed twice or model motion differs from f64 at{at} channel{c}"
            );
        }
        checked += 1;
        let base = [
            (x as f32 + 0.5 - current_jitter[0] + previous_jitter[0]) / W as f32,
            (y as f32 + 0.5 - current_jitter[1] + previous_jitter[1]) / H as f32,
        ];
        if sample([base[0] + pixel.motion[0], base[1] + pixel.motion[1]])
            .is_some_and(|old| old.surface == pixel.surface)
        {
            warped += 1;
        }
        if sample(base).is_some_and(|old| old.surface == pixel.surface) {
            zero_camera += 1;
        }
    }
    assert!(checked > 600, "jittered f64 witnesses{checked}");
    assert!(
        warped > 300 && warped > zero_camera + 50,
        "physical previous HDR/identity warp{warped} cameraOnly{zero_camera}"
    );
    cache.commit_presented(token(22)).unwrap();
    cache.stage(token(23), state(), &moved).unwrap();
    motion
        .upload_blocks(
            &gpu.queue,
            token(23),
            cache.presented_poses(),
            cache.matches(token(23)).unwrap(),
            Parameters {
                previous_unjittered_view_projection: camera().view_projection,
                inverse_extent_jitter: [1. / W as f32, 1. / H as f32, 0., 0.],
            },
            &mut UploadBudget::new(u64::MAX),
        )
        .unwrap();
    let stationary = render(
        &gpu,
        &mut t,
        &mut models,
        &motion,
        &targets,
        &probe,
        token(23),
    );
    assert!(
        stationary
            .iter()
            .filter(|p| p.depth[0] > 0.)
            .all(|p| p.motion[..2] == [0., 0.]),
        "static geometry must have exact zero velocity"
    );
    cache.discard(token(23)).unwrap();
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
    println!(
        "actualModel currentJitter{current_jitter:?} previousJitter{previous_jitter:?} f64{checked} physicalPreviousSurfaceWarp{warped} zeroCameraOnly{zero_camera} staticExactZero"
    );
}
fn rotate(p: [f64; 3], axis: usize, angle: f64) -> [f64; 3] {
    let (s, c) = angle.sin_cos();
    match axis {
        0 => [p[0], p[1] * c - p[2] * s, p[1] * s + p[2] * c],
        1 => [p[0] * c + p[2] * s, p[1], -p[0] * s + p[2] * c],
        _ => [p[0] * c - p[1] * s, p[0] * s + p[1] * c, p[2]],
    }
}
fn point(v: &BlockVertex, i: BlockInstance) -> [f64; 3] {
    let mut p = v.position.map(f64::from);
    let pivot = v.pivot.map(f64::from);
    let angle = match v.bone {
        1 => Some((0, -f64::from(i.phase) * std::f64::consts::FRAC_PI_2)),
        2 => Some((2, f64::from(i.phase))),
        4 => Some((0, -std::f64::consts::FRAC_PI_8)),
        5 => Some((1, f64::from(i.phase))),
        _ => None,
    };
    if let Some((axis, angle)) = angle {
        p = rotate(std::array::from_fn(|a| p[a] - pivot[a]), axis, angle);
        p = std::array::from_fn(|a| p[a] + pivot[a]);
    } else if v.bone == 3 {
        p[1] *= f64::from(i.amount);
    }
    p = rotate(p, 1, -f64::from(i.yaw));
    std::array::from_fn(|a| p[a] + f64::from(i.position[a]))
}
fn clip(p: [f64; 3]) -> [f64; 4] {
    clip_with(camera(), p)
}
fn clip_with(camera: Camera, p: [f64; 3]) -> [f64; 4] {
    let m = camera.view_projection;
    std::array::from_fn(|r| {
        (0..3).map(|c| f64::from(m[c][r]) * p[c]).sum::<f64>() + f64::from(m[3][r])
    })
}
fn expected(
    vertices: &[BlockVertex],
    current: BlockInstance,
    previous: BlockInstance,
    x: u32,
    y: u32,
) -> Option<[f64; 3]> {
    let current = std::array::from_fn(|i| clip(point(&vertices[i], current)));
    let old = std::array::from_fn(|i| clip(point(&vertices[i], previous)));
    expected_projected(current, old, x, y)
}
fn expected_projected(
    current: [[f64; 4]; 3],
    old: [[f64; 4]; 3],
    x: u32,
    y: u32,
) -> Option<[f64; 3]> {
    let s = current.map(|v| {
        [
            (v[0] / v[3] * 0.5 + 0.5) * f64::from(W),
            (0.5 - v[1] / v[3] * 0.5) * f64::from(H),
        ]
    });
    let area =
        (s[1][1] - s[2][1]) * (s[0][0] - s[2][0]) + (s[2][0] - s[1][0]) * (s[0][1] - s[2][1]);
    if area.abs() < 1e-12 {
        return None;
    }
    let px = f64::from(x) + 0.5;
    let py = f64::from(y) + 0.5;
    let a = ((s[1][1] - s[2][1]) * (px - s[2][0]) + (s[2][0] - s[1][0]) * (py - s[2][1])) / area;
    let b = ((s[2][1] - s[0][1]) * (px - s[2][0]) + (s[0][0] - s[2][0]) * (py - s[2][1])) / area;
    let weights = [a, b, 1. - a - b];
    if weights.iter().any(|w| *w < 0.015) {
        return None;
    }
    let previous: [f64; 4] =
        std::array::from_fn(|c| (0..3).map(|i| weights[i] * old[i][c] / current[i][3]).sum());
    if previous[3] <= 0. {
        return None;
    }
    let uv = [
        previous[0] / previous[3] * 0.5 + 0.5,
        0.5 - previous[1] / previous[3] * 0.5,
    ];
    if uv.iter().any(|v| !(0. ..=1.).contains(v)) {
        return None;
    }
    Some([
        uv[0] - px / f64::from(W),
        uv[1] - py / f64::from(H),
        previous[2] / previous[3],
    ])
}
#[test]
fn actual_gpu_chest_and_rotor_motion_uses_presented_bones_and_exact_depth_equal_visibility() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let a = atlas();
    let mut t = terrain(&gpu, 1, &a);
    let mut models = BlockModelsGpu::new(
        &gpu.device,
        &gpu.queue,
        t.geometry(),
        BlockConfig {
            max_instances: 4,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(matches!(
        ModelMotionGpu::blocks(&gpu.device, &models),
        Err(model_motion::Error::NotPrepared)
    ));
    models
        .reserve_slots(&gpu.queue, 4, &mut UploadBudget::new(u64::MAX))
        .unwrap();
    assert_eq!(
        models
            .prepare_temporal_raster(&gpu.device)
            .unwrap()
            .pipeline_compilations,
        2
    );
    assert_eq!(
        models
            .prepare_temporal_raster(&gpu.device)
            .unwrap()
            .pipeline_compilations,
        0
    );
    let mut motion = ModelMotionGpu::blocks(&gpu.device, &models).unwrap();
    assert_eq!(motion.startup_pipeline_compilations(), 2);
    assert_eq!(motion.buffer_bytes(), 4 * 96 + 80);
    let targets = MotionTargets::new(
        &gpu.device,
        &gpu.queue,
        Some(TargetConfig {
            extent: [W, H],
            samples: 1,
            max_pixels: u64::from(W) * u64::from(H),
            memory_budget_bytes: 1024 * 1024,
        }),
    )
    .unwrap()
    .unwrap();
    let probe = Probe::new(&gpu, &t, &targets);
    let mut cache = PresentedMotion::new(4, 32.).unwrap();
    let initial = rows(false);
    let poses: Vec<_> = initial.iter().map(|p| p.pose).collect();
    models
        .upload(&gpu.queue, &poses, &mut UploadBudget::new(u64::MAX))
        .unwrap();
    motion.synchronize_blocks(&models).unwrap();
    cache.stage(token(1), state(), &initial).unwrap();
    let parameters = Parameters {
        previous_unjittered_view_projection: camera().view_projection,
        inverse_extent_jitter: [1. / W as f32, 1. / H as f32, 0., 0.],
    };
    motion
        .upload_blocks(
            &gpu.queue,
            token(1),
            cache.presented_poses(),
            cache.matches(token(1)).unwrap(),
            parameters,
            &mut UploadBudget::new(u64::MAX),
        )
        .unwrap();
    let first = render(
        &gpu,
        &mut t,
        &mut models,
        &motion,
        &targets,
        &probe,
        token(1),
    );
    let first_count = first.iter().filter(|p| p.depth[0] > 0.).count();
    assert!(first_count > 300);
    assert!(
        first
            .iter()
            .filter(|p| p.depth[0] > 0.)
            .all(|p| p.surface[3] > 0 && p.motion[3] == 0.),
        "first frame must seed exactID with invalid prior motion"
    );
    cache.commit_presented(token(1)).unwrap();
    let moved = rows(true);
    cache.stage(token(2), state(), &moved).unwrap();
    let bytes = motion
        .required_block_upload_bytes(
            cache.presented_poses(),
            cache.matches(token(2)).unwrap(),
            parameters,
        )
        .unwrap();
    let mut denied = UploadBudget::new(bytes - 1);
    assert_eq!(
        motion.upload_blocks(
            &gpu.queue,
            token(2),
            cache.presented_poses(),
            cache.matches(token(2)).unwrap(),
            parameters,
            &mut denied
        ),
        Err(model_motion::Error::UploadBudget)
    );
    assert_eq!(denied.remaining_bytes, bytes - 1);
    let unchanged = render(
        &gpu,
        &mut t,
        &mut models,
        &motion,
        &targets,
        &probe,
        token(1),
    );
    assert!(
        first
            .iter()
            .zip(&unchanged)
            .all(|(a, b)| bytemuck::bytes_of(a) == bytemuck::bytes_of(b)),
        "budget refusal changed the existing pixels/metadata"
    );
    let poses: Vec<_> = moved.iter().map(|p| p.pose).collect();
    models
        .upload(&gpu.queue, &poses, &mut UploadBudget::new(u64::MAX))
        .unwrap();
    motion.synchronize_blocks(&models).unwrap();
    motion
        .upload_blocks(
            &gpu.queue,
            token(2),
            cache.presented_poses(),
            cache.matches(token(2)).unwrap(),
            parameters,
            &mut UploadBudget::new(bytes),
        )
        .unwrap();
    let pixels = render(
        &gpu,
        &mut t,
        &mut models,
        &motion,
        &targets,
        &probe,
        token(2),
    );
    let catalog = BlockCatalog::new();
    let mut checked = [0; 2];
    let mut animated = [0; 2];
    let mut camera_only_wrong = 0;
    let mut max_error = 0_f64;
    let mut witnesses = Vec::new();
    for (y, line) in pixels.chunks_exact(W as usize).enumerate() {
        for (x, pixel) in line.iter().enumerate() {
            if pixel.depth[0] <= 0. {
                continue;
            }
            assert!(pixel.surface[3] > 0, "depth-equal coverage hole at{x},{y}");
            assert_eq!(pixel.motion[3], 1.);
            let uid = u64::from(pixel.surface[1]) | (u64::from(pixel.surface[2]) << 32);
            let row = if uid == 11 {
                0
            } else {
                assert_eq!(uid, 22);
                1
            };
            let current = moved
                .iter()
                .find(|p| p.key == initial[row].key)
                .unwrap()
                .pose;
            let triangle = (pixel.surface[0] & 0xffffff) as usize;
            let vertices = &catalog.vertices[triangle * 3..triangle * 3 + 3];
            assert!(vertices.iter().all(|v| v.kind == current.kind));
            let Some(want) = expected(vertices, current, initial[row].pose, x as u32, y as u32)
            else {
                continue;
            };
            for (c, want) in want.into_iter().enumerate() {
                let error = (f64::from(pixel.motion[c]) - want).abs();
                max_error = max_error.max(error);
                let bound = want.abs() / 1024. + 2e-6;
                assert!(
                    error <= bound,
                    "motion mismatch{id:?} pixel{x},{y} channel{c}:actual{} expected{want} error{error} halfbound{bound}",
                    pixel.motion[c],
                    id = pixel.surface
                );
            }
            checked[row] += 1;
            witnesses.push(Witness {
                pixel: x as u32 + y as u32 * W,
                surface: pixel.surface,
                reserved: 0,
                expected: want,
            });
            if vertices[0].bone != 0 {
                animated[row] += 1;
            }
            if pixel.motion[0].hypot(pixel.motion[1]) > 0.002 {
                camera_only_wrong += 1;
            }
        }
    }
    assert!(
        checked.iter().all(|n| *n > 80),
        "independent f64 witnesses{checked:?}"
    );
    assert!(
        animated.iter().all(|n| *n > 10),
        "moving bone witnesses{animated:?}"
    );
    assert!(
        camera_only_wrong > 150,
        "zero camera-only vector must fail actual object motion"
    );
    dump_dawn(
        false,
        camera(),
        bytemuck::cast_slice(&catalog.vertices),
        bytemuck::cast_slice(&catalog.ranges),
        bytemuck::cast_slice(&moved.map(|p| p.pose)),
        bytemuck::cast_slice(cache.presented_poses()),
        cache.matches(token(2)).unwrap(),
        parameters,
        &witnesses,
    );
    cache.commit_presented(token(2)).unwrap();
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
    println!(
        "modelMotion actualChest+Rotor currentDepthPixels{first_count} f64Witnesses{checked:?} movingBones{animated:?} zeroCameraWrong{camera_only_wrong} maxHalfError{max_error} startup4motion+color defaultUnchanged"
    );
}

fn human_camera() -> Camera {
    Camera::perspective(
        [0., 2.2, 6.],
        [0., 1., 0.],
        [0., 1., 0.],
        0.6,
        1.,
        0.1,
        100.,
    )
}
fn human_rows(phase: f32) -> [ObjectPose<EntityInstance>; 2] {
    std::array::from_fn(|i| ObjectPose {
        // These UUIDs share all low64 bits. Neither the instance's deliberately
        // colliding legacy id nor a truncation is a temporal surface identity.
        key: ObjectKey::named(2, 0, [0xfeed_face_dead_beef, i as u64 + 1]).unwrap(),
        topology: 1,
        pose: EntityInstance {
            position: [(i as f32 - 0.5) * 1.6 + phase * 0.05, 0., 0.],
            body_yaw: 20. + phase * 3.,
            head_yaw: -15. + phase * 17.,
            pitch: phase * 9.,
            limb_phase: phase,
            limb_amount: 0.8,
            kind: 5,
            flags: if i == 1 {
                render::entity_gpu::CROUCH
            } else {
                0
            },
            color: (phase * 0.3).to_bits(),
            id_low: 7,
            id_high: 9,
            death_roll: phase * 0.2,
            scale: 1.,
            reserved: phase * 0.08,
        },
        visible: true,
        discontinuity: false,
    })
}
fn human_point(v: &ModelVertex, i: EntityInstance) -> [f64; 3] {
    assert_eq!(v.kind, 5);
    let swing = (f64::from(i.limb_phase) * 0.6662).cos() * 1.4 * f64::from(i.limb_amount).min(1.);
    let mut pitch = f64::from(v.base_pitch);
    let mut yaw = 0.;
    let idle = (f64::from(f32::from_bits(i.color)) * 0.067).sin() * 0.025;
    match v.bone {
        1 => {
            pitch -= f64::from(i.pitch).to_radians();
            yaw = -f64::from(i.head_yaw - i.body_yaw).to_radians();
        }
        2 => pitch += swing,
        3 => pitch -= swing,
        6 => pitch += -swing / 1.4 + idle - f64::from(i.reserved),
        7 => pitch += swing / 1.4 - idle,
        _ => {}
    }
    let local = rotate(rotate(v.position.map(f64::from), 0, pitch), 1, yaw);
    let scale = f64::from(i.scale) * 0.9;
    let mut p = std::array::from_fn(|a| (local[a] + f64::from(v.pivot[a])) * scale);
    p[1] -= scale * 0.5;
    p = rotate(p, 2, f64::from(i.death_roll).to_radians());
    p[1] += scale * 0.5;
    p = rotate(
        p,
        1,
        std::f64::consts::PI - f64::from(i.body_yaw).to_radians(),
    );
    if i.flags & render::entity_gpu::CROUCH != 0 {
        p[1] *= 0.833333333;
    }
    std::array::from_fn(|a| p[a] + f64::from(i.position[a]))
}
#[allow(clippy::too_many_arguments)]
fn render_entities(
    gpu: &render::Gpu,
    t: &mut TerrainGpu,
    entities: &mut EntityGpu,
    motion: &ModelMotionGpu,
    targets: &MotionTargets,
    probe: &Probe,
    frame: Token,
) -> Vec<Pixel> {
    let c = human_camera();
    let mut e = gpu.device.create_command_encoder(&Default::default());
    entities.prepare_frame(&gpu.queue, &mut e, c).unwrap();
    t.invalidate();
    t.encode_scene_with_extra(&gpu.queue, &mut e, &c, CullingMode::Frustum, false, |p| {
        entities.draw(p)
    })
    .unwrap();
    targets.encode(&mut e, t.scene_depth_msaa_view(), |p| {
        motion.draw(p, frame).unwrap()
    });
    probe.encode(&mut e);
    gpu.queue.submit([e.finish()]);
    probe.pixels(gpu)
}
#[test]
fn actual_human_bones_use_last_presented_uuid_poses_after_a_discarded_gpu_frame() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut t = terrain(&gpu, 1, &atlas());
    let mut entities = EntityGpu::new(
        &gpu.device,
        &gpu.queue,
        t.geometry(),
        EntityConfig {
            max_entities: 4,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert_eq!(
        entities
            .prepare_temporal_raster(&gpu.device)
            .unwrap()
            .pipeline_compilations,
        1
    );
    let mut motion = ModelMotionGpu::entities(&gpu.device, &entities).unwrap();
    assert_eq!(motion.startup_pipeline_compilations(), 1);
    let targets = MotionTargets::new(
        &gpu.device,
        &gpu.queue,
        Some(TargetConfig {
            extent: [W, H],
            samples: 1,
            max_pixels: u64::from(W) * u64::from(H),
            memory_budget_bytes: 1_000_000,
        }),
    )
    .unwrap()
    .unwrap();
    let probe = Probe::new(&gpu, &t, &targets);
    let mut cache = PresentedMotion::new(4, 32.).unwrap();
    let state = Frame {
        unjittered_view_projection: human_camera().view_projection,
        ..state()
    };
    let parameters = Parameters {
        previous_unjittered_view_projection: human_camera().view_projection,
        inverse_extent_jitter: [1. / W as f32, 1. / H as f32, 0., 0.],
    };
    let initial = human_rows(0.2);
    let mut first = Vec::new();
    let mut final_pixels = Vec::new();
    let mut current = human_rows(2.7);
    current.reverse();
    for (id, rows) in [(10, initial), (11, human_rows(4.8)), (12, current)] {
        cache.stage(token(id), state, &rows).unwrap();
        let poses = rows.map(|r| r.pose);
        entities
            .upload(&gpu.queue, &poses, &[], &mut UploadBudget::new(u64::MAX))
            .unwrap();
        motion.synchronize_entities(&entities).unwrap();
        motion
            .upload_entities(
                &gpu.queue,
                token(id),
                cache.presented_poses(),
                cache.matches(token(id)).unwrap(),
                parameters,
                &mut UploadBudget::new(u64::MAX),
            )
            .unwrap();
        let pixels = render_entities(
            &gpu,
            &mut t,
            &mut entities,
            &motion,
            &targets,
            &probe,
            token(id),
        );
        if id == 10 {
            first = pixels;
            cache.commit_presented(token(id)).unwrap();
        } else if id == 11 {
            assert!(pixels.iter().zip(&first).any(|(a, b)| a.depth != b.depth));
            cache.discard(token(id)).unwrap();
        } else {
            final_pixels = pixels;
        }
    }
    let matches = cache.matches(token(12)).unwrap();
    assert_ne!(matches[0].surface[1..3], matches[1].surface[1..3]);
    let catalog = ModelCatalog::new();
    let mut checked = [0; 2];
    let mut articulated = [0; 2];
    let mut zero_wrong = 0;
    let mut witnesses = Vec::new();
    for (index, pixel) in final_pixels.iter().enumerate() {
        if pixel.depth[0] <= 0. {
            continue;
        }
        assert_ne!(
            pixel.surface[3], 0,
            "human motion depth-equal hole at{index}"
        );
        assert_eq!(pixel.motion[3], 1.);
        let row = matches
            .iter()
            .position(|m| {
                m.surface[1..] == pixel.surface[1..]
                    && (m.surface[0] >> 24) == pixel.surface[0] >> 24
            })
            .unwrap();
        let old = initial
            .iter()
            .find(|old| old.key == current[row].key)
            .unwrap()
            .pose;
        let at = (pixel.surface[0] & 0xffffff) as usize * 3;
        let vertices = &catalog.vertices[at..at + 3];
        let want = expected_projected(
            std::array::from_fn(|v| {
                clip_with(human_camera(), human_point(&vertices[v], current[row].pose))
            }),
            std::array::from_fn(|v| clip_with(human_camera(), human_point(&vertices[v], old))),
            (index % W as usize) as u32,
            (index / W as usize) as u32,
        );
        let Some(want) = want else {
            continue;
        };
        for (channel, want) in want.into_iter().enumerate() {
            let error = (f64::from(pixel.motion[channel]) - want).abs();
            assert!(
                error <= want.abs() / 1024. + 2e-6,
                "human f64 mismatch pixel{index} channel{channel} actual{} expected{want}",
                pixel.motion[channel]
            );
        }
        checked[row] += 1;
        witnesses.push(Witness {
            pixel: index as u32,
            surface: pixel.surface,
            reserved: 0,
            expected: want,
        });
        if vertices[0].bone != 0 {
            articulated[row] += 1;
        }
        if pixel.motion[0].hypot(pixel.motion[1]) > 0.002 {
            zero_wrong += 1;
        }
    }
    assert!(
        checked.iter().all(|n| *n > 80),
        "human witnesses{checked:?}"
    );
    assert!(
        articulated.iter().all(|n| *n > 30),
        "human articulated witnesses{articulated:?}"
    );
    assert!(
        zero_wrong > 150,
        "object movement must reject camera-only zero vectors"
    );
    let ranges: Vec<_> = catalog
        .ranges
        .iter()
        .map(|r| [r.vertex_count, r.first_vertex, 0, 0])
        .collect();
    dump_dawn(
        true,
        human_camera(),
        bytemuck::cast_slice(&catalog.vertices),
        bytemuck::cast_slice(&ranges),
        bytemuck::cast_slice(&current.map(|p| p.pose)),
        bytemuck::cast_slice(cache.presented_poses()),
        matches,
        parameters,
        &witnesses,
    );
    cache.commit_presented(token(12)).unwrap();
    // Warm the same actual GPU banks once after the reordered presentation.
    cache.stage(token(13), state, &current).unwrap();
    motion
        .upload_entities(
            &gpu.queue,
            token(13),
            cache.presented_poses(),
            cache.matches(token(13)).unwrap(),
            parameters,
            &mut UploadBudget::new(u64::MAX),
        )
        .unwrap();
    cache.commit_presented(token(13)).unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for id in 14..1014 {
        cache.stage(token(id), state, &current).unwrap();
        assert_eq!(
            motion
                .upload_entities(
                    &gpu.queue,
                    token(id),
                    cache.presented_poses(),
                    cache.matches(token(id)).unwrap(),
                    parameters,
                    &mut UploadBudget::new(0)
                )
                .unwrap()
                .upload_bytes,
            0
        );
        cache.commit_presented(token(id)).unwrap();
    }
    let allocations = scope.finish();
    assert_eq!(allocations.allocating_calls(), 0);
    assert_eq!(allocations.dealloc_calls, 0);
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
    println!(
        "humanMotion fullUUIDcollidingLegacyIDs f64Witnesses{checked:?} articulated{articulated:?} zeroCameraWrong{zero_wrong} discardedFrameNotPresented"
    );
}

#[test]
fn default_one_and_four_sample_color_and_depth_stay_bitwise_without_motion_allocation() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    for samples in [1, 4] {
        let mut t = terrain(&gpu, samples, &atlas());
        let mut entities = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            t.geometry(),
            EntityConfig {
                max_entities: 4,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let mut models = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            t.geometry(),
            BlockConfig {
                max_instances: 4,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let mut shared_entities = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            t.geometry(),
            EntityConfig {
                max_entities: 4,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let mut shared_models = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            t.geometry(),
            BlockConfig {
                max_instances: 4,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        if samples == 1 {
            shared_entities
                .prepare_temporal_raster(&gpu.device)
                .unwrap();
            shared_models.prepare_temporal_raster(&gpu.device).unwrap();
        } else {
            assert!(
                shared_entities
                    .prepare_temporal_raster(&gpu.device)
                    .is_err()
            );
            assert!(shared_models.prepare_temporal_raster(&gpu.device).is_err());
        }
        let poses = human_rows(1.7).map(|p| p.pose);
        let props = rows(true).map(|p| p.pose);
        for e in [&mut entities, &mut shared_entities] {
            e.upload(&gpu.queue, &poses, &[], &mut UploadBudget::new(u64::MAX))
                .unwrap();
        }
        for m in [&mut models, &mut shared_models] {
            m.upload(&gpu.queue, &props, &mut UploadBudget::new(u64::MAX))
                .unwrap();
        }
        let probe = Probe::color(&gpu, &t, samples);
        let mut frame = |entities: &mut EntityGpu, models: &mut BlockModelsGpu| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            entities
                .prepare_frame(&gpu.queue, &mut encoder, camera())
                .unwrap();
            models
                .prepare_frame(&gpu.queue, &mut encoder, camera())
                .unwrap();
            t.invalidate();
            t.encode_scene_with_extra(
                &gpu.queue,
                &mut encoder,
                &camera(),
                CullingMode::Frustum,
                false,
                |pass| entities.draw(pass) + models.draw(pass),
            )
            .unwrap();
            probe.encode(&mut encoder);
            gpu.queue.submit([encoder.finish()]);
            probe.pixels(&gpu)
        };
        let old = frame(&mut entities, &mut models);
        let scope = telemetry::AllocationScope::begin().unwrap();
        assert!(
            MotionTargets::new(&gpu.device, &gpu.queue, None)
                .unwrap()
                .is_none()
        );
        let counts = scope.finish();
        assert_eq!(counts.allocating_calls(), 0);
        assert_eq!(counts.dealloc_calls, 0);
        let repeated = frame(&mut entities, &mut models);
        let optional = frame(&mut shared_entities, &mut shared_models);
        assert!(old.iter().any(|p| p.depth[0] > 0.));
        let color_changed = old
            .iter()
            .zip(&optional)
            .filter(|(a, b)| a.hdr != b.hdr)
            .count();
        let depth_changed = old
            .iter()
            .zip(&optional)
            .filter(|(a, b)| a.depth != b.depth)
            .count();
        let depth_max = old
            .iter()
            .zip(&optional)
            .flat_map(|(a, b)| a.depth.into_iter().zip(b.depth).map(|(a, b)| (a - b).abs()))
            .fold(0_f32, f32::max);
        eprintln!(
            "temporalVariant diagnostic sample{samples} colorChanged{color_changed} depthChanged{depth_changed} maxDepth{depth_max}"
        );
        assert!(
            old.iter()
                .zip(&repeated)
                .all(|(a, b)| bytemuck::bytes_of(a) == bytemuck::bytes_of(b)),
            "default samples{samples} changed after disabled motion"
        );
        if samples == 1 {
            // Explicitly enabled TAA uses invariant vertex arithmetic. It can
            // change contraction/rounding of depth; this is not the default
            // path above. Its motion Equal pass is independently checked at
            // every actual model depth pixel in the articulated-model tests.
            assert!(old.iter().zip(&optional).all(|(a, b)| a.hdr == b.hdr));
            assert!(
                old.iter()
                    .zip(&optional)
                    .all(|(a, b)| a.depth.map(|v| v > 0.) == b.depth.map(|v| v > 0.))
            );
        } else {
            assert!(
                old.iter()
                    .zip(&optional)
                    .all(|(a, b)| bytemuck::bytes_of(a) == bytemuck::bytes_of(b)),
                "rejected temporal preparation changed default four-sample HDR/depth"
            );
        }
    }
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
}
