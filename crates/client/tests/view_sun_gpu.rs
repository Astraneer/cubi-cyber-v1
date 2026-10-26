#![cfg(not(target_arch = "wasm32"))]
//! Shared View clock/energy integration. This public replica fixture is separate
//! from actual server/product screenshots and the2048 continuous-Sun H5 route.
use client::{
    ClientScene, SceneConfig, native_pool,
    view::{ClientView, EnvironmentProfile},
};
use core_types::{BlockPos, Cell, SpaceId, Tick, Version};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    post::{Exposure, PostConfig},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use std::time::{Duration, Instant};

fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (send, receive) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            send.send(result).unwrap()
        });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receive.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
struct Probe {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    output: wgpu::Buffer,
}
impl Probe {
    fn new(gpu: &render::Gpu, view: &mut ClientView) -> Self {
        let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {label:Some("whole View solar direction and surface energy oracle"),source:wgpu::ShaderSource::Wgsl(r#"
struct Sampling{matrices:array<mat4x4<f32>,6>,sun:vec4<f32>,params:vec4<u32>};
@group(0)@binding(0)var hdr:texture_2d<f32>;
@group(0)@binding(1)var depth:texture_depth_multisampled_2d;
@group(0)@binding(2)var<storage,read>atmo:array<vec4<f32>,5>;
@group(0)@binding(3)var<uniform>shadow:Sampling;
@group(0)@binding(4)var<storage,read_write>out:array<vec4<f32>>;
@compute @workgroup_size(8,8)fn probe(@builtin(global_invocation_id)i:vec3<u32>){
 if i.x>=96u||i.y>=64u{return;}let p=vec2<i32>(i.xy);var solid=true;
 for(var sample=0;sample<4;sample++){solid=solid&&textureLoad(depth,p,sample)>0.;}
 out[i.x+96u*i.y]=vec4(textureLoad(hdr,p,0).rgb,select(0.,1.,solid));
 if i.x==0u&&i.y==0u{out[6144]=atmo[3];out[6145]=atmo[4];out[6146]=shadow.sun;out[6147]=vec4<f32>(shadow.params);for(var k=0u;k<3u;k++){out[6148u+k]=atmo[k];}}
}
"#.into())});
        let pipeline = gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &shader,
                entry_point: Some("probe"),
                compilation_options: Default::default(),
                cache: None,
            });
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("completed View sun receiver values"),
            size: 6151 * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let hdr = view.renderer_mut().resolved_hdr_view().clone();
        let depth = view.renderer_mut().scene_depth_msaa_view().clone();
        let atmo = view
            .environment_mut()
            .unwrap()
            .gpu
            .atmosphere_lighting_buffer()
            .clone();
        let shadow = view
            .lighting_mut()
            .unwrap()
            .owner
            .shadows()
            .sampling_buffer()
            .clone();
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&hdr),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&depth),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: atmo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: shadow.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        Self {
            pipeline,
            bind,
            output,
        }
    }
    fn capture(&self, gpu: &render::Gpu) -> Vec<[f32; 4]> {
        let target = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 6151 * 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(12, 8, 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &target, 0, 6151 * 16);
        gpu.queue.submit([encoder.finish()]);
        read(gpu, &target)
            .chunks_exact(16)
            .map(|bytes| {
                std::array::from_fn(|i| {
                    f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
                })
            })
            .collect()
    }
}
#[test]
fn actual_view_public_day_night_shares_sun_with_shadow_and_keeps_raw_sky_certified() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
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
    let mut lighting =
        client::lighting::ClientLightingConfig::default().with_authoritative_source();
    lighting.shadows.resolution = 64;
    lighting.shadows.max_updates_per_frame = 3;
    lighting.shadows.sun_step_radians = 1e-6;
    lighting.shadows.projected_bounds = true;
    lighting.shadows.dynamic_casters = true;
    view.prepare_lighting(&gpu.device, &gpu.queue, lighting)
        .unwrap();
    view.prepare_environment(
        &gpu.device,
        &gpu.queue,
        96,
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
    let pages = view.scene.requested_pages().collect::<Vec<_>>();
    for page in &pages {
        let mut cells = Box::new([Cell::AIR; 4096]);
        if *page == center.page() {
            for z in 0..16 {
                for x in 0..16 {
                    cells[x + 16 * (z + 16 * 7)] = Cell::from_parts(rules::ids::STONE, 0);
                }
            }
        }
        view.scene
            .import_authoritative_page(
                &net::PageUpdate::Snapshot(net::PageSnapshot {
                    page: *page,
                    version: 1,
                    source_version: Version(1),
                    cells,
                }),
                Tick(0),
            )
            .unwrap();
        view.scene
            .import_authoritative_light(
                &net::light::Update::Snapshot(net::light::Snapshot {
                    header: net::light::Header {
                        page: *page,
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
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 96,
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
    let deadline = Instant::now() + Duration::from_secs(20);
    view.set_public_clock(Tick(6000), 0.).unwrap();
    while !view.is_ready() {
        assert!(
            Instant::now() < deadline,
            "source/volume/terrain settle {:?}",
            view.lighting_stats()
        );
        view.scene
            .pump_native(&workers, Tick(0), &mut ClientScene::default_budget(), 8)
            .unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        std::thread::yield_now();
    }
    let probe = Probe::new(&gpu, &mut view);
    let startup = view.stats().pipeline_compilations;
    let mut day_lit = 0;
    let mut night_black = 0;
    for tick in [6000, 18000] {
        view.set_public_clock(Tick(tick), 0.).unwrap();
        view.advance_input(0.).unwrap();
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 96, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        let values = probe.capture(&gpu);
        let sun = view.sun_clock().unwrap().sun_direction;
        for (axis, expected_sun) in sun.iter().enumerate() {
            assert!(
                (values[6145][axis] - expected_sun).abs() < 0.000002,
                "AtmoSun mismatch"
            );
            assert!(
                (values[6146][axis] - expected_sun).abs() < 0.000002,
                "ShadowSun mismatch"
            );
        }
        assert_eq!(values[6147][0], 7.);
        let receivers = values[..6144]
            .iter()
            .filter(|p| p[3] == 1.)
            .collect::<Vec<_>>();
        assert!(receivers.len() > 100);
        if tick == 6000 {
            day_lit = receivers
                .iter()
                .filter(|p| p[..3].iter().any(|v| *v > 0.01))
                .count();
            assert!(day_lit > 100);
            assert!(values[6144][..3].iter().sum::<f32>() > 1.);
        } else {
            assert!(values[6144][..3].iter().all(|v| *v == 0.));
            assert!(values[6148..6151].iter().flatten().all(|v| *v == 0.));
            night_black = receivers
                .iter()
                .filter(|p| p[..3].iter().all(|v| *v == 0.))
                .count();
            assert_eq!(
                night_black,
                receivers.len(),
                "white ambient leak under canonical downSun andSky15"
            );
        }
        assert_eq!(view.stats().frame_pipeline_compilations, 0);
        assert_eq!(view.stats().pipeline_compilations, startup);
        assert!(view.is_ready());
        for page in &pages {
            assert!(view.scene.authoritative_light_version(*page).is_some());
        }
        assert_eq!(view.scene.read_authoritative_light(center).unwrap().sky, 15);
    }
    println!(
        "M4 WHOLE_VIEW publicTick6000/18000 Atmo+ShadowSun same; dayLit{day_lit}/nightBlack{night_black}; rawSky15 certificates unchanged; pipelines0"
    );
}
