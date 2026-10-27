//! Prepared M10 frame graph. Each consumer writes a separate HDR target.
//! World sampling and authoritative fluid/lighting updates belong to the client.
use crate::contact_ao::{
    ContactAoConfig, ContactAoError, ContactAoGpu, ContactAoInputs, ContactAoWork,
};
mod temporal;
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
use temporal::TemporalAttachment;
pub use temporal::TemporalInputs;
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentConfig {
    pub atmosphere: AtmosphereConfig,
    pub contact_ao: Option<ContactAoConfig>,
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
    pub contact_depth_reads: u64,
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
    pub temporal: crate::taa::TaaWork,
    pub temporal_optics: crate::temporal_optics_mask::Work,
    pub motion_draws: u32,
    pub contact_ao: ContactAoWork,
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
    ContactAo(ContactAoError),
    Temporal(crate::taa::Error),
    TemporalOptics(crate::temporal_optics_mask::Error),
    Motion(crate::model_motion::Error),
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
error_from!(ContactAoError, ContactAo);
error_from!(crate::taa::Error, Temporal);
error_from!(crate::model_motion::Error, Motion);
error_from!(crate::temporal_optics_mask::Error, TemporalOptics);

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
    sample_count: u32,
    config: EnvironmentConfig,
    atmosphere: AtmosphereGpu,
    contact_ao: Option<ContactAoGpu>,
    atmosphere_state: AtmosphereState,
    solar_scale: f32,
    water: Option<WaterGpu>,
    clouds: Option<CloudGpu>,
    gi: Option<GiGpu>,
    post: PostGpu,
    temporal: Option<Box<TemporalAttachment>>,
    aerial: Option<AerialGpu>,
    celestial: Option<CelestialGpu>,
    moon_phase: u8,
    rotation_offset: f32,
    rotation_exact: Option<f32>,
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
        let sample_count = terrain.sample_count();
        if !state.valid() {
            return Err(EnvironmentError::InvalidState);
        }
        if config.gi.is_some() && lighting.is_none() {
            return Err(EnvironmentError::MissingLighting);
        }
        let reflection_setup_bytes =
            u64::from(config.water.is_some_and(|water| water.reflection_steps > 0)) * 16;
        if budget.remaining_bytes < reflection_setup_bytes {
            return Err(EnvironmentError::UploadBudget);
        }
        budget.remaining_bytes -= reflection_setup_bytes;
        let mut atmosphere =
            AtmosphereGpu::new(device, queue, config.atmosphere, state.atmosphere(), budget)?;
        atmosphere.set_state_and_solar(queue, state.atmosphere(), state.sun_intensity, budget)?;
        let gi = if let Some(config) = config.gi {
            Some(GiGpu::new(device, queue, config, budget)?)
        } else {
            None
        };
        terrain.prepare_scene_sampling(device)?;
        if config.contact_ao.is_some_and(|config| config.quality > 0) {
            terrain.prepare_ambient_mrt(device)?;
        }
        terrain.attach_environment_lighting(device, &atmosphere, lighting, gi.as_ref())?;
        let contact_ao = if let Some(config) = config.contact_ao {
            ContactAoGpu::new(
                device,
                queue,
                width,
                height,
                sample_count,
                config,
                ContactAoInputs {
                    hdr: terrain.scene_color_msaa_view().expect("prepared sampling"),
                    ambient: terrain.ambient_msaa_view().unwrap_or_else(|| {
                        terrain.scene_color_msaa_view().expect("prepared sampling")
                    }),
                    depth: terrain.scene_depth_msaa_view(),
                    statistics: None,
                },
                budget,
            )?
        } else {
            None
        };
        let mut water = if let Some(config) = config.water {
            Some(WaterGpu::new_with_samples(
                device,
                queue,
                config,
                width,
                height,
                WaterInputs {
                    color_msaa: contact_ao.as_ref().map_or_else(
                        || terrain.scene_color_msaa_view().expect("prepared sampling"),
                        ContactAoGpu::color_msaa,
                    ),
                    depth_msaa: terrain.scene_depth_msaa_view(),
                    sky: atmosphere.sky_view(),
                    transmittance: atmosphere.transmittance_view(),
                },
                lighting.map(|pair| pair.0),
                sample_count,
            )?)
        } else {
            None
        };
        let clouds = if let Some(config) = config.clouds {
            Some(CloudGpu::new_with_samples(
                device,
                queue,
                config,
                width,
                height,
                CloudInputs {
                    transmittance: atmosphere.transmittance_view(),
                    sky: atmosphere.sky_view(),
                    scene_color_msaa: water.as_ref().map_or_else(
                        || {
                            contact_ao.as_ref().map_or_else(
                                || terrain.scene_color_msaa_view().expect("prepared sampling"),
                                ContactAoGpu::color_msaa,
                            )
                        },
                        WaterGpu::color_msaa,
                    ),
                    scene_depth_msaa: water
                        .as_ref()
                        .map_or_else(|| terrain.scene_depth_msaa_view(), WaterGpu::depth_msaa),
                },
                sample_count,
            )?)
        } else {
            None
        };
        let source = clouds
            .as_ref()
            .map(CloudGpu::resolved_view)
            .or_else(|| water.as_ref().map(WaterGpu::resolved_view))
            .or_else(|| contact_ao.as_ref().map(ContactAoGpu::resolved_view))
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
            + u32::from(contact_ao.is_some())
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
            sample_count,
            config,
            atmosphere,
            contact_ao,
            atmosphere_state: state.atmosphere(),
            solar_scale: state.sun_intensity,
            water,
            clouds,
            gi,
            post,
            temporal: None,
            aerial: None,
            celestial: None,
            moon_phase: 0,
            rotation_offset: 0.0,
            rotation_exact: None,
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
        if budget.remaining_bytes
            < self.post.resize_upload_bytes(width, height)
                + u64::from(self.contact_ao.is_some()) * 16
        {
            return Err(EnvironmentError::UploadBudget);
        }
        terrain.resize(device, width, height)?;
        if let Some(temporal) = &mut self.temporal {
            temporal.inputs_dirty = true;
        }
        if let Some(ao) = &mut self.contact_ao {
            ao.resize_inputs(
                device,
                queue,
                width,
                height,
                ContactAoInputs {
                    hdr: terrain.scene_color_msaa_view().expect("prepared sampling"),
                    ambient: terrain.ambient_msaa_view().expect("prepared ambient"),
                    depth: terrain.scene_depth_msaa_view(),
                    statistics: None,
                },
                budget,
            )?;
        }
        if let Some(water) = &mut self.water {
            water.resize_inputs(
                device,
                width,
                height,
                WaterInputs {
                    color_msaa: self.contact_ao.as_ref().map_or_else(
                        || terrain.scene_color_msaa_view().expect("prepared sampling"),
                        ContactAoGpu::color_msaa,
                    ),
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
                        || {
                            self.contact_ao.as_ref().map_or_else(
                                || terrain.scene_color_msaa_view().expect("prepared sampling"),
                                ContactAoGpu::color_msaa,
                            )
                        },
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
                                || {
                                    self.contact_ao.as_ref().map_or_else(
                                        || {
                                            terrain
                                                .scene_color_msaa_view()
                                                .expect("prepared sampling")
                                        },
                                        ContactAoGpu::color_msaa,
                                    )
                                },
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
            .or_else(|| self.contact_ao.as_ref().map(ContactAoGpu::resolved_view))
            .unwrap_or_else(|| terrain.resolved_hdr_view());
        self.post
            .resize_input(device, queue, source, width, height, budget)?;
        self.last_work = EnvironmentWork::default();
        Ok(())
    }
    /// Auxiliary node pipelines only. Terrain's final variants and sky draw
    /// are already counted by TerrainGpu::take_work and must not be added twice.
    pub fn contact_ao(&self) -> Option<&ContactAoGpu> {
        self.contact_ao.as_ref()
    }
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
        let mut celestial = CelestialGpu::new_with_samples_and_ambient(
            device,
            queue,
            &self.atmosphere,
            config,
            budget,
            self.sample_count,
            self.contact_ao.is_some(),
        )?;
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
        let mut aerial = AerialGpu::new_with_samples(
            device,
            queue,
            config,
            width,
            height,
            AerialInputs {
                color_msaa: self.water.as_ref().map_or_else(
                    || {
                        self.contact_ao.as_ref().map_or_else(
                            || terrain.scene_color_msaa_view().expect("prepared sampling"),
                            ContactAoGpu::color_msaa,
                        )
                    },
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
            self.sample_count,
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
        self.rotation_exact = None;
        Ok(())
    }
    /// A public canonical clock already supplies the full rotation. Do not add
    /// local presentation elapsed time a second time.
    pub fn set_celestial_clock(
        &mut self,
        phase: u8,
        rotation_radians: f32,
    ) -> Result<(), EnvironmentError> {
        if phase > 7 || !rotation_radians.is_finite() {
            return Err(EnvironmentError::InvalidState);
        }
        self.moon_phase = phase;
        self.rotation_exact = Some(rotation_radians.rem_euclid(std::f32::consts::TAU));
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
            rotation_radians: self.rotation_exact.unwrap_or(
                self.rotation_offset + state.time_seconds * (std::f32::consts::TAU / 1200.0),
            ),
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
    pub fn celestials(&self) -> Option<&CelestialGpu> {
        self.celestial.as_ref()
    }
    pub fn post(&self) -> &PostGpu {
        &self.post
    }
    pub fn post_mut(&mut self) -> &mut PostGpu {
        &mut self.post
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
        if self
            .temporal
            .as_ref()
            .is_some_and(|owner| owner.gpu.pending().is_some())
        {
            return true;
        }
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
        self.temporal_source_needed_with_rays(terrain, camera, mode, state, gi_rays)
            || self.post.needs_redraw(self.output_revision)
    }
    /// HDR inputs only, excluding Post adaptation/presentation. Query before
    /// beginning a temporal candidate, using its last presented raster camera
    /// when the unjittered source camera has not changed.
    pub fn temporal_source_needed_with_rays(
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
    }
    /// Present a previously completed HDR while atomic near/far admission is
    /// pending. No camera, atmosphere, GI, water or scene commands are encoded.
    /// None means no valid retained HDR exists (also after a resource resize).
    pub fn encode_cached_present(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        remaining_pixels: &mut u64,
    ) -> Result<Option<EnvironmentWork>, EnvironmentError> {
        self.last_work = EnvironmentWork::default();
        if !self.encoded || !self.post.has_cached_source() {
            return Ok(None);
        }
        let mut temporal_work = crate::taa::TaaWork::default();
        if let Some(temporal) = &mut self.temporal {
            if temporal.inputs_dirty
                || temporal.gpu.pending().is_some()
                || temporal.gpu.presented().is_none()
            {
                return Ok(None);
            }
            // Do not issue the restore copy if the complete cached Post plan
            // cannot run. History remains the last actually presented frame.
            if self.post.pending_cached_work().pixels() > *remaining_pixels {
                return Err(EnvironmentError::WorkBudget);
            }
            temporal_work.history_copy_bytes = temporal.gpu.restore_presented_output(encoder)?;
        }
        let post = self
            .post
            .present_cached(encoder, output, remaining_pixels)?;
        let work = EnvironmentWork {
            temporal: temporal_work,
            passes: post.passes,
            draws: post.draws,
            post,
            ..EnvironmentWork::default()
        };
        self.last_work = work;
        Ok(Some(work))
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
        self.encode_with_temporal(
            queue,
            encoder,
            terrain,
            output,
            camera,
            mode,
            state,
            budget,
            extra,
            0,
            |_, _, _| Err(EnvironmentError::Temporal(crate::taa::Error::InvalidFrame)),
        )
    }
    /// The callback clears/rasterizes actual object motion into the prepared
    /// inputs. Its exact upload credit participates in the same global preflight.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_with_temporal(
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
        motion_upload_bytes: u64,
        motion: impl FnOnce(
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &mut UploadBudget,
        ) -> Result<u32, EnvironmentError>,
    ) -> Result<EnvironmentWork, EnvironmentError> {
        self.last_work = EnvironmentWork::default();
        if !state.valid() || camera.inverse_view_projection().is_none() {
            return Err(EnvironmentError::InvalidState);
        }
        let token = self.temporal.as_ref().and_then(|owner| owner.gpu.pending());
        let temporal_plan = self
            .temporal
            .as_ref()
            .map(|owner| {
                if owner.inputs_dirty {
                    return Err(crate::taa::Error::InvalidFrame);
                }
                token.map(|token| owner.gpu.pending_work(token)).transpose()
            })
            .transpose()?
            .flatten();
        let optics_plan = self
            .temporal
            .as_ref()
            .filter(|_| temporal_plan.is_some())
            .and_then(|owner| owner.optics.as_ref())
            .map(|optics| optics.pending_work(self.water.as_ref(), self.clouds.as_ref()))
            .transpose()?
            .unwrap_or_default();
        if temporal_plan.is_none()
            && !self.needs_redraw_with_rays(terrain, camera, mode, state, budget.gi_rays)
        {
            return Ok(self.last_work);
        }
        let post_token = self
            .temporal
            .as_ref()
            .and_then(|owner| owner.post_frame)
            .or_else(|| token.map(crate::post::PostFrameToken::from));
        if let Some(token) = token
            && post_token != Some(token.into())
        {
            return Err(EnvironmentError::InvalidState);
        }
        if self.temporal.is_some() {
            self.post
                .validate_temporal_begin(post_token.ok_or(EnvironmentError::InvalidState)?)?;
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
        let scene_needed = temporal_plan.is_some()
            || self.scene_needed_with_gi(terrain, camera, mode, state, gi_plan > 0);
        if scene_needed && self.temporal.is_some() && token.is_none() {
            return Err(EnvironmentError::Temporal(crate::taa::Error::InvalidFrame));
        }
        let ao_plan = self
            .contact_ao
            .as_ref()
            .map_or(Ok(ContactAoWork::default()), |ao| {
                ao.pending_work(
                    *camera,
                    terrain.hdr_revision().wrapping_add(u64::from(scene_needed)),
                )
            })?;
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
        let next_output = self.output_revision.wrapping_add(u64::from(
            precloud_changed || cloud_plan.passes > 0 || temporal_plan.is_some(),
        ));
        let post_plan = self.post.pending_work(next_output);
        if ao_plan.max_depth_reads > budget.contact_depth_reads
            || atmosphere_work.integration_steps > budget.atmosphere_steps
            || water_plan.max_reflection_steps > budget.water_reflection_steps
            || cloud_plan.max_density_samples > budget.cloud_density_samples
            || aerial_plan.integration_steps > budget.aerial_steps
            || aerial_plan.composition_samples > budget.aerial_samples
            || post_plan
                .pixels()
                .saturating_add(temporal_plan.map_or(0, |work| work.pixels))
                .saturating_add(optics_plan.pixels)
                > budget.post_pixels
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
            + aerial_plan.upload_bytes
            + ao_plan.upload_bytes;
        let parameter_bytes = parameter_bytes.saturating_add(temporal_plan.map_or(0, |work| {
            work.upload_bytes.saturating_add(motion_upload_bytes)
        }));
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
        if let Some(ao) = &mut self.contact_ao
            && ao_plan.passes > 0
        {
            work.contact_ao = ao.encode(
                queue,
                encoder,
                *camera,
                terrain.hdr_revision(),
                &mut upload,
                budget.contact_depth_reads,
            )?;
            budget.contact_depth_reads -= work.contact_ao.max_depth_reads;
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
        if let (Some(owner), Some(token)) = (&mut self.temporal, token) {
            work.motion_draws = motion(queue, encoder, &mut upload)?;
            if let Some(optics) = &owner.optics {
                work.temporal_optics = optics.encode(
                    encoder,
                    self.water.as_ref(),
                    self.clouds.as_ref(),
                    budget.post_pixels,
                )?;
                budget.post_pixels -= work.temporal_optics.pixels;
            }
            work.temporal = owner.gpu.encode(queue, encoder, token, &mut upload)?;
            budget.post_pixels -= work.temporal.pixels;
        }
        self.post
            .set_delta(queue, state.delta_seconds, &mut upload)?;
        work.post = if let Some(token) = post_token {
            let work = self.post.encode_temporal(
                encoder,
                output,
                next_output,
                &mut budget.post_pixels,
                token,
            )?;
            self.temporal
                .as_mut()
                .expect("temporal post token")
                .post_frame = None;
            work
        } else {
            self.post
                .encode(encoder, output, next_output, &mut budget.post_pixels)?
        };
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
            + work.aerial.passes
            + work.contact_ao.passes
            + work.temporal_optics.passes
            + work.temporal.passes
            + u32::from(temporal_plan.is_some());
        work.draws = work.terrain.draw_calls
            + work.water.draws
            + work.clouds.draws
            + work.post.draws
            + work.aerial.draws
            + work.contact_ao.draws
            + work.temporal_optics.draws
            + work.temporal.passes
            + work.motion_draws;
        self.last_work = work;
        self.encoded = true;
        Ok(work)
    }
    /// Call if an encoded graph was abandoned, rather than submitted/presented.
    pub fn invalidate(&mut self, terrain: &mut TerrainGpu, tick: u64) {
        self.invalidate_scene_sources(terrain, tick);
        self.post.invalidate();
    }
    /// An abandoned, unsubmitted scene did not execute the GPU work which its
    /// CPU node caches recorded. Rebuild those sources while preserving the
    /// independently restored Post/exposure cache for UI-only presentation.
    pub fn invalidate_scene_sources(&mut self, terrain: &mut TerrainGpu, tick: u64) {
        self.last_work = EnvironmentWork::default();
        terrain.invalidate();
        self.atmosphere.invalidate();
        if let Some(ao) = &mut self.contact_ao {
            ao.invalidate();
        }
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
    }
    pub fn extra_timing_sources(&self) -> [Option<TimingSource<'_>>; 10] {
        [
            self.contact_ao
                .as_ref()
                .filter(|_| self.last_work.contact_ao.passes > 0)
                .and_then(|ao| timing(ao.timestamp_buffer(), ao.graph(), |_| 6)),
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
                            5..=8 => 13,
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
            self.temporal
                .as_ref()
                .filter(|_| {
                    self.last_work.temporal.passes > 0
                        || self.last_work.temporal.history_copy_bytes > 0
                })
                .and_then(|owner| owner.gpu.timing_source())
                .map(|mut source| {
                    source.kinds = [13; 32];
                    source
                }),
            self.temporal
                .as_ref()
                .filter(|_| self.last_work.temporal_optics.passes > 0)
                .and_then(|owner| owner.optics.as_ref())
                .and_then(crate::temporal_optics_mask::TemporalOpticsMask::timing_source)
                .map(|mut source| {
                    source.kinds = [13; 32];
                    source
                }),
        ]
    }
    pub fn timing_sources<'a>(&'a self, terrain: &'a TerrainGpu) -> [Option<TimingSource<'a>>; 11] {
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
        let [
            ao,
            multiple,
            aerial,
            water,
            clouds,
            post,
            gi,
            atmosphere,
            temporal,
            optics,
        ] = self.extra_timing_sources();
        [
            scene, ao, water, aerial, multiple, clouds, post, gi, atmosphere, temporal, optics,
        ]
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
