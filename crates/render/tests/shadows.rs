#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    light_volume::{
        LIGHT_PAGE_BYTES, LIGHT_SAMPLES, LightUpdate, LightUploadError, LightVolumeGpu, PackedLight,
    },
    shadows::{CASCADES, ShadowConfig, ShadowGpu, WorldBounds},
    terrain_gpu::{AtlasImage, GpuConfig, PageHandle, TerrainGpu, UploadBudget, translation},
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn box_mesh() -> PageMesh {
    let mut cells = [0; 4096];
    for y in 0..4 {
        for z in 6..10 {
            for x in 6..10 {
                cells[x + 16 * (z + 16 * y)] = 1;
            }
        }
    }
    let mut mesh = PageMesh::default();
    mesh_page(
        &PageHalo::from_page(&cells, 0),
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
    mesh
}
fn read_buffer(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
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
fn depths(gpu: &render::Gpu, shadow: &ShadowGpu) -> Vec<f32> {
    let resolution = shadow.cache().config().resolution;
    let row = (resolution * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shadow depth oracle readback"),
        size: u64::from(row) * u64::from(resolution) * 3,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: shadow.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(resolution),
            },
        },
        wgpu::Extent3d {
            width: resolution,
            height: resolution,
            depth_or_array_layers: 3,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read_buffer(gpu, &buffer)
        .chunks_exact(row as usize)
        .flat_map(|r| {
            r[..resolution as usize * 4]
                .chunks_exact(4)
                .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        })
        .collect()
}
fn render_shadow(gpu: &render::Gpu, shadow: &mut ShadowGpu) -> render::shadows::ShadowWork {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = shadow.encode(&gpu.queue, &mut encoder);
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    work
}
#[test]
fn cached_cascades_render_real_geometry_and_dirty_rectangles_match_full_redraw() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        assert_eq!(gpu.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            64,
            64,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 512,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mesh = box_mesh();
        let mut budget = UploadBudget::new(1_000_000);
        let handle = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &mesh,
                &mut budget,
            )
            .unwrap();
        let config = ShadowConfig {
            resolution: 128,
            half_widths: [16., 32., 64.],
            depth_half_extent: 32.,
            ..ShadowConfig::default()
        };
        let mut shadow = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            config,
            [8., 0., 8.],
            [0., 1., 0.],
        )
        .unwrap();
        let mut draw_work = 0;
        for remaining in (0..3).rev() {
            let work = render_shadow(&gpu, &mut shadow);
            assert_eq!(work.cascade_updates, 1);
            assert_eq!(work.pending_cascades, remaining);
            draw_work += work.draw_calls;
        }
        let baseline = depths(&gpu, &shadow);
        let mut certified = 0;
        for cascade in 0..CASCADES {
            let radius = config.half_widths[cascade];
            for y in 0..128 {
                for x in 0..128 {
                    let world_x = 8. + (0.5 - (x as f32 + 0.5) / 128.) * 2. * radius;
                    let world_z = 8. + (0.5 - (y as f32 + 0.5) / 128.) * 2. * radius;
                    let inside =
                        world_x > 6.01 && world_x < 9.99 && world_z > 6.01 && world_z < 9.99;
                    let away =
                        !(5.99..=10.01).contains(&world_x) || !(5.99..=10.01).contains(&world_z);
                    let actual = baseline[cascade * 128 * 128 + y * 128 + x];
                    if inside {
                        assert!(
                            (actual - (0.5 + 4. / 64.)).abs() < 0.00001,
                            "analytic AABB depth incorrect {actual}"
                        );
                        certified += 1;
                    } else if away {
                        assert_eq!(
                            actual, 0.,
                            "unexpected shadow coverage at {world_x},{world_z}"
                        );
                    }
                }
            }
        }
        assert!(
            certified > 200,
            "all-black depth must fail analytic positive control"
        );
        // Thousands of idle owner calls must issue no raster/compute/upload work.
        let positive = telemetry::AllocationScope::begin().unwrap();
        let memory = vec![1; std::hint::black_box(32)];
        std::hint::black_box(&memory);
        drop(memory);
        assert!(positive.finish().allocating_calls() > 0);
        let idle = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            shadow.update_view([8., 0., 8.], [0., 1., 0.]).unwrap();
            assert_eq!(shadow.cache().pending(), 0);
        }
        let idle = idle.finish();
        assert_eq!(idle.allocating_calls(), 0);
        let mut static_draws = 0;
        for _ in 0..12 {
            let work = render_shadow(&gpu, &mut shadow);
            assert_eq!(work.uniform_upload_bytes, 0);
            assert_eq!(work.cascade_updates, 0);
            static_draws += work.draw_calls;
        }
        assert_eq!(static_draws, 0);
        assert_eq!(
            depths(&gpu, &shadow),
            baseline,
            "static shadow flicker must be exactly zero"
        );
        // Subtexel motion keeps the world's light grid unchanged. This is a strict
        // temporal 0‰ test, below H5's 0.02‰ bound, with geometry verified above.
        for step in 0..20 {
            shadow
                .update_view([8. + step as f32 * 0.0001, 0., 8.], [0., 1., 0.])
                .unwrap();
            assert_eq!(render_shadow(&gpu, &mut shadow).cascade_updates, 0);
        }
        assert_eq!(depths(&gpu, &shadow), baseline);
        // Remove the old occluder and move it. Both footprints are invalidated;
        // clearing only the new footprint would leave a stale shadow (negative case).
        terrain
            .update_transform(&gpu.queue, handle, translation([3., 0., 0.]), &mut budget)
            .unwrap();
        shadow.invalidate_bounds(WorldBounds::new([6., 0., 6.], [10., 4., 10.]).unwrap());
        shadow.invalidate_bounds(WorldBounds::new([9., 0., 6.], [13., 4., 10.]).unwrap());
        let mut dirty_pixels = 0;
        for _ in 0..3 {
            let work = render_shadow(&gpu, &mut shadow);
            dirty_pixels += work.clear_pixels;
        }
        let patched = depths(&gpu, &shadow);
        assert_ne!(patched, baseline, "moving geometry must alter actual depth");
        assert!(
            dirty_pixels < 3 * 128 * 128 / 8,
            "one local move cleared entire maps: {dirty_pixels}"
        );
        shadow.invalidate_all();
        for _ in 0..3 {
            render_shadow(&gpu, &mut shadow);
        }
        assert_eq!(
            depths(&gpu, &shadow),
            patched,
            "dirty shadow cache differs from full-redraw oracle"
        );
        println!(
            "M4 shadow H5 local: initialdraws={draw_work},12staticframesdraws={static_draws},fullRedrawControlDraws={},dirtyPixels={dirty_pixels},flickerPermille=0,analyticCertifiedPixels={certified}",
            12 * 3 * 5
        );
    });
}
#[test]
fn light_volume_dirty_patches_do_not_touch_geometry_and_reject_stale_slots() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut lights = LightVolumeGpu::new(&gpu.device, 2).unwrap();
        let handle = PageHandle {
            slot: 0,
            generation: 1,
        };
        let zero = [PackedLight::default(); LIGHT_SAMPLES];
        let mut budget = UploadBudget::new(100_000);
        assert_eq!(
            lights
                .upload_page(&gpu.queue, handle, 1, &zero, &mut budget)
                .unwrap(),
            LIGHT_PAGE_BYTES
        );
        assert_eq!(
            lights
                .upload_page(&gpu.queue, handle, 1, &zero, &mut budget)
                .unwrap(),
            0
        );
        let red = PackedLight::new(0, 15, [15, 0, 0]).unwrap();
        let blue = PackedLight::new(15, 14, [0, 0, 15]).unwrap();
        let updates = [
            LightUpdate {
                index: 12,
                value: red,
            },
            LightUpdate {
                index: 13,
                value: blue,
            },
            LightUpdate {
                index: 100,
                value: red,
            },
        ];
        assert_eq!(
            lights
                .upload_patch(&gpu.queue, handle, 2, &updates, &mut budget)
                .unwrap(),
            12
        );
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light samples oracle"),
            size: LIGHT_PAGE_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(lights.buffer(), 0, &buffer, 0, LIGHT_PAGE_BYTES);
        gpu.queue.submit([encoder.finish()]);
        let bytes = read_buffer(&gpu, &buffer);
        for i in 0..LIGHT_SAMPLES {
            let actual = u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
            let expected = updates
                .iter()
                .find(|u| usize::from(u.index) == i)
                .map_or(0, |u| u.value.raw());
            assert_eq!(actual, expected);
        }
        assert_eq!(
            lights.upload_patch(
                &gpu.queue,
                PageHandle {
                    generation: 2,
                    ..handle
                },
                3,
                &updates,
                &mut budget
            ),
            Err(LightUploadError::StaleHandle)
        );
        let no_budget = budget.remaining_bytes;
        budget.remaining_bytes = 0;
        assert_eq!(
            lights.upload_patch(&gpu.queue, handle, 3, &updates, &mut budget),
            Err(LightUploadError::UploadBudget)
        );
        budget.remaining_bytes = no_budget;
        assert_eq!(
            lights.upload_patch(
                &gpu.queue,
                handle,
                3,
                &[updates[1], updates[0]],
                &mut budget
            ),
            Err(LightUploadError::InvalidIndex)
        );
        assert_eq!(lights.take_upload_bytes(), LIGHT_PAGE_BYTES + 12);
    });
}

struct ReceiverProbe {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    texture: wgpu::Texture,
    buffer: wgpu::Buffer,
}
impl ReceiverProbe {
    fn new(gpu: &render::Gpu, shadow: &ShadowGpu) -> Self {
        let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("fixed world receivers shadow temporal oracle"),source:wgpu::ShaderSource::Wgsl(format!("{}\n{}",include_str!("../src/shadow_sampling.wgsl"),r#"
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
@fragment fn fragment(@builtin(position)p:vec4<f32>)->@location(0)vec4<f32>{let receiver=vec3(-4.+p.x*0.1875,0.,-4.+p.y*0.1875);let shadow=cascade_shadow(receiver,vec3(0.,1.,0.));return vec4(vec3(shadow),1.);}
"#).into())});
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shadow receiver bindings"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 9,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 10,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 11,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                ],
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fixed receiver shadow bindings"),
            layout: &layout,
            entries: &[
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
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("receiver oracle pipeline"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("actual shadow sampler receiver"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            });
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("receiver oracle image"),
            size: wgpu::Extent3d {
                width: 128,
                height: 128,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("receiver image readback"),
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bind,
            texture,
            buffer,
        }
    }
    fn capture(&self, gpu: &render::Gpu) -> Vec<u8> {
        let view = self.texture.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fixed world shadow samples"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(128 * 4),
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
        read_buffer(gpu, &self.buffer)
    }
}
fn ray_hits_box(origin: [f32; 3], direction: [f32; 3]) -> bool {
    let mut near = 0_f32;
    let mut far = f32::INFINITY;
    let low = [6., 0., 6.];
    let high = [10., 4., 10.];
    for axis in 0..3 {
        if direction[axis].abs() < 0.000001 {
            if origin[axis] < low[axis] || origin[axis] > high[axis] {
                return false;
            }
            continue;
        }
        let a = (low[axis] - origin[axis]) / direction[axis];
        let b = (high[axis] - origin[axis]) / direction[axis];
        near = near.max(a.min(b));
        far = far.min(a.max(b));
    }
    far > near + 0.0001
}
#[test]
fn walking_grid_and_stepped_sun_match_independent_ray_visibility() {
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
                max_quads: 512,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &box_mesh(),
                &mut UploadBudget::new(10000),
            )
            .unwrap();
        let config = ShadowConfig {
            resolution: 128,
            half_widths: [16., 32., 64.],
            depth_half_extent: 32.,
            ..ShadowConfig::default()
        };
        let sun = [0.3, 0.8, 0.2];
        let mut shadow = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            config,
            [8., 0., 8.],
            sun,
        )
        .unwrap();
        for _ in 0..3 {
            render_shadow(&gpu, &mut shadow);
        }
        let probe = ReceiverProbe::new(&gpu, &shadow);
        let baseline = probe.capture(&gpu);
        let sampling = shadow.cache().sampling();
        let direction = [sampling.sun[0], sampling.sun[1], sampling.sun[2]];
        let mut dark = 0;
        let mut lit = 0;
        for y in 0..128 {
            for x in 0..128 {
                let p = [
                    -4. + (x as f32 + 0.5) * 0.1875,
                    0.,
                    -4. + (y as f32 + 0.5) * 0.1875,
                ];
                let witnesses =
                    [[-0.4, -0.4], [-0.4, 0.4], [0.4, -0.4], [0.4, 0.4], [0., 0.]].map(|offset| {
                        ray_hits_box([p[0] + offset[0], 0., p[2] + offset[1]], direction)
                    });
                let value = baseline[(x + y * 128) * 4];
                if witnesses.iter().all(|v| *v) {
                    assert!(
                        value <= 64,
                        "shadow ray oracle expects occlusion at {p:?}, got{value}"
                    );
                    dark += 1;
                } else if witnesses.iter().all(|v| !*v) {
                    assert!(
                        value >= 191,
                        "ray oracle expects sky visibility at {p:?}, got{value}"
                    );
                    lit += 1;
                }
            }
        }
        assert!(
            dark > 100 && lit > 10000,
            "constant image must fail independent visibility oracle {dark}/{lit}"
        );
        let mut maximum_flicker = 0_f64;
        let mut actual_updates = 0;
        for step in 1..=32 {
            shadow
                .update_view([8. + step as f32 * 0.1, 0., 8. + step as f32 * 0.07], sun)
                .unwrap();
            actual_updates += render_shadow(&gpu, &mut shadow).cascade_updates;
            let image = probe.capture(&gpu);
            let difference: u64 = image
                .chunks_exact(4)
                .zip(baseline.chunks_exact(4))
                .map(|(a, b)| u64::from(a[0].abs_diff(b[0])))
                .sum();
            let permille = difference as f64 / (128. * 128. * 255.) * 1000.;
            maximum_flicker = maximum_flicker.max(permille);
        }
        assert!(
            actual_updates > 10,
            "walking fixture did not cross texel snap boundaries"
        );
        assert!(
            maximum_flicker <= 0.02,
            "world-fixed shadow mask flicker {maximum_flicker}‰ exceeds H5 0.02‰"
        );
        // A real sun step must alter the projected shadow, then become stable again.
        shadow
            .update_view([11.2, 0., 10.24], [0.6, 0.8, -0.2])
            .unwrap();
        for _ in 0..3 {
            render_shadow(&gpu, &mut shadow);
        }
        let changed = probe.capture(&gpu);
        assert_ne!(changed, baseline, "sun step did not rotate actual shadow");
        assert_eq!(render_shadow(&gpu, &mut shadow).cascade_updates, 0);
        assert_eq!(probe.capture(&gpu), changed);
        println!(
            "H5 walking32frames updates={actual_updates},worldReceiverFlickerPermille={maximum_flicker},rayCertifiedShadow={dark},rayCertifiedSky={lit}; sunStepChanged=true"
        );
    });
}

#[test]
fn overlay_alias_and_model_uniform_headers_sample_actual_gpu_light() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut lights = LightVolumeGpu::new(&gpu.device, 4).unwrap();
        let base = PageHandle {
            slot: 0,
            generation: 1,
        };
        let overlay = PageHandle {
            slot: 1,
            generation: 1,
        };
        let model = PageHandle {
            slot: 2,
            generation: 1,
        };
        let red = PackedLight::new(0, 15, [15, 0, 0]).unwrap();
        let green = PackedLight::new(0, 15, [0, 15, 0]).unwrap();
        let blue = PackedLight::new(0, 15, [0, 0, 15]).unwrap();
        let mut budget = UploadBudget::new(1000000);
        lights
            .upload_page(&gpu.queue, base, 1, &[red; LIGHT_SAMPLES], &mut budget)
            .unwrap();
        assert_eq!(
            lights
                .alias_page(&gpu.queue, overlay, base, &mut budget)
                .unwrap(),
            4
        );
        assert_eq!(
            lights
                .upload_uniform(&gpu.queue, model, 1, blue, &mut budget)
                .unwrap(),
            4
        );
        assert_eq!(
            lights.alias_page(
                &gpu.queue,
                PageHandle {
                    slot: 3,
                    generation: 1
                },
                overlay,
                &mut budget
            ),
            Err(LightUploadError::StaleHandle)
        );
        assert_eq!(
            lights.upload_patch(
                &gpu.queue,
                model,
                2,
                &[LightUpdate {
                    index: 1,
                    value: red
                }],
                &mut budget
            ),
            Err(LightUploadError::StaleHandle)
        );
        let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("actual packed light header shader oracle"),source:wgpu::ShaderSource::Wgsl(format!("{}\n{}",include_str!("../src/light_volume.wgsl"),r#"
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
@fragment fn fragment(@builtin(position)p:vec4<f32>)->@location(0)vec4<f32>{return vec4(sample_page_light(u32(p.x),vec3(8.,8.,8.),vec3(0.,1.,0.)).rgb,1.);}
"#).into())});
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("light storage read oracle"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 12,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("light volume binding"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 12,
                resource: lights.buffer().as_entire_binding(),
            }],
        });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("light header oracle layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("light header oracle"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            });
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("three light roles"),
            size: wgpu::Extent3d {
                width: 3,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("three light colors readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let capture = || {
            let view = texture.create_view(&Default::default());
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("actual light roles read"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 3,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit([encoder.finish()]);
            read_buffer(&gpu, &buffer)[..12].to_vec()
        };
        assert_eq!(capture(), [255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255]);
        lights
            .upload_page(&gpu.queue, base, 2, &[green; LIGHT_SAMPLES], &mut budget)
            .unwrap();
        assert_eq!(capture(), [0, 255, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]);
        assert_eq!(
            lights
                .alias_page(&gpu.queue, overlay, base, &mut budget)
                .unwrap(),
            0
        );
        assert_eq!(lights.take_upload_bytes(), 2 * LIGHT_PAGE_BYTES + 8);
    });
}

#[test]
fn fixed_edit_shell_casts_the_same_shadow_as_final_greedy_geometry() {
    pollster::block_on(async {
        use render::terrain_mesh::{OwnerPatch, mesh_cell_patch};
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
                max_pages: 8,
                max_quads: 65536,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mut cells = [0; 4096];
        for y in 0..4 {
            for z in 6..10 {
                for x in 6..10 {
                    cells[x + 16 * (z + 16 * y)] = 1;
                }
            }
        }
        let build = |cells: &[u32; 4096]| {
            let mut mesh = PageMesh::default();
            mesh_page(
                &PageHalo::from_page(cells, 0),
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
            mesh
        };
        let old = build(&cells);
        let mut budget = UploadBudget::new(1000000);
        let base = terrain
            .upload_page(&gpu.queue, None, 1, translation([0.; 3]), &old, &mut budget)
            .unwrap();
        let config = ShadowConfig {
            resolution: 128,
            half_widths: [16., 32., 64.],
            depth_half_extent: 32.,
            max_updates_per_frame: 3,
            ..ShadowConfig::default()
        };
        let mut shadow = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            config,
            [8., 0., 8.],
            [0., 1., 0.],
        )
        .unwrap();
        render_shadow(&gpu, &mut shadow);
        let before = depths(&gpu, &shadow);
        cells[7 + 16 * (7 + 16 * 3)] = 0;
        let full = build(&cells);
        let halo = PageHalo::from_page(&cells, 0);
        let mut mask = [0u32; 128];
        let mut updates = Vec::new();
        let mut patch = PageMesh::with_capacity([6; 3]);
        for y in 2..=4 {
            for z in 6..=8 {
                for x in 6..=8 {
                    let owner = x + 16 * z + 256 * y;
                    mask[owner / 32] |= 1 << (owner % 32);
                    mesh_cell_patch(
                        &[[x as u8, y as u8, z as u8]],
                        |p| {
                            if halo.get(p[0], p[1], p[2]) == 0 {
                                CellMaterial::EMPTY
                            } else {
                                CellMaterial::opaque(3)
                            }
                        },
                        &mut patch,
                    )
                    .unwrap();
                    updates.push(OwnerPatch::from_mesh(owner as u16, &patch).unwrap());
                }
            }
        }
        updates.sort_by_key(|p| p.owner);
        let overlay = terrain
            .publish_owner_patches(
                &gpu.queue,
                base,
                None,
                2,
                translation([0.; 3]),
                &mask,
                &updates,
                &mut budget,
            )
            .unwrap();
        shadow.invalidate_bounds(WorldBounds::new([6., 2., 6.], [9., 5., 9.]).unwrap());
        render_shadow(&gpu, &mut shadow);
        let fast = depths(&gpu, &shadow);
        assert_ne!(fast, before, "edit shell did not alter actual shadow");
        terrain
            .finish_edit(
                &gpu.queue,
                base,
                3,
                translation([0.; 3]),
                &full,
                &[overlay],
                &mut budget,
            )
            .unwrap();
        shadow.invalidate_all();
        render_shadow(&gpu, &mut shadow);
        let final_depth = depths(&gpu, &shadow);
        assert_eq!(
            fast, final_depth,
            "fixed H1 shadow shell must match final full geometry"
        );
    });
}
