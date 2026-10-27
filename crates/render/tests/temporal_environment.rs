#![cfg(not(target_arch = "wasm32"))]
//! Graph integration only: the motion pass clears unsupported surfaces. Actual
//! model/terrain vectors and optical masks have independent source image gates.
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::{
        EnvironmentBudget, EnvironmentConfig, EnvironmentError, EnvironmentGpu, EnvironmentState,
        TemporalInputs,
    },
    model_motion::{MotionTargets, TargetConfig, Work},
    post::{Exposure, PostConfig},
    taa::{ResetReasons, TaaConfig, TaaFrame},
    terrain_gpu::{
        AtlasImage, AtlasMip, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
use std::cell::Cell;
const EXTENT: [u32; 2] = [64, 32];
const SURFACE: [u32; 2] = [80, 40];
const PIXELS: u64 = 64 * 32;
const TONE_PIXELS: u64 = 80 * 40;

struct Fixture {
    terrain: TerrainGpu,
    environment: EnvironmentGpu,
    motion: Option<MotionTargets>,
    texture: wgpu::Texture,
    output: wgpu::TextureView,
    camera: Camera,
    state: EnvironmentState,
}
impl Fixture {
    fn new(gpu: &render::Gpu, temporal: bool, post: PostConfig) -> Self {
        let atlas = AtlasImage {
            tile_size: 1,
            layers: 1,
            mips: vec![AtlasMip {
                size: 1,
                rgba: vec![180, 90, 40, 255],
                tint_mask: vec![255],
            }],
        };
        let mut terrain = TerrainGpu::new_with_samples(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            EXTENT[0],
            EXTENT[1],
            OutputTransfer::ShaderSrgb,
            GpuConfig {
                max_pages: 4,
                max_quads: 4096,
                occlusion: true,
            },
            &atlas,
            1,
        )
        .unwrap();
        let state = EnvironmentState {
            eye: [26., 31., 29.],
            sun: [0.35, 0.8, 0.2],
            sun_intensity: 1.,
            altitude_km: 0.072,
            time_seconds: 0.,
            delta_seconds: 1. / 60.,
            tick: 0,
            underwater: false,
        };
        let mut upload = UploadBudget::new(1_000_000);
        let mut environment = EnvironmentGpu::new(
            &gpu.device,
            &gpu.queue,
            &mut terrain,
            EXTENT[0],
            EXTENT[1],
            OutputTransfer::ShaderSrgb,
            EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                post,
                ..Default::default()
            },
            state,
            None,
            &mut upload,
        )
        .unwrap();
        environment
            .post_mut()
            .set_output_extent_with_device(&gpu.device, SURFACE[0], SURFACE[1])
            .unwrap();
        let motion = MotionTargets::new(
            &gpu.device,
            &gpu.queue,
            temporal.then_some(TargetConfig {
                extent: EXTENT,
                samples: 1,
                max_pixels: PIXELS,
                memory_budget_bytes: 1024 * 1024,
            }),
        )
        .unwrap();
        if let Some(motion) = &motion {
            environment
                .prepare_temporal(
                    &gpu.device,
                    &gpu.queue,
                    &terrain,
                    TemporalInputs {
                        motion: motion.motion_view(),
                        surface: motion.surface_view(),
                    },
                    TaaConfig::default(),
                    &mut upload,
                )
                .unwrap();
        }
        let mut mesh = PageMesh::default();
        mesh_page(
            &PageHalo::from_page(&[1; 4096], 0),
            |raw| {
                if raw == 0 {
                    CellMaterial::EMPTY
                } else {
                    CellMaterial::opaque(0)
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
                &mut upload,
            )
            .unwrap();
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("temporal graph physical output"),
            size: wgpu::Extent3d {
                width: SURFACE[0],
                height: SURFACE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output = texture.create_view(&Default::default());
        Self {
            terrain,
            environment,
            motion,
            texture,
            output,
            camera: Camera::perspective(state.eye, [8.; 3], [0., 1., 0.], 1., 2., 0.1, 200.),
            state,
        }
    }
    fn encode(
        &mut self,
        gpu: &render::Gpu,
        encoder: &mut wgpu::CommandEncoder,
        budget: &mut EnvironmentBudget,
        callbacks: &Cell<u32>,
    ) -> Result<render::environment_gpu::EnvironmentWork, EnvironmentError> {
        let depth = self.terrain.scene_depth_msaa_view().clone();
        let targets = self.motion.as_ref();
        self.environment.encode_with_temporal(
            &gpu.queue,
            encoder,
            &mut self.terrain,
            &self.output,
            &self.camera,
            CullingMode::FrustumAndHiZ,
            self.state,
            budget,
            |_| 0,
            0,
            |_, encoder, _| {
                callbacks.set(callbacks.get() + 1);
                targets
                    .unwrap()
                    .encode(encoder, &depth, |_| Work::default());
                Ok(0)
            },
        )
    }
}
fn budget() -> EnvironmentBudget {
    EnvironmentBudget {
        contact_depth_reads: 0,
        aerial_steps: 1_000_000,
        aerial_samples: 1_000_000,
        upload_bytes: 10000,
        atmosphere_steps: 100000,
        water_reflection_steps: 0,
        cloud_density_samples: 0,
        gi_rays: 0,
        post_pixels: 1_000_000,
    }
}
fn frame(id: u64) -> TaaFrame {
    TaaFrame {
        frame: id,
        space: 0,
        scene_revision: 1,
        geometry_epoch: 1,
        source_epoch: 1,
        jitter_pixels: [0.; 2],
        reset: ResetReasons::NONE,
    }
}
fn image(gpu: &render::Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let row = (SURFACE[0] * 4).div_ceil(256) * 256;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("completed temporal graph pixels"),
        size: u64::from(row) * u64::from(SURFACE[1]),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(SURFACE[1]),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let (send, receive) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| send.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    receive.recv().unwrap().unwrap();
    let mapped = readback.slice(..).get_mapped_range();
    let pixels = mapped
        .chunks_exact(row as usize)
        .flat_map(|line| line[..SURFACE[0] as usize * 4].iter().copied())
        .collect();
    drop(mapped);
    readback.unmap();
    pixels
}

#[test]
fn temporal_graph_preflights_motion_then_resolves_hdr_before_physical_tone() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let post = PostConfig {
        exposure: Exposure::Manual(1.),
        bloom: None,
        ..Default::default()
    };
    let mut plain = Fixture::new(&gpu, false, post);
    let mut temporal = Fixture::new(&gpu, true, post);
    assert!(plain.environment.temporal().is_none());
    assert!(plain.motion.is_none());
    assert_eq!(
        temporal.environment.startup_pipeline_compilations(),
        plain.environment.startup_pipeline_compilations() + 1
    );
    assert_eq!(temporal.environment.post().output_extent(), SURFACE);
    let token = temporal
        .environment
        .temporal_mut()
        .unwrap()
        .begin_frame(frame(1))
        .unwrap();
    let calls = Cell::new(0);
    let before_revision = temporal.terrain.hdr_revision();
    temporal.terrain.take_work();
    for denied in [
        EnvironmentBudget {
            post_pixels: TONE_PIXELS + PIXELS - 1,
            ..budget()
        },
        EnvironmentBudget {
            upload_bytes: 0,
            ..budget()
        },
    ] {
        let mut limit = denied;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert!(matches!(
            temporal.encode(&gpu, &mut encoder, &mut limit, &calls),
            Err(EnvironmentError::WorkBudget | EnvironmentError::UploadBudget)
        ));
        assert_eq!(calls.get(), 0);
        assert_eq!(temporal.terrain.hdr_revision(), before_revision);
        assert_eq!(temporal.terrain.take_work().upload_bytes, 0);
        assert_eq!(
            temporal.environment.temporal().unwrap().pending(),
            Some(token)
        );
        assert!(
            temporal
                .environment
                .temporal()
                .unwrap()
                .presented()
                .is_none()
        );
    }
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let baseline = plain
        .encode(&gpu, &mut encoder, &mut budget(), &calls)
        .unwrap();
    assert_eq!(calls.get(), 0);
    let work = temporal
        .encode(&gpu, &mut encoder, &mut budget(), &calls)
        .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(work.temporal.passes, 1);
    assert_eq!(work.temporal.pixels, PIXELS);
    assert_eq!(work.post.tone_pixels, TONE_PIXELS);
    assert_eq!(work.post.aa_pixels, 0);
    assert_eq!(work.upload_bytes, baseline.upload_bytes + 64);
    assert_eq!(work.passes, baseline.passes + 2);
    let sources: Vec<_> = temporal
        .environment
        .timing_sources(&temporal.terrain)
        .into_iter()
        .flatten()
        .chain(temporal.motion.as_ref().unwrap().timing_source(14))
        .collect();
    assert!(sources.len() <= 16);
    assert!(sources.iter().map(|source| source.count).sum::<u32>() <= 64);
    gpu.queue.submit([encoder.finish()]);
    let first = image(&gpu, &temporal.texture);
    assert_eq!(first, image(&gpu, &plain.texture));
    assert!(first.chunks_exact(4).filter(|p| p[0] > 50).count() > 100);
    temporal
        .environment
        .post_mut()
        .mark_temporal_submitted(token.into())
        .unwrap();
    temporal
        .environment
        .post()
        .validate_temporal_commit(token.into())
        .unwrap();
    temporal
        .environment
        .temporal()
        .unwrap()
        .validate_commit(token)
        .unwrap();
    temporal
        .environment
        .post_mut()
        .commit_temporal_presented(token.into())
        .unwrap();
    temporal
        .environment
        .temporal_mut()
        .unwrap()
        .commit_presented(token)
        .unwrap();
    // A rendered candidate that was never presented cannot replace the front.
    let token = temporal
        .environment
        .temporal_mut()
        .unwrap()
        .begin_frame(frame(2))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    temporal
        .encode(&gpu, &mut encoder, &mut budget(), &calls)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    temporal
        .environment
        .post_mut()
        .mark_temporal_submitted(token.into())
        .unwrap();
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    temporal
        .environment
        .temporal_mut()
        .unwrap()
        .discard(token)
        .unwrap();
    temporal
        .environment
        .post_mut()
        .discard_temporal(token.into())
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(matches!(
        temporal.environment.encode_cached_present(
            &mut encoder,
            &temporal.output,
            &mut (TONE_PIXELS - 1)
        ),
        Err(EnvironmentError::WorkBudget)
    ));
    let mut cached_pixels = TONE_PIXELS;
    let cached = temporal
        .environment
        .encode_cached_present(&mut encoder, &temporal.output, &mut cached_pixels)
        .unwrap()
        .unwrap();
    assert_eq!(cached.temporal.history_copy_bytes, PIXELS * 8);
    assert_eq!(
        (cached.passes, cached.post.tone_pixels, cached.upload_bytes),
        (1, TONE_PIXELS, 0)
    );
    assert_eq!(
        (cached.post.histogram_samples, cached.post.bloom_texels),
        (0, 0)
    );
    gpu.queue.submit([encoder.finish()]);
    let post_restore = temporal
        .environment
        .post()
        .temporal_restore_token()
        .unwrap();
    let hdr_restore = temporal
        .environment
        .temporal()
        .unwrap()
        .output_restore_token()
        .unwrap();
    temporal
        .environment
        .post_mut()
        .mark_temporal_restore_submitted(post_restore)
        .unwrap();
    temporal
        .environment
        .temporal_mut()
        .unwrap()
        .mark_output_restore_submitted(hdr_restore)
        .unwrap();
    assert_eq!(image(&gpu, &temporal.texture), first);
    assert_eq!(
        temporal
            .environment
            .temporal()
            .unwrap()
            .presented()
            .unwrap()
            .state
            .frame,
        1
    );
}

#[test]
fn temporal_auto_exposure_and_maximum_bloom_stay_inside_capture_query_limits() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut config = PostConfig::default();
    config.bloom.as_mut().unwrap().levels = 6;
    let mut fixture = Fixture::new(&gpu, true, config);
    let token = fixture
        .environment
        .temporal_mut()
        .unwrap()
        .begin_frame(frame(1))
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let work = fixture
        .encode(&gpu, &mut encoder, &mut budget(), &Cell::new(0))
        .unwrap();
    assert!(work.post.histogram_samples > 0);
    assert!(work.post.bloom_texels > 0);
    let sources: Vec<_> = fixture
        .environment
        .timing_sources(&fixture.terrain)
        .into_iter()
        .flatten()
        .chain(fixture.motion.as_ref().unwrap().timing_source(14))
        .collect();
    let queries: u32 = sources.iter().map(|source| source.count).sum();
    let source_count = sources.len();
    assert!(
        source_count <= 16 && queries <= 64,
        "sources={} queries={queries}",
        source_count
    );
    drop(sources);
    gpu.queue.submit([encoder.finish()]);
    assert!(
        image(&gpu, &fixture.texture)
            .chunks_exact(4)
            .any(|p| p[0] > 0)
    );
    fixture
        .environment
        .post_mut()
        .mark_temporal_submitted(token.into())
        .unwrap();
    fixture
        .environment
        .post_mut()
        .commit_temporal_presented(token.into())
        .unwrap();
    fixture
        .environment
        .temporal_mut()
        .unwrap()
        .commit_presented(token)
        .unwrap();
    println!(
        "TEMPORAL_ENVIRONMENT_QUERY_OK sources={} queries={queries} post_passes={}",
        source_count, work.post.passes
    );
}
