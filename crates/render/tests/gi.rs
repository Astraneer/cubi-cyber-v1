#![cfg(not(target_arch = "wasm32"))]
use render::gi::{
    self, CellUpdate, GiConfig, GiError, GiGpu, GiVoxel, Priority, ProbeSample, RAYS,
};
use render::terrain_gpu::UploadBudget;

fn read(gpu: &render::Gpu, source: &wgpu::Buffer) -> Vec<u8> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("GI numeric oracle readback"),
        size: source.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(source, 0, &buffer, 0, source.size());
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
fn probes(gpu: &render::Gpu, gi: &GiGpu) -> Vec<ProbeSample> {
    read(gpu, gi.probe_buffer())
        .chunks_exact(128)
        .map(bytemuck::pod_read_unaligned)
        .collect()
}
fn statistics(gpu: &render::Gpu, gi: &GiGpu) -> [u32; 4] {
    let bytes = read(gpu, gi.statistics_buffer());
    std::array::from_fn(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
}
fn config(priority: Priority) -> GiConfig {
    GiConfig {
        world_origin_page: [0; 3],
        world_pages: [1; 3],
        probe_origin: [1.5; 3],
        probe_counts: [4; 3],
        probe_spacing: 2.0,
        rays_per_probe: 32,
        max_steps: 64,
        max_distance: 24.0,
        max_jobs: 64,
        priority,
    }
}
fn room(closed: bool, radiance: [f32; 3]) -> [GiVoxel; 4096] {
    let mut world = [GiVoxel::EMPTY; 4096];
    for y in 0..16 {
        for z in 0..16 {
            world[12 + 16 * (z + 16 * y)] = GiVoxel::opaque_reflected(radiance).unwrap();
            if closed {
                world[8 + 16 * (z + 16 * y)] = GiVoxel::opaque_reflected([0.0; 3]).unwrap();
            }
        }
    }
    world
}
fn plane(x: u16, value: GiVoxel) -> Vec<CellUpdate> {
    let mut updates = Vec::with_capacity(256);
    for y in 0..16u16 {
        for z in 0..16u16 {
            updates.push(CellUpdate {
                local: x + 16 * (z + 16 * y),
                value,
            });
        }
    }
    updates
}
fn tick(
    gpu: &render::Gpu,
    gi: &mut GiGpu,
    tick: u64,
    rays: u32,
    budget: &mut UploadBudget,
) -> gi::GiWork {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = gi.trace_tick(&gpu.queue, &mut encoder, tick, budget, &mut { rays });
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    work
}

// Independent analytic ray/box intersection, not a second voxel DDA.
fn analytic_wall(origin: [f32; 3], source: [f32; 3]) -> [[f64; 4]; 3] {
    let mut coefficients = [[0.0; 4]; 3];
    for ray in (0..64).step_by(2) {
        let direction = RAYS[ray];
        if direction[0] <= 0.0 {
            continue;
        }
        let distance = (12.0 - f64::from(origin[0])) / f64::from(direction[0]);
        if !(0.0..24.0).contains(&distance) {
            continue;
        }
        let y = f64::from(origin[1]) + distance * f64::from(direction[1]);
        let z = f64::from(origin[2]) + distance * f64::from(direction[2]);
        if !(0.0..16.0).contains(&y) || !(0.0..16.0).contains(&z) {
            continue;
        }
        let basis = [
            0.2820947918,
            0.4886025119 * f64::from(direction[1]),
            0.4886025119 * f64::from(direction[2]),
            0.4886025119 * f64::from(direction[0]),
        ];
        for channel in 0..3 {
            for (coefficient, basis_value) in basis.iter().enumerate() {
                let convolution = if coefficient == 0 {
                    std::f64::consts::PI
                } else {
                    2.0 * std::f64::consts::PI / 3.0
                };
                coefficients[channel][coefficient] += f64::from(source[channel])
                    * basis_value
                    * convolution
                    * 4.0
                    * std::f64::consts::PI
                    / 32.0;
            }
        }
    }
    coefficients
}
fn change_fraction(
    samples: &[ProbeSample],
    expected: &[[[f64; 4]; 3]],
    visible: std::ops::Range<usize>,
) -> f64 {
    let mut remaining = 0.0;
    let mut total = 0.0;
    for index in visible {
        let actual = [
            samples[index].sh_r,
            samples[index].sh_g,
            samples[index].sh_b,
        ];
        for (actual_channel, expected_channel) in actual.iter().zip(&expected[index]) {
            for (actual_value, target) in actual_channel.iter().zip(expected_channel) {
                total += target.abs();
                remaining += (f64::from(*actual_value) - target).abs();
            }
        }
    }
    assert!(total > 0.0);
    (1.0 - remaining / total).clamp(0.0, 1.0)
}

#[test]
fn edited_visible_gpu_probes_reach_h6_within_six_ticks_and_fifo_fails() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let mut budget = UploadBudget::new(2_000_000);
        let mut priority = GiGpu::new(
            &gpu.device,
            &gpu.queue,
            config(Priority::EditedVisible),
            &mut budget,
        )
        .unwrap();
        let mut fifo = GiGpu::new(
            &gpu.device,
            &gpu.queue,
            config(Priority::FifoControl),
            &mut budget,
        )
        .unwrap();
        let reflected = GiVoxel::opaque_reflected([1.0, 0.25, 0.0])
            .unwrap()
            .reflected();
        for gi in [&mut priority, &mut fifo] {
            assert_eq!(gi.probe_count(), 64);
            for index in 40..64 {
                gi.set_visible(index, true).unwrap();
            }
            gi.upload_page(
                &gpu.queue,
                [0; 3],
                1,
                &room(true, [1.0, 0.25, 0.0]),
                0,
                &mut budget,
            )
            .unwrap();
            let warm = tick(&gpu, gi, 0, 64 * 38, &mut budget);
            assert_eq!(warm.updated_probes, 64);
            assert_eq!(warm.rays, 64 * 38);
            assert_eq!(gi.pending(), 0);
            assert!(
                probes(&gpu, gi).iter().all(|p| p.sh_r == [0.0; 4]),
                "reflected light leaked through the closed wall"
            );
        }
        let initial = probes(&gpu, &priority);
        let expected = initial
            .iter()
            .map(|p| analytic_wall([p.position[0], p.position[1], p.position[2]], reflected))
            .collect::<Vec<_>>();
        for gi in [&mut priority, &mut fifo] {
            let bytes = gi
                .upload_cells(
                    &gpu.queue,
                    [0; 3],
                    2,
                    &plane(8, GiVoxel::EMPTY),
                    1,
                    &mut budget,
                )
                .unwrap();
            assert_eq!(bytes, 256 * 4 + 96);
        }
        let mut tick90 = None;
        let mut fractions = Vec::new();
        let mut maximum_steps = 0;
        for time in 1..=6 {
            for gi in [&mut priority, &mut fifo] {
                let work = tick(&gpu, gi, time, 4 * 38, &mut budget);
                assert_eq!(work.updated_probes, 4);
                assert_eq!(work.rays, 152);
                assert_eq!(work.upload_bytes, 64);
                let stats = statistics(&gpu, gi);
                assert_eq!(stats[3], 4);
                assert!(u64::from(stats[0]) <= work.max_dda_steps);
                assert!(stats[0] >= work.rays);
                maximum_steps = maximum_steps.max(stats[0]);
            }
            let fraction = change_fraction(&probes(&gpu, &priority), &expected, 40..64);
            fractions.push(fraction);
            if fraction >= 0.9 && tick90.is_none() {
                tick90 = Some(time);
            }
        }
        let visible = probes(&gpu, &priority);
        let fifo_fraction = change_fraction(&probes(&gpu, &fifo), &expected, 40..64);
        assert!(
            tick90.is_some_and(|tick| tick <= 6),
            "H6 wall response did not reach 90%: {fractions:?}"
        );
        assert!(
            fifo_fraction < 0.1,
            "FIFO negative control unexpectedly met H6: {fifo_fraction}"
        );
        let mut maximum_error = 0.0f64;
        for index in 40..64 {
            let actual = [
                visible[index].sh_r,
                visible[index].sh_g,
                visible[index].sh_b,
            ];
            for (actual_channel, expected_channel) in actual.iter().zip(&expected[index]) {
                for (actual_value, expected_value) in actual_channel.iter().zip(expected_channel) {
                    maximum_error =
                        maximum_error.max((f64::from(*actual_value) - expected_value).abs());
                }
            }
        }
        assert!(
            maximum_error < 0.0001,
            "GI DDA/SH differs from analytic ray-box reference: {maximum_error}"
        );
        assert!(visible[63].irradiance([1.0, 0.0, 0.0])[0] > 0.1);
        tick(&gpu, &mut priority, 8, 64 * 38, &mut budget);
        assert_eq!(priority.pending(), 0);
        let lit = probes(&gpu, &priority);
        priority
            .upload_cells(
                &gpu.queue,
                [0; 3],
                3,
                &plane(12, GiVoxel::opaque_reflected([0.0; 3]).unwrap()),
                10,
                &mut budget,
            )
            .unwrap();
        let old_energy = lit[40..]
            .iter()
            .flat_map(|p| p.sh_r)
            .map(f32::abs)
            .sum::<f32>();
        let mut lamp_tick90 = None;
        for time in 10..16 {
            tick(&gpu, &mut priority, time, 4 * 38, &mut budget);
            let remaining = probes(&gpu, &priority)[40..]
                .iter()
                .flat_map(|p| p.sh_r)
                .map(f32::abs)
                .sum::<f32>();
            if 1.0 - remaining / old_energy >= 0.9 && lamp_tick90.is_none() {
                lamp_tick90 = Some(time - 9);
            }
        }
        assert!(lamp_tick90.is_some_and(|tick| tick <= 6));
        tick(&gpu, &mut priority, 18, 64 * 38, &mut budget);
        let stable = read(&gpu, priority.probe_buffer());
        let before = budget.remaining_bytes;
        for time in 19..27 {
            let work = tick(&gpu, &mut priority, time, 64 * 38, &mut budget);
            assert_eq!(work.rays, 0);
            assert_eq!(work.passes, 0);
            assert_eq!(work.upload_bytes, 0);
            assert_eq!(
                read(&gpu, priority.probe_buffer()),
                stable,
                "static GI flickered"
            );
        }
        assert_eq!(budget.remaining_bytes, before);
        let _sampling = priority.sampling_buffer();
        let _default = GiConfig::default();
        println!(
            "GI_H6_GPU_OK probes=64 visible=24 rays_per_tick=152 wall_tick90={} lamp_off_tick90={} FIFO_fraction_at6={fifo_fraction:.6} max_analytic_SH_error={maximum_error:.8} max_actual_DDA_steps={maximum_steps} static_rays=0 static_upload_bytes=0 static_flicker=0",
            tick90.unwrap(),
            lamp_tick90.unwrap()
        );
    });
}

#[test]
fn unknown_and_obsolete_source_jobs_cannot_publish_light() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let mut budget = UploadBudget::new(2_000_000);
        let mut gi = GiGpu::new(
            &gpu.device,
            &gpu.queue,
            config(Priority::EditedVisible),
            &mut budget,
        )
        .unwrap();
        let mut unknown = room(false, [1.0, 0.0, 0.0]);
        for y in 0..16 {
            for z in 0..16 {
                unknown[8 + 16 * (z + 16 * y)] = GiVoxel::UNKNOWN;
            }
        }
        gi.upload_page(&gpu.queue, [0; 3], 1, &unknown, 0, &mut budget)
            .unwrap();
        tick(&gpu, &mut gi, 0, 64 * 38, &mut budget);
        assert!(probes(&gpu, &gi).iter().all(|p| p.sh_r == [0.0; 4]));
        assert!(
            statistics(&gpu, &gi)[2] > 0,
            "Unknown termination was not exercised"
        );
        gi.upload_cells(
            &gpu.queue,
            [0; 3],
            2,
            &plane(8, GiVoxel::EMPTY),
            1,
            &mut budget,
        )
        .unwrap();
        tick(&gpu, &mut gi, 1, 64 * 38, &mut budget);
        let red = probes(&gpu, &gi);
        assert!(red.iter().any(|p| p.sh_r[0] > 0.1));
        gi.upload_cells(
            &gpu.queue,
            [0; 3],
            3,
            &plane(12, GiVoxel::opaque_reflected([0.0; 3]).unwrap()),
            2,
            &mut budget,
        )
        .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let old = gi.trace_tick(&gpu.queue, &mut encoder, 2, &mut budget, &mut (64 * 38));
        assert_eq!(old.updated_probes, 64);
        gi.upload_cells(
            &gpu.queue,
            [0; 3],
            4,
            &plane(12, GiVoxel::opaque_reflected([0.0, 0.0, 1.0]).unwrap()),
            3,
            &mut budget,
        )
        .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        assert_eq!(
            statistics(&gpu, &gi)[3],
            0,
            "obsolete source jobs replaced live probes"
        );
        let retained = probes(&gpu, &gi);
        assert!(
            retained
                .iter()
                .zip(&red)
                .all(|(a, b)| a.sh_r == b.sh_r && a.stamps == b.stamps)
        );
        assert_eq!(gi.pending(), 64);
        tick(&gpu, &mut gi, 3, 64 * 38, &mut budget);
        let blue = probes(&gpu, &gi);
        assert!(blue.iter().all(|p| p.sh_r == [0.0; 4]));
        assert!(blue.iter().any(|p| p.sh_b[0] > 0.1));
        assert!(blue.iter().all(|p| p.stamps[1] == gi.source_revision()));
        let before = budget.remaining_bytes;
        assert_eq!(
            gi.upload_cells(
                &gpu.queue,
                [0; 3],
                3,
                &plane(12, GiVoxel::EMPTY),
                4,
                &mut budget
            ),
            Err(GiError::StaleVersion)
        );
        assert_eq!(budget.remaining_bytes, before);
        let mut denied = UploadBudget::new(0);
        assert_eq!(
            gi.upload_cells(
                &gpu.queue,
                [0; 3],
                5,
                &plane(12, GiVoxel::EMPTY),
                4,
                &mut denied
            ),
            Err(GiError::UploadBudget)
        );
        gi.invalidate(4);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let deferred = gi.trace_tick(&gpu.queue, &mut encoder, 4, &mut budget, &mut 0);
        assert_eq!(deferred.updated_probes, 0);
        assert_eq!(gi.pending(), 64);
        assert!(GiVoxel::opaque_reflected([9.0, 0.0, 0.0]).is_none());
        assert!(GiVoxel::opaque_reflected([f32::NAN, 0.0, 0.0]).is_none());
        println!(
            "GI_STALE_GPU_OK unknown_light_leaks=0 obsolete_published=0 fresh_source_revision={} deferred_pending=64",
            gi.source_revision()
        );
    });
}

#[test]
fn probe_receiver_conserves_lambert_energy_in_a_uniform_room() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = render::Gpu::request(&instance, None, true)
            .await
            .expect("software Vulkan required");
        let mut budget = UploadBudget::new(2_000_000);
        let config = GiConfig {
            rays_per_probe: 64,
            ..config(Priority::EditedVisible)
        };
        let mut gi = GiGpu::new(&gpu.device, &gpu.queue, config, &mut budget).unwrap();
        let surface = GiVoxel::opaque_reflected([1.0, 0.5, 0.25]).unwrap();
        let mut room = [GiVoxel::EMPTY; 4096];
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    if x == 0 || x == 15 || y == 0 || y == 15 || z == 0 || z == 15 {
                        room[x + 16 * (z + 16 * y)] = surface;
                    }
                }
            }
        }
        gi.upload_page(&gpu.queue, [0; 3], 1, &room, 0, &mut budget)
            .unwrap();
        tick(&gpu, &mut gi, 0, 64 * 70, &mut budget);
        assert_eq!(statistics(&gpu, &gi)[2], 0);
        let source = format!(
            "{}\n{}",
            include_str!("../src/gi_sampling.wgsl"),
            r#"
struct Vertex{@builtin(position)position:vec4<f32>,@location(0)uv:vec2<f32>};
@vertex fn vertex(@builtin(vertex_index)index:u32)->Vertex{let p=array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));var output:Vertex;output.position=vec4(p[index],0.0,1.0);output.uv=p[index]*0.5+vec2(0.5);return output;}
@fragment fn fragment(input:Vertex)->@location(0)vec4<f32>{let world=vec3(2.0+input.uv.x*4.0,4.0,2.0+input.uv.y*4.0);return vec4(reflected_irradiance(world,vec3(0.0,1.0,0.0))/3.1415926536,1.0);}
"#
        );
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("GI Lambert energy receiver"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("GI receiver-only sampling layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 13,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 14,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("actual reflected GI probes"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 13,
                    resource: gi.sampling_buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 14,
                    resource: gi.probe_buffer().as_entire_binding(),
                },
            ],
        });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("GI receiver pipeline"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("GI Lambert radiance image"),
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
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            });
        let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("GI uniform-room HDR reference"),
            size: wgpu::Extent3d {
                width: 64,
                height: 32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GI receiver HDR pixels"),
            size: 64 * 32 * 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view = output.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("GI energy oracle image"),
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
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(64 * 16),
                    rows_per_image: Some(32),
                },
            },
            wgpu::Extent3d {
                width: 64,
                height: 32,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        rx.recv().unwrap().unwrap();
        let bytes = buffer.slice(..).get_mapped_range();
        let expected = surface.reflected();
        let mut error = 0.0f32;
        for pixel in bytes.chunks_exact(16) {
            for channel in 0..3 {
                let value =
                    f32::from_le_bytes(pixel[channel * 4..channel * 4 + 4].try_into().unwrap());
                error = error.max((value - expected[channel]).abs());
            }
        }
        assert!(
            error < 0.0001,
            "probe convolution or Lambert normalization created/lost reflected energy: {error}"
        );
        println!(
            "GI_RECEIVER_GPU_OK hdr_pixels=2048 max_energy_error={error:.8} escaped_sky=excluded direct_emission=excluded"
        );
    });
}
