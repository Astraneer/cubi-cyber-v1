//! Differential execution of the production material fragment and vertex pull.
//! Shader variants change only shadow functions; the 416-byte uniform and full
//! eight-storage-buffer binding layout remain identical on every fresh device.
#![cfg(not(target_arch = "wasm32"))]

use render::{
    gi::{GiConfig, GiGpu},
    light_volume::{LIGHT_SAMPLES, LightVolumeGpu, PackedLight},
    material_pack::{Limits, MaterialPackGpu, PreparedMaterials},
    shadows::{ShadowConfig, ShadowGpu},
    terrain_gpu::{AtlasImage, AtlasMip, Camera, GpuConfig, TerrainGpu, UploadBudget},
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
use wgpu::util::DeviceExt;

const CURRENT: &str = include_str!("../src/shadow_sampling.wgsl");
// Own project implementation from 13d0484, the last verified Windows revision.
// Keep the CURRENT struct/bindings above it; this is a function-only control.
const LEGACY_FUNCTION: &str = r#"
fn cascade_shadow(world_position:vec3<f32>,world_normal:vec3<f32>)->f32 {
    let direction=normalize(shadow_sampling.sun.xyz);
    let bias=shadow_sampling.sun.w*(1.0+2.0*(1.0-max(dot(world_normal,direction),0.0)));
    for(var cascade=0u;cascade<3u;cascade++) {
        if (shadow_sampling.params.x&(1u<<cascade))==0u {continue;}
        let p=shadow_sampling.matrices[cascade]*vec4(world_position,1.0);
        if abs(p.x)>0.96||abs(p.y)>0.96||p.z<0.0||p.z>1.0 {continue;}
        let uv=vec2(p.x*0.5+0.5,0.5-p.y*0.5);
        let texel=1.0/f32(shadow_sampling.params.y);
        var visibility=0.0;
        for(var y=0;y<2;y++){for(var x=0;x<2;x++){
            let offset=(vec2<f32>(f32(x),f32(y))-vec2(0.5))*texel;
            visibility+=textureSampleCompareLevel(shadow_depth,shadow_compare,uv+offset,i32(cascade),p.z+bias);
        }}
        return visibility*0.25;
    }
    return 1.0;
}
"#;

#[derive(Clone, Copy, Debug)]
enum Variant {
    Current,
    LegacyFunction,
    ConstantLayers,
    ExplicitPcf,
    ConstantExplicitPcf,
}

fn annotation(variant: Variant, status: &str) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stdout().lock(),
        "\n::notice title=Shadow fragment differential::{variant:?} {status} struct=416 storage=8 trace_dispatches=0"
    );
}

fn sampling_source(variant: Variant) -> String {
    let (declarations, functions) = CURRENT.split_once("fn shadow_layer(").unwrap();
    match variant {
        Variant::Current => CURRENT.to_owned(),
        Variant::LegacyFunction => format!("{declarations}{LEGACY_FUNCTION}"),
        Variant::ExplicitPcf => explicit_pcf(CURRENT),
        Variant::ConstantExplicitPcf => explicit_pcf(&sampling_source(Variant::ConstantLayers)),
        Variant::ConstantLayers => {
            let layer = functions.split_once("fn cascade_shadow(").unwrap().0;
            let layer = format!("fn shadow_layer({layer}");
            let layer = layer.replacen("cascade:u32,", "", 1);
            let mut output = declarations.to_owned();
            for cascade in 0..3 {
                output.push_str(
                    &layer
                        .replace("shadow_layer", &format!("constant_layer_{cascade}"))
                        .replace("cascade", &format!("{cascade}u")),
                );
            }
            output.push_str("fn cascade_shadow(world_position:vec3<f32>,world_normal:vec3<f32>)->f32 {let direction=normalize(shadow_sampling.sun.xyz);let bias=shadow_sampling.sun.w*(1.+2.*(1.-max(dot(world_normal,direction),0.)));\n");
            for cascade in 0..3 {
                output.push_str(&format!(
                    "if (shadow_sampling.params.x&{mask}u)!=0u {{let p=shadow_sampling.desired_matrices[{cascade}u]*vec4(world_position,1.);let edge=max(abs(p.x),abs(p.y));if !(edge>=.96||p.z<0.||p.z>1.) {{let here=constant_layer_{cascade}(world_position,bias,world_normal);if here.y!=0. {{\n",
                    mask=1<<cascade
                ));
                if cascade != 2 {
                    output.push_str("if edge>.60 {\n");
                    for farther in cascade + 1..3 {
                        output.push_str(&format!("let next{farther}=constant_layer_{farther}(world_position,bias,world_normal);if next{farther}.y>0. {{return mix(here.x,next{farther}.x,smoothstep(.60,.96,edge));}}\n"));
                    }
                    output.push_str("}\n");
                }
                output.push_str("return here.x;}}}\n");
            }
            output.push_str("return 1.;}\n");
            output
        }
    }
}

fn explicit_pcf(source: &str) -> String {
    let mut output = source.to_owned();
    while let Some(start) = output.find("    for(var y=0;") {
        let end = start + output[start..].find("    return vec2(visibility").unwrap();
        let mut taps = String::new();
        // The constant-layer variant has literal indices instead of the original
        // parameter. Preserve that function's argument in all four taps.
        let body = &output[start..end];
        let argument = body
            .split_once("i32(")
            .unwrap()
            .1
            .split_once(')')
            .unwrap()
            .0;
        // Keep the original y-major tap order and floating-point addition order.
        for offset in ["-.5,-.5", ".5,-.5", "-.5,.5", ".5,.5"] {
            taps.push_str(&format!("visibility+=textureSampleCompareLevel(shadow_depth,shadow_compare,uv+vec2({offset})*texel,i32({argument}),p.z+bias+plane_bias);\n"));
        }
        output.replace_range(start..end, &taps);
    }
    output
}

fn buffer_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    uniform: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: if uniform {
                wgpu::BufferBindingType::Uniform
            } else {
                wgpu::BufferBindingType::Storage { read_only: true }
            },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_layout(binding: u32, depth: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: if depth {
                wgpu::TextureSampleType::Depth
            } else {
                wgpu::TextureSampleType::Float { filterable: true }
            },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn binary16(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1. } else { -1. };
    let exponent = (bits >> 10) & 31;
    let fraction = f32::from(bits & 1023);
    match exponent {
        0 => sign * fraction * 2f32.powi(-24),
        31 if fraction == 0. => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1. + fraction / 1024.) * 2f32.powi(i32::from(exponent) - 15),
    }
}

fn differential(variant: Variant) {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.on_uncaptured_error(Box::new(move |error| {
        let message = error.to_string();
        let details = message
            .lines()
            .filter(|line| {
                line.contains("X35")
                    || line.contains("Shader")
                    || line.contains("shader compilation")
            })
            .collect::<Vec<_>>()
            .join(" ");
        let details: String = details.chars().take(500).collect();
        annotation(variant, &format!("FAIL validation {details}"));
        panic!("{variant:?} GPU validation {message}");
    }));
    gpu.device.set_device_lost_callback(move |reason, message| {
        if reason != wgpu::DeviceLostReason::Destroyed {
            annotation(variant, "FAIL device lost");
            eprintln!(
                "SHADOW_DIFFERENTIAL_DEVICE_LOST {variant:?} reason={reason:?} message={message}"
            );
        }
    });
    let atlas = AtlasImage {
        tile_size: 4,
        layers: 1,
        mips: vec![AtlasMip {
            size: 4,
            rgba: [255, 0, 0, 255].repeat(16),
            tint_mask: vec![137; 16],
        }],
    };
    let prepared = PreparedMaterials::prepare(&atlas, &[], 1, Limits::default()).unwrap();
    let pack = MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap();
    let mut terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        96,
        96,
        render::OutputTransfer::ShaderSrgb,
        GpuConfig {
            max_pages: 1,
            max_quads: 32768,
            occlusion: true,
        },
        &atlas,
    )
    .unwrap();
    terrain.prepare_unit_rasterization().unwrap();
    let mut mesh = PageMesh::default();
    mesh_page(
        &PageHalo::from_page(&[1; 4096], 0),
        |id| {
            if id == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial::opaque(0)
            }
        },
        &mut MeshScratch::new(),
        &mut mesh,
    );
    let handle = terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            render::terrain_gpu::translation([0.; 3]),
            &mesh,
            &mut UploadBudget::new(1_000_000),
        )
        .unwrap();
    let geometry = terrain.geometry();
    let shadows = ShadowGpu::new(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        ShadowConfig {
            resolution: 64,
            ..Default::default()
        },
        [8.; 3],
        [0.35, 0.8, 0.2],
    )
    .unwrap();
    assert_eq!(shadows.sampling_buffer().size(), 416);
    let mut lights = LightVolumeGpu::new(&gpu.device, 1).unwrap();
    lights
        .upload_page(
            &gpu.queue,
            handle,
            1,
            &[PackedLight::new(15, 15, [2, 1, 0]).unwrap(); LIGHT_SAMPLES],
            &mut UploadBudget::new(1_000_000),
        )
        .unwrap();
    let gi = GiGpu::new(
        &gpu.device,
        &gpu.queue,
        GiConfig {
            world_origin_page: [0; 3],
            world_pages: [1; 3],
            probe_origin: [0.5; 3],
            probe_counts: [2; 3],
            rays_per_probe: 16,
            max_steps: 16,
            max_distance: 8.,
            max_jobs: 8,
            ..Default::default()
        },
        &mut UploadBudget::new(1_000_000),
    )
    .unwrap();
    let camera = Camera::perspective(
        [27., 21., 30.],
        [8.; 3],
        [0., 1., 0.],
        60f32.to_radians(),
        1.,
        0.1,
        200.,
    );
    let mut camera_words = bytemuck::cast_slice::<_, u32>(&camera.view_projection).to_vec();
    camera_words.extend([96f32.to_bits(), 96f32.to_bits(), 0, 0, 1, 32768, 2, 0]);
    let camera_buffer = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("exact production camera"),
            contents: bytemuck::cast_slice(&camera_words),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let mut visible = Vec::new();
    for (index, quad) in mesh.iter().enumerate() {
        let [width, height] = quad.size();
        for tile in 0..u32::from(width) * u32::from(height) {
            visible.push(((index as u32) << 8) | tile);
        }
    }
    assert_eq!(visible.len(), 1536);
    let visible_buffer = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("exact production unit visible entries"),
            contents: bytemuck::cast_slice(&visible),
            usage: wgpu::BufferUsages::STORAGE,
        });
    let material_words = [
        27f32.to_bits(),
        21f32.to_bits(),
        30f32.to_bits(),
        0,
        1,
        0,
        0,
        0,
    ];
    let material_buffer = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("enabled PBR no POM"),
            contents: bytemuck::cast_slice(&material_words),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let vertex = wgpu::ShaderStages::VERTEX;
    let fragment = wgpu::ShaderStages::FRAGMENT;
    let mut layout_entries = vec![
        buffer_layout(0, vertex, true),
        buffer_layout(1, vertex | fragment, false),
        buffer_layout(2, vertex, false),
        buffer_layout(3, vertex, false),
        buffer_layout(4, fragment, false),
        texture_layout(5, false),
        texture_layout(6, false),
        buffer_layout(8, fragment, false),
        buffer_layout(9, fragment, true),
        texture_layout(10, true),
        buffer_layout(12, fragment, false),
        buffer_layout(13, fragment, true),
        buffer_layout(14, fragment, false),
        buffer_layout(16, fragment, false),
        texture_layout(17, false),
        texture_layout(18, false),
        buffer_layout(19, fragment, true),
    ];
    for (binding, ty) in [
        (7, wgpu::SamplerBindingType::Filtering),
        (11, wgpu::SamplerBindingType::Comparison),
    ] {
        layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding,
            visibility: fragment,
            ty: wgpu::BindingType::Sampler(ty),
            count: None,
        });
    }
    assert_eq!(
        layout_entries
            .iter()
            .filter(|entry| matches!(
                entry.ty,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { .. },
                    ..
                }
            ))
            .count(),
        8
    );
    let layout = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("production full material layout"),
            entries: &layout_entries,
        });
    fn buffer(binding: u32, value: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry {
            binding,
            resource: value.as_entire_binding(),
        }
    }
    fn texture(binding: u32, value: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(value),
        }
    }
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("production material resources"),
        layout: &layout,
        entries: &[
            buffer(0, &camera_buffer),
            buffer(1, geometry.objects),
            buffer(2, geometry.quads),
            buffer(3, &visible_buffer),
            buffer(4, geometry.materials),
            texture(5, &pack.albedo),
            texture(6, &pack.tint),
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::Sampler(&pack.sampler),
            },
            buffer(8, geometry.edit_masks),
            buffer(9, shadows.sampling_buffer()),
            texture(10, shadows.depth_view()),
            wgpu::BindGroupEntry {
                binding: 11,
                resource: wgpu::BindingResource::Sampler(shadows.sampler()),
            },
            buffer(12, lights.buffer()),
            buffer(13, gi.sampling_buffer()),
            buffer(14, gi.probe_buffer()),
            buffer(16, geometry.edit_neighbors),
            texture(17, &pack.normal),
            texture(18, &pack.specular),
            buffer(19, &material_buffer),
        ],
    });
    let source = render::terrain_gpu::material_shader_source(true, true);
    assert_eq!(source.matches(CURRENT).count(), 1);
    let source = source.replace(CURRENT, &sampling_source(variant));
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("isolated production shadow function differential"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("exact material vertex and fragment differential"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: 4,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
    let make_target = |label, samples, format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: 96,
                height: 96,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let color = make_target(
        "production HDR AA4",
        4,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let depth = make_target(
        "production reverse-Z AA4",
        4,
        wgpu::TextureFormat::Depth32Float,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let resolved = make_target(
        "material differential HDR readback",
        1,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material differential readback"),
        size: 96 * 96 * 8,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let color_view = color.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        let resolve_view = resolved.create_view(&Default::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("production color-only differential"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: Some(&resolve_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..4, 0..visible.len() as u32);
    }
    encoder.copy_texture_to_buffer(
        resolved.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(96 * 8),
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
    let (send, receive) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    if let Err(error) = gpu.device.poll(wgpu::PollType::Wait) {
        annotation(variant, "FAIL poll");
        panic!("{variant:?} poll {error:?}");
    }
    let result = receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("bounded material map callback");
    if result.is_err() {
        annotation(variant, "FAIL map");
    }
    result.unwrap();
    let bytes = readback.slice(..).get_mapped_range();
    let pixels: Vec<_> = bytes
        .chunks_exact(8)
        .map(|pixel| {
            std::array::from_fn::<_, 4, _>(|channel| {
                binary16(u16::from_le_bytes([
                    pixel[channel * 2],
                    pixel[channel * 2 + 1],
                ]))
            })
        })
        .collect();
    assert!(pixels.iter().flatten().all(|value| value.is_finite()));
    let red = pixels.iter().filter(|p| p[0] > p[1] + 0.01).count();
    assert!(red > 100, "actual red terrain must be visible: {red}");
    annotation(variant, "PASS actual1536units");
}

#[test]
fn current_shadow_function_with_current_uniform_layout() {
    differential(Variant::Current);
}
#[test]
fn legacy_shadow_function_with_current_uniform_layout() {
    differential(Variant::LegacyFunction);
}
#[test]
fn constant_shadow_layers_with_current_uniform_layout() {
    differential(Variant::ConstantLayers);
}

#[test]
fn explicit_pcf_taps_with_current_uniform_layout() {
    differential(Variant::ExplicitPcf);
}

#[test]
fn constant_layers_and_explicit_pcf_taps_with_current_uniform_layout() {
    differential(Variant::ConstantExplicitPcf);
}
