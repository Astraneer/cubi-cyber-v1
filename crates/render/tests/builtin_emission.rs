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
        }
    }
    fn frame(&mut self, gpu: &render::Gpu) -> Vec<u8> {
        self.terrain.invalidate();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.proxy
            .prepare_frame_extent(
                &gpu.queue,
                &mut encoder,
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
                &mut encoder,
                &self.camera,
                CullingMode::Frustum,
                false,
                |pass| self.proxy.draw(pass),
            )
            .unwrap();
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
        assert_eq!(
            before, after,
            "contact AO must not remove emitted radiance in a known-dark scene"
        );
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
