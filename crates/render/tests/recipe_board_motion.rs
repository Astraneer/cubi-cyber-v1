#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    light_volume::PackedLight,
    model_motion::{MotionTargets, TargetConfig},
    recipe_board_motion::{Error, RecipeBoardMotionGpu},
    recipe_boards::{Config, Images, Instance, Lighting, RecipeBoardGpu},
    taa::{self, PresentedHistory, ResetReasons, TaaFrame},
    terrain_gpu::{AtlasImage, AtlasMip, Camera, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOC: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
const W: u32 = 32;
const H: u32 = 32;
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Pixel {
    motion: [f32; 4],
    surface: [u32; 4],
}
fn capture(gpu: &render::Gpu, targets: &MotionTargets) -> Vec<Pixel> {
    let module=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:None,source:wgpu::ShaderSource::Wgsl("struct Pixel{motion:vec4<f32>,surface:vec4<u32>};@group(0)@binding(0)var m:texture_2d<f32>;@group(0)@binding(1)var s:texture_2d<u32>;@group(0)@binding(2)var<storage,read_write>o:array<Pixel>;@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){if p.x<32u&&p.y<32u{o[p.x+32u*p.y]=Pixel(textureLoad(m,vec2<i32>(p.xy),0),textureLoad(s,vec2<i32>(p.xy),0));}}".into())});
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
        pass.dispatch_workgroups(4, 4, 1);
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
fn base(x: f32) -> Camera {
    Camera {
        view_projection: [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [x, 0., 0., 1.],
        ],
    }
}
fn state(n: u64, jitter: [f32; 2]) -> TaaFrame {
    TaaFrame {
        frame: n,
        space: 0,
        scene_revision: 1,
        geometry_epoch: 1,
        source_epoch: 1,
        jitter_pixels: jitter,
        reset: ResetReasons::NONE,
    }
}
#[allow(clippy::too_many_arguments)]
fn frame(
    gpu: &render::Gpu,
    source: &mut RecipeBoardGpu,
    motion: &mut RecipeBoardMotionGpu,
    targets: &MotionTargets,
    history: &mut PresentedHistory,
    n: u64,
    offset: f32,
    jitter: [f32; 2],
    enabled: bool,
    session: u64,
) -> (taa::FrameToken, Vec<Pixel>) {
    let card = Instance {
        center: [0., 0., 0.5],
        right: [1., 0., 0.],
        up: [0., 1., 0.],
        size: [1.5; 2],
        layer: 0,
        light: PackedLight::new(0, 0, [15; 3]).unwrap(),
        enabled,
    };
    let camera = taa::jitter_camera(base(offset), [W, H], jitter).unwrap();
    source
        .prepare_frame(&gpu.queue, camera, &[card], &mut UploadBudget::new(10000))
        .unwrap();
    let token = history.begin(state(n, jitter)).unwrap();
    if n == 1 {
        let mut denied = UploadBudget::new(0);
        assert_eq!(
            motion.prepare_frame(
                &gpu.queue,
                source,
                Some(session),
                token,
                base(offset),
                [W, H],
                jitter,
                &mut denied
            ),
            Err(Error::UploadBudget)
        );
        assert_eq!(denied.remaining_bytes, 0);
        assert_eq!(motion.pending(), None);
    }
    motion
        .prepare_frame(
            &gpu.queue,
            source,
            Some(session),
            token,
            base(offset),
            [W, H],
            jitter,
            &mut UploadBudget::new(10000),
        )
        .unwrap();
    assert_eq!(motion.validate_commit(token), Err(Error::NotEncoded));
    let tex = |format| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
    };
    let color = tex(wgpu::TextureFormat::Rgba16Float);
    let depth = tex(wgpu::TextureFormat::Depth32Float);
    let c = color.create_view(&Default::default());
    let d = depth.create_view(&Default::default());
    let mut e = gpu.device.create_command_encoder(&Default::default());
    {
        let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &c,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &d,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        source.draw(&mut p);
    }
    targets.encode(&mut e, &d, |p| render::model_motion::Work {
        draws: motion.draw(p, token).unwrap().draws,
        ..Default::default()
    });
    motion.mark_encoded(token).unwrap();
    history.mark_encoded(token).unwrap();
    gpu.queue.submit([e.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    (token, capture(gpu, targets))
}
fn commit(m: &mut RecipeBoardMotionGpu, h: &mut PresentedHistory, t: taa::FrameToken) {
    m.validate_commit(t).unwrap();
    m.commit_presented(t).unwrap();
    h.commit_presented(t, 8).unwrap();
}
#[test]
fn actual_equal_motion_keeps_full_session_lifetimes_and_only_presented_camera_without_idle_allocations()
 {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 1,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![255; 4],
            tint_mask: vec![0],
        }],
    };
    let renderer = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        W,
        H,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 1,
            max_quads: 32,
            occlusion: false,
        },
        &atlas,
        1,
    )
    .unwrap();
    let mut source = RecipeBoardGpu::new(
        &gpu.device,
        &gpu.queue,
        renderer.geometry(),
        Images {
            width: 1,
            height: 1,
            layers: 1,
            rgba: &[255; 4],
        },
        Config {
            max_boards: 1,
            memory_budget_bytes: 1024,
        },
        Lighting::default(),
    )
    .unwrap();
    assert!(matches!(
        RecipeBoardMotionGpu::new(&gpu.device, &source, 191),
        Err(Error::MemoryBudget)
    ));
    let mut motion = RecipeBoardMotionGpu::new(&gpu.device, &source, 192).unwrap();
    assert_eq!(motion.buffer_bytes(), 192);
    assert_eq!(motion.startup_pipeline_compilations(), 1);
    let targets = MotionTargets::new(
        &gpu.device,
        &gpu.queue,
        Some(TargetConfig {
            extent: [W, H],
            samples: 1,
            max_pixels: u64::from(W * H),
            memory_budget_bytes: 100000,
        }),
    )
    .unwrap()
    .unwrap();
    let mut history = PresentedHistory::default();
    let uid = 0xfedc_ba98_7654_3210;
    let (t, a) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        1,
        0.,
        [0.; 2],
        true,
        uid,
    );
    let known = a.iter().filter(|p| p.surface[0] >> 24 == 19).count();
    assert_eq!(known, 576);
    for p in a.iter().filter(|p| p.surface[0] != 0) {
        assert_eq!(&p.surface[1..], &[0x76543210, 0xfedcba98, 1]);
        assert_eq!(p.motion, [0.; 4]);
    }
    commit(&mut motion, &mut history, t);
    let (t, a) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        2,
        0.,
        [0.; 2],
        true,
        uid,
    );
    assert_eq!(a.iter().filter(|p| p.motion[3] == 1.).count(), 576);
    assert!(
        a.iter()
            .filter(|p| p.motion[3] == 1.)
            .all(|p| p.motion == [0., 0., 0.5, 1.])
    );
    commit(&mut motion, &mut history, t);
    let (t, _) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        3,
        -0.25,
        [0.375, -0.25],
        true,
        uid,
    );
    motion.discard(t).unwrap();
    history.discard(t).unwrap();
    let (t, a) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        4,
        -0.125,
        [-0.25, 0.125],
        true,
        uid,
    );
    let mut witnesses = 0;
    for p in a.iter().filter(|p| p.motion[3] == 1.) {
        assert!(
            (p.motion[0] - 0.0625).abs() <= 0.00003052,
            "actual {:?}",
            p.motion
        );
        assert!(p.motion[1].abs() < 0.00000006);
        assert_eq!(p.motion[2], 0.5);
        witnesses += 1;
    }
    assert!(witnesses > 500);
    commit(&mut motion, &mut history, t);
    let (t, a) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        5,
        0.,
        [0.; 2],
        false,
        uid,
    );
    assert!(a.iter().all(|p| p.surface == [0; 4]));
    commit(&mut motion, &mut history, t);
    let (t, a) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        6,
        0.,
        [0.; 2],
        true,
        uid,
    );
    for p in a.iter().filter(|p| p.surface[0] != 0) {
        assert_eq!(p.surface[3], 2);
        assert_eq!(p.motion, [0.; 4]);
    }
    commit(&mut motion, &mut history, t);
    let (t, a) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        7,
        0.,
        [0.; 2],
        true,
        uid + 1,
    );
    for p in a.iter().filter(|p| p.surface[0] != 0) {
        assert_eq!(p.surface[3], 3);
        assert_eq!(p.surface[1], 0x76543211);
        assert_eq!(p.motion, [0.; 4]);
    }
    commit(&mut motion, &mut history, t);
    let (t, _) = frame(
        &gpu,
        &mut source,
        &mut motion,
        &targets,
        &mut history,
        8,
        0.,
        [0.; 2],
        true,
        uid + 1,
    );
    commit(&mut motion, &mut history, t);
    let scope = telemetry::allocator::AllocationScope::begin().unwrap();
    for n in 9..1009 {
        let t = history.begin(state(n, [0.; 2])).unwrap();
        let w = motion
            .prepare_frame(
                &gpu.queue,
                &source,
                Some(uid + 1),
                t,
                base(0.),
                [W, H],
                [0.; 2],
                &mut UploadBudget::new(0),
            )
            .unwrap();
        assert_eq!(w, Default::default());
        motion.discard(t).unwrap();
        history.discard(t).unwrap();
    }
    let counts = scope.finish();
    assert_eq!(counts.alloc_calls, 0);
    assert_eq!(counts.dealloc_calls, 0);
    eprintln!(
        "BOARD_MOTION19_OK pixels={known} pan_f64_witnesses={witnesses} bytes192=true discard_previous_PRESENTED=true session64_exact=true lifetimeABA=true idle1000_alloc0=true"
    );
}
