//! Prepared M10 frame graph. Each consumer writes a separate HDR target.
//! World sampling and authoritative fluid/lighting updates belong to the client.
use crate::{
    OutputTransfer,
    atmosphere::{
        AtmosphereConfig, AtmosphereError, AtmosphereGpu, AtmosphereState, AtmosphereWork,
    },
    cloud_gpu::{CloudConfig, CloudError, CloudGpu, CloudInputs, CloudState, CloudWork},
    gi::{GiConfig, GiError, GiGpu, GiWork},
    gpu_time::{PassGraph, TimingSource},
    light_volume::LightVolumeGpu,
    post::{Exposure, PostConfig, PostError, PostGpu, PostWork},
    shadows::ShadowGpu,
    terrain_gpu::{
        Camera, CullingMode, GpuPassKind, TerrainError, TerrainFrameWork, TerrainGpu, UploadBudget,
    },
    water_gpu::{WaterConfig, WaterError, WaterGpu, WaterInputs, WaterState, WaterWork},
};
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentConfig {
    pub atmosphere: AtmosphereConfig,
    pub clouds: Option<CloudConfig>,
    pub water: Option<WaterConfig>,
    pub gi: Option<GiConfig>,
    pub post: PostConfig,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvironmentState {
    pub eye: [f32; 3],
    pub sun: [f32; 3],
    pub sun_intensity: f32,
    pub altitude_km: f32,
    pub time_seconds: f32,
    pub delta_seconds: f32,
    pub tick: u64,
    pub underwater: bool,
}
impl EnvironmentState {
    fn atmosphere(self) -> AtmosphereState {
        AtmosphereState {
            sun_direction: self.sun,
            altitude_km: self.altitude_km,
        }
    }
    fn valid(self) -> bool {
        self.eye
            .iter()
            .chain(self.sun.iter())
            .chain(
                [
                    self.sun_intensity,
                    self.altitude_km,
                    self.time_seconds,
                    self.delta_seconds,
                ]
                .iter(),
            )
            .all(|v| v.is_finite())
            && self.sun.iter().map(|v| v * v).sum::<f32>() > 1e-12
            && self.sun_intensity >= 0.
            && (0.0..100.).contains(&self.altitude_km)
            && (0.0..=0.1).contains(&self.delta_seconds)
    }
}
/// Independent logical-work limits; no limit is inferred from target-GPU time.
#[derive(Clone, Copy, Debug)]
pub struct EnvironmentBudget {
    pub upload_bytes: u64,
    pub atmosphere_steps: u64,
    pub water_reflection_steps: u64,
    pub cloud_density_samples: u64,
    pub gi_rays: u32,
    pub post_pixels: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentWork {
    pub atmosphere: AtmosphereWork,
    pub terrain: TerrainFrameWork,
    pub water: WaterWork,
    pub clouds: CloudWork,
    pub gi: GiWork,
    pub post: PostWork,
    pub upload_bytes: u64,
    pub passes: u32,
    pub draws: u32,
}
#[derive(Debug, PartialEq, Eq)]
pub enum EnvironmentError {
    InvalidState,
    MissingLighting,
    UploadBudget,
    WorkBudget,
    Atmosphere(AtmosphereError),
    Terrain(TerrainError),
    Water(WaterError),
    Cloud(CloudError),
    Gi(GiError),
    Post(PostError),
}
impl std::fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EnvironmentError {}
macro_rules! error_from {
    ($type:ty,$variant:ident) => {
        impl From<$type> for EnvironmentError {
            fn from(value: $type) -> Self {
                Self::$variant(value)
            }
        }
    };
}
error_from!(AtmosphereError, Atmosphere);
error_from!(TerrainError, Terrain);
error_from!(WaterError, Water);
error_from!(CloudError, Cloud);
error_from!(GiError, Gi);
error_from!(PostError, Post);

pub struct EnvironmentGpu {
    config: EnvironmentConfig,
    atmosphere: AtmosphereGpu,
    atmosphere_state: AtmosphereState,
    water: Option<WaterGpu>,
    clouds: Option<CloudGpu>,
    gi: Option<GiGpu>,
    post: PostGpu,
    water_cover_ready: bool,
    precloud_revision: u64,
    output_revision: u64,
    last_delta: f32,
    last_gi_attempt: Option<(u64, u32)>,
    last_work: EnvironmentWork,
}
impl EnvironmentGpu {
    /// Prepare once during loading, before the first terrain frame. GI requires
    /// the actual M4 resources so the receiver never substitutes a fake light field.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        width: u32,
        height: u32,
        transfer: OutputTransfer,
        config: EnvironmentConfig,
        state: EnvironmentState,
        lighting: Option<(&ShadowGpu, &LightVolumeGpu)>,
        budget: &mut UploadBudget,
    ) -> Result<Self, EnvironmentError> {
        if !state.valid() {
            return Err(EnvironmentError::InvalidState);
        }
        if config.gi.is_some() && lighting.is_none() {
            return Err(EnvironmentError::MissingLighting);
        }
        let atmosphere =
            AtmosphereGpu::new(device, queue, config.atmosphere, state.atmosphere(), budget)?;
        let gi = if let Some(config) = config.gi {
            Some(GiGpu::new(device, queue, config, budget)?)
        } else {
            None
        };
        terrain.prepare_scene_sampling(device)?;
        terrain.attach_atmosphere(device, &atmosphere)?;
        if let (Some(gi), Some((shadows, lights))) = (&gi, lighting) {
            terrain.attach_lighting_with_gi(device, shadows, lights, gi)?;
        }
        let water = if let Some(config) = config.water {
            Some(WaterGpu::new(
                device,
                queue,
                config,
                width,
                height,
                WaterInputs {
                    color_msaa: terrain.scene_color_msaa_view().expect("prepared sampling"),
                    depth_msaa: terrain.scene_depth_msaa_view(),
                    sky: atmosphere.sky_view(),
                    transmittance: atmosphere.transmittance_view(),
                },
                lighting.map(|pair| pair.0),
            )?)
        } else {
            None
        };
        let clouds = if let Some(config) = config.clouds {
            Some(CloudGpu::new(
                device,
                queue,
                config,
                width,
                height,
                CloudInputs {
                    transmittance: atmosphere.transmittance_view(),
                    sky: atmosphere.sky_view(),
                    scene_color_msaa: water.as_ref().map_or_else(
                        || terrain.scene_color_msaa_view().expect("prepared sampling"),
                        WaterGpu::color_msaa,
                    ),
                    scene_depth_msaa: water
                        .as_ref()
                        .map_or_else(|| terrain.scene_depth_msaa_view(), WaterGpu::depth_msaa),
                },
            )?)
        } else {
            None
        };
        let source = clouds
            .as_ref()
            .map(CloudGpu::resolved_view)
            .or_else(|| water.as_ref().map(WaterGpu::resolved_view))
            .unwrap_or_else(|| terrain.resolved_hdr_view());
        let post = PostGpu::new(
            device,
            queue,
            source,
            width,
            height,
            terrain.output_format(),
            transfer,
            config.post,
            budget,
        )?;
        Ok(Self {
            config,
            atmosphere,
            atmosphere_state: state.atmosphere(),
            water,
            clouds,
            gi,
            post,
            water_cover_ready: false,
            precloud_revision: 0,
            output_revision: 0,
            last_delta: 1. / 60.,
            last_gi_attempt: None,
            last_work: EnvironmentWork::default(),
        })
    }
    /// Resize targets and bindings while retaining every prepared pipeline,
    /// descriptor arena, water quad and GI probe. No full-LUT invalidation.
    #[allow(clippy::too_many_arguments)]
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        width: u32,
        height: u32,
        budget: &mut UploadBudget,
    ) -> Result<(), EnvironmentError> {
        if budget.remaining_bytes < 64 {
            return Err(EnvironmentError::UploadBudget);
        }
        terrain.resize(device, width, height)?;
        if let Some(water) = &mut self.water {
            water.resize_inputs(
                device,
                width,
                height,
                WaterInputs {
                    color_msaa: terrain.scene_color_msaa_view().expect("prepared sampling"),
                    depth_msaa: terrain.scene_depth_msaa_view(),
                    sky: self.atmosphere.sky_view(),
                    transmittance: self.atmosphere.transmittance_view(),
                },
            )?;
        }
        if let Some(clouds) = &mut self.clouds {
            clouds.resize_inputs(
                device,
                width,
                height,
                CloudInputs {
                    transmittance: self.atmosphere.transmittance_view(),
                    sky: self.atmosphere.sky_view(),
                    scene_color_msaa: self.water.as_ref().map_or_else(
                        || terrain.scene_color_msaa_view().expect("prepared sampling"),
                        WaterGpu::color_msaa,
                    ),
                    scene_depth_msaa: self
                        .water
                        .as_ref()
                        .map_or_else(|| terrain.scene_depth_msaa_view(), WaterGpu::depth_msaa),
                },
            )?;
        }
        let source = self
            .clouds
            .as_ref()
            .map(CloudGpu::resolved_view)
            .or_else(|| self.water.as_ref().map(WaterGpu::resolved_view))
            .unwrap_or_else(|| terrain.resolved_hdr_view());
        self.post
            .resize_input(device, queue, source, width, height, budget)?;
        self.last_work = EnvironmentWork::default();
        Ok(())
    }
    pub fn water_mut(&mut self) -> Option<&mut WaterGpu> {
        self.water.as_mut()
    }
    pub fn gi_mut(&mut self) -> Option<&mut GiGpu> {
        self.gi.as_mut()
    }
    pub fn gi(&self) -> Option<&GiGpu> {
        self.gi.as_ref()
    }
    /// The client certifies that physical quads cover every placeholder page,
    /// including its current H1 overlay. Never hide incomplete fluid geometry.
    pub fn set_water_cover_ready(
        &mut self,
        terrain: &mut TerrainGpu,
        ready: bool,
    ) -> Result<(), EnvironmentError> {
        if ready && self.water.is_none() {
            return Err(EnvironmentError::InvalidState);
        }
        if self.water_cover_ready != ready {
            self.water_cover_ready = ready;
            terrain.invalidate();
        }
        Ok(())
    }
    fn water_state(
        &self,
        camera: Camera,
        state: EnvironmentState,
        scene_revision: u64,
    ) -> WaterState {
        WaterState {
            camera,
            eye: state.eye,
            time_seconds: if self.water_cover_ready {
                state.time_seconds
            } else {
                0.
            },
            sun: state.sun,
            sun_intensity: state.sun_intensity,
            underwater: state.underwater,
            scene_revision,
        }
    }
    fn cloud_state(
        &self,
        camera: Camera,
        state: EnvironmentState,
        scene_revision: u64,
    ) -> CloudState {
        let animated = self
            .config
            .clouds
            .is_some_and(|config| config.coverage > 0. && config.wind != [0.; 2]);
        CloudState {
            camera,
            eye: state.eye,
            time_seconds: if animated { state.time_seconds } else { 0. },
            sun: state.sun,
            sun_intensity: state.sun_intensity,
            scene_revision,
        }
    }
    pub fn needs_redraw(
        &self,
        terrain: &TerrainGpu,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
    ) -> bool {
        terrain.needs_redraw(camera, mode)
            || self.atmosphere.pending_work().passes > 0
            || self.atmosphere_state != state.atmosphere()
            || self.gi.as_ref().is_some_and(|gi| {
                gi.pending() > 0 && self.last_gi_attempt != Some((state.tick, gi.source_revision()))
            })
            || self.water.as_ref().is_some_and(|water| {
                water.needs_redraw(self.water_state(*camera, state, terrain.hdr_revision()))
            })
            || self.clouds.as_ref().is_some_and(|clouds| {
                clouds.needs_redraw(self.cloud_state(*camera, state, self.precloud_revision))
            })
            || self.post.needs_redraw(self.output_revision)
    }
    /// Complete preflight precedes queue writes. Shadow/light updates and CPU
    /// source publications must precede this call; prepared extras draw in HDR.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        output: &wgpu::TextureView,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
        budget: &mut EnvironmentBudget,
        extra: impl FnOnce(&mut wgpu::RenderPass<'_>) -> u32,
    ) -> Result<EnvironmentWork, EnvironmentError> {
        self.last_work = EnvironmentWork::default();
        if !state.valid() || camera.inverse_view_projection().is_none() {
            return Err(EnvironmentError::InvalidState);
        }
        if !self.needs_redraw(terrain, camera, mode, state) {
            return Ok(self.last_work);
        }
        let mut atmosphere_work = self.atmosphere.pending_work();
        let atmosphere_changed = self.atmosphere_state != state.atmosphere();
        if atmosphere_changed {
            let pixels = u64::from(self.config.atmosphere.sky_size[0])
                * u64::from(self.config.atmosphere.sky_size[1]);
            atmosphere_work.integration_steps = atmosphere_work
                .integration_steps
                .max(pixels * u64::from(self.config.atmosphere.sky_steps));
            // If transmittance is still pending, sky is already included.
        }
        let gi_plan = self
            .gi
            .as_ref()
            .filter(|gi| {
                gi.pending() > 0 && self.last_gi_attempt != Some((state.tick, gi.source_revision()))
            })
            .map_or(0, |gi| {
                let config = self.config.gi.expect("configured GI");
                gi.pending()
                    .min(config.max_jobs)
                    .min(budget.gi_rays / (config.rays_per_probe + 6))
            });
        let scene_needed = terrain.needs_redraw(camera, mode)
            || atmosphere_work.passes > 0
            || atmosphere_changed
            || gi_plan > 0;
        let water_state = self.water_state(
            *camera,
            state,
            terrain.hdr_revision().wrapping_add(u64::from(scene_needed)),
        );
        let water_plan = if let Some(water) = &self.water {
            water.pending_work(water_state)?
        } else {
            WaterWork::default()
        };
        let precloud_changed = scene_needed || water_plan.passes > 0;
        let next_precloud = self
            .precloud_revision
            .wrapping_add(u64::from(precloud_changed));
        let cloud_state = self.cloud_state(*camera, state, next_precloud);
        let cloud_plan = if let Some(clouds) = &self.clouds {
            if clouds.needs_redraw(cloud_state) {
                clouds.worst_work()
            } else {
                CloudWork::default()
            }
        } else {
            CloudWork::default()
        };
        let next_output = self
            .output_revision
            .wrapping_add(u64::from(precloud_changed || cloud_plan.passes > 0));
        let post_plan = self.post.pending_work(next_output);
        if atmosphere_work.integration_steps > budget.atmosphere_steps
            || water_plan.max_reflection_steps > budget.water_reflection_steps
            || cloud_plan.max_density_samples > budget.cloud_density_samples
            || post_plan.pixels() > budget.post_pixels
        {
            return Err(EnvironmentError::WorkBudget);
        }
        let delta_changed = matches!(self.config.post.exposure, Exposure::Automatic { .. })
            && self.last_delta != state.delta_seconds;
        let parameter_bytes = u64::from(atmosphere_changed) * 96
            + u64::from(water_plan.passes > 0) * 256
            + u64::from(cloud_plan.passes > 0) * 176
            + u64::from(delta_changed) * 64
            + u64::from(scene_needed) * 256
            + u64::from(gi_plan) * 16;
        if parameter_bytes > budget.upload_bytes {
            return Err(EnvironmentError::UploadBudget);
        }
        let mut upload = UploadBudget::new(budget.upload_bytes);
        self.atmosphere
            .set_state(queue, state.atmosphere(), &mut upload)?;
        let mut work = EnvironmentWork {
            atmosphere: self
                .atmosphere
                .encode_update(encoder, &mut budget.atmosphere_steps)?,
            ..EnvironmentWork::default()
        };
        self.atmosphere_state = state.atmosphere();
        if gi_plan > 0 {
            let gi = self.gi.as_mut().expect("planned GI");
            work.gi = gi.trace_tick(queue, encoder, state.tick, &mut upload, &mut budget.gi_rays);
            self.last_gi_attempt = Some((state.tick, gi.source_revision()));
        }
        if work.atmosphere.passes > 0 || work.gi.updated_probes > 0 {
            terrain.invalidate();
        }
        if scene_needed {
            work.terrain = terrain.encode_scene_with_extra(
                queue,
                encoder,
                camera,
                mode,
                self.water_cover_ready,
                extra,
            )?;
            upload.remaining_bytes -= work.terrain.camera_upload_bytes;
        }
        if let Some(water) = &mut self.water
            && water_plan.passes > 0
        {
            work.water = water.encode(
                queue,
                encoder,
                water_state,
                &mut upload,
                budget.water_reflection_steps,
            )?;
            budget.water_reflection_steps -= work.water.max_reflection_steps;
        }
        if let Some(clouds) = &mut self.clouds
            && cloud_plan.passes > 0
        {
            work.clouds = clouds.encode(
                queue,
                encoder,
                cloud_state,
                &mut upload,
                budget.cloud_density_samples,
            )?;
            budget.cloud_density_samples -= work.clouds.max_density_samples;
        }
        self.post
            .set_delta(queue, state.delta_seconds, &mut upload)?;
        work.post = self
            .post
            .encode(encoder, output, next_output, &mut budget.post_pixels)?;
        self.last_delta = state.delta_seconds;
        self.precloud_revision = next_precloud;
        self.output_revision = next_output;
        work.upload_bytes = budget.upload_bytes - upload.remaining_bytes;
        budget.upload_bytes = upload.remaining_bytes;
        work.passes = work.atmosphere.passes
            + work.terrain.passes
            + work.water.passes
            + work.clouds.passes
            + work.gi.passes
            + work.post.passes;
        work.draws =
            work.terrain.draw_calls + work.water.draws + work.clouds.draws + work.post.draws;
        self.last_work = work;
        Ok(work)
    }
    /// Call if an encoded graph was abandoned, rather than submitted/presented.
    pub fn invalidate(&mut self, terrain: &mut TerrainGpu, tick: u64) {
        terrain.invalidate();
        self.atmosphere.invalidate();
        if let Some(water) = &mut self.water {
            water.invalidate();
        }
        if let Some(clouds) = &mut self.clouds {
            clouds.invalidate();
        }
        if let Some(gi) = &mut self.gi {
            gi.invalidate(tick);
        }
        self.last_gi_attempt = None;
        self.post.invalidate();
    }
    pub fn extra_timing_sources(&self) -> [Option<TimingSource<'_>>; 5] {
        [
            self.water
                .as_ref()
                .filter(|_| self.last_work.water.passes > 0)
                .and_then(|water| timing(water.timestamp_buffer(), water.graph(), |_| 8)),
            self.clouds
                .as_ref()
                .filter(|_| self.last_work.clouds.passes > 0)
                .and_then(|clouds| timing(clouds.timestamp_buffer(), clouds.graph(), |_| 9)),
            (self.last_work.post.passes > 0)
                .then(|| {
                    timing(
                        self.post.timestamp_buffer(),
                        self.post.frame_graph(),
                        |kind| match kind {
                            1..=3 => 11,
                            4 => 12,
                            5 => 13,
                            _ => 255,
                        },
                    )
                })
                .flatten(),
            self.gi
                .as_ref()
                .filter(|_| self.last_work.gi.passes > 0)
                .and_then(|gi| timing(gi.timestamp_buffer(), gi.frame_graph(), |_| 10)),
            (self.last_work.atmosphere.passes > 0)
                .then(|| {
                    timing(
                        self.atmosphere.timestamp_buffer(),
                        self.atmosphere.frame_graph(),
                        |_| 5,
                    )
                })
                .flatten(),
        ]
    }
    pub fn timing_sources<'a>(&'a self, terrain: &'a TerrainGpu) -> [Option<TimingSource<'a>>; 6] {
        let scene = if self.last_work.terrain.passes > 0 {
            terrain.timestamp_buffer().map(|(buffer, period)| {
                let graph = terrain.frame_graph();
                let mut kinds = [255; 32];
                for (index, &kind) in graph.passes[..graph.count as usize].iter().enumerate() {
                    kinds[index] = match kind {
                        GpuPassKind::CullFrustum => 0,
                        GpuPassKind::DepthPrepass => 1,
                        GpuPassKind::HiZBase | GpuPassKind::HiZReduce => 2,
                        GpuPassKind::CullOcclusion => 3,
                        GpuPassKind::Color => 6,
                        GpuPassKind::Presentation => 13,
                        _ => 255,
                    };
                }
                TimingSource {
                    buffer,
                    period,
                    kinds,
                    count: graph.count,
                }
            })
        } else {
            None
        };
        let [water, clouds, post, gi, atmosphere] = self.extra_timing_sources();
        [scene, water, clouds, post, gi, atmosphere]
    }
}
fn timing<const N: usize>(
    source: Option<(&wgpu::Buffer, f32)>,
    graph: PassGraph<N>,
    kind: impl Fn(u32) -> u8,
) -> Option<TimingSource<'_>> {
    source.filter(|_| graph.count > 0).map(|(buffer, period)| {
        let mut kinds = [255; 32];
        for (index, &value) in graph.kinds[..graph.count as usize].iter().enumerate() {
            kinds[index] = kind(value);
        }
        TimingSource {
            buffer,
            period,
            kinds,
            count: graph.count,
        }
    })
}
