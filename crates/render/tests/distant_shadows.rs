#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    distant_shadows::{DistantShadowGpu, HeightShadowPolicy},
    height_gpu::{HeightGpu, HeightGpuConfig},
    height_tile::{HeightTile, HeightTileKey},
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::{LodProxyGpu, ProxyConfig, ProxyPayload},
    shadows::{
        DynamicShadowCaster, ShadowConfig, ShadowGpu, ShadowNode, ShadowNodeTimings, ShadowWork,
    },
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget, translation},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn tile(x: i32, z: i32, height: i32, layer: u16) -> HeightTile {
    let mut known = [255; 37];
    known[36] = 1;
    HeightTile {
        key: HeightTileKey {
            space: 0,
            level: 0,
            x,
            z,
        },
        source: 1,
        heights: [height; 289],
        known,
        atlas: [layer; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
    }
}
fn building(kind: SurfaceKind, material: u16) -> ProxyPayload {
    let mut page = PublicPage::filled(PublicVoxel::Air);
    for y in 0..16 {
        for z in 4..12 {
            for x in 4..12 {
                page.set(
                    [x, y, z],
                    PublicVoxel::Solid {
                        material,
                        tint: u32::MAX,
                        kind,
                    },
                );
            }
        }
    }
    ProxyPayload::from_public(&MaskedRayProxy::from_protected(&page, 4), 186).unwrap()
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let result = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    result
}
fn frame(gpu: &render::Gpu, shadow: &mut ShadowGpu, far: &mut DistantShadowGpu) -> ShadowWork {
    frame_timed(gpu, shadow, far, None, None)
}
fn frame_timed(
    gpu: &render::Gpu,
    shadow: &mut ShadowGpu,
    far: &mut DistantShadowGpu,
    timing: Option<(&wgpu::Buffer, u64)>,
    nodes: Option<(&ShadowNodeTimings, &wgpu::Buffer, u64)>,
) -> ShadowWork {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    if let Some((nodes, _, _)) = nodes {
        nodes.begin_frame();
    }
    far.set_diagnostic_timings(nodes.map(|(nodes, _, _)| nodes.clone()));
    let work = shadow
        .encode_dynamic_with_diagnostics(
            &gpu.queue,
            &mut encoder,
            far,
            nodes.map(|(nodes, _, _)| nodes),
        )
        .unwrap();
    if let Some((target, offset)) = timing {
        let source = shadow
            .timing_source()
            .expect("actual timestamp queries required for far-shadow cost");
        assert_eq!(source.count, 1);
        encoder.copy_buffer_to_buffer(source.buffer, 0, target, offset, 16);
    }
    if let Some((nodes, target, offset)) = nodes {
        nodes.copy_into(&mut encoder, target, offset);
    }
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    work
}
// Only this diagnostic test requests native in-pass timestamps. When unavailable,
// the original whole-graph/functional/cost assertions still run unchanged.
async fn diagnostic_gpu(instance: &wgpu::Instance) -> render::Gpu {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: true,
            compatible_surface: None,
        })
        .await
        .unwrap();
    let features = if adapter.features().contains(ShadowNodeTimings::FEATURES) {
        ShadowNodeTimings::FEATURES
    } else {
        adapter.features() & wgpu::Features::TIMESTAMP_QUERY
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("far shadow diagnostic GPU"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        })
        .await
        .unwrap();
    render::Gpu {
        adapter,
        device,
        queue,
    }
}
fn report_nodes(gpu: &render::Gpu, data: &[u8], masks: &[[u32; 2]; 12], total: [f64; 2]) {
    let period = f64::from(gpu.queue.get_timestamp_period());
    for (mode, name) in ["sparse", "full"].into_iter().enumerate() {
        let mut ns = [0f64; ShadowNode::ALL.len()];
        let mut samples = [0u32; ShadowNode::ALL.len()];
        for (frame, masks) in masks.iter().enumerate() {
            for cascade in 0..3 {
                for (index, node) in ShadowNode::ALL.into_iter().enumerate() {
                    let slot = node.slot(cascade);
                    if masks[mode] & (1 << slot) == 0 {
                        continue;
                    }
                    let offset = ((frame * 2 + mode) * ShadowNodeTimings::SLOTS + slot) * 16;
                    let start = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let end = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                    assert!(
                        end >= start,
                        "{name} {node:?} timestamp order at frame{frame}/cascade{cascade}"
                    );
                    ns[index] += (end - start) as f64 * period;
                    samples[index] += 1;
                }
            }
        }
        let value = |node: ShadowNode| ns[node as usize];
        let details = ShadowNode::ALL
            .into_iter()
            .map(|node| {
                format!(
                    "{}={:.0}/{}",
                    node.label(),
                    value(node),
                    samples[node as usize]
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "\n::notice title=H5 diagnostic SOME nodes {name}::total_ns={:.0} clear_ns={:.0} restore_ns={:.0} cull_ns={:.0} draw_ns={:.0} nodes_ns={:.0} {details}",
            total[mode],
            value(ShadowNode::Clear),
            value(ShadowNode::Restore),
            value(ShadowNode::StaticCull) + value(ShadowNode::Cull),
            value(ShadowNode::StaticDraw)
                + value(ShadowNode::HeightDraw)
                + value(ShadowNode::OpaqueDraw)
                + value(ShadowNode::CutoutDraw),
            ns.iter().sum::<f64>(),
        );
    }
}
fn whole_ns(gpu: &render::Gpu, timings: &wgpu::Buffer) -> [f64; 2] {
    let mut ns = [0f64; 2];
    for row in read(gpu, timings).chunks_exact(32) {
        for (k, sum) in ns.iter_mut().enumerate() {
            let start = u64::from_le_bytes(row[k * 16..k * 16 + 8].try_into().unwrap());
            let end = u64::from_le_bytes(row[k * 16 + 8..k * 16 + 16].try_into().unwrap());
            *sum += (end - start) as f64 * f64::from(gpu.queue.get_timestamp_period());
        }
    }
    assert!(ns.iter().all(|v| v.is_finite() && *v > 0.));
    ns
}
fn depth(gpu: &render::Gpu, shadow: &ShadowGpu) -> Vec<u8> {
    depth_texture(gpu, shadow, shadow.sampled_texture())
}
fn depth_texture(gpu: &render::Gpu, shadow: &ShadowGpu, texture: &wgpu::Texture) -> Vec<u8> {
    let n = shadow.cache().config().resolution;
    let row = n * 4;
    assert_eq!(row % 256, 0);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("distant caster full depth oracle"),
        size: u64::from(row) * u64::from(n) * 3,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(n),
            },
        },
        wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: 3,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
}
struct Receiver {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    output: wgpu::Buffer,
}
impl Receiver {
    fn new(gpu: &render::Gpu, shadow: &ShadowGpu) -> Self {
        let source = format!(
            "{}\n{}",
            include_str!("../src/shadow_sampling.wgsl"),
            r#"
@group(0)@binding(0)var<storage,read_write>output:array<f32>;
@compute @workgroup_size(8,8)fn probe(@builtin(global_invocation_id)p:vec3<u32>){if p.x>=128u||p.y>=128u{return;}let world=vec3((f32(p.x)+.5)*.5,0.,(f32(p.y)+.5)*.5);output[p.x+128u*p.y]=cascade_shadow(world,vec3(0.,1.,0.));}
"#
        );
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("independent world ground receivers"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline = gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("actual ground shadow visibility"),
                layout: None,
                module: &shader,
                entry_point: Some("probe"),
                compilation_options: Default::default(),
                cache: None,
            });
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow ground visibility values"),
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
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
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: self.output.size(),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &readback, 0, self.output.size());
        gpu.queue.submit([encoder.finish()]);
        read(gpu, &readback)
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }
}
// Independent f64 analytic rays. Bounds are authored in world coordinates, not
// reconstructed from proxy words or GPU matrices. Insets exclude PCF edges.
fn visibility_witnesses(values: &[f32], hill: bool) -> (usize, usize) {
    let (mut dark, mut clear) = (0, 0);
    for z in 0..128 {
        for x in 0..128 {
            let px = (x as f64 + 0.5) * 0.5;
            let pz = (z as f64 + 0.5) * 0.5;
            let inside_hill = hill && (4. ..12.).contains(&px) && (4. ..12.).contains(&pz);
            let inside_building = (10. ..22.).contains(&px) && (23. ..25.).contains(&pz);
            let definitely_clear = (36. ..60.).contains(&px)
                || (36. ..60.).contains(&pz)
                || ((4. ..12.).contains(&px) && (17. ..19.).contains(&pz));
            if inside_hill || inside_building {
                assert!(
                    values[x + 128 * z] < 1e-5,
                    "lost far caster at[{px},{pz}]:{}",
                    values[x + 128 * z]
                );
                dark += 1;
            }
            if definitely_clear {
                assert!(
                    (values[x + 128 * z] - 1.).abs() < 1e-5,
                    "false water/Unknown/protected caster at[{px},{pz}]:{}",
                    values[x + 128 * z]
                );
                clear += 1;
            }
        }
    }
    assert_eq!(dark, if hill { 352 } else { 96 });
    assert!(clear > 8000);
    (dark, clear)
}
#[test]
fn active_public_hill_and_building_cast_on_near_receivers_and_sparse_matches_full() {
    pollster::block_on(async {
        let instance = render::instance();
        let gpu = diagnostic_gpu(&instance).await;
        let mut atlas = AtlasImage::donor().unwrap();
        for mip in &mut atlas.mips {
            let bytes = mip.size as usize * mip.size as usize * 4;
            for (layer, alpha) in [(1, 0), (2, 255), (3, 127), (4, 128)] {
                for pixel in mip.rgba[layer * bytes..(layer + 1) * bytes].chunks_exact_mut(4) {
                    pixel[3] = alpha;
                }
            }
        }
        let terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            64,
            64,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 4,
                max_quads: 4096,
                occlusion: false,
            },
            &atlas,
        )
        .unwrap();
        let mut heights = HeightGpu::new(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig {
                max_tiles: 8,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        let mut proxies = LodProxyGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ProxyConfig {
                max_objects: 8,
                max_payload_words: 65536,
                atlas_layers: 186,
                instrument: false,
            },
            None,
        )
        .unwrap();
        let mut budget = UploadBudget::new(1_000_000);
        let hill = heights
            .upload(
                &gpu.queue,
                None,
                &tile(2, 0, 32, 0),
                0,
                [0; 3],
                true,
                &mut budget,
            )
            .unwrap();
        heights
            .upload(
                &gpu.queue,
                None,
                &tile(2, 1, 32, 1),
                0,
                [0; 3],
                true,
                &mut budget,
            )
            .unwrap(); // water layer: forbidden
        let mut unknown = tile(3, 2, 48, 0);
        unknown.known.fill(0);
        unknown.heights.fill(0);
        heights
            .upload(&gpu.queue, None, &unknown, 0, [0; 3], true, &mut budget)
            .unwrap();
        let mut protected = tile(3, 3, 48, 0);
        protected.protected.fill(255);
        heights
            .upload(&gpu.queue, None, &protected, 0, [0; 3], true, &mut budget)
            .unwrap();
        heights
            .upload(
                &gpu.queue,
                None,
                &tile(4, 2, 64, 0),
                0,
                [0; 3],
                false,
                &mut budget,
            )
            .unwrap(); // incomplete staging front
        let proxy = proxies
            .upload(
                &gpu.queue,
                None,
                1,
                translation([32., 16., 16.]),
                [0.; 4],
                &building(SurfaceKind::Opaque, 0),
                &mut budget,
            )
            .unwrap();
        let config = ShadowConfig {
            resolution: 2048,
            half_widths: [64., 128., 256.],
            depth_half_extent: 256.,
            max_updates_per_frame: 3,
            sun_step_radians: 1e-6,
            dynamic_casters: true,
            projected_bounds: true,
            ..ShadowConfig::default()
        };
        let mut sparse = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            config,
            [16., 0., 16.],
            [1., 1., 0.],
        )
        .unwrap();
        let mut full = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                projected_bounds: false,
                ..config
            },
            [16., 0., 16.],
            [1., 1., 0.],
        )
        .unwrap();
        let policy = HeightShadowPolicy::from_opaque_layers([0]).unwrap();
        let mut a = DistantShadowGpu::new(
            &gpu.device,
            heights.shadow_source(),
            proxies.shadow_source(),
            &sparse,
            policy,
        )
        .unwrap();
        let mut b = DistantShadowGpu::new(
            &gpu.device,
            heights.shadow_source(),
            proxies.shadow_source(),
            &full,
            policy,
        )
        .unwrap();
        assert_eq!(
            a.source_key(),
            [0, 0],
            "uncertified front was enabled during loading"
        );
        a.set_enabled(true);
        b.set_enabled(true);
        let source_key = a.source_key();
        let mut mismatched = heights.shadow_source();
        mismatched.max_tiles += 1;
        assert_eq!(
            a.synchronize_sources(mismatched, proxies.shadow_source()),
            Err(render::distant_shadows::Error::SourceNeedsReprepare)
        );
        assert_eq!(a.source_key(), source_key);
        assert_eq!(a.startup_pipeline_compilations(), 6);
        let first = frame(&gpu, &mut sparse, &mut a);
        frame(&gpu, &mut full, &mut b);
        assert_eq!(depth(&gpu, &sparse), depth(&gpu, &full));
        assert_eq!(first.dynamic_cull_dispatches, 6);
        assert_eq!(first.dynamic_draw_calls, 12);
        let receiver = Receiver::new(&gpu, &sparse);
        let witnesses = visibility_witnesses(&receiver.capture(&gpu), true);
        // Updates to the primary camera's compacted lists cannot affect shadow work.
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            let work = sparse
                .encode_dynamic(&gpu.queue, &mut encoder, &mut a)
                .unwrap();
            assert_eq!(work.dynamic_cull_dispatches + work.dynamic_draw_calls, 0);
            assert_eq!(work.uniform_upload_bytes, 0);
        }
        let idle = scope.finish();
        assert_eq!(idle.allocating_calls(), 0);
        assert_eq!(idle.dealloc_calls, 0);
        gpu.queue.submit([encoder.finish()]);
        // The 3D near source clips the same surface as color, independently of
        // the player's compacted list. Its revision must revoke the old shadow.
        heights
            .update_near_cover(
                &gpu.queue,
                Some([[2, 1, 0], [3, 3, 1]]),
                &[],
                [0; 3],
                &mut budget,
            )
            .unwrap();
        a.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        b.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        frame(&gpu, &mut sparse, &mut a);
        full.invalidate_all();
        frame(&gpu, &mut full, &mut b);
        assert_eq!(depth(&gpu, &sparse), depth(&gpu, &full));
        visibility_witnesses(&receiver.capture(&gpu), false);
        heights
            .update_near_cover(&gpu.queue, None, &[], [0; 3], &mut budget)
            .unwrap();
        a.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        b.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        frame(&gpu, &mut sparse, &mut a);
        full.invalidate_all();
        frame(&gpu, &mut full, &mut b);
        assert_eq!(depth(&gpu, &sparse), depth(&gpu, &full));
        visibility_witnesses(&receiver.capture(&gpu), true);
        heights
            .set_enabled_batch(&gpu.queue, &[(hill, false)], &mut budget)
            .unwrap();
        a.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        b.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        let removal = frame(&gpu, &mut sparse, &mut a);
        full.invalidate_all();
        frame(&gpu, &mut full, &mut b);
        assert_eq!(depth(&gpu, &sparse), depth(&gpu, &full));
        visibility_witnesses(&receiver.capture(&gpu), false);
        assert_eq!(removal.gpu_restore_mask, 7);
        // Binary directional depth has no A2C: both transparent coverage and
        // the exact 127/128 threshold are checked using real public quad data.
        for (version, material, casts) in [(2, 1, false), (3, 2, true), (4, 3, false), (5, 4, true)]
        {
            let payload = building(SurfaceKind::Cutout, material);
            assert!(payload.cutout_quads() > 0);
            proxies
                .upload(
                    &gpu.queue,
                    Some(proxy),
                    version,
                    translation([32., 16., 16.]),
                    [0.; 4],
                    &payload,
                    &mut budget,
                )
                .unwrap();
            a.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
                .unwrap();
            b.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
                .unwrap();
            frame(&gpu, &mut sparse, &mut a);
            full.invalidate_all();
            frame(&gpu, &mut full, &mut b);
            let actual = depth(&gpu, &sparse);
            assert_eq!(actual, depth(&gpu, &full));
            if casts {
                visibility_witnesses(&receiver.capture(&gpu), false);
            } else {
                assert!(
                    actual.iter().all(|b| *b == 0),
                    "transparent Cutout cast opaque depth"
                );
                assert!(receiver.capture(&gpu).iter().all(|v| *v == 1.));
            }
        }
        heights
            .set_enabled_batch(&gpu.queue, &[(hill, true)], &mut budget)
            .unwrap();
        a.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        b.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        // Every cascade follows current Sun. Both timings enclose the complete
        // graph, including caster compute and restore, with identical sources.
        let timings = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("far shadow whole interval timestamps"),
            size: 12 * 32,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Keep the original cost oracle free of all optional node queries.
        let mut original_work = [[ShadowWork::default(); 2]; 12];
        for i in 0u32..12 {
            let angle = std::f64::consts::FRAC_PI_4 + f64::from(i) * 0.0004;
            let sun = [angle.cos() as f32, angle.sin() as f32, 0.];
            sparse.update_view([16., 0., 16.], sun).unwrap();
            full.update_view([16., 0., 16.], sun).unwrap();
            full.invalidate_all();
            let work = frame_timed(
                &gpu,
                &mut sparse,
                &mut a,
                Some((&timings, u64::from(i) * 32)),
                None,
            );
            original_work[i as usize][0] = work;
            assert_eq!(work.pending_cascades, 0);
            assert_eq!(work.dynamic_copy_bytes, 0);
            original_work[i as usize][1] = frame_timed(
                &gpu,
                &mut full,
                &mut b,
                Some((&timings, u64::from(i) * 32 + 16)),
                None,
            );
            assert_eq!(
                depth(&gpu, &sparse),
                depth(&gpu, &full),
                "current-Sun depth mismatch at frame{i}"
            );
            visibility_witnesses(&receiver.capture(&gpu), true);
        }
        let ns = whole_ns(&gpu, &timings);
        let adapter = gpu.adapter.get_info();
        println!(
            "\n::notice title=H5 original NONE TOTAL::backend={:?} adapter={} currentSun12 resolution2048 maps96MiB whole_ns_sparse={} whole_ns_full={} ratio={:.6} exactDepth=BITWISE independentReceivers=PASS copies=0 strictOracle=ORIGINAL",
            adapter.backend,
            adapter.name,
            ns[0],
            ns[1],
            ns[1] / ns[0]
        );
        if let Some(nodes) = ShadowNodeTimings::new(&gpu.device) {
            let diagnostic_total = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("separate diagnostic shadow whole timestamps"),
                size: 12 * 32,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let node_readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("separate disjoint shadow node timestamp readback"),
                size: 12 * 2 * ShadowNodeTimings::CAPTURE_BYTES,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            // Restore the original starting Sun and empty prior caster depth.
            // This second, explicitly warm sweep must execute the same work,
            // but its query overhead never enters the original 2x assertion.
            let angle = std::f64::consts::FRAC_PI_4;
            let sun = [angle.cos() as f32, angle.sin() as f32, 0.];
            sparse.update_view([16., 0., 16.], sun).unwrap();
            full.update_view([16., 0., 16.], sun).unwrap();
            frame(&gpu, &mut sparse, &mut a);
            frame(&gpu, &mut full, &mut b);
            a.set_enabled(false);
            b.set_enabled(false);
            frame(&gpu, &mut sparse, &mut a);
            frame(&gpu, &mut full, &mut b);
            a.set_enabled(true);
            b.set_enabled(true);
            let mut node_masks = [[0u32; 2]; 12];
            for i in 0u32..12 {
                let angle = std::f64::consts::FRAC_PI_4 + f64::from(i) * 0.0004;
                let sun = [angle.cos() as f32, angle.sin() as f32, 0.];
                sparse.update_view([16., 0., 16.], sun).unwrap();
                full.update_view([16., 0., 16.], sun).unwrap();
                full.invalidate_all();
                let work = frame_timed(
                    &gpu,
                    &mut sparse,
                    &mut a,
                    Some((&diagnostic_total, u64::from(i) * 32)),
                    Some((
                        &nodes,
                        &node_readback,
                        u64::from(i) * 2 * ShadowNodeTimings::CAPTURE_BYTES,
                    )),
                );
                node_masks[i as usize][0] = nodes.written_slots();
                assert_eq!(
                    work, original_work[i as usize][0],
                    "sparse diagnostic changed work at frame{i}"
                );
                let work = frame_timed(
                    &gpu,
                    &mut full,
                    &mut b,
                    Some((&diagnostic_total, u64::from(i) * 32 + 16)),
                    Some((
                        &nodes,
                        &node_readback,
                        (u64::from(i) * 2 + 1) * ShadowNodeTimings::CAPTURE_BYTES,
                    )),
                );
                node_masks[i as usize][1] = nodes.written_slots();
                assert_eq!(
                    work, original_work[i as usize][1],
                    "full diagnostic changed work at frame{i}"
                );
                assert_eq!(
                    depth(&gpu, &sparse),
                    depth(&gpu, &full),
                    "diagnostic current-Sun depth mismatch at frame{i}"
                );
                visibility_witnesses(&receiver.capture(&gpu), true);
            }
            let diagnostic_ns = whole_ns(&gpu, &diagnostic_total);
            println!(
                "\n::notice title=H5 diagnostic SOME TOTAL::warm_reset sameSun12 sameWork=EXACT whole_ns_sparse={} whole_ns_full={} ratio={:.6} strictOracle=NOT_USED queryOverhead=MEASURED_SEPARATELY",
                diagnostic_ns[0],
                diagnostic_ns[1],
                diagnostic_ns[1] / diagnostic_ns[0]
            );
            report_nodes(
                &gpu,
                &read(&gpu, &node_readback),
                &node_masks,
                diagnostic_ns,
            );
        } else {
            println!(
                "\n::notice title=H5 far shadow nodes::UNKNOWN: adapter lacks native inside-pass timestamp features; TOTAL and all original assertions retained"
            );
        }
        assert!(
            ns[1] >= 2. * ns[0],
            "far fixture failed H5 cost: sparse{} full{}",
            ns[0],
            ns[1]
        );
        println!(
            "far_shadow hill_building dark={} clear={} buffers={} startup={} first_draws={} removal_restore_mask={} static1000=0alloc/0upload/0draw",
            witnesses.0,
            witnesses.1,
            a.buffer_bytes(),
            a.startup_pipeline_compilations(),
            first.dynamic_draw_calls,
            removal.gpu_restore_mask
        );
        let before = depth(&gpu, &sparse);
        heights.rebase(&gpu.queue, [0, 0, 16], &mut budget).unwrap();
        proxies
            .set_models_batch(
                &gpu.queue,
                &[(proxy, translation([32., 16., 0.]))],
                &mut budget,
            )
            .unwrap();
        a.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        b.synchronize_sources(heights.shadow_source(), proxies.shadow_source())
            .unwrap();
        let angle = std::f64::consts::FRAC_PI_4 + 11. * 0.0004;
        let sun = [angle.cos() as f32, angle.sin() as f32, 0.];
        sparse.update_view([16., 0., 0.], sun).unwrap();
        full.update_view([16., 0., 0.], sun).unwrap();
        full.invalidate_all();
        frame(&gpu, &mut sparse, &mut a);
        frame(&gpu, &mut full, &mut b);
        assert_eq!(
            depth(&gpu, &sparse),
            before,
            "page-aligned lateral origin shift changed depth"
        );
        assert_eq!(depth(&gpu, &sparse), depth(&gpu, &full));
        a.set_enabled(false);
        b.set_enabled(false);
        frame(&gpu, &mut sparse, &mut a);
        full.invalidate_all();
        frame(&gpu, &mut full, &mut b);
        assert_eq!(depth(&gpu, &sparse), depth(&gpu, &full));
        assert!(
            depth(&gpu, &sparse).iter().all(|b| *b == 0),
            "revoked front retained stale shadow"
        );
    });
}

// Exact specialization control: same immutable atlas, public quads and camera;
// the reference clears only the new facts and therefore executes the old sample.
struct AlphaBank {
    terrain: TerrainGpu,
    heights: HeightGpu,
    proxies: LodProxyGpu,
    shadow: ShadowGpu,
    far: DistantShadowGpu,
    handle: render::lod_gpu::ProxyHandle,
}
impl AlphaBank {
    fn new(gpu: &render::Gpu, atlas: &AtlasImage, facts: bool) -> Self {
        Self::with_resolution(gpu, atlas, facts, 128)
    }
    fn with_resolution(
        gpu: &render::Gpu,
        atlas: &AtlasImage,
        facts: bool,
        resolution: u32,
    ) -> Self {
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            64,
            64,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 4,
                max_quads: 4096,
                occlusion: false,
            },
            atlas,
        )
        .unwrap();
        if !facts {
            for layer in 0..atlas.layers {
                let tint = u32::from(layer == 40 || layer == 41);
                gpu.queue.write_buffer(
                    terrain.geometry().materials,
                    u64::from(layer) * 16 + 4,
                    &tint.to_le_bytes(),
                );
            }
        }
        let mut cells = [0; 4096];
        for y in 0..16 {
            for z in 4..12 {
                for x in 4..12 {
                    cells[x + 16 * (z + 16 * y)] = 1;
                }
            }
        }
        let mut mesh = render::terrain_mesh::PageMesh::default();
        render::terrain_mesh::mesh_page(
            &render::terrain_mesh::PageHalo::from_page(&cells, 0),
            |cell| {
                if cell == 0 {
                    render::terrain_mesh::CellMaterial::EMPTY
                } else {
                    render::terrain_mesh::CellMaterial {
                        class: render::terrain_mesh::MeshClass::Cutout,
                        ..render::terrain_mesh::CellMaterial::opaque(4)
                    }
                }
            },
            &mut render::terrain_mesh::MeshScratch::new(),
            &mut mesh,
        );
        let mut budget = UploadBudget::new(1_000_000);
        terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0., 16., 16.]),
                &mesh,
                &mut budget,
            )
            .unwrap();
        let heights = HeightGpu::new(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig {
                max_tiles: 8,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        let mut proxies = LodProxyGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ProxyConfig {
                max_objects: 8,
                max_payload_words: 65536,
                atlas_layers: 186,
                instrument: false,
            },
            None,
        )
        .unwrap();
        let handle = proxies
            .upload(
                &gpu.queue,
                None,
                1,
                translation([32., 16., 16.]),
                [0.; 4],
                &building(SurfaceKind::Cutout, 4),
                &mut budget,
            )
            .unwrap();
        let shadow = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution,
                half_widths: [64., 128., 256.],
                depth_half_extent: 256.,
                max_updates_per_frame: 3,
                sun_step_radians: 1e-6,
                dynamic_casters: true,
                projected_bounds: true,
                ..Default::default()
            },
            [16., 0., 16.],
            [1., 1., 0.],
        )
        .unwrap();
        let mut far = DistantShadowGpu::new(
            &gpu.device,
            heights.shadow_source(),
            proxies.shadow_source(),
            &shadow,
            HeightShadowPolicy::from_opaque_layers([0]).unwrap(),
        )
        .unwrap();
        far.set_enabled(true);
        Self {
            terrain,
            heights,
            proxies,
            shadow,
            far,
            handle,
        }
    }
    fn set_material(&mut self, gpu: &render::Gpu, material: u16, version: u64) {
        self.proxies
            .upload(
                &gpu.queue,
                Some(self.handle),
                version,
                translation([32., 16., 16.]),
                [0.; 4],
                &building(SurfaceKind::Cutout, material),
                &mut UploadBudget::new(1_000_000),
            )
            .unwrap();
        self.far
            .synchronize_sources(self.heights.shadow_source(), self.proxies.shadow_source())
            .unwrap();
    }
    fn frame(&mut self, gpu: &render::Gpu) -> ShadowWork {
        frame(gpu, &mut self.shadow, &mut self.far)
    }
}
#[test]
fn mip0_alpha_specialization_is_bitwise_and_inplace_atlas_revokes_both_shadow_caches() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut atlas = AtlasImage::donor().unwrap();
        for mip in &mut atlas.mips {
            let pixels = (mip.size * mip.size) as usize;
            for (layer, alpha) in [(1, 0), (2, 127), (3, 128), (4, 255), (5, 0)] {
                for (i, pixel) in mip.rgba[layer * pixels * 4..(layer + 1) * pixels * 4]
                    .chunks_exact_mut(4)
                    .enumerate()
                {
                    pixel[3] = if layer == 5 && i % 2 == 0 { 255 } else { alpha };
                }
            }
        }
        let mut fast = AlphaBank::new(&gpu, &atlas, true);
        let mut reference = AlphaBank::new(&gpu, &atlas, false);
        for (version, material) in [1, 2, 3, 4, 5].into_iter().enumerate() {
            fast.set_material(&gpu, material, version as u64 + 2);
            reference.set_material(&gpu, material, version as u64 + 2);
            fast.frame(&gpu);
            reference.shadow.invalidate_all();
            reference.frame(&gpu);
            assert_eq!(
                depth(&gpu, &fast.shadow),
                depth(&gpu, &reference.shadow),
                "alpha material{material} changed depth"
            );
            assert_eq!(
                depth_texture(&gpu, &fast.shadow, fast.shadow.texture()),
                depth_texture(&gpu, &reference.shadow, reference.shadow.texture())
            );
        }
        fast.set_material(&gpu, 4, 20);
        reference.set_material(&gpu, 4, 20);
        fast.frame(&gpu);
        reference.frame(&gpu);
        let original = depth(&gpu, &fast.shadow);
        assert!(original.iter().any(|v| *v != 0));
        for (index, alpha) in [0, 127, 128, 255, 17, 255].into_iter().enumerate() {
            let mut changed = AtlasImage::donor().unwrap();
            for mip in &mut changed.mips {
                let pixels = (mip.size * mip.size) as usize;
                for (i, pixel) in mip.rgba[4 * pixels * 4..5 * pixels * 4]
                    .chunks_exact_mut(4)
                    .enumerate()
                {
                    pixel[3] = if alpha == 17 {
                        if i % 2 == 0 { 255 } else { 0 }
                    } else {
                        alpha
                    };
                }
            }
            let prepared = render::material_pack::PreparedMaterials::prepare(
                &changed,
                &[],
                index as u64 + 1,
                Default::default(),
            )
            .unwrap();
            let before = fast.terrain.geometry().shadow_alpha_state.revision();
            let cull_before = fast.far.cull_key();
            render::material_pack::apply_albedo_in_place(
                &gpu.queue,
                fast.terrain.geometry(),
                &prepared,
            )
            .unwrap();
            render::material_pack::apply_albedo_in_place(
                &gpu.queue,
                reference.terrain.geometry(),
                &prepared,
            )
            .unwrap();
            assert_eq!(
                fast.terrain.geometry().shadow_alpha_state.revision(),
                before + 1
            );
            assert!(fast.shadow.needs_work());
            assert!(!fast.shadow.presentation_ready());
            // The old sampler-free PASS list is unusable even before the owner
            // synchronizes its unchanged geometry/front keys.
            let empty = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("stale alpha cull rejection"),
                size: wgpu::Extent3d {
                    width: 128,
                    height: 128,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let mut stale = gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = stale.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &empty.create_view(&Default::default()),
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                assert_eq!(
                    fast.far.draw_shadow(&mut pass, 0),
                    0,
                    "stale PASS list survived atlas upload"
                );
            }
            gpu.queue.submit([stale.finish()]);
            assert!(
                fast.far
                    .synchronize_sources(fast.heights.shadow_source(), fast.proxies.shadow_source())
                    .unwrap()
            );
            assert_eq!(fast.far.cull_key().0, cull_before.0);
            assert_eq!(fast.far.cull_key().1, cull_before.1 + 1);
            let work = fast.frame(&gpu);
            reference.shadow.invalidate_all();
            reference.frame(&gpu);
            assert_eq!(
                work.static_updated_mask, 7,
                "fixed Sun/source did not rebuild all near maps"
            );
            assert_eq!(
                work.gpu_restore_mask, 7,
                "fixed Sun/source did not rebuild combined far maps"
            );
            assert!(fast.shadow.presentation_ready());
            assert!(!fast.shadow.needs_work());
            let actual = depth(&gpu, &fast.shadow);
            assert_eq!(actual, depth(&gpu, &reference.shadow));
            let near = depth_texture(&gpu, &fast.shadow, fast.shadow.texture());
            assert_eq!(
                near,
                depth_texture(&gpu, &reference.shadow, reference.shadow.texture())
            );
            if alpha < 128 && alpha != 17 {
                assert!(actual.iter().all(|v| *v == 0));
                assert!(near.iter().all(|v| *v == 0));
            }
            if alpha == 255 {
                assert_eq!(
                    actual, original,
                    "reset to original atlas did not restore image"
                );
            }
        }
        // Reject incompatible input before any texture/metadata/epoch publication.
        let invalid = render::material_pack::PreparedMaterials::prepare(
            &AtlasImage {
                tile_size: 4,
                layers: 1,
                mips: vec![render::terrain_gpu::AtlasMip {
                    size: 4,
                    rgba: vec![255; 64],
                    tint_mask: vec![255; 16],
                }],
            },
            &[],
            99,
            Default::default(),
        )
        .unwrap();
        let before = fast.terrain.geometry().shadow_alpha_state.revision();
        assert_eq!(
            render::material_pack::apply_albedo_in_place(
                &gpu.queue,
                fast.terrain.geometry(),
                &invalid
            ),
            Err(render::material_pack::MaterialError::Target)
        );
        assert_eq!(
            fast.terrain.geometry().shadow_alpha_state.revision(),
            before
        );
        assert!(!fast.shadow.needs_work());
        assert_eq!(depth(&gpu, &fast.shadow), original);
        // Hidden cascades keep their old-epoch obligation until enabled again.
        fast.shadow.set_visibility(true, 1).unwrap();
        fast.frame(&gpu);
        let reset =
            render::material_pack::PreparedMaterials::prepare(&atlas, &[], 100, Default::default())
                .unwrap();
        render::material_pack::apply_albedo_in_place(&gpu.queue, fast.terrain.geometry(), &reset)
            .unwrap();
        assert!(!fast.shadow.presentation_ready());
        let one = fast.frame(&gpu);
        assert_eq!(one.static_updated_mask, 1);
        assert!(fast.shadow.presentation_ready());
        assert!(!fast.shadow.needs_work());
        fast.shadow.set_visibility(true, 3).unwrap();
        assert!(!fast.shadow.presentation_ready());
        let other = fast.frame(&gpu);
        assert_eq!(other.static_updated_mask, 6);
        assert!(fast.shadow.presentation_ready());
        assert!(!fast.shadow.needs_work());
        assert_eq!(depth(&gpu, &fast.shadow), original);
        let mut e = gpu.device.create_command_encoder(&Default::default());
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..1000 {
            assert!(!fast.shadow.needs_work());
            let work = fast
                .shadow
                .encode_dynamic(&gpu.queue, &mut e, &mut fast.far)
                .unwrap();
            assert_eq!(
                work.cascade_updates + work.dynamic_draw_calls + work.dynamic_cull_dispatches,
                0
            );
            assert_eq!(work.uniform_upload_bytes, 0);
        }
        let counts = scope.finish();
        assert_eq!(counts.allocating_calls(), 0);
        assert_eq!(counts.dealloc_calls, 0);
        gpu.queue.submit([e.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        println!(
            "ALPHA_NATIVE exact0/127/128/255/mixed=BITWISE inplace6=fixedSunNearAndFarRefresh reset=BITWISE idle1000=0alloc"
        );
    });
}

#[test]
fn loading_alpha_cost_uses_identical_public_quads_and_reports_mixed_fallback_separately() {
    pollster::block_on(async {
        let gpu = diagnostic_gpu(&render::instance()).await;
        for mixed in [false, true] {
            let mut atlas = AtlasImage::donor().unwrap();
            for mip in &mut atlas.mips {
                let pixels = (mip.size * mip.size) as usize;
                for (i, pixel) in mip.rgba[4 * pixels * 4..5 * pixels * 4]
                    .chunks_exact_mut(4)
                    .enumerate()
                {
                    pixel[3] = if mixed && i % 2 == 0 { 0 } else { 255 };
                }
            }
            let mut fast = AlphaBank::with_resolution(&gpu, &atlas, true, 2048);
            let mut sampled = AlphaBank::with_resolution(&gpu, &atlas, false, 2048);
            fast.frame(&gpu);
            sampled.frame(&gpu);
            let timings = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("identical alpha fact/sample total scopes"),
                size: 6 * 32,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            for frame in 0..6u64 {
                let angle = std::f64::consts::FRAC_PI_4 + (frame + 1) as f64 * 0.0004;
                let sun = [angle.cos() as f32, angle.sin() as f32, 0.];
                fast.shadow.update_view([16., 0., 16.], sun).unwrap();
                sampled.shadow.update_view([16., 0., 16.], sun).unwrap();
                let mut works = [ShadowWork::default(); 2];
                // Balance first/second encoder and texture-cache order.
                for kind in if frame % 2 == 0 { [0, 1] } else { [1, 0] } {
                    let bank = if kind == 0 { &mut fast } else { &mut sampled };
                    works[kind] = frame_timed(
                        &gpu,
                        &mut bank.shadow,
                        &mut bank.far,
                        Some((&timings, frame * 32 + kind as u64 * 16)),
                        None,
                    );
                }
                assert_eq!(
                    works[0], works[1],
                    "alpha specialization changed actual shadow graph work"
                );
            }
            assert_eq!(depth(&gpu, &fast.shadow), depth(&gpu, &sampled.shadow));
            let ns = whole_ns(&gpu, &timings);
            println!(
                "\n::notice title=H5 alpha exact same-scene control::mixed={mixed} currentSun6 resolution2048 maps96MiB publicObjects1 cutoutQuads64 sourceEdits0 facts_ns={} original_sample_ns={} original_over_facts={:.6} depth=BITWISE work=EXACT timestamps=TOTAL order=alternating noNodeQueries=1",
                ns[0],
                ns[1],
                ns[1] / ns[0]
            );
        }
    });
}
