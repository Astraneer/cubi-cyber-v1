use super::*;
use render::{
    block_models::{BlockConfig, BlockInstance, BlockModelsGpu, PORT_MARKER_ONLY, kind},
    entity_gpu::{EntityConfig, EntityGpu, EntityInstance},
    shadows::ShadowCasterPair,
    terrain_mesh::MeshClass,
};

use super::signals;

struct MaskProbe {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    output: wgpu::Buffer,
}
impl MaskProbe {
    fn new(gpu: &render::Gpu, shadow: &ShadowGpu) -> Self {
        let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("independent hot shadow mask readback"),
            source: wgpu::ShaderSource::Wgsl("struct S {matrices:array<mat4x4<f32>,6>,sun:vec4<f32>,params:vec4<u32>};@group(0)@binding(0)var<uniform>s:S;@group(0)@binding(1)var<storage,read_write>o:array<u32>;@compute @workgroup_size(1)fn probe(){o[0]=s.params.x;}".into()),
        });
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
            label: None,
            size: 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: shadow.sampling_buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
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
    fn capture(&self, gpu: &render::Gpu) -> u32 {
        let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &read, 0, 4);
        gpu.queue.submit([encoder.finish()]);
        u32::from_le_bytes(read_buffer(gpu, &read).try_into().unwrap())
    }
}

fn combined_depths(gpu: &render::Gpu, shadow: &ShadowGpu) -> Vec<f32> {
    let n = shadow.cache().config().resolution;
    let row = n * 4;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sparse shadow exact depth oracle"),
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
    read_buffer(gpu, &buffer)
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect()
}

#[test]
fn gpu_projected_old_new_clear_matches_full_depth_for_sun_models_and_removal() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            32,
            32,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 4096,
                occlusion: false,
            },
            &AtlasImage::donor().unwrap(),
        )
        .unwrap();
        let mut cells = [0; 4096];
        for y in 0..8 {
            for z in 6..10 {
                for x in 6..10 {
                    cells[x + 16 * (z + 16 * y)] = if y < 4 { 1 } else { 2 };
                }
            }
        }
        let mut mesh = PageMesh::default();
        mesh_page(
            &PageHalo::from_page(&cells, 0),
            |v| match v {
                0 => CellMaterial::EMPTY,
                1 => CellMaterial::opaque(3),
                _ => CellMaterial {
                    class: MeshClass::Cutout,
                    occludes: false,
                    casts_ao: false,
                    ..CellMaterial::opaque(12)
                },
            },
            &mut MeshScratch::new(),
            &mut mesh,
        );
        assert!(!mesh.cutout.is_empty());
        let mut handle = Some(
            terrain
                .upload_page(
                    &gpu.queue,
                    None,
                    1,
                    translation([0.; 3]),
                    &mesh,
                    &mut UploadBudget::new(100000),
                )
                .unwrap(),
        );
        let config = ShadowConfig {
            resolution: 128,
            half_widths: [32., 128., 512.],
            depth_half_extent: 64.,
            max_updates_per_frame: 3,
            sun_step_radians: 0.000001,
            dynamic_casters: true,
            ..Default::default()
        };
        let mut sparse = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                projected_bounds: true,
                ..config
            },
            [0.; 3],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        let mut full = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            config,
            [0.; 3],
            [0.3, 0.8, 0.2],
        )
        .unwrap();
        let models = |shadow: &ShadowGpu| {
            (
                EntityGpu::new(
                    &gpu.device,
                    &gpu.queue,
                    terrain.geometry(),
                    EntityConfig {
                        max_entities: 4,
                        atlas_layers: 186,
                    },
                    Some(shadow),
                )
                .unwrap(),
                BlockModelsGpu::new(
                    &gpu.device,
                    &gpu.queue,
                    terrain.geometry(),
                    BlockConfig {
                        max_instances: 4,
                        atlas_layers: 186,
                    },
                    Some(shadow),
                )
                .unwrap(),
            )
        };
        let (mut se, mut sb) = models(&sparse);
        let (mut fe, mut fb) = models(&full);
        let mask_probe = MaskProbe::new(&gpu, &sparse);
        let layer_probe = signals::LayerProbe::new(&gpu, &sparse);
        let initial_pipeline_count = sparse.startup_pipeline_compilations();
        let mut exact_pixels = 0u64;
        for frame in 0..18 {
            if frame == 6 {
                terrain
                    .release_page(
                        &gpu.queue,
                        handle.take().unwrap(),
                        &mut UploadBudget::new(100000),
                    )
                    .unwrap();
            }
            if frame == 8 {
                handle = Some(
                    terrain
                        .upload_page(
                            &gpu.queue,
                            None,
                            2,
                            translation([-32., 0., 0.]),
                            &mesh,
                            &mut UploadBudget::new(100000),
                        )
                        .unwrap(),
                );
            }
            if frame == 10 {
                handle = Some(
                    terrain
                        .upload_page(
                            &gpu.queue,
                            handle,
                            3,
                            translation([-32., 65., 0.]),
                            &mesh,
                            &mut UploadBudget::new(100000),
                        )
                        .unwrap(),
                );
            }
            let mob = EntityInstance {
                position: [
                    -12. + frame as f32 * 1.7,
                    if frame == 9 { 64. } else { 0. },
                    -8.,
                ],
                body_yaw: frame as f32 * 17.,
                head_yaw: frame as f32 * 23.,
                pitch: 12.,
                limb_phase: frame as f32,
                limb_amount: 0.8,
                kind: 5,
                flags: 0,
                color: 0,
                id_low: 1,
                id_high: 0,
                death_roll: 0.,
                scale: 1.,
                reserved: 0.,
            };
            let chest = BlockInstance {
                position: [14., 0., 4.],
                kind: kind::CHEST,
                phase: frame as f32 / 18.,
                ..Default::default()
            };
            // The distant annotation is a strict negative: it cannot enlarge a caster rectangle.
            let marker = BlockInstance {
                position: [1000.; 3],
                kind: kind::CUBE_MACHINE,
                flags: PORT_MARKER_ONLY,
                ..Default::default()
            };
            let block_instances = [chest, marker];
            for (e, b) in [(&mut se, &mut sb), (&mut fe, &mut fb)] {
                e.upload(
                    &gpu.queue,
                    if frame < 14 {
                        std::slice::from_ref(&mob)
                    } else {
                        &[]
                    },
                    &[],
                    &mut UploadBudget::new(100000),
                )
                .unwrap();
                b.upload(
                    &gpu.queue,
                    if frame < 12 {
                        &block_instances[..]
                    } else {
                        &block_instances[1..]
                    },
                    &mut UploadBudget::new(100000),
                )
                .unwrap();
            }
            let sun = [0.3 + frame as f32 * 0.002, 0.8, 0.2];
            let center = [frame as f32 * 0.05, 0., 0.];
            sparse.update_view(center, sun).unwrap();
            full.update_view(center, sun).unwrap();
            full.invalidate_all();
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = sparse
                .encode_dynamic(
                    &gpu.queue,
                    &mut encoder,
                    &mut ShadowCasterPair {
                        first: &mut se,
                        second: &mut sb,
                    },
                )
                .unwrap();
            full.encode_dynamic(
                &gpu.queue,
                &mut encoder,
                &mut ShadowCasterPair {
                    first: &mut fe,
                    second: &mut fb,
                },
            )
            .unwrap();
            let metrics = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("exact sparse GPU pixel counts"),
                size: 768,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(
                sparse.footprint_metrics_buffer().unwrap(),
                0,
                &metrics,
                0,
                768,
            );
            gpu.queue.submit([encoder.finish()]);
            assert_eq!(work.cascade_updates, 3);
            assert_eq!(work.gpu_clear_mask, 7);
            assert_eq!(work.gpu_restore_mask, 7);
            assert_eq!(work.dynamic_copy_bytes, 0);
            let bytes = read_buffer(&gpu, &metrics);
            for cascade in 0..3 {
                for field in [96, 100] {
                    let at = cascade * 256 + field;
                    exact_pixels +=
                        u64::from(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()));
                }
            }
            assert_eq!(
                depths(&gpu, &sparse),
                depths(&gpu, &full),
                "static depth mismatch frame{frame}"
            );
            assert_eq!(
                combined_depths(&gpu, &sparse),
                combined_depths(&gpu, &full),
                "combined depth mismatch frame{frame}"
            );
        }
        let full_pixels = 18 * 128u64.pow(2) * 3 * 2;
        assert!(
            exact_pixels < full_pixels / 2,
            "no useful rectangle reduction {exact_pixels}/{full_pixels}"
        );
        terrain
            .upload_page(
                &gpu.queue,
                handle,
                4,
                translation([0.; 3]),
                &mesh,
                &mut UploadBudget::new(100000),
            )
            .unwrap();
        for shadow in [&mut sparse, &mut full] {
            shadow.update_view([32., 0., 8.], [0., 1., 0.]).unwrap();
            shadow.invalidate_all();
        }
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        sparse
            .encode_dynamic(
                &gpu.queue,
                &mut encoder,
                &mut ShadowCasterPair {
                    first: &mut se,
                    second: &mut sb,
                },
            )
            .unwrap();
        full.encode_dynamic(
            &gpu.queue,
            &mut encoder,
            &mut ShadowCasterPair {
                first: &mut fe,
                second: &mut fb,
            },
        )
        .unwrap();
        gpu.queue.submit([encoder.finish()]);
        for (enabled, cascades, mask, updates) in [
            (false, 3, 0, 0),
            (true, 1, 1, 1),
            (true, 2, 3, 1),
            (true, 3, 7, 1),
        ] {
            assert!(sparse.set_visibility(enabled, cascades).unwrap());
            assert!(sparse.needs_work());
            assert!(!sparse.presentation_ready());
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = sparse
                .encode_dynamic(
                    &gpu.queue,
                    &mut encoder,
                    &mut ShadowCasterPair {
                        first: &mut se,
                        second: &mut sb,
                    },
                )
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
            assert_eq!(work.cascade_updates, updates);
            assert_eq!(mask_probe.capture(&gpu), mask);
            let image = layer_probe.capture(&gpu, 0);
            let expected = signals::expected(&sparse.cache().sampling(), &image);
            let error = image
                .iter()
                .zip(&expected)
                .map(|(pixel, expected)| (f64::from(pixel[3]) - expected).abs())
                .sum::<f64>()
                / image.len() as f64
                * 1000.;
            assert!(
                error <= 0.02,
                "active{cascades}/enabled{enabled} f64 blend error{error}"
            );
            if enabled {
                assert!(image.iter().filter(|p| p[3] < 0.75).count() > 20);
            } else {
                assert!(image.iter().all(|p| p[3] == 1.));
            }

            assert_eq!(sparse.enabled(), enabled);
            assert_eq!(sparse.active_cascades(), cascades);
            assert!(sparse.presentation_ready());
            assert_eq!(work.pending_cascades, 0);
            assert!(!sparse.needs_work());
            assert_eq!(
                sparse.startup_pipeline_compilations(),
                initial_pipeline_count
            );
            if !enabled {
                assert_eq!(work.dynamic_cull_dispatches, 0);
                assert_eq!(work.dynamic_draw_calls, 0);
                assert_eq!(work.gpu_restore_mask, 0);
            }
        }
        assert_eq!(combined_depths(&gpu, &sparse), combined_depths(&gpu, &full));
        println!(
            "H5 GPU sparse old+new Sun/model/alpha/removal:18frames static+combined BITWISE; exactClearRestorePixels{exact_pixels}/{full_pixels}; depthCopies0;hotmaskGPU0/1/3/7 valid; runtimePipelines0"
        );
    });
}
