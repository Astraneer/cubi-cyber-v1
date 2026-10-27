#![cfg(not(target_arch = "wasm32"))]
//! Resolver-only analytic motion/identity fixture. The actual model motion
//! backend has its own gates; authored vectors here do not certify that source.
pub use render::{gpu_time, terrain_gpu};
#[path = "../src/taa.rs"]
#[allow(dead_code)]
mod taa;
use bytemuck::{Pod, Zeroable};
use taa::*;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
const W: u32 = 32;
const H: u32 = 16;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Source {
    color: [f32; 4],
    motion: [f32; 4],
    identity: [u32; 4],
    other: [f32; 4],
}
fn data(phase: u32) -> Source {
    Source {
        color: [17., 0., 17., 1.],
        motion: [0., 0., 0.5, 1.],
        identity: [0x10001, 0xFEDC_BA98, 0xFF80_0001, 99],
        other: [0.5, phase as f32, 0., 0.],
    }
}
struct Fixture {
    _textures: [wgpu::Texture; 4],
    views: [wgpu::TextureView; 4],
    uniform: wgpu::Buffer,
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
}
impl Fixture {
    fn new(gpu: &render::Gpu) -> Self {
        let formats = [
            HDR_FORMAT,
            MOTION_FORMAT,
            SURFACE_FORMAT,
            wgpu::TextureFormat::Depth32Float,
        ];
        let textures = std::array::from_fn(|i| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("analytic single-sample temporal source"),
                size: wgpu::Extent3d {
                    width: W,
                    height: H,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: formats[i],
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        });
        let views = std::array::from_fn(|i| textures[i].create_view(&Default::default()));
        let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("independent HDR/checker/identity fixture"),source:wgpu::ShaderSource::Wgsl(r#"
struct Source{color:vec4<f32>,motion:vec4<f32>,identity:vec4<u32>,other:vec4<f32>};@group(0)@binding(0)var<uniform>s:Source;
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
struct Out{@location(0)color:vec4<f32>,@location(1)motion:vec4<f32>,@location(2)identity:vec4<u32>,@builtin(frag_depth)depth:f32};
@fragment fn fragment(@builtin(position)p:vec4<f32>)->Out{let odd=((u32(p.x)+u32(s.other.y))&1u)!=0u;return Out(select(vec4(s.color.r,0.,0.,1.),vec4(0.,0.,s.color.b,1.),odd),s.motion,s.identity,s.other.x);}
"#.into())});
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("32-byte fixture MRT"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[
                        target(HDR_FORMAT),
                        target(MOTION_FORMAT),
                        target(SURFACE_FORMAT),
                    ],
                }),
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Always,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            });
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        Self {
            _textures: textures,
            views,
            uniform,
            pipeline,
            bind,
        }
    }
    fn inputs(&self) -> TaaInputs<'_> {
        TaaInputs {
            hdr: &self.views[0],
            motion: &self.views[1],
            surface: &self.views[2],
            depth: &self.views[3],
            extent: [W, H],
            samples: 1,
        }
    }
    fn encode(&self, gpu: &render::Gpu, encoder: &mut wgpu::CommandEncoder, data: Source) {
        gpu.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&data));
        let attachment = |view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("authored vector source fixture"),
            color_attachments: &[
                attachment(&self.views[0]),
                attachment(&self.views[1]),
                attachment(&self.views[2]),
            ],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.views[3],
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        p.set_pipeline(&self.pipeline);
        p.set_bind_group(0, &self.bind, &[]);
        p.draw(0..3, 0..1);
    }
}
fn frame(n: u64) -> TaaFrame {
    TaaFrame {
        frame: n,
        space: 0,
        scene_revision: 1,
        geometry_epoch: 1,
        source_epoch: 1,
        jitter_pixels: [0.; 2],
        reset: ResetReasons::NONE,
    }
}
fn submit(
    gpu: &render::Gpu,
    fixture: &Fixture,
    taa: &mut TaaGpu,
    state: TaaFrame,
    source: Source,
) -> FrameToken {
    let t = taa.begin_frame(state).unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    fixture.encode(gpu, &mut encoder, source);
    let work = taa
        .encode(
            &gpu.queue,
            &mut encoder,
            t,
            &mut terrain_gpu::UploadBudget::new(64),
        )
        .unwrap();
    assert_eq!(work.pixels, u64::from(W * H));
    assert_eq!(work.history_copy_bytes, u64::from(W * H * 8));
    assert_eq!(work.upload_bytes, 64);
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    t
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let out = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    out
}
fn capture(gpu: &render::Gpu, taa: &TaaGpu) -> (Vec<[f32; 4]>, [u32; 4]) {
    let module=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("temporal floating HDR oracle readback"),source:wgpu::ShaderSource::Wgsl("@group(0)@binding(0)var t:texture_2d<f32>;@group(0)@binding(1)var<storage,read_write>o:array<vec4<f32>>;@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){if p.x<32u&&p.y<16u{o[p.x+32u*p.y]=textureLoad(t,vec2<i32>(p.xy),0);}}".into())});
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
        size: u64::from(W * H * 16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let target = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(W * H * 16 + 16),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(taa.output_view()),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut e = gpu.device.create_command_encoder(&Default::default());
    {
        let mut p = e.begin_compute_pass(&Default::default());
        p.set_pipeline(&pipeline);
        p.set_bind_group(0, &bind, &[]);
        p.dispatch_workgroups(W / 8, H / 8, 1);
    }
    e.copy_buffer_to_buffer(&output, 0, &target, 0, u64::from(W * H * 16));
    if let Some(stats) = taa.statistics_buffer() {
        e.copy_buffer_to_buffer(stats, 0, &target, u64::from(W * H * 16), 16);
    }
    gpu.queue.submit([e.finish()]);
    let b = read(gpu, &target);
    let colors = b[..W as usize * H as usize * 16]
        .chunks_exact(16)
        .map(|v| {
            std::array::from_fn(|i| f32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().unwrap()))
        })
        .collect();
    let stats = std::array::from_fn(|i| {
        u32::from_le_bytes(
            b[W as usize * H as usize * 16 + i * 4..W as usize * H as usize * 16 + i * 4 + 4]
                .try_into()
                .unwrap(),
        )
    });
    (colors, stats)
}
fn checker(colors: &[[f32; 4]], phase: u32) {
    for (y, row) in colors.chunks_exact(W as usize).enumerate() {
        for (x, v) in row.iter().enumerate() {
            let expected = if (x as u32 + phase) & 1 == 0 {
                [17., 0., 0., 1.]
            } else {
                [0., 0., 17., 1.]
            };
            assert_eq!(*v, expected, "HDR checker{x}/{y}");
        }
    }
}
#[test]
fn exact_surface_depth_edit_and_real_vectors_control_presented_history() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let source = Fixture::new(&gpu);
        let config = TaaConfig {
            statistics: true,
            ..Default::default()
        };
        let scope = telemetry::AllocationScope::begin().unwrap();
        assert!(
            TaaGpu::new(
                &gpu.device,
                &gpu.queue,
                None,
                TaaInputs {
                    samples: 4,
                    ..source.inputs()
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
        for mode in 0..12 {
            let mut taa = TaaGpu::new(&gpu.device, &gpu.queue, Some(config), source.inputs())
                .unwrap()
                .unwrap();
            let mut first = data(0);
            if mode == 6 {
                first.other[0] = 0.;
                first.motion[2] = 0.;
            }
            let t = submit(&gpu, &source, &mut taa, frame(1), first);
            assert!(!taa.presented_output_ready());
            assert!(taa.presented().is_none());
            taa.commit_presented(t).unwrap();
            assert!(taa.presented_output_ready());
            checker(&capture(&gpu, &taa).0, 0);
            let mut second = data(1);
            let mut state = frame(2);
            match mode {
                1 => second.identity[2] ^= 1,
                2 => second.motion[2] = 0.25,
                3 => state.geometry_epoch = 2,
                4 => state.source_epoch = 2,
                5 => second.motion[0] = -1. / W as f32,
                6 => {
                    second.other[0] = 0.;
                    second.motion[2] = 0.;
                }
                7 => state.reset = ResetReasons::CAMERA_CUT,
                8 => state.space = 42,
                9 => second.identity[0] ^= 1, // sameobject, differentcell/face/part
                10 => second.identity[3] += 1,
                11 => second.motion[3] = 0., // unsupportedreactive/sky/water
                _ => {}
            }
            let t = submit(&gpu, &source, &mut taa, state, second);
            let (image, counts) = capture(&gpu, &taa);
            assert_eq!(counts[0], W * H);
            assert!(u64::from(counts[1]) <= u64::from(W * H) * 28);
            if mode == 0 || mode == 6 {
                assert!(image.iter().all(|v| *v == [8.5, 0., 8.5, 1.]));
                assert_eq!(counts[2], W * H);
            } else {
                checker(&image, 1);
                if mode != 5 {
                    assert_eq!(counts[2], 0);
                }
            }
            // Dropped candidates leave the old presented color/poses/camera intact.
            taa.discard(t).unwrap();
            assert_eq!(taa.presented().unwrap().state.frame, 1);
            assert!(!taa.presented_output_ready());
            let mut e = gpu.device.create_command_encoder(&Default::default());
            assert_eq!(
                taa.restore_presented_output(&mut e).unwrap(),
                u64::from(W * H * 8)
            );
            let abandoned = taa.output_restore_token().unwrap();
            assert!(!taa.presented_output_ready());
            assert_eq!(taa.begin_frame(frame(3)), Err(Error::Busy));
            drop(e);
            taa.discard_output_restore(abandoned).unwrap();
            assert!(!taa.presented_output_ready());
            assert_eq!(
                capture(&gpu, &taa).0,
                image,
                "an abandoned restore was submitted"
            );
            let mut e = gpu.device.create_command_encoder(&Default::default());
            assert_eq!(
                taa.restore_presented_output(&mut e).unwrap(),
                u64::from(W * H * 8)
            );
            let restoration = taa.output_restore_token().unwrap();
            assert!(restoration.serial() > abandoned.serial());
            assert_eq!(
                taa.mark_output_restore_submitted(abandoned),
                Err(Error::WrongToken)
            );
            assert!(!taa.presented_output_ready());
            gpu.queue.submit([e.finish()]);
            taa.mark_output_restore_submitted(restoration).unwrap();
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            checker(&capture(&gpu, &taa).0, 0);
            assert!(taa.presented_output_ready());
            println!(
                "TAA resolver mode{mode} fragments{} loads{} accepted{} rejected{} owned{} feature{} pipeline1",
                counts[0],
                counts[1],
                counts[2],
                counts[3],
                taa.allocation_bytes(),
                taa.feature_bytes()
            );
        }
    });
}
#[test]
fn denied_temporal_credit_never_overwrites_front_and_retry_commits_once() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let source = Fixture::new(&gpu);
        let mut taa = TaaGpu::new(
            &gpu.device,
            &gpu.queue,
            Some(TaaConfig::default()),
            source.inputs(),
        )
        .unwrap()
        .unwrap();
        let t = submit(&gpu, &source, &mut taa, frame(1), data(0));
        taa.commit_presented(t).unwrap();
        let before = capture(&gpu, &taa).0;
        let next = taa.begin_frame(frame(2)).unwrap();
        let mut e = gpu.device.create_command_encoder(&Default::default());
        let mut budget = terrain_gpu::UploadBudget::new(63);
        assert_eq!(
            taa.encode(&gpu.queue, &mut e, next, &mut budget),
            Err(Error::UploadBudget)
        );
        assert_eq!(budget.remaining_bytes, 63);
        assert_eq!(taa.presented().unwrap().state.frame, 1);
        assert_eq!(taa.commit_presented(next), Err(Error::NotEncoded));
        assert_eq!(capture(&gpu, &taa).0, before);
        source.encode(&gpu, &mut e, data(1));
        taa.encode(
            &gpu.queue,
            &mut e,
            next,
            &mut terrain_gpu::UploadBudget::new(64),
        )
        .unwrap();
        assert_eq!(
            taa.encode(
                &gpu.queue,
                &mut e,
                next,
                &mut terrain_gpu::UploadBudget::new(64)
            ),
            Err(Error::Busy)
        );
        gpu.queue.submit([e.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        taa.commit_presented(next).unwrap();
        assert_eq!(taa.commit_presented(next), Err(Error::WrongToken));
        assert_eq!(taa.presented().unwrap().state.frame, 2);
        assert_eq!(taa.pipeline_compilations(), 1);
        for n in 3..=8 {
            let token = submit(&gpu, &source, &mut taa, frame(n), data(1));
            taa.commit_presented(token).unwrap();
        }
        assert!(!taa.needs_refinement(1));
        let before = capture(&gpu, &taa).0;
        taa.clear_timing();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            assert!(!taa.needs_refinement(1));
            assert!(taa.presented_output_ready());
            assert!(taa.pending().is_none());
            let _ = taa.next_jitter();
        }
        assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
        assert_eq!(taa.graph().count, 0);
        assert_eq!(capture(&gpu, &taa).0, before);
    });
}
