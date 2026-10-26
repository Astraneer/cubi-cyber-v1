//! Prepared M10 frame graph. Each consumer writes a separate HDR target.
//! World sampling and authoritative fluid/lighting updates belong to the client.
use crate::{
    OutputTransfer,
    aerial_gpu::{
        AerialConfig, AerialError, AerialGpu, AerialInputs, AerialShadows, AerialState, AerialWork,
    },
    atmosphere::{
        AtmosphereConfig, AtmosphereError, AtmosphereGpu, AtmosphereState, AtmosphereWork,
    },
    celestial::{CelestialConfig, CelestialError, CelestialGpu, CelestialState, CelestialWork},
    cloud_gpu::{CloudConfig, CloudError, CloudGpu, CloudInputs, CloudState, CloudWork},
    gi::{GiConfig, GiError, GiGpu, GiWork},
    gpu_time::{PassGraph, TimingSource},
    light_volume::LightVolumeGpu,
    multiple_scattering::MultipleScatteringConfig,
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
    pub aerial_steps: u64,
    pub aerial_samples: u64,
    pub atmosphere_steps: u64,
    pub water_reflection_steps: u64,
    pub cloud_density_samples: u64,
    pub gi_rays: u32,
    pub post_pixels: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentWork {
    pub aerial: AerialWork,
    pub celestial: CelestialWork,
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
    Celestial(CelestialError),
    Aerial(AerialError),
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
error_from!(CelestialError, Celestial);
error_from!(AerialError, Aerial);

struct AerialShadowOwner {
    parameters: wgpu::Buffer,
    depth: wgpu::TextureView,
    sampler: wgpu::Sampler,
}
impl AerialShadowOwner {
    fn refs(&self) -> AerialShadows<'_> {
        AerialShadows {
            parameters: &self.parameters,
            depth: &self.depth,
            sampler: &self.sampler,
        }
    }
}
pub struct EnvironmentGpu {
    config: EnvironmentConfig,
    atmosphere: AtmosphereGpu,
    atmosphere_state: AtmosphereState,
    solar_scale: f32,
    water: Option<WaterGpu>,
    clouds: Option<CloudGpu>,
    gi: Option<GiGpu>,
    post: PostGpu,
    aerial: Option<AerialGpu>,
    celestial: Option<CelestialGpu>,
    moon_phase: u8,
    rotation_offset: f32,
    encoded: bool,
    water_cover_ready: bool,
    precloud_revision: u64,
    preaerial_revision: u64,
    atmosphere_revision: u64,
    aerial_shadows: Option<AerialShadowOwner>,
    shadow_revision: u64,
    output_revision: u64,
    last_delta: f32,
    last_gi_attempt: Option<(u64, u32)>,
    directional_light: Option<(AtmosphereState, f32, u64)>,
    last_work: EnvironmentWork,
    startup_pipeline_compilations: u32,
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
        let mut atmosphere =
            AtmosphereGpu::new(device, queue, config.atmosphere, state.atmosphere(), budget)?;
        atmosphere.set_state_and_solar(queue, state.atmosphere(), state.sun_intensity, budget)?;
        let gi = if let Some(config) = config.gi {
            Some(GiGpu::new(device, queue, config, budget)?)
        } else {
            None
        };
        terrain.prepare_scene_sampling(device)?;
        terrain.attach_environment_lighting(device, &atmosphere, lighting, gi.as_ref())?;
        let mut water = if let Some(config) = config.water {
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
        let startup_pipeline_compilations = 2
            + post.pipeline_count()
            + water
                .as_mut()
                .map_or(0, |water| water.take_work().pipeline_compilations)
            + clouds
                .as_ref()
                .map_or(0, |clouds| clouds.startup_work().pipeline_compilations)
            + u32::from(gi.is_some()) * 2;
        if let Some(water) = &mut water {
            water.set_surface_enabled(false);
        }
        Ok(Self {
            config,
            atmosphere,
            atmosphere_state: state.atmosphere(),
            solar_scale: state.sun_intensity,
            water,
            clouds,
            gi,
            post,
            aerial: None,
            celestial: None,
            moon_phase: 0,
            rotation_offset: 0.0,
            encoded: false,
            water_cover_ready: false,
            precloud_revision: 0,
            preaerial_revision: 0,
            atmosphere_revision: 0,
            aerial_shadows: lighting.map(|(shadows, _)| AerialShadowOwner {
                parameters: shadows.sampling_buffer().clone(),
                depth: shadows.depth_view().clone(),
                sampler: shadows.sampler().clone(),
            }),
            shadow_revision: 0,
            output_revision: 0,
            last_delta: 1. / 60.,
            last_gi_attempt: None,
            directional_light: None,
            last_work: EnvironmentWork::default(),
            startup_pipeline_compilations,
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
        if width == 0 || height == 0 {
            return Err(EnvironmentError::Terrain(TerrainError::InvalidSize));
        }
        if self
            .aerial
            .as_ref()
            .is_some_and(|aerial| !aerial.can_resize(device, width, height))
        {
            return Err(EnvironmentError::Aerial(AerialError::InvalidConfig));
        }
        if budget.remaining_bytes < self.post.resize_upload_bytes(width, height) {
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
        if let Some(aerial) = &mut self.aerial {
            aerial.resize_inputs(
                device,
                width,
                height,
                AerialInputs {
                    color_msaa: self.water.as_ref().map_or_else(
                        || terrain.scene_color_msaa_view().expect("prepared sampling"),
                        WaterGpu::color_msaa,
                    ),
                    depth_msaa: self
                        .water
                        .as_ref()
                        .map_or_else(|| terrain.scene_depth_msaa_view(), WaterGpu::depth_msaa),
                    optics: self.atmosphere.optical_parameters(),
                    multiple_scattering: self.atmosphere.multiple_scattering_view(),
                    shadows: self.aerial_shadows.as_ref().map(AerialShadowOwner::refs),
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
                    scene_color_msaa: self
                        .aerial
                        .as_ref()
                        .map(AerialGpu::color_msaa)
                        .unwrap_or_else(|| {
                            self.water.as_ref().map_or_else(
                                || terrain.scene_color_msaa_view().expect("prepared sampling"),
                                WaterGpu::color_msaa,
                            )
                        }),
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
            .or_else(|| self.aerial.as_ref().map(AerialGpu::resolved_view))
            .or_else(|| self.water.as_ref().map(WaterGpu::resolved_view))
            .unwrap_or_else(|| terrain.resolved_hdr_view());
        self.post
            .resize_input(device, queue, source, width, height, budget)?;
        self.last_work = EnvironmentWork::default();
        Ok(())
    }
    /// Auxiliary node pipelines only. Terrain's final variants and sky draw
    /// are already counted by TerrainGpu::take_work and must not be added twice.
    pub fn startup_pipeline_compilations(&self) -> u32 {
        self.startup_pipeline_compilations
    }
    pub fn take_startup_pipeline_compilations(&mut self) -> u32 {
        std::mem::take(&mut self.startup_pipeline_compilations)
    }
    /// Optional resources are created before the first encoded frame only.
    pub fn prepare_celestials(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: CelestialConfig,
        budget: &mut UploadBudget,
    ) -> Result<(), EnvironmentError> {
        if self.encoded || self.celestial.is_some() {
            return Err(EnvironmentError::Terrain(TerrainError::LatePreparation));
        }
        let mut celestial = CelestialGpu::new(device, queue, &self.atmosphere, config, budget)?;
        self.startup_pipeline_compilations += celestial.take_work().pipeline_compilations;
        self.celestial = Some(celestial);
        Ok(())
    }
    /// Prepare the unit-illuminance transfer before aerial resources and frames.
    pub fn prepare_multiple_scattering(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: MultipleScatteringConfig,
        budget: &mut UploadBudget,
    ) -> Result<(), EnvironmentError> {
        if self.encoded || self.aerial.is_some() {
            return Err(EnvironmentError::Terrain(TerrainError::LatePreparation));
        }
        let work = self
            .atmosphere
            .prepare_multiple_scattering(device, queue, config, budget)?;
        self.startup_pipeline_compilations += work.pipeline_compilations;
        Ok(())
    }
    /// Insert aerial perspective after water and before clouds, during loading.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_aerial(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        width: u32,
        height: u32,
        config: AerialConfig,
        budget: &mut UploadBudget,
    ) -> Result<(), EnvironmentError> {
        if self.encoded || self.aerial.is_some() {
            return Err(EnvironmentError::Terrain(TerrainError::LatePreparation));
        }
        if width == 0 || height == 0 {
            return Err(EnvironmentError::Terrain(TerrainError::InvalidSize));
        }
        if budget.remaining_bytes < self.post.resize_upload_bytes(width, height) {
            return Err(EnvironmentError::UploadBudget);
        }
        let mut aerial = AerialGpu::new(
            device,
            queue,
            config,
            width,
            height,
            AerialInputs {
                color_msaa: self.water.as_ref().map_or_else(
                    || terrain.scene_color_msaa_view().expect("prepared sampling"),
                    WaterGpu::color_msaa,
                ),
                depth_msaa: self
                    .water
                    .as_ref()
                    .map_or_else(|| terrain.scene_depth_msaa_view(), WaterGpu::depth_msaa),
                optics: self.atmosphere.optical_parameters(),
                multiple_scattering: self.atmosphere.multiple_scattering_view(),
                shadows: self.aerial_shadows.as_ref().map(AerialShadowOwner::refs),
                transmittance: self.atmosphere.transmittance_view(),
            },
        )?;
        self.startup_pipeline_compilations += aerial.take_startup_work().pipeline_compilations;
        if let Some(clouds) = &mut self.clouds {
            clouds.resize_inputs(
                device,
                width,
                height,
                CloudInputs {
                    transmittance: self.atmosphere.transmittance_view(),
                    sky: self.atmosphere.sky_view(),
                    scene_color_msaa: aerial.color_msaa(),
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
            .unwrap_or_else(|| aerial.resolved_view());
        self.post
            .resize_input(device, queue, source, width, height, budget)?;
        self.aerial = Some(aerial);
        Ok(())
    }
    fn aerial_state(
        &self,
        camera: Camera,
        state: EnvironmentState,
        source_revision: u64,
        atmosphere_revision: u64,
    ) -> AerialState {
        AerialState {
            camera,
            eye: state.eye,
            source_revision,
            atmosphere_revision,
            shadow_revision: self.shadow_revision,
        }
    }
    pub fn set_celestial_calendar(
        &mut self,
        phase: u8,
        rotation_offset: f32,
    ) -> Result<(), EnvironmentError> {
        if phase > 7 || !rotation_offset.is_finite() {
            return Err(EnvironmentError::InvalidState);
        }
        self.moon_phase = phase;
        self.rotation_offset = rotation_offset;
        Ok(())
    }
    /// Update after M4 shadow encoding; source-only color edits need no fog rebuild.
    pub fn set_shadow_revision(&mut self, revision: u64) {
        self.shadow_revision = revision;
    }
    fn celestial_state(&self, camera: Camera, state: EnvironmentState) -> CelestialState {
        CelestialState {
            camera,
            sun: state.sun,
            moon: state.sun.map(|v| -v),
            rotation_radians: self.rotation_offset
                + state.time_seconds * (std::f32::consts::TAU / 1200.0),
            time_seconds: state.time_seconds,
            moon_phase: self.moon_phase,
            sun_enabled: true,
            moon_radiance: 0.2,
            star_radiance: 1.0,
            altitude_km: state.altitude_km,
        }
    }
    pub fn atmosphere(&self) -> &AtmosphereGpu {
        &self.atmosphere
    }
    /// Loading-only first bounce using the same atmosphere and cached cascades.
    pub fn prepare_directional_gi(
        &mut self,
        device: &wgpu::Device,
        shadows: &ShadowGpu,
    ) -> Result<(), EnvironmentError> {
        if self.encoded {
            return Err(EnvironmentError::Terrain(TerrainError::LatePreparation));
        }
        self.gi
            .as_mut()
            .ok_or(EnvironmentError::InvalidState)?
            .prepare_directional(device, &self.atmosphere, shadows)?;
        self.startup_pipeline_compilations += 2;
        Ok(())
    }
    pub fn atmosphere_lighting_buffer(&self) -> &wgpu::Buffer {
        self.atmosphere.lighting_buffer()
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
        if let Some(water) = &mut self.water {
            water.set_surface_enabled(ready);
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
            sun_intensity: state.sun_intensity * crate::atmosphere::SOLAR_IRRADIANCE,
            underwater: state.underwater && self.water_cover_ready,
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
            sun_intensity: state.sun_intensity * crate::atmosphere::SOLAR_IRRADIANCE,
            scene_revision,
        }
    }
    /// A UI redraw can acquire a new surface while the HDR graph stays cached.
    pub fn request_present(&mut self) {
        self.post.request_present();
    }
    fn gi_pending(&self, state: EnvironmentState) -> bool {
        self.gi.as_ref().is_some_and(|gi| {
            self.directional_light_changed(state)
                || (gi.pending() > 0
                    && self.last_gi_attempt != Some((state.tick, gi.source_revision())))
        })
    }
    fn directional_light_changed(&self, state: EnvironmentState) -> bool {
        self.gi.as_ref().is_some_and(|gi| gi.directional_enabled())
            && self.directional_light
                != Some((
                    state.atmosphere(),
                    state.sun_intensity,
                    self.shadow_revision,
                ))
    }
    /// Predict color-scene work before preparing optional model culling.
    /// GI prediction assumes the configured frame budget can update one probe.
    pub fn scene_needed(
        &self,
        terrain: &TerrainGpu,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
    ) -> bool {
        self.scene_needed_with_rays(terrain, camera, mode, state, u32::MAX)
    }
    pub fn scene_needed_with_rays(
        &self,
        terrain: &TerrainGpu,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
        gi_rays: u32,
    ) -> bool {
        let gi_planned = self.gi_pending(state)
            && self
                .gi
                .as_ref()
                .is_some_and(|gi| gi_rays >= gi.config().rays_per_probe + 6);
        self.scene_needed_with_gi(terrain, camera, mode, state, gi_planned)
    }
    fn scene_needed_with_gi(
        &self,
        terrain: &TerrainGpu,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
        gi_planned: bool,
    ) -> bool {
        terrain.needs_redraw(camera, mode)
            || self.atmosphere.pending_work().passes > 0
            || self.atmosphere_state != state.atmosphere()
            || self.solar_scale != state.sun_intensity
            || gi_planned
            || self.celestial.as_ref().is_some_and(|celestial| {
                celestial.needs_redraw(self.celestial_state(*camera, state))
            })
    }
    pub fn needs_redraw(
        &self,
        terrain: &TerrainGpu,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
    ) -> bool {
        self.needs_redraw_with_rays(terrain, camera, mode, state, u32::MAX)
    }
    pub fn needs_redraw_with_rays(
        &self,
        terrain: &TerrainGpu,
        camera: &Camera,
        mode: CullingMode,
        state: EnvironmentState,
        gi_rays: u32,
    ) -> bool {
        self.scene_needed_with_rays(terrain, camera, mode, state, gi_rays)
            || self.water.as_ref().is_some_and(|water| {
                water.needs_redraw(self.water_state(*camera, state, terrain.hdr_revision()))
            })
            || self.clouds.as_ref().is_some_and(|clouds| {
                clouds.needs_redraw(self.cloud_state(*camera, state, self.precloud_revision))
            })
            || self.aerial.as_ref().is_some_and(|aerial| {
                aerial.needs_redraw(self.aerial_state(
                    *camera,
                    state,
                    self.preaerial_revision,
                    self.atmosphere_revision,
                ))
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
        if !self.needs_redraw_with_rays(terrain, camera, mode, state, budget.gi_rays) {
            return Ok(self.last_work);
        }
        let atmosphere_work = self
            .atmosphere
            .pending_work_for(state.atmosphere(), state.sun_intensity)?;
        let atmosphere_changed =
            self.atmosphere_state != state.atmosphere() || self.solar_scale != state.sun_intensity;
        let directional_changed = self.directional_light_changed(state);
        let relight_bytes = if directional_changed {
            self.gi
                .as_ref()
                .expect("directional GI")
                .lighting_change_upload_bytes()?
        } else {
            0
        };
        let gi_plan = self
            .gi
            .as_ref()
            .filter(|gi| {
                directional_changed
                    || (gi.pending() > 0
                        && self.last_gi_attempt != Some((state.tick, gi.source_revision())))
            })
            .map_or(0, |gi| {
                let config = self.config.gi.expect("configured GI");
                (if directional_changed {
                    gi.probe_count() as u32
                } else {
                    gi.pending()
                })
                .min(config.max_jobs)
                .min(budget.gi_rays / (config.rays_per_probe + 6))
            });
        let scene_needed = self.scene_needed_with_gi(terrain, camera, mode, state, gi_plan > 0);
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
        let preaerial_changed = scene_needed || water_plan.passes > 0;
        let next_preaerial = self
            .preaerial_revision
            .wrapping_add(u64::from(preaerial_changed));
        let next_atmosphere = self
            .atmosphere_revision
            .wrapping_add(u64::from(atmosphere_work.passes > 0));
        let aerial_state = self.aerial_state(*camera, state, next_preaerial, next_atmosphere);
        let aerial_plan = self
            .aerial
            .as_ref()
            .map_or_else(AerialWork::default, |aerial| {
                aerial.pending_work(aerial_state)
            });
        let precloud_changed = preaerial_changed || aerial_plan.passes > 0;
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
            || aerial_plan.integration_steps > budget.aerial_steps
            || aerial_plan.composition_samples > budget.aerial_samples
            || post_plan.pixels() > budget.post_pixels
        {
            return Err(EnvironmentError::WorkBudget);
        }
        let delta_changed = matches!(self.config.post.exposure, Exposure::Automatic { .. })
            && self.last_delta != state.delta_seconds;
        let celestial_state = self.celestial_state(*camera, state);
        let celestial_upload = u64::from(
            self.celestial
                .as_ref()
                .is_some_and(|celestial| celestial.needs_redraw(celestial_state)),
        ) * 144;
        let parameter_bytes = u64::from(atmosphere_changed) * 96
            + u64::from(water_plan.passes > 0) * 256
            + u64::from(cloud_plan.passes > 0) * 176
            + u64::from(delta_changed) * 64
            + u64::from(scene_needed) * 256
            + u64::from(gi_plan) * 16
            + relight_bytes
            + celestial_upload
            + aerial_plan.upload_bytes;
        if parameter_bytes > budget.upload_bytes {
            return Err(EnvironmentError::UploadBudget);
        }
        let mut upload = UploadBudget::new(budget.upload_bytes);
        self.atmosphere.set_state_and_solar(
            queue,
            state.atmosphere(),
            state.sun_intensity,
            &mut upload,
        )?;
        let mut work = EnvironmentWork {
            atmosphere: self
                .atmosphere
                .encode_update(encoder, &mut budget.atmosphere_steps)?,
            ..EnvironmentWork::default()
        };
        self.atmosphere_state = state.atmosphere();
        self.solar_scale = state.sun_intensity;
        if directional_changed {
            if let Some(gi) = &mut self.gi {
                work.gi.upload_bytes = gi.invalidate_lighting(queue, state.tick, &mut upload)?;
            }
            self.directional_light = Some((
                state.atmosphere(),
                state.sun_intensity,
                self.shadow_revision,
            ));
            self.last_gi_attempt = None;
        }
        if gi_plan > 0 {
            let gi = self.gi.as_mut().expect("planned GI");
            let mut traced =
                gi.trace_tick(queue, encoder, state.tick, &mut upload, &mut budget.gi_rays);
            traced.upload_bytes += work.gi.upload_bytes;
            work.gi = traced;
            self.last_gi_attempt = Some((state.tick, gi.source_revision()));
        }
        if work.atmosphere.passes > 0 || work.gi.updated_probes > 0 {
            terrain.invalidate();
        }
        if scene_needed {
            if let Some(celestial) = &mut self.celestial {
                work.celestial = celestial.prepare_frame(queue, celestial_state, &mut upload)?;
            }
            let mut celestial_draws = 0;
            work.terrain = terrain.encode_scene_with_extra(
                queue,
                encoder,
                camera,
                mode,
                self.water_cover_ready,
                |pass| {
                    celestial_draws = self
                        .celestial
                        .as_ref()
                        .map_or(0, |celestial| celestial.draw(pass));
                    celestial_draws + extra(pass)
                },
            )?;
            work.celestial.draws = celestial_draws;
            work.celestial.vertices = self
                .celestial
                .as_ref()
                .map_or(0, CelestialGpu::vertex_count);
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
        if let Some(aerial) = &mut self.aerial
            && aerial_plan.passes > 0
        {
            work.aerial = aerial.encode(
                queue,
                encoder,
                aerial_state,
                &mut upload,
                &mut budget.aerial_steps,
                &mut budget.aerial_samples,
            )?;
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
        self.preaerial_revision = next_preaerial;
        self.atmosphere_revision = next_atmosphere;
        self.output_revision = next_output;
        work.upload_bytes = budget.upload_bytes - upload.remaining_bytes;
        budget.upload_bytes = upload.remaining_bytes;
        work.passes = work.atmosphere.passes
            + work.terrain.passes
            + work.water.passes
            + work.clouds.passes
            + work.gi.passes
            + work.post.passes
            + work.aerial.passes;
        work.draws = work.terrain.draw_calls
            + work.water.draws
            + work.clouds.draws
            + work.post.draws
            + work.aerial.draws;
        self.last_work = work;
        self.encoded = true;
        Ok(work)
    }
    /// Call if an encoded graph was abandoned, rather than submitted/presented.
    pub fn invalidate(&mut self, terrain: &mut TerrainGpu, tick: u64) {
        terrain.invalidate();
        self.atmosphere.invalidate();
        if let Some(water) = &mut self.water {
            water.invalidate();
        }
        if let Some(aerial) = &mut self.aerial {
            aerial.invalidate();
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
    pub fn extra_timing_sources(&self) -> [Option<TimingSource<'_>>; 7] {
        [
            (self.last_work.atmosphere.passes > 0)
                .then(|| {
                    self.atmosphere.multiple_scattering_timing().and_then(
                        |(buffer, period, graph)| timing(Some((buffer, period)), graph, |_| 5),
                    )
                })
                .flatten(),
            self.aerial
                .as_ref()
                .filter(|_| self.last_work.aerial.passes > 0)
                .and_then(|aerial| timing(aerial.timestamp_buffer(), aerial.frame_graph(), |_| 5)),
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
    pub fn timing_sources<'a>(&'a self, terrain: &'a TerrainGpu) -> [Option<TimingSource<'a>>; 8] {
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
        let [multiple, aerial, water, clouds, post, gi, atmosphere] = self.extra_timing_sources();
        [scene, water, aerial, multiple, clouds, post, gi, atmosphere]
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
