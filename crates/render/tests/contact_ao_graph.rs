#![cfg(not(target_arch = "wasm32"))]
//! Actual full opaque/model MRT -> contact AO -> post graph.
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    block_models::{BlockConfig, BlockInstance, BlockModelsGpu},
    celestial::CelestialConfig,
    contact_ao::ContactAoConfig,
    entity_gpu::{EntityConfig, EntityGpu, EntityInstance},
    environment_gpu::{
        EnvironmentBudget, EnvironmentConfig, EnvironmentGpu, EnvironmentState, EnvironmentWork,
    },
    height_gpu::{HeightGpu, HeightGpuConfig},
    height_tile::{HeightTile, HeightTileKey},
    light_volume::PackedLight,
    lod::{MaskedRayProxy, PublicPage, PublicVoxel},
    lod_gpu::{LodProxyGpu, ProxyCamera, ProxyConfig, ProxyPayload},
    post::{Exposure, PostConfig},
    terrain_gpu::{
        AtlasImage, AtlasMip, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
const W: u32 = 64;
fn budget() -> EnvironmentBudget {
    EnvironmentBudget {
        upload_bytes: 1_000_000,
        contact_depth_reads: 100_000_000,
        atmosphere_steps: 1_000_000,
        aerial_steps: 0,
        aerial_samples: 0,
        water_reflection_steps: 0,
        cloud_density_samples: 0,
        gi_rays: 0,
        post_pixels: 1_000_000,
    }
}
fn read(gpu: &render::Gpu, b: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = b.slice(..).get_mapped_range().to_vec();
    b.unmap();
    bytes
}
fn output(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("joint contact AO final output"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
struct Scene {
    terrain: TerrainGpu,
    env: EnvironmentGpu,
    heights: HeightGpu,
    lod: LodProxyGpu,
    entities: EntityGpu,
    blocks: BlockModelsGpu,
    camera: Camera,
    state: EnvironmentState,
    output: wgpu::Texture,
    width: u32,
    height: u32,
}
impl Scene {
    fn new(gpu: &render::Gpu, samples: u32, quality: Option<u8>) -> Self {
        let atlas = AtlasImage {
            tile_size: 1,
            layers: 186,
            mips: vec![AtlasMip {
                size: 1,
                rgba: vec![255; 186 * 4],
                tint_mask: vec![255; 186],
            }],
        };
        let mut terrain = TerrainGpu::new_with_samples(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            W,
            W,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 65536,
                occlusion: false,
            },
            &atlas,
            samples,
        )
        .unwrap();
        let mut halo = PageHalo::filled(0);
        for z in 0..16 {
            for x in 0..16 {
                halo.set(x, 0, z, 1);
            }
        }
        for y in 1..7 {
            for z in 2..14 {
                halo.set(8, y, z, 1);
            }
        }
        let mut mesh = PageMesh::default();
        mesh_page(
            &halo,
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
        terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                translation([0.; 3]),
                &mesh,
                &mut UploadBudget::new(1_000_000),
            )
            .unwrap();
        let materials = render::material_pack::PreparedMaterials::prepare(
            &atlas,
            &[],
            1,
            render::material_pack::Limits::default(),
        )
        .unwrap();
        terrain
            .prepare_material_pack(
                &gpu.device,
                &gpu.queue,
                render::material_pack::MaterialPackGpu::upload(&gpu.device, &gpu.queue, &materials)
                    .unwrap(),
                true,
            )
            .unwrap();
        let eye = [10., 10., 30.];
        terrain.set_material_view(&gpu.queue, eye, 0).unwrap();
        let camera = Camera::perspective(eye, [8., 1., 8.], [0., 1., 0.], 1., 1., 0.1, 200.);
        let state = EnvironmentState {
            eye,
            sun: [0., 1., 0.],
            sun_intensity: 1.,
            altitude_km: 0.,
            time_seconds: 0.,
            delta_seconds: 0.05,
            tick: 1,
            underwater: false,
        };
        let mut env = EnvironmentGpu::new(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            W,
            W,
            OutputTransfer::Attachment,
            EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                contact_ao: quality.map(|quality| ContactAoConfig {
                    quality,
                    ..Default::default()
                }),
                post: PostConfig {
                    exposure: Exposure::Manual(1.),
                    bloom: None,
                    ..Default::default()
                },
                ..Default::default()
            },
            state,
            None,
            &mut UploadBudget::new(1_000_000),
        )
        .unwrap();
        env.prepare_celestials(
            &gpu.device,
            &gpu.queue,
            CelestialConfig {
                stars: 0,
                ..Default::default()
            },
            &mut UploadBudget::new(1_000_000),
        )
        .unwrap();
        let mut heights = HeightGpu::new_with_atmosphere(
            &gpu.device,
            terrain.geometry(),
            HeightGpuConfig {
                max_tiles: 4,
                atlas_layers: 186,
            },
            None,
            env.atmosphere(),
        )
        .unwrap();
        let mut tile = HeightTile {
            key: HeightTileKey {
                space: 0,
                level: 0,
                x: 1,
                z: 0,
            },
            source: 1,
            heights: [1; 289],
            known: [255; 37],
            atlas: [3; 256],
            tint: [u32::MAX; 256],
            protected: [0; 32],
        };
        tile.known[36] = 1;
        heights
            .upload(
                &gpu.queue,
                None,
                &tile,
                0,
                [0; 3],
                true,
                &mut UploadBudget::new(100000),
            )
            .unwrap();
        let mut lod = LodProxyGpu::new_with_atmosphere(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ProxyConfig {
                max_objects: 4,
                max_payload_words: 65536,
                atlas_layers: 186,
                instrument: false,
            },
            None,
            env.atmosphere(),
        )
        .unwrap();
        let mut page = PublicPage::filled(PublicVoxel::Air);
        for y in 1..5 {
            for z in 4..12 {
                for x in 10..16 {
                    page.set([x, y, z], PublicVoxel::solid(3, u32::MAX));
                }
            }
        }
        let proxy = MaskedRayProxy::from_protected(&page, 4);
        let payload = ProxyPayload::from_public(&proxy, 186).unwrap();
        lod.upload(
            &gpu.queue,
            None,
            1,
            translation([-16., 0., 0.]),
            [0., 0., 0., 1.],
            &payload,
            &mut UploadBudget::new(100000),
        )
        .unwrap();
        let mut entities = EntityGpu::new_with_environment(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 4,
                atlas_layers: 186,
            },
            None,
            None,
            env.atmosphere(),
        )
        .unwrap();
        entities
            .upload(
                &gpu.queue,
                &[EntityInstance {
                    position: [11., 1., 6.],
                    body_yaw: 0.,
                    head_yaw: 0.,
                    pitch: 0.,
                    limb_phase: 0.,
                    limb_amount: 0.,
                    kind: 5,
                    flags: 0,
                    color: 0,
                    id_low: 1,
                    id_high: 0,
                    death_roll: 0.,
                    scale: 1.,
                    reserved: 0.,
                }],
                &[PackedLight::new(15, 0, [0; 3]).unwrap()],
                &mut UploadBudget::new(10000),
            )
            .unwrap();
        let mut blocks = BlockModelsGpu::new_with_atmosphere(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            BlockConfig {
                max_instances: 4,
                atlas_layers: 186,
            },
            None,
            env.atmosphere(),
        )
        .unwrap();
        blocks
            .upload(
                &gpu.queue,
                &[BlockInstance {
                    position: [5., 1., 8.],
                    ..Default::default()
                }],
                &mut UploadBudget::new(10000),
            )
            .unwrap();
        Self {
            terrain,
            env,
            heights,
            lod,
            entities,
            blocks,
            camera,
            state,
            output: output(&gpu.device, W, W),
            width: W,
            height: W,
        }
    }
    fn frame(&mut self, gpu: &render::Gpu) -> EnvironmentWork {
        let mut e = gpu.device.create_command_encoder(&Default::default());
        self.heights
            .prepare_frame(&gpu.queue, &mut e, self.camera)
            .unwrap();
        self.lod
            .prepare_frame_extent(
                &gpu.queue,
                &mut e,
                ProxyCamera {
                    camera: self.camera,
                    eye: self.state.eye,
                    viewport_height: self.height,
                    orthographic_direction: None,
                },
                self.width,
            )
            .unwrap();
        self.entities
            .prepare_frame(&gpu.queue, &mut e, self.camera)
            .unwrap();
        self.blocks
            .prepare_frame(&gpu.queue, &mut e, self.camera)
            .unwrap();
        let view = self.output.create_view(&Default::default());
        let work = self
            .env
            .encode(
                &gpu.queue,
                &mut e,
                &mut self.terrain,
                &view,
                &self.camera,
                CullingMode::Frustum,
                self.state,
                &mut budget(),
                |pass| {
                    self.heights.draw(pass)
                        + self.lod.draw(pass)
                        + self.entities.draw(pass)
                        + self.blocks.draw(pass)
                },
            )
            .unwrap();
        gpu.queue.submit([e.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        work
    }
    fn pixels(&self, gpu: &render::Gpu) -> Vec<u8> {
        let row = (self.width * 4).div_ceil(256) * 256;
        let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * self.height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut e = gpu.device.create_command_encoder(&Default::default());
        e.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &b,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([e.finish()]);
        read(gpu, &b)
    }
    fn sample_values(&self, gpu: &render::Gpu) -> Vec<[[f32; 4]; 4]> {
        let samples = self.terrain.sample_count();
        let code = format!(
            "@group(0)@binding(0)var source:texture_multisampled_2d<f32>;@group(0)@binding(1)var ambient:texture_multisampled_2d<f32>;@group(0)@binding(2)var result:texture_multisampled_2d<f32>;@group(0)@binding(3)var depth:texture_depth_multisampled_2d;@group(0)@binding(4)var<storage,read_write>out:array<vec4<f32>>;@compute @workgroup_size(8,8)fn probe(@builtin(global_invocation_id)i:vec3<u32>){{if i.x>={}u||i.y>={}u{{return;}}let p=vec2<i32>(i.xy);for(var s=0u;s<{}u;s++){{let at=(i.x+{}u*i.y)*{}u*4u+s*4u;out[at]=textureLoad(source,p,i32(s));out[at+1u]=textureLoad(ambient,p,i32(s));out[at+2u]=textureLoad(result,p,i32(s));out[at+3u]=vec4(textureLoad(depth,p,i32(s)));}}}}",
            self.width, self.height, samples, self.width, samples
        );
        let code = if samples == 1 {
            code.replace("texture_depth_multisampled_2d", "texture_depth_2d")
                .replace("texture_multisampled_2d", "texture_2d")
        } else {
            code
        };
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("jointGraph independent energy sample readback"),
                source: wgpu::ShaderSource::Wgsl(code.into()),
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
        let bytes = u64::from(self.width * self.height * samples) * 64;
        let values = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let hdr = self.terrain.scene_color_msaa_view().unwrap();
        let ambient = self.terrain.ambient_msaa_view().unwrap_or(hdr);
        let result = self.env.contact_ao().map_or(hdr, |ao| ao.color_msaa());
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(hdr),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(ambient),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(result),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(
                        self.terrain.scene_depth_msaa_view(),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: values.as_entire_binding(),
                },
            ],
        });
        let mut e = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = e.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(self.width.div_ceil(8), self.height.div_ceil(8), 1);
        }
        e.copy_buffer_to_buffer(&values, 0, &readback, 0, bytes);
        gpu.queue.submit([e.finish()]);
        read(gpu, &readback)
            .chunks_exact(64)
            .map(|record| {
                std::array::from_fn(|v| {
                    std::array::from_fn(|c| {
                        f32::from_le_bytes(
                            record[v * 16 + c * 4..v * 16 + c * 4 + 4]
                                .try_into()
                                .unwrap(),
                        )
                    })
                })
            })
            .collect()
    }
}
#[test]
fn complete_scene_contact_graph_preserves_zero_quality_and_direct_energy_with_all_mrt_variants() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    for samples in [1, 4] {
        let mut baseline = Scene::new(&gpu, samples, None);
        let basework = baseline.frame(&gpu);
        assert_eq!(basework.contact_ao.passes, 0);
        let basepixels = baseline.pixels(&gpu);
        let mut off = Scene::new(&gpu, samples, Some(0));
        let work = off.frame(&gpu);
        assert_eq!(work.contact_ao.passes, 0);
        assert!(off.env.contact_ao().is_none());
        assert!(!off.terrain.geometry().ambient_mrt);
        assert_eq!(off.terrain.ambient_target_bytes(), 0);
        assert_eq!(basepixels, off.pixels(&gpu), "quality0 changed baseline");
        for quality in [1, 2] {
            let mut scene = Scene::new(&gpu, samples, Some(quality));
            assert!(scene.heights.ambient_mrt() && scene.lod.ambient_mrt());
            let startup = scene.env.startup_pipeline_compilations();
            let work = scene.frame(&gpu);
            assert_eq!(work.contact_ao.passes, 1);
            assert_eq!(
                work.contact_ao.composition_samples,
                u64::from(W * W * samples)
            );
            assert_eq!(
                scene.terrain.ambient_target_bytes(),
                u64::from(W * W * samples) * 8
            );
            let values = scene.sample_values(&gpu);
            let mut contacts = 0;
            let mut geometry = 0;
            let mut sky = 0;
            let mut direct_floor = 0;
            for [source, ambient, result, depth] in &values {
                if depth[0] == 0. {
                    assert_eq!(source, result, "AO changed sky/celestial");
                    sky += 1;
                    continue;
                }
                geometry += 1;
                for c in 0..3 {
                    assert!(
                        result[c] >= source[c] - ambient[c].max(0.) - 0.01,
                        "direct energy removed {:?}",
                        [source, ambient, result]
                    );
                    assert!(result[c] <= source[c] + 0.01);
                    if source[c] - ambient[c] > 1. {
                        direct_floor += 1;
                    }
                }
                if (0..3).any(|c| source[c] - result[c] > 0.001) {
                    contacts += 1;
                }
            }
            assert!(
                contacts > 10 && geometry > 100 && sky > 100 && direct_floor > 100,
                "insufficient physical scene witnesses {contacts}/{geometry}/{sky}/{direct_floor}"
            );
            let cached = scene.frame(&gpu);
            assert_eq!(cached.passes, 0);
            assert_eq!(cached.upload_bytes, 0);
            assert_eq!(scene.env.startup_pipeline_compilations(), startup);
            let eye = scene.state.eye;
            scene.camera = Camera::perspective(
                eye,
                [eye[0], eye[1] + 10., eye[2]],
                [0., 0., 1.],
                1.,
                1.,
                0.1,
                200.,
            );
            scene.frame(&gpu);
            let sky_samples = scene.sample_values(&gpu);
            assert!(sky_samples.iter().filter(|p| p[3][0] == 0.).count() > 100);
            for p in sky_samples {
                if p[3][0] == 0. {
                    assert_eq!(p[0], p[2]);
                    assert!(p[1][..3].iter().all(|v| *v == 0.));
                }
            }
            scene.width = 80;
            scene.height = 48;
            scene
                .env
                .resize(
                    &gpu.device,
                    &gpu.queue,
                    &mut scene.terrain,
                    80,
                    48,
                    &mut UploadBudget::new(1_000_000),
                )
                .unwrap();
            scene.output = output(&gpu.device, 80, 48);
            scene.camera =
                Camera::perspective(eye, [8., 1., 8.], [0., 1., 0.], 1., 80. / 48., 0.1, 200.);
            let resized = scene.frame(&gpu);
            assert_eq!(resized.contact_ao.passes, 1);
            assert_eq!(scene.env.startup_pipeline_compilations(), startup);
            assert_eq!(scene.env.contact_ao().unwrap().extent(), [80, 48]);
            println!(
                "AO jointGraph samples{samples} quality{quality} contacts{contacts}/geometry{geometry}/sky{sky}; directfloorSamples{direct_floor}; Terrain+PBR+Height+Proxy+Entity+Chest+Sky+Celestial compatible; AO0BITWISE,cache0,resize0pipelines"
            );
        }
    }
}
