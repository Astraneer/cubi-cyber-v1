#![cfg(not(target_arch = "wasm32"))]

use render::material_pack::{
    self, LayerSource, Limits, MaterialError, MaterialPackGpu, PreparedMaterials,
};
use render::terrain_gpu::{AtlasImage, AtlasMip, GpuConfig, TerrainGpu};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

struct MaterialGpuDiagnostics {
    phase: AtomicU32,
    lost: Mutex<Option<String>>,
}
fn gpu_diagnostics(gpu: &render::Gpu) -> Arc<MaterialGpuDiagnostics> {
    let diagnostics = Arc::new(MaterialGpuDiagnostics {
        phase: AtomicU32::new(0),
        lost: Mutex::new(None),
    });
    let lost = Arc::clone(&diagnostics);
    gpu.device.set_device_lost_callback(move |reason, message| {
        if reason != wgpu::DeviceLostReason::Destroyed {
            let text = format!(
                "phase={} reason={reason:?} message={message}",
                lost.phase.load(Ordering::Relaxed)
            );
            eprintln!("MATERIAL_DEVICE_LOST {text}");
            *lost.lost.lock().unwrap() = Some(text);
        }
    });
    let error = Arc::clone(&diagnostics);
    gpu.device.on_uncaptured_error(Box::new(move |failure| {
        panic!(
            "MATERIAL_GPU_ERROR phase={} {failure}",
            error.phase.load(Ordering::Relaxed)
        );
    }));
    diagnostics
}

fn base(size: u32, layers: u32) -> AtlasImage {
    AtlasImage {
        tile_size: size,
        layers,
        mips: vec![AtlasMip {
            size,
            rgba: [255, 0, 0, 255].repeat((size * size * layers) as usize),
            tint_mask: vec![137; (size * size * layers) as usize],
        }],
    }
}
fn png(size: u32, frames: u32, color: png::ColorType, pixel: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, size, size * frames);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&pixel.repeat((size * size * frames) as usize))
            .unwrap();
    }
    output
}

#[test]
fn png_preflight_limits_invalid_and_animated_inputs_before_publication() {
    let original = base(4, 4);
    let valid = png(8, 2, png::ColorType::Rgba, &[0, 255, 0, 255]);
    let source = LayerSource {
        layer: 1,
        albedo_png: Some(&valid),
        normal_png: None,
        specular_png: None,
    };
    let prepared = PreparedMaterials::prepare(&original, &[source], 9, Limits::default()).unwrap();
    assert_eq!(
        (
            prepared.revision,
            prepared.imported_layers,
            prepared.decoded_pngs,
            prepared.animation_strips
        ),
        (9, 1, 1, 1)
    );
    assert_eq!(prepared.atlas().tile_size, 8);
    assert_eq!(
        &prepared.atlas().mips[0].rgba[8 * 8 * 4..][..4],
        &[0, 255, 0, 255]
    );
    assert_eq!(prepared.atlas().mips.last().unwrap().size, 1);
    assert_eq!(original.mips[0].rgba[..4], [255, 0, 0, 255]);
    assert!(matches!(
        PreparedMaterials::prepare(&original, &[source, source], 0, Limits::default()),
        Err(MaterialError::Duplicate)
    ));
    assert!(matches!(
        PreparedMaterials::prepare(
            &original,
            &[source],
            0,
            Limits {
                gpu_bytes: 1,
                ..Limits::default()
            }
        ),
        Err(MaterialError::Limit)
    ));
    assert!(matches!(
        PreparedMaterials::prepare(
            &original,
            &[source],
            0,
            Limits {
                decoded_bytes: 500,
                ..Limits::default()
            }
        ),
        Err(MaterialError::Limit)
    ));
    let second = LayerSource { layer: 2, ..source };
    assert!(matches!(
        PreparedMaterials::prepare(
            &original,
            &[source, second],
            0,
            Limits {
                decoded_bytes: 600,
                ..Limits::default()
            }
        ),
        Err(MaterialError::Limit)
    ));
    let wrong = LayerSource { layer: 4, ..source };
    assert!(matches!(
        PreparedMaterials::prepare(&original, &[wrong], 0, Limits::default()),
        Err(MaterialError::Layer)
    ));
    let mut damaged = valid.clone();
    damaged[29] ^= 1;
    let wrong = LayerSource {
        albedo_png: Some(&damaged),
        ..source
    };
    assert!(matches!(
        PreparedMaterials::prepare(&original, &[wrong], 0, Limits::default()),
        Err(MaterialError::Png)
    ));
    damaged[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    let wrong = LayerSource {
        albedo_png: Some(&damaged),
        ..source
    };
    assert!(matches!(
        PreparedMaterials::prepare(&original, &[wrong], 0, Limits::default()),
        Err(MaterialError::Dimensions)
    ));
}

fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<f32> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let result = buffer
        .slice(..)
        .get_mapped_range()
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    buffer.unmap();
    result
}
fn probe(
    gpu: &render::Gpu,
    materials: &MaterialPackGpu,
    albedo: &wgpu::TextureView,
    tint: &wgpu::TextureView,
) -> Vec<f32> {
    let code = format!(
        "{}\n{}",
        material_pack::WGSL,
        r#"
@group(0) @binding(0) var albedo:texture_2d_array<f32>;
@group(0) @binding(1) var normals:texture_2d_array<f32>;
@group(0) @binding(2) var specs:texture_2d_array<f32>;
@group(0) @binding(3) var tint:texture_2d_array<f32>;
@group(0) @binding(4) var sharp:sampler;
@group(0) @binding(5) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id:vec3<u32>){
 let layer=i32(id.x);let index=id.x*7u;
 let sample=decode_labpbr(textureLoad(normals,vec2(0),layer,0),textureLoad(specs,vec2(0),layer,0));
 result[index]=vec4(sample.normal_dx,sample.ao);
 result[index+1u]=vec4(sample.height,sample.roughness,sample.dielectric_f0,f32(sample.metal_id));
 result[index+2u]=vec4(sample.porosity,sample.subsurface,sample.emission,f32(sample.metal_reserved));
 result[index+3u]=textureLoad(albedo,vec2(0),layer,0);
 let near=labpbr_parallax_uv(vec2(0.5),vec3(0.2,0.1,1.0),2.0,0.1,1000000u,normals,sharp,layer);
 let far=labpbr_parallax_uv(vec2(0.5),vec3(0.2,0.1,1.0),8.0,0.1,1000000u,normals,sharp,layer);
 result[index+4u]=vec4(near,far);
 result[index+5u]=vec4(labpbr_world_normal(sample,vec3(1.0,0.0,0.0),vec3(0.0,-1.0,0.0),vec3(0.0,0.0,1.0)),textureLoad(tint,vec2(0),layer,0).r);
 result[index+6u]=textureSampleLevel(albedo,sharp,vec2(0.1),layer,1.0);
}
"#
    );
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("actual local PNG channel oracle"),
            source: wgpu::ShaderSource::Wgsl(code.into()),
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("material channel projection"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let length = u64::from(materials.layers) * 7 * 16;
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: length,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: length,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(albedo),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&materials.normal),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&materials.specular),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(tint),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(&materials.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(materials.layers, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, length);
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &readback)
}
fn near(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
}

#[test]
fn actual_gpu_preserves_linear_channels_sentinels_directx_axes_and_bounded_pom() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .expect("software Vulkan required");
        let albedo = png(8, 1, png::ColorType::Rgba, &[128, 64, 32, 255]);
        let normal = png(8, 1, png::ColorType::Rgba, &[128, 192, 127, 0]);
        let spec = [[128, 10, 64, 255], [255, 230, 255, 254], [0, 244, 65, 0]]
            .map(|p| png(8, 1, png::ColorType::Rgba, &p));
        let rgb = png(8, 1, png::ColorType::Rgb, &[64, 10, 32]);
        let sources: Vec<_> = (0..4)
            .map(|layer| LayerSource {
                layer,
                albedo_png: Some(&albedo),
                normal_png: Some(&normal),
                specular_png: Some(if layer < 3 {
                    &spec[layer as usize]
                } else {
                    &rgb
                }),
            })
            .collect();
        let prepared =
            PreparedMaterials::prepare(&base(4, 4), &sources, 42, Limits::default()).unwrap();
        let materials = MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap();
        assert_eq!(materials.upload_bytes, prepared.gpu_bytes);
        assert_eq!(
            (materials.revision, materials.resolution, materials.layers),
            (42, 8, 4)
        );
        let values = probe(&gpu, &materials, &materials.albedo, &materials.tint);
        assert!(values.iter().all(|v| v.is_finite()));
        for (index, layer) in values.chunks_exact(28).enumerate() {
            near(layer[1], 192.0 / 255.0 * 2.0 - 1.0);
            near(layer[3], 127.0 / 255.0);
            near(layer[4], 0.0);
            let reference = ((128.0_f32 / 255.0 + 0.055) / 1.055).powf(2.4);
            // Texture-unit sRGB conversion permits implementation approximation.
            assert!((layer[12] - reference).abs() < 1.0 / 255.0);
            near(layer[16], 0.48);
            near(layer[17], 0.49);
            near(layer[18], 0.5);
            near(layer[19], 0.5);
            assert!(
                layer[21] < 0.0,
                "DirectX +V must follow supplied down tangent"
            );
            near(layer[23], 137.0 / 255.0);
            near(layer[24], layer[12]);
            match index {
                0 => {
                    near(layer[5], (1.0_f32 - 128.0 / 255.0).powi(2));
                    near(layer[6], 10.0 / 255.0);
                    near(layer[7], 0.0);
                    near(layer[8], 1.0);
                    near(layer[10], 0.0);
                }
                1 => {
                    near(layer[5], 0.0);
                    near(layer[6], 0.0);
                    near(layer[7], 230.0);
                    near(layer[9], 1.0);
                    near(layer[10], 1.0);
                    near(layer[11], 0.0);
                }
                2 => {
                    near(layer[7], 244.0);
                    near(layer[9], 0.0);
                    near(layer[10], 0.0);
                    near(layer[11], 1.0);
                }
                3 => {
                    near(layer[10], 0.0);
                    near(layer[8], 0.5);
                }
                _ => unreachable!(),
            }
        }
        let original = PreparedMaterials::prepare(&base(4, 4), &[], 0, Limits::default()).unwrap();
        let terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            16,
            16,
            render::OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 1,
                max_quads: 64,
                occlusion: false,
            },
            original.atlas(),
        )
        .unwrap();
        let before = probe(
            &gpu,
            &materials,
            terrain.geometry().colors,
            terrain.geometry().tint_masks,
        );
        near(before[12], 1.0);
        near(before[13], 0.0);
        assert_eq!(
            material_pack::apply_albedo_in_place(&gpu.queue, terrain.geometry(), &prepared),
            Ok((4, 4 * (16 + 4 + 1) * 5))
        );
        let after = probe(
            &gpu,
            &materials,
            terrain.geometry().colors,
            terrain.geometry().tint_masks,
        );
        near(after[12], values[12]);
        near(after[13], values[13]);
        near(after[14], values[14]);
        assert_ne!(&before[12..16], &after[12..16]);
    });
}

fn terrain(gpu: &render::Gpu, atlas: &AtlasImage) -> TerrainGpu {
    let mut terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        96,
        render::OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 1,
            max_quads: 32_768,
            occlusion: true,
        },
        atlas,
    )
    .unwrap();
    terrain.prepare_unit_rasterization().unwrap();
    let halo = render::terrain_mesh::PageHalo::from_page(&[1; 4096], 0);
    let mut mesh = render::terrain_mesh::PageMesh::default();
    render::terrain_mesh::mesh_page(
        &halo,
        |cell| {
            if cell == 0 {
                render::terrain_mesh::CellMaterial::EMPTY
            } else {
                render::terrain_mesh::CellMaterial::opaque(0)
            }
        },
        &mut render::terrain_mesh::MeshScratch::new(),
        &mut mesh,
    );
    terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            render::terrain_gpu::translation([0.0; 3]),
            &mesh,
            &mut render::terrain_gpu::UploadBudget::new(10_000_000),
        )
        .unwrap();
    terrain
}
fn image(gpu: &render::Gpu, terrain: &mut TerrainGpu) -> Vec<u8> {
    image_with_diagnostics(gpu, terrain, None)
}
fn image_with_diagnostics(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    diagnostics: Option<&MaterialGpuDiagnostics>,
) -> Vec<u8> {
    let phase = diagnostics.map_or(0, |d| d.phase.load(Ordering::Relaxed));
    if diagnostics.is_some() {
        eprintln!(
            "MATERIAL_FRAME_BEGIN phase={phase} adapter={:?}",
            gpu.adapter.get_info()
        );
    }
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual optional material terrain image"),
        size: wgpu::Extent3d {
            width: 96,
            height: 96,
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
        label: None,
        size: 512 * 96,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    terrain
        .encode_frame(
            &gpu.queue,
            &mut encoder,
            &output.create_view(&Default::default()),
            &render::terrain_gpu::Camera::perspective(
                [27.0, 21.0, 30.0],
                [8.0, 8.0, 8.0],
                [0.0, 1.0, 0.0],
                60.0_f32.to_radians(),
                1.0,
                0.1,
                200.0,
            ),
            render::terrain_gpu::CullingMode::None,
        )
        .unwrap();
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
                bytes_per_row: Some(512),
                rows_per_image: Some(96),
            },
        },
        wgpu::Extent3d {
            width: 96,
            height: 96,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let result = loop {
        gpu.device
            .poll(wgpu::PollType::Poll)
            .unwrap_or_else(|error| panic!("MATERIAL_POLL_FAILED phase={phase} {error:?}"));
        match rx.try_recv() {
            Ok(result) => break result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(error) => panic!("MATERIAL_MAP_CHANNEL_FAILED phase={phase} {error:?}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "MATERIAL_READBACK_TIMEOUT phase={phase} device_lost={:?}",
            diagnostics.and_then(|d| d.lost.lock().unwrap().clone())
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    result.unwrap_or_else(|error| {
        panic!(
            "MATERIAL_READBACK_FAILED phase={phase} map={error:?} adapter={:?} device_lost={:?}",
            gpu.adapter.get_info(),
            diagnostics.and_then(|d| d.lost.lock().unwrap().clone())
        )
    });
    let result = buffer
        .slice(..)
        .get_mapped_range()
        .chunks_exact(512)
        .flat_map(|line| line[..384].iter().copied())
        .collect();
    buffer.unmap();
    if diagnostics.is_some() {
        eprintln!("MATERIAL_FRAME_COMPLETE phase={phase}");
    }
    result
}
#[test]
fn prepared_optional_terrain_variant_preserves_no_pack_pixels_and_replaces_without_compiling() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .expect("software Vulkan required");
        let original = PreparedMaterials::prepare(&base(4, 4), &[], 0, Limits::default()).unwrap();
        let mut plain = terrain(&gpu, original.atlas());
        let expected = image(&gpu, &mut plain);
        let mut imported = terrain(&gpu, original.atlas());
        imported
            .prepare_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &original).unwrap(),
                false,
            )
            .unwrap();
        assert_eq!(
            image(&gpu, &mut imported),
            expected,
            "disabled prepared variant must preserve NoPack pixels"
        );
        let color = png(8, 1, png::ColorType::Rgba, &[64, 128, 192, 255]);
        let flat = png(8, 1, png::ColorType::Rgba, &[128, 128, 255, 255]);
        let tilted = png(8, 1, png::ColorType::Rgba, &[200, 170, 255, 255]);
        let spec = png(8, 1, png::ColorType::Rgba, &[64, 10, 0, 255]);
        let emissive = png(8, 1, png::ColorType::Rgba, &[64, 10, 0, 254]);
        let make = |normal: &[u8], specular: &[u8], revision| {
            PreparedMaterials::prepare(
                original.atlas(),
                &[LayerSource {
                    layer: 0,
                    albedo_png: Some(&color),
                    normal_png: Some(normal),
                    specular_png: Some(specular),
                }],
                revision,
                Limits::default(),
            )
            .unwrap()
        };
        imported.take_work();
        let prepared = make(&flat, &spec, 1);
        imported
            .replace_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap(),
                true,
            )
            .unwrap();
        imported
            .set_material_view(&gpu.queue, [27.0, 21.0, 30.0], 0)
            .unwrap();
        assert_eq!(imported.material_pack_revision(), Some(1));
        let flat_image = image(&gpu, &mut imported);
        assert_ne!(flat_image, expected);
        let work = imported.take_work();
        assert_eq!(work.pipeline_compilations, 0);
        assert!(work.upload_bytes >= prepared.gpu_bytes);
        let prepared = make(&tilted, &spec, 2);
        imported
            .replace_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap(),
                true,
            )
            .unwrap();
        let tilted_image = image(&gpu, &mut imported);
        assert!(
            flat_image
                .iter()
                .zip(&tilted_image)
                .filter(|(a, b)| a != b)
                .count()
                > 500,
            "normal map must affect real terrain lighting"
        );
        let prepared = make(&flat, &emissive, 3);
        imported
            .replace_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap(),
                true,
            )
            .unwrap();
        let emitted = image(&gpu, &mut imported);
        let sum = |pixels: &[u8]| {
            pixels
                .chunks_exact(4)
                .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
                .sum::<u64>()
        };
        assert!(sum(&emitted) > sum(&flat_image));
        let metal = png(8, 1, png::ColorType::Rgba, &[64, 231, 0, 255]);
        let prepared = make(&flat, &metal, 4);
        imported
            .replace_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap(),
                true,
            )
            .unwrap();
        let gold = image(&gpu, &mut imported);
        let metal = png(8, 1, png::ColorType::Rgba, &[64, 230, 0, 255]);
        let prepared = make(&flat, &metal, 5);
        imported
            .replace_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap(),
                true,
            )
            .unwrap();
        let iron = image(&gpu, &mut imported);
        assert!(
            gold.iter().zip(&iron).filter(|(a, b)| a != b).count() > 500,
            "documented conductor F0 must affect real terrain pixels"
        );
        assert_eq!(imported.take_work().pipeline_compilations, 0);
    });
}

#[test]
fn local_materials_and_light_gi_attachments_preserve_each_other_in_either_order() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .expect("software Vulkan required");
        let diagnostics = gpu_diagnostics(&gpu);
        let original = PreparedMaterials::prepare(&base(4, 4), &[], 0, Limits::default()).unwrap();
        let normal = png(4, 1, png::ColorType::Rgba, &[145, 155, 255, 255]);
        let spec = png(4, 1, png::ColorType::Rgba, &[64, 10, 0, 255]);
        let pack = PreparedMaterials::prepare(
            original.atlas(),
            &[LayerSource {
                layer: 0,
                albedo_png: None,
                normal_png: Some(&normal),
                specular_png: Some(&spec),
            }],
            7,
            Limits::default(),
        )
        .unwrap();
        let mut first = terrain(&gpu, original.atlas());
        let mut second = terrain(&gpu, original.atlas());
        let shadows = render::shadows::ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            first.geometry(),
            render::shadows::ShadowConfig {
                resolution: 64,
                ..Default::default()
            },
            [8.0; 3],
            [0.35, 0.8, 0.2],
        )
        .unwrap();
        let mut lights = render::light_volume::LightVolumeGpu::new(&gpu.device, 1).unwrap();
        lights
            .upload_page(
                &gpu.queue,
                render::terrain_gpu::PageHandle {
                    slot: 0,
                    generation: 1,
                },
                1,
                &[render::light_volume::PackedLight::new(15, 15, [2, 1, 0]).unwrap();
                    render::light_volume::LIGHT_SAMPLES],
                &mut render::terrain_gpu::UploadBudget::new(1_000_000),
            )
            .unwrap();
        let gi = render::gi::GiGpu::new(
            &gpu.device,
            &gpu.queue,
            render::gi::GiConfig {
                world_origin_page: [0; 3],
                world_pages: [1; 3],
                probe_origin: [0.5; 3],
                probe_counts: [2; 3],
                rays_per_probe: 16,
                max_steps: 16,
                max_distance: 8.0,
                max_jobs: 8,
                ..Default::default()
            },
            &mut render::terrain_gpu::UploadBudget::new(1_000_000),
        )
        .unwrap();
        eprintln!("MATERIAL_GI_PIPELINES_READY trace_dispatches=0");
        first
            .prepare_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &pack).unwrap(),
                true,
            )
            .unwrap();
        first
            .attach_lighting_with_gi(&gpu.device, &shadows, &lights, &gi)
            .unwrap();
        second
            .attach_lighting_with_gi(&gpu.device, &shadows, &lights, &gi)
            .unwrap();
        second
            .prepare_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &pack).unwrap(),
                true,
            )
            .unwrap();
        for renderer in [&mut first, &mut second] {
            renderer
                .set_material_view(&gpu.queue, [27.0, 21.0, 30.0], 0)
                .unwrap();
        }
        diagnostics.phase.store(1, Ordering::Relaxed);
        let expected = image_with_diagnostics(&gpu, &mut first, Some(&diagnostics));
        diagnostics.phase.store(2, Ordering::Relaxed);
        assert_eq!(
            image_with_diagnostics(&gpu, &mut second, Some(&diagnostics)),
            expected
        );
        let reserved_spec = png(4, 1, png::ColorType::Rgba, &[64, 244, 0, 255]);
        let reserved_prepared = PreparedMaterials::prepare(
            original.atlas(),
            &[LayerSource {
                layer: 0,
                albedo_png: None,
                normal_png: None,
                specular_png: Some(&reserved_spec),
            }],
            99,
            Limits::default(),
        )
        .unwrap();
        let reserved =
            MaterialPackGpu::upload(&gpu.device, &gpu.queue, &reserved_prepared).unwrap();
        assert_eq!(
            first.replace_material_pack(&gpu.device, &gpu.queue, reserved, true),
            Err(render::terrain_gpu::TerrainError::InvalidMaterial)
        );
        assert_eq!(first.material_pack_revision(), Some(7));
        first.take_work();
        first
            .replace_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &pack).unwrap(),
                true,
            )
            .unwrap();
        diagnostics.phase.store(3, Ordering::Relaxed);
        assert_eq!(
            image_with_diagnostics(&gpu, &mut first, Some(&diagnostics)),
            expected
        );
        assert_eq!(first.take_work().pipeline_compilations, 0);
    });
}
