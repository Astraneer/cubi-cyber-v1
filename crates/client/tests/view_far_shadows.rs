#![cfg(not(target_arch = "wasm32"))]
//! Real shared View publication/guard route, independent world-space receivers.
use client::{
    ClientScene, SceneConfig, native_pool,
    view::{ClientView, EnvironmentProfile, horizon},
};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, Version};
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    post::{Exposure, PostConfig},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn tile(key: HeightKey, source: u64) -> Box<HeightTile> {
    let mut known = [255; 37];
    known[36] = 1;
    Box::new(HeightTile {
        key,
        source,
        error: None,
        heights: [40; 289],
        known,
        atlas: [rules::ids::L_STONE as u16; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: Replacement::None,
    })
}
fn geometry(y: i32) -> net::PublicGeometry {
    let mut occupied = [0; 512];
    if y == 2 {
        for ly in 0..16 {
            for z in 4..12 {
                for x in 4..12 {
                    let i = x + 16 * (z + 16 * ly);
                    occupied[i / 8] |= 1 << (i % 8);
                }
            }
        }
    }
    net::PublicGeometry {
        page: PageId {
            space: SpaceId(0),
            x: 1,
            y,
            z: 0,
        },
        source_version: Version(42),
        view_version: 2,
        known: [255; 512],
        occupied,
        palette: if y == 2 {
            vec![net::PublicSurface {
                atlas_layer: rules::ids::L_STONE as u16,
                tint: u32::MAX,
                kind: net::PublicSurfaceKind::Opaque,
            }]
        } else {
            Vec::new()
        },
        index_bits: 0,
        indices: Vec::new(),
    }
}
fn frame(gpu: &render::Gpu, view: &mut ClientView, output: &wgpu::TextureView) {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    view.encode(&gpu.queue, &mut encoder, output, 64, 64)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<f32> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |v| tx.send(v).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let out = buffer
        .slice(..)
        .get_mapped_range()
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect();
    buffer.unmap();
    out
}
struct Probe {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    output: wgpu::Buffer,
}
impl Probe {
    fn new(gpu: &render::Gpu, view: &mut ClientView) -> Self {
        let source = format!(
            "{}\n{}",
            include_str!("../../render/src/shadow_sampling.wgsl"),
            r#"
@group(0)@binding(0)var<storage,read_write>output:array<f32>;
@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){if p.x<64u&&p.y<64u{output[p.x+64u*p.y]=cascade_shadow(vec3((f32(p.x)+.5)*.25,8.,(f32(p.y)+.5)*.25),vec3(0.,1.,0.));}}
"#
        );
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("actual View near shadow receivers"),
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
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let shadow = view.lighting_mut().unwrap().owner.shadows();
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: shadow.sampling_buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(shadow.depth_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(shadow.sampler()),
                },
            ],
        });
        Self {
            pipeline,
            bind,
            output,
        }
    }
    fn capture(&self, gpu: &render::Gpu) -> Vec<f32> {
        let target = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: self.output.size(),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut p = encoder.begin_compute_pass(&Default::default());
            p.set_pipeline(&self.pipeline);
            p.set_bind_group(0, &self.bind, &[]);
            p.dispatch_workgroups(8, 8, 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &target, 0, self.output.size());
        gpu.queue.submit([encoder.finish()]);
        read(gpu, &target)
    }
}
fn witnesses(values: &[f32], cast: bool, building: bool) -> (usize, usize) {
    let (mut dark, mut clear) = (0, 0);
    for z in 0..64 {
        for x in 0..64 {
            let wx = (x as f64 + 0.5) * 0.25;
            let wz = (z as f64 + 0.5) * 0.25;
            // f64 world rays: the hill atY40 inX16..32 covers this interior atY8;
            // the building AABB[X20..28,Y32..48,Z4..12] covers the same dark subset.
            if (4. ..12.).contains(&wx) && (6. ..10.).contains(&wz) {
                let v = values[x + 64 * z];
                assert!(
                    (v - if cast { 0. } else { 1. }).abs() < 1e-5,
                    "near witness[{wx},{wz}]={v},cast{cast}"
                );
                dark += 1;
            }
            if building
                && (4. ..12.).contains(&wx)
                && ((1. ..3.).contains(&wz) || (13. ..15.).contains(&wz))
            {
                assert!(
                    (values[x + 64 * z] - 1.).abs() < 1e-5,
                    "phantom far wall at[{wx},{wz}]"
                );
                clear += 1;
            }
        }
    }
    assert_eq!(dark, 512);
    if building {
        assert_eq!(clear, 512);
    }
    (dark, clear)
}
#[test]
fn shared_view_publishes_far_casters_revokes_guard_and_retains_static_cache() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 32,
            max_quads: 65536,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    let center = BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 8,
        z: 8,
    };
    let mut view = ClientView::new(
        renderer,
        SceneConfig {
            center,
            horizontal_radius: 0,
            vertical_radius: 0,
            max_loaded_pages: 32,
            authoritative_remote: true,
            ..SceneConfig::forest()
        },
    )
    .unwrap();
    let mut light = client::lighting::ClientLightingConfig::default().with_authoritative_source();
    light.shadows.resolution = 256;
    light.shadows.half_widths = [32., 64., 128.];
    light.shadows.max_updates_per_frame = 3;
    light.shadows.sun_step_radians = 1e-6;
    light.shadows.projected_bounds = true;
    light.shadows.dynamic_casters = true;
    view.prepare_lighting(&gpu.device, &gpu.queue, light)
        .unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        64,
        64,
        OutputTransfer::Attachment,
        EnvironmentProfile {
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                post: PostConfig {
                    exposure: Exposure::Manual(1.),
                    bloom: None,
                    ..Default::default()
                },
                ..Default::default()
            },
            celestials: None,
            aerial: None,
            multiple_scattering: None,
            ..Default::default()
        },
        &atlas,
    )
    .unwrap();
    view.prepare_entities(&gpu.device, &gpu.queue).unwrap();
    view.prepare_horizon(
        &gpu.device,
        &gpu.queue,
        horizon::Config {
            max_tiles: 64,
            max_sources: 128,
            max_proxy_pages: 16,
            horizon_level: 0,
            ..Default::default()
        },
    )
    .unwrap();
    view.prepare_distant_shadows(&gpu.device).unwrap();
    assert_eq!(view.distant_shadow_stats().unwrap().startup_pipelines, 6);
    assert!(!view.distant_shadow_stats().unwrap().enabled);
    for page in view.scene.requested_pages().collect::<Vec<_>>() {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if page == center.page() {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * (z + 16 * 7)] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
        }
        view.scene
            .import_authoritative_page(
                &net::PageUpdate::Snapshot(net::PageSnapshot {
                    page,
                    version: 1,
                    source_version: Version(1),
                    applied_action: Default::default(),
                    cells,
                }),
                Tick(0),
            )
            .unwrap();
        view.scene
            .import_authoritative_light(
                &net::light::Update::Snapshot(net::light::Snapshot {
                    header: net::light::Header {
                        page,
                        cell_view: 1,
                        version: 1,
                        source: Version(1),
                    },
                    values: Box::new([net::light::pack(15, 0, [0; 3]).unwrap(); 4096]),
                }),
                Tick(0),
            )
            .unwrap();
    }
    let tick = (0..6000)
        .min_by(|&a, &b| {
            let score = |t| {
                let s = client::celestial_clock::sample(Tick(t), 0., None)
                    .unwrap()
                    .sun_direction;
                (s[0] - 0.5 * s[1]).abs()
            };
            score(a).total_cmp(&score(b))
        })
        .unwrap();
    view.set_public_clock(Tick(tick), 0.).unwrap();
    let sun = view.sun_clock().unwrap().sun_direction;
    assert!((sun[0] / sun[1] - 0.5).abs() < 0.002);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let workers = native_pool(2, 16).unwrap();
    let mut admitted = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        assert!(
            Instant::now() < deadline,
            "far bootstrap {:?}/{:?}/{:?}",
            view.horizon_stats(),
            view.horizon_job_stats(),
            view.lighting_stats()
        );
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        for key in view.horizon_wanted_height().collect::<Vec<_>>() {
            if admitted.insert(key) {
                view.ingest_remote_height(tile(key, 1)).unwrap();
            }
        }
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap_or_else(|error| {
                panic!(
                    "upload {error}; horizon{:?}; jobs{:?}; lighting{:?}",
                    view.horizon_stats(),
                    view.horizon_job_stats(),
                    view.lighting_stats()
                )
            });
        frame(&gpu, &mut view, &output);
        if view.is_ready()
            && view.has_committed_far_front()
            && view.distant_shadow_stats().unwrap().enabled
        {
            break;
        }
        std::thread::yield_now();
    }
    let probe = Probe::new(&gpu, &mut view);
    witnesses(&probe.capture(&gpu), true, false);
    let key = HeightKey {
        space: 0,
        level: 0,
        x: 1,
        z: 0,
    };
    let mut replacement = tile(key, 2);
    replacement.protected.fill(255);
    replacement.replacement = Replacement::Column {
        min_y: 0,
        max_y: 7,
        geometry: (0..8)
            .map(|y| GeometryStamp {
                page: PageId {
                    space: SpaceId(0),
                    x: 1,
                    y,
                    z: 0,
                },
                source: Version(42),
                view: 2,
            })
            .collect(),
    };
    view.ingest_remote_height(replacement).unwrap();
    for y in 0..8 {
        view.ingest_remote_geometry(&geometry(y)).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        assert!(
            Instant::now() < deadline,
            "proxy publication {:?}/{:?}",
            view.horizon_stats(),
            view.horizon_job_stats()
        );
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap_or_else(|error| {
                panic!(
                    "upload {error}; horizon{:?}; jobs{:?}; lighting{:?}",
                    view.horizon_stats(),
                    view.horizon_job_stats(),
                    view.lighting_stats()
                )
            });
        frame(&gpu, &mut view, &output);
        if view.horizon_mut().unwrap().front_source(key) == Some(2) {
            break;
        }
        std::thread::yield_now();
    }
    witnesses(&probe.capture(&gpu), true, true);
    view.advance_input(0.).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    view.horizon_mut().unwrap().set_display_space(1);
    frame(&gpu, &mut view, &output);
    assert!(!view.distant_shadow_stats().unwrap().enabled);
    witnesses(&probe.capture(&gpu), false, false);
    view.horizon_mut().unwrap().set_display_space(0);
    view.advance_input(0.).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    frame(&gpu, &mut view, &output);
    witnesses(&probe.capture(&gpu), true, true);
    let stats = view.lighting_stats().unwrap();
    let source = view.distant_shadow_stats().unwrap();
    // Follow the actual shell's idle gate before allocating command handles.
    // Forced UI presentation legitimately records Post passes in wgpu below.
    view.advance_input(0.).unwrap();
    view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
        .unwrap();
    let _ = view.take_frame_work();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        assert!(!view.needs_redraw(64, 64));
    }
    let allocations = scope.finish();
    assert_eq!(allocations.allocating_calls(), 0);
    assert_eq!(allocations.dealloc_calls, 0);
    assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
    for _ in 0..20 {
        frame(&gpu, &mut view, &output);
    }
    let final_stats = view.lighting_stats().unwrap();
    assert_eq!(final_stats.dynamic_shadow_draws, stats.dynamic_shadow_draws);
    assert_eq!(view.distant_shadow_stats().unwrap(), source);
    println!(
        "view_farShadow tick{tick} sun{sun:?} heightFront{} proxyFront{} sourceChanges{} startup5 idle1000=0alloc/0work/0redraw forcedPresent20=0farDraws",
        view.horizon_stats().unwrap().height_front,
        view.horizon_stats().unwrap().proxy_front,
        source.source_changes
    );
}
