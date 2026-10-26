#![cfg(not(target_arch = "wasm32"))]
//! Builtin lava is outgoing radiance, independent of known-dark lighting/AO.
use render::{
    OutputTransfer,
    contact_ao::{ContactAoConfig, ContactAoGpu, ContactAoInputs},
    light_volume::{LightVolumeGpu, PackedLight},
    lod::{MaskedRayProxy, PublicPage, PublicVoxel},
    lod_gpu::{LodProxyGpu, ProxyCamera, ProxyConfig, ProxyPayload},
    material_pack::{LayerSource, Limits, MaterialPackGpu, PreparedMaterials},
    shadows::{ShadowConfig, ShadowGpu},
    terrain_gpu::{
        AtlasImage, AtlasMip, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
    terrain_mesh::{Face, PageMesh, QuadDescriptor},
};
const W: u32 = 128;
const H: u32 = 64;
const LAVA: usize = 185;
fn atlas() -> AtlasImage {
    AtlasImage {
        tile_size: 1,
        layers: 186,
        mips: vec![AtlasMip {
            size: 1,
            rgba: [255, 96, 0, 255].repeat(186),
            tint_mask: vec![255; 186],
        }],
    }
}
fn specular(alpha: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0, 10, 0, alpha])
            .unwrap();
    }
    bytes
}
fn prepared(alpha: Option<u8>) -> PreparedMaterials {
    let bytes = alpha.map(specular);
    let sources: Vec<_> = bytes
        .as_ref()
        .map(|bytes| LayerSource {
            layer: LAVA as u32,
            albedo_png: None,
            normal_png: None,
            specular_png: Some(bytes),
        })
        .into_iter()
        .collect();
    PreparedMaterials::prepare(&atlas(), &sources, 1, Limits::default()).unwrap()
}
#[test]
fn explicit_specular_sentinels_replace_builtin_fallback_without_mutating_defaults() {
    let builtin = prepared(None);
    assert_eq!(builtin.material_metadata()[LAVA][2], 3);
    assert_eq!(f32::from_bits(builtin.material_metadata()[LAVA][3]), 1.);
    assert_eq!(builtin.material_metadata()[LAVA - 1][2..], [0, 0]);
    for alpha in [0, 1, 128, 254, 255] {
        let local = prepared(Some(alpha));
        let entry = local.material_metadata()[LAVA];
        let expected = if alpha == 255 {
            0.
        } else {
            f32::from(alpha) / 254.
        };
        assert_eq!(entry[2], u32::from(expected > 0.));
        assert!((f32::from_bits(entry[3]) - expected).abs() < 0.000001);
        assert_eq!(local.atlas().mips[0].rgba, builtin.atlas().mips[0].rgba);
    }
}
fn half(bytes: &[u8], pixel: usize, channel: usize) -> f32 {
    let at = pixel * 8 + channel * 2;
    let bits = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let sign = if bits & 32768 != 0 { -1. } else { 1. };
    let exponent = (bits >> 10) & 31;
    let mantissa = f32::from(bits & 1023);
    if exponent == 0 {
        sign * mantissa * 2f32.powi(-24)
    } else if exponent == 31 {
        f32::NAN
    } else {
        sign * (1. + mantissa / 1024.) * 2f32.powi(i32::from(exponent) - 15)
    }
}
fn read(gpu: &render::Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("night lava linear HDR oracle"),
        size: u64::from(W * H * 8),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 8),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
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
struct Scene {
    terrain: TerrainGpu,
    proxy: LodProxyGpu,
    ao: ContactAoGpu,
    camera: Camera,
    metadata: [u32; 4],
    samples: u32,
    frame: u32,
}
impl Scene {
    fn new(gpu: &render::Gpu, samples: u32) -> Self {
        let mut terrain = TerrainGpu::new_with_samples(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            W,
            H,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 2,
                max_quads: 1024,
                occlusion: false,
            },
            &atlas(),
            samples,
        )
        .unwrap();
        terrain.prepare_scene_sampling(&gpu.device).unwrap();
        terrain.prepare_ambient_mrt(&gpu.device).unwrap();
        let mut mesh = PageMesh::default();
        for (x, material) in [(0, 185), (8, 184)] {
            mesh.opaque.push(
                QuadDescriptor::new([x, 1, 16], Face::Up, [8, 16], material, [0; 4], u32::MAX)
                    .unwrap(),
            );
        }
        let base = terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &mesh,
                &mut UploadBudget::new(100000),
            )
            .unwrap();
        let shadows = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution: 32,
                enabled: false,
                ..Default::default()
            },
            [16., 0., 8.],
            [0., -1., 0.],
        )
        .unwrap();
        let mut light = LightVolumeGpu::new(&gpu.device, 2).unwrap();
        light
            .upload_uniform(
                &gpu.queue,
                base,
                1,
                PackedLight::new(0, 0, [0; 3]).unwrap(),
                &mut UploadBudget::new(100),
            )
            .unwrap();
        terrain
            .attach_lighting(&gpu.device, &shadows, &light)
            .unwrap();
        terrain
            .prepare_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared(None)).unwrap(),
                false,
            )
            .unwrap();
        let mut proxy = LodProxyGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ProxyConfig {
                max_objects: 1,
                max_payload_words: 4096,
                atlas_layers: 186,
                instrument: false,
            },
            Some(&shadows),
        )
        .unwrap();
        let mut public = PublicPage::filled(PublicVoxel::Air);
        for z in 0..16 {
            for x in 0..16 {
                public.set(
                    [x, 0, z],
                    PublicVoxel::solid(if x < 8 { 185 } else { 184 }, u32::MAX),
                );
            }
        }
        let payload =
            ProxyPayload::from_public(&MaskedRayProxy::from_protected(&public, 4), 186).unwrap();
        proxy
            .upload(
                &gpu.queue,
                None,
                1,
                translation([16., 0., 0.]),
                [0.; 4],
                &payload,
                &mut UploadBudget::new(100000),
            )
            .unwrap();
        let camera = Camera::orthographic(
            [16., 24., 8.],
            [16., 0., 8.],
            [0., 0., -1.],
            10.,
            2.,
            0.1,
            100.,
        );
        let ao = ContactAoGpu::new(
            &gpu.device,
            &gpu.queue,
            W,
            H,
            samples,
            ContactAoConfig::default(),
            ContactAoInputs {
                hdr: terrain.scene_color_msaa_view().unwrap(),
                ambient: terrain.ambient_msaa_view().unwrap(),
                depth: terrain.scene_depth_msaa_view(),
                statistics: None,
            },
            &mut UploadBudget::new(10000),
        )
        .unwrap()
        .unwrap();
        terrain.take_work();
        proxy.take_work();
        Self {
            terrain,
            proxy,
            ao,
            camera,
            metadata: prepared(None).material_metadata()[LAVA],
            samples,
            frame: 0,
        }
    }
    fn encode_scene(&mut self, gpu: &render::Gpu, encoder: &mut wgpu::CommandEncoder) {
        self.terrain.invalidate();
        self.proxy
            .prepare_frame_extent(
                &gpu.queue,
                encoder,
                ProxyCamera {
                    camera: self.camera,
                    eye: [16., 24., 8.],
                    viewport_height: H,
                    orthographic_direction: Some([0., -1., 0.]),
                },
                W,
            )
            .unwrap();
        self.terrain
            .encode_scene_with_extra(
                &gpu.queue,
                encoder,
                &self.camera,
                CullingMode::Frustum,
                false,
                |pass| self.proxy.draw(pass),
            )
            .unwrap();
    }
    fn frame(&mut self, gpu: &render::Gpu) -> Vec<u8> {
        self.frame += 1;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.encode_scene(gpu, &mut encoder);
        let work = self
            .ao
            .encode(
                &gpu.queue,
                &mut encoder,
                self.camera,
                self.terrain.hdr_revision(),
                &mut UploadBudget::new(10000),
                10_000_000,
            )
            .unwrap();
        assert_eq!(work.passes, 1);
        gpu.queue.submit([encoder.finish()]);
        let before = read(gpu, self.terrain.resolved_hdr_texture());
        let after = read(gpu, self.ao.resolved_texture());
        exact_image(&before, &after, self.samples, self.frame, "shipping AO");
        after
    }
    fn replace(&mut self, gpu: &render::Gpu, alpha: Option<u8>, enabled: bool) {
        self.terrain.take_work();
        let prepared = prepared(alpha);
        let metadata = if enabled {
            prepared.material_metadata()[LAVA]
        } else {
            crate::prepared(None).material_metadata()[LAVA]
        };
        let expected_bytes = prepared.gpu_bytes
            + 32
            + if self.metadata != metadata {
                186 * 16
            } else {
                0
            };
        self.terrain
            .replace_material_pack(
                &gpu.device,
                &gpu.queue,
                MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap(),
                enabled,
            )
            .unwrap();
        let work = self.terrain.take_work();
        assert_eq!(work.pipeline_compilations, 0);
        assert_eq!(work.quads_emitted, 0);
        assert_eq!(work.cells_scanned, 0);
        assert_eq!(work.upload_bytes, expected_bytes);
        self.metadata = metadata;
    }
}
fn exact_image(before: &[u8], after: &[u8], samples: u32, frame: u32, label: &str) {
    assert!(
        compare_image(before, after, samples, frame, label),
        "contact AO must not remove emitted radiance in a known-dark scene; AA{samples} frame{frame} {label}"
    );
}
fn compare_image(before: &[u8], after: &[u8], samples: u32, frame: u32, label: &str) -> bool {
    let first = before.iter().zip(after).position(|(a, b)| a != b);
    let differing = before.iter().zip(after).filter(|(a, b)| a != b).count();
    if before == after {
        return true;
    }
    eprintln!(
        "::error title=BUILTIN_EMISSION_CONTROL::AA{samples} frame{frame} {label}: byteDiff={differing} firstByte={first:?} sourceNonzero={} outputNonzero={} sourceHead={:?} outputHead={:?}",
        before.iter().filter(|&&byte| byte != 0).count(),
        after.iter().filter(|&&byte| byte != 0).count(),
        &before[..before.len().min(16)],
        &after[..after.len().min(16)],
    );
    false
}
struct CopyControl {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    color: wgpu::TextureView,
    resolved_view: wgpu::TextureView,
    resolved: wgpu::Texture,
    samples: u32,
    label: &'static str,
}
impl CopyControl {
    fn new(gpu: &render::Gpu, scene: &Scene, uv: bool, builtin: bool) -> Self {
        let samples = scene.samples;
        let label = match (uv, builtin) {
            (false, false) => "SV_Position fixed mip0",
            (true, false) => "UV fixed mip0",
            (true, true) => "UV builtin sample index",
            (false, true) => "SV_Position builtin sample index",
        };
        let source = r#"
@group(0)@binding(0)var source:TEXTURE_TYPE<f32>;
struct V{@builtin(position)position:vec4<f32>,@location(0)@interpolate(linear,sample)uv:vec2<f32>};
@vertex fn vertex(@builtin(vertex_index)i:u32)->V{
 let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
 var out:V;out.position=vec4(p[i],0.,1.);out.uv=vec2(p[i].x*.5+.5,.5-p[i].y*.5);return out;
}
@fragment fn fragment(input:V,@builtin(sample_index)sample:u32)->@location(0)vec4<f32>{
 return textureLoad(source,PIXEL,INDEX);
}
"#
        .replace(
            "TEXTURE_TYPE",
            if samples == 1 {
                "texture_2d"
            } else {
                "texture_multisampled_2d"
            },
        )
        .replace(
            "PIXEL",
            if uv {
                "vec2<i32>(input.uv*vec2(128.,64.))"
            } else {
                "vec2<i32>(input.position.xy)"
            },
        )
        .replace("INDEX", if builtin { "i32(sample)" } else { "0" });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: samples > 1,
                    },
                    count: None,
                }],
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    scene.terrain.scene_color_msaa_view().unwrap(),
                ),
            }],
        });
        Self::with_bindings(gpu, samples, &layout, bind, &source, label)
    }
    fn with_bindings(
        gpu: &render::Gpu,
        samples: u32,
        layout: &wgpu::BindGroupLayout,
        bind: wgpu::BindGroup,
        source: &str,
        label: &'static str,
    ) -> Self {
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[layout],
                push_constant_ranges: &[],
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: samples,
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
        let make = |count| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: W,
                    height: H,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: count,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | if count == 1 {
                        wgpu::TextureUsages::COPY_SRC
                    } else {
                        wgpu::TextureUsages::empty()
                    },
                view_formats: &[],
            })
        };
        let texture = make(samples);
        let resolved = if samples == 1 {
            texture.clone()
        } else {
            make(1)
        };
        Self {
            pipeline,
            bind,
            color: texture.create_view(&Default::default()),
            resolved_view: resolved.create_view(&Default::default()),
            resolved,
            samples,
            label,
        }
    }
    fn encode(&self, encoder: &mut wgpu::CommandEncoder) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(self.label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.color,
                depth_slice: None,
                resolve_target: (self.samples > 1).then_some(&self.resolved_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.,
                        g: 0.,
                        b: 1.,
                        a: 1.,
                    }),
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
}
fn source_passthrough_controls(samples: u32) {
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true))
        .expect("actual GPU required");
    let mut scene = Scene::new(&gpu, samples);
    let mut controls = Vec::new();
    if samples == 1 {
        controls.push(CopyControl::new(&gpu, &scene, false, false));
        controls.push(CopyControl::new(&gpu, &scene, true, false));
    }
    controls.push(CopyControl::new(&gpu, &scene, false, true));
    controls.push(CopyControl::new(&gpu, &scene, true, true));
    let mut zero = ContactAoGpu::new(
        &gpu.device,
        &gpu.queue,
        W,
        H,
        samples,
        ContactAoConfig {
            strength: 0.,
            ..Default::default()
        },
        ContactAoInputs {
            hdr: scene.terrain.scene_color_msaa_view().unwrap(),
            ambient: scene.terrain.ambient_msaa_view().unwrap(),
            depth: scene.terrain.scene_depth_msaa_view(),
            statistics: None,
        },
        &mut UploadBudget::new(10000),
    )
    .unwrap()
    .unwrap();
    let source = render::contact_ao::shader(samples, false);
    let (prefix, _) = source.split_once("@fragment fn fragment").unwrap();
    let same_bind_entry = r#"
@fragment fn fragment(input:ScreenVertex,@builtin(sample_index)sample:u32)->@location(0)vec4<f32>{
 return textureLoad(hdr,vec2<i32>(input.uv*DIMENSIONS),i32(sample));
}
"#;
    let (layout, bind) = scene.ao.diagnostic_bindings();
    for (label, dimensions) in [
        ("bind constant dimensions", "vec2(128.,64.)"),
        ("bind uniform dimensions", "vec2<f32>(params.image.xy)"),
    ] {
        controls.push(CopyControl::with_bindings(
            &gpu,
            samples,
            layout,
            bind.clone(),
            &format!(
                "{prefix}{}",
                same_bind_entry.replace("DIMENSIONS", dimensions)
            ),
            label,
        ));
    }
    let no_visibility = source.replace("let ao=visibility(position,sample);", "let ao=1.;");
    assert_ne!(source, no_visibility);
    controls.push(CopyControl::with_bindings(
        &gpu,
        samples,
        layout,
        bind.clone(),
        &no_visibility,
        "full entry no visibility call",
    ));
    let legacy_two_return = format!(
        r#"{prefix}
@fragment fn fragment(input:ScreenVertex,@builtin(sample_index)sample:u32)->@location(0)vec4<f32>{{
 let position=vec4(input.uv*vec2<f32>(params.image.xy),0.,1.);record_fragment();
 let pixel=vec2<i32>(position.xy);let original=textureLoad(hdr,pixel,i32(sample));
 let component=textureLoad(ambient,pixel,i32(sample)).rgb;
 if all(component==vec3(0.)){{return original;}}
 let ao=visibility(position,sample);if ao==1.{{return original;}}
 return vec4(max(original.rgb-component*(1.-ao),vec3(0.)),original.a);
}}
"#
    );
    controls.push(CopyControl::with_bindings(
        &gpu,
        samples,
        layout,
        bind.clone(),
        &legacy_two_return,
        "legacy two-return calibration",
    ));
    let counters = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("AO WARP actual fragment counters"),
        size: 16,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    gpu.queue.write_buffer(&counters, 0, &[0; 16]);
    let mut instrumented = ContactAoGpu::new(
        &gpu.device,
        &gpu.queue,
        W,
        H,
        samples,
        ContactAoConfig::default(),
        ContactAoInputs {
            hdr: scene.terrain.scene_color_msaa_view().unwrap(),
            ambient: scene.terrain.ambient_msaa_view().unwrap(),
            depth: scene.terrain.scene_depth_msaa_view(),
            statistics: Some(&counters),
        },
        &mut UploadBudget::new(10000),
    )
    .unwrap()
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    scene.encode_scene(&gpu, &mut encoder);
    scene
        .ao
        .encode(
            &gpu.queue,
            &mut encoder,
            scene.camera,
            scene.terrain.hdr_revision(),
            &mut UploadBudget::new(10000),
            10_000_000,
        )
        .unwrap();
    for control in &controls {
        control.encode(&mut encoder);
    }
    zero.encode(
        &gpu.queue,
        &mut encoder,
        scene.camera,
        scene.terrain.hdr_revision(),
        &mut UploadBudget::new(10000),
        10_000_000,
    )
    .unwrap();
    instrumented
        .encode(
            &gpu.queue,
            &mut encoder,
            scene.camera,
            scene.terrain.hdr_revision(),
            &mut UploadBudget::new(10000),
            10_000_000,
        )
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let before = read(&gpu, scene.terrain.resolved_hdr_texture());
    assert_eq!(energy(&before, 1.), 192);
    let mut exact = true;
    let mut matrix = Vec::new();
    for control in &controls {
        let image = read(&gpu, &control.resolved);
        if control.label == "legacy two-return calibration" {
            // Historical failing shader is diagnostic only. Every production
            // path below retains its exact, unconditional image assertion.
            matrix.push(format!("legacyTwoReturn={}", u32::from(before == image)));
            continue;
        }
        let passed = compare_image(&before, &image, samples, 0, control.label);
        exact &= passed;
        matrix.push(format!("{}={}", control.label, u32::from(passed)));
    }
    for (label, texture) in [
        ("shipping", scene.ao.resolved_texture()),
        ("strength0", zero.resolved_texture()),
        ("instrumented", instrumented.resolved_texture()),
    ] {
        let passed = compare_image(&before, &read(&gpu, texture), samples, 0, label);
        exact &= passed;
        matrix.push(format!("{label}={}", u32::from(passed)));
    }
    let stats = read_counters(&gpu, &counters);
    let expected_fragments = W * H * samples;
    eprintln!(
        "::notice title=AO_CONTROL_MATRIX::AA{samples} sourceNonzero={} {} counters={stats:?} expectedFragments={expected_fragments}",
        before.iter().filter(|&&byte| byte != 0).count(),
        matrix.join("; "),
    );
    assert_eq!(
        stats[0], expected_fragments,
        "actual AO fragment invocations"
    );
    assert!(
        exact,
        "AA{samples} actual same-frame passthrough controls differ"
    );
}
fn read_counters(gpu: &render::Gpu, source: &wgpu::Buffer) -> [u32; 4] {
    let target = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("AO WARP counter readback"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(source, 0, &target, 0, 16);
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    target.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).unwrap();
    });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let words = {
        let data = target.slice(..).get_mapped_range();
        std::array::from_fn(|i| u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()))
    };
    target.unmap();
    words
}
#[test]
fn contact_ao_source_passthrough_aa1_controls() {
    source_passthrough_controls(1);
}
#[test]
fn contact_ao_source_passthrough_aa4_controls() {
    source_passthrough_controls(4);
}
fn energy(bytes: &[u8], expected: f32) -> usize {
    let mut witnesses = 0;
    // World x4/12 (near) and x20/28 (proxy), away from all MSAA/voxel boundaries.
    for (x, emissive) in [(26usize, true), (51, false), (77, true), (102, false)] {
        for y in 24..40 {
            for dx in 0..3 {
                let pixel = y * W as usize + x + dx;
                assert_eq!(
                    half(bytes, pixel, 3),
                    1.,
                    "missing geometry witness x={x} y={y}"
                );
                let actual = half(bytes, pixel, 0);
                let target = if emissive { expected } else { 0. };
                assert!(
                    (actual - target).abs() < 0.004,
                    "night radiance x={x}: {actual} != {target}"
                );
                assert!(half(bytes, pixel, 1) <= actual);
                assert_eq!(half(bytes, pixel, 2), 0.);
                witnesses += 1;
            }
        }
    }
    witnesses
}
#[test]
fn night_lava_near_proxy_and_contact_ao_preserve_emission_and_explicit_pack_override() {
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true))
        .expect("actual GPU required");
    for samples in [1, 4] {
        let mut scene = Scene::new(&gpu, samples);
        assert_eq!(energy(&scene.frame(&gpu), 1.), 192);
        scene.replace(&gpu, None, true);
        energy(&scene.frame(&gpu), 1.);
        for alpha in [255, 0, 128, 254] {
            scene.replace(&gpu, Some(alpha), true);
            energy(
                &scene.frame(&gpu),
                if alpha == 255 {
                    0.
                } else {
                    f32::from(alpha) / 254.
                },
            );
        }
        scene.replace(&gpu, Some(0), false);
        energy(&scene.frame(&gpu), 1.);
        eprintln!(
            "Lava emission AA{samples}: 7 frames × 192 near/proxy HDR witnesses; AO unchanged, explicit _s overrides, replacement 0 pipelines/geometry"
        );
    }
}
