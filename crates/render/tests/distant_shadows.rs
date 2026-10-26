#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    distant_shadows::{DistantShadowGpu, HeightShadowPolicy},
    height_gpu::{HeightGpu, HeightGpuConfig},
    height_tile::{HeightTile, HeightTileKey},
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::{LodProxyGpu, ProxyConfig, ProxyPayload},
    shadows::{ShadowConfig, ShadowGpu, ShadowWork},
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
    frame_timed(gpu, shadow, far, None)
}
fn frame_timed(
    gpu: &render::Gpu,
    shadow: &mut ShadowGpu,
    far: &mut DistantShadowGpu,
    timing: Option<(&wgpu::Buffer, u64)>,
) -> ShadowWork {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = shadow
        .encode_dynamic(&gpu.queue, &mut encoder, far)
        .unwrap();
    if let Some((target, offset)) = timing {
        let source = shadow
            .timing_source()
            .expect("actual timestamp queries required for far-shadow cost");
        assert_eq!(source.count, 1);
        encoder.copy_buffer_to_buffer(source.buffer, 0, target, offset, 16);
    }
    gpu.queue.submit([encoder.finish()]);
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    work
}
fn depth(gpu: &render::Gpu, shadow: &ShadowGpu) -> Vec<u8> {
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
            texture: shadow.sampled_texture(),
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
        let gpu = render::Gpu::request(&instance, None, true).await.unwrap();
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
        assert_eq!(a.startup_pipeline_compilations(), 5);
        let first = frame(&gpu, &mut sparse, &mut a);
        frame(&gpu, &mut full, &mut b);
        assert_eq!(depth(&gpu, &sparse), depth(&gpu, &full));
        assert_eq!(first.dynamic_cull_dispatches, 6);
        assert_eq!(first.dynamic_draw_calls, 9);
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
            );
            assert_eq!(work.pending_cascades, 0);
            assert_eq!(work.dynamic_copy_bytes, 0);
            frame_timed(
                &gpu,
                &mut full,
                &mut b,
                Some((&timings, u64::from(i) * 32 + 16)),
            );
            assert_eq!(
                depth(&gpu, &sparse),
                depth(&gpu, &full),
                "current-Sun depth mismatch at frame{i}"
            );
            visibility_witnesses(&receiver.capture(&gpu), true);
        }
        let mut ns = [0f64; 2];
        for row in read(&gpu, &timings).chunks_exact(32) {
            for (k, sum) in ns.iter_mut().enumerate() {
                let start = u64::from_le_bytes(row[k * 16..k * 16 + 8].try_into().unwrap());
                let end = u64::from_le_bytes(row[k * 16 + 8..k * 16 + 16].try_into().unwrap());
                *sum += (end - start) as f64 * f64::from(gpu.queue.get_timestamp_period());
            }
        }
        assert!(ns.iter().all(|v| v.is_finite() && *v > 0.));
        assert!(
            ns[1] >= 2. * ns[0],
            "far fixture failed H5 cost: sparse{} full{}",
            ns[0],
            ns[1]
        );
        println!(
            "far_shadow currentSun12 resolution2048 maps96MiB whole_ns_sparse={} whole_ns_full={} ratio={:.6} exactDepth=BITWISE independentReceivers=PASS copies=0",
            ns[0],
            ns[1],
            ns[1] / ns[0]
        );
        println!(
            "far_shadow hill_building dark={} clear={} buffers={} startup=5 first_draws=9 removal_restore_mask={} static1000=0alloc/0upload/0draw",
            witnesses.0,
            witnesses.1,
            a.buffer_bytes(),
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
