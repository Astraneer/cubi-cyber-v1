//! Optional whole-scene presentation transaction. A submitted encoder is not a
//! presented frame; every history and pose bank commits behind the same fence.
use super::{ClientView, model_motion::ViewModelMotion};
use core_types::Version;
use render::{
    environment_gpu::{EnvironmentError, EnvironmentState, TemporalInputs},
    far_motion::{Config as FarConfig, FarMotionGpu},
    model_motion::TargetConfig,
    post::PostFrameToken,
    presented_motion::Frame,
    recipe_board_motion::RecipeBoardMotionGpu,
    taa::{FrameToken, ResetReasons, TaaConfig, TaaFrame},
    terrain_gpu::{Camera, CullingMode, TerrainFrameWork, UploadBudget},
    terrain_motion::TerrainMotionGpu,
};

pub const MAX_TEMPORAL_BYTES: u64 = 256 * 1024 * 1024;
#[derive(Clone, Copy, Debug)]
pub struct Capacities {
    pub terrain_pages: u32,
    pub entity_instances: Option<u32>,
    pub prop_instances: Option<u32>,
    pub far: Option<(u32, u32)>,
    pub recipe_boards: Option<u32>,
    pub optical_mask: bool,
}
/// Includes all optional images exactly once, every GPU bank/parameter/query
/// buffer, and Post's exposure checkpoint. Safe to call before replacing a View.
pub fn required_bytes(
    config: TaaConfig,
    extent: [u32; 2],
    max_dimension: u32,
    timestamps: bool,
    caps: Capacities,
) -> Result<u64, String> {
    let limit = config.memory_budget_bytes.min(MAX_TEMPORAL_BYTES);
    let target = TargetConfig {
        extent,
        samples: 1,
        max_pixels: config.max_pixels,
        // Pure cost calculation only. Enforce the complete graph's unchanged
        // hard limit below, so the error reports its actual requested MiB.
        memory_budget_bytes: u64::MAX,
    };
    let models = ViewModelMotion::required_allocation_bytes(
        target,
        max_dimension,
        timestamps,
        caps.entity_instances,
        caps.prop_instances,
    )?;
    let images = TaaConfig {
        memory_budget_bytes: u64::MAX,
        ..config
    }
    .allocation_bytes(extent)
    .map_err(|e| e.to_string())?;
    let external =
        u64::from(extent[0]) * u64::from(extent[1]) * render::taa::MOTION_TEXTURE_BYTES_PER_PIXEL;
    let mut bytes = images
        .checked_add(
            models
                .checked_sub(external)
                .ok_or("temporal image accounting")?,
        )
        .and_then(|v| v.checked_add(config.required_constant_bytes(timestamps)))
        .and_then(|v| {
            v.checked_add(TerrainMotionGpu::required_buffer_bytes(
                caps.terrain_pages,
                timestamps,
            ))
        })
        .and_then(|v| v.checked_add(16))
        .ok_or("temporal graph allocation overflow")?;
    if let Some((height, proxy)) = caps.far {
        bytes = bytes
            .checked_add(FarMotionGpu::required_buffer_bytes(
                height, proxy, timestamps,
            ))
            .ok_or("far motion allocation overflow")?;
    }
    if let Some(capacity) = caps.recipe_boards {
        bytes = bytes
            .checked_add(
                RecipeBoardMotionGpu::required_buffer_bytes(capacity).map_err(|e| e.to_string())?,
            )
            .ok_or("recipe board motion allocation overflow")?;
    }
    if caps.optical_mask && timestamps {
        bytes = bytes
            .checked_add(256)
            .ok_or("optical query allocation overflow")?;
    }
    if bytes > limit {
        return Err(crate::antialiasing::Error::TemporalMemory {
            required: bytes,
            maximum: limit,
        }
        .to_string());
    }
    Ok(bytes)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GeometryKey {
    near: u64,
    edit: u64,
    far: [u64; 2],
    center: core_types::PageId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceKey {
    space: u32,
    material: Option<u64>,
    // Actual in-place atlas publication, independent of host/import revisions.
    material_upload: u64,
    props_epoch: u64,
    light_ready: bool,
}
#[derive(Clone, Copy)]
struct SceneCandidate {
    token: FrameToken,
    base: Camera,
    draw: Camera,
    world: Version,
    uploads: u64,
    visual: u64,
    edit: u64,
    props: bool,
    props_epoch: u64,
    work: TerrainFrameWork,
    gi_started: bool,
    encoded: bool,
    submitted: bool,
}
#[derive(Clone, Copy)]
struct PostCandidate {
    token: PostFrameToken,
    props_epoch: u64,
    encoded: bool,
    submitted: bool,
}
pub struct ViewTemporal {
    models: ViewModelMotion,
    terrain: TerrainMotionGpu,
    far: Option<FarMotionGpu>,
    boards: Option<RecipeBoardMotionGpu>,
    config: TaaConfig,
    bytes: u64,
    frame_id: u64,
    last_frame: u64,
    scene_revision: u64,
    geometry_epoch: u64,
    source_epoch: u64,
    geometry: Option<GeometryKey>,
    source: Option<SourceKey>,
    presented_base: Option<Camera>,
    presented_draw: Option<Camera>,
    candidate: Option<SceneCandidate>,
    post: Option<PostCandidate>,
}
impl ViewTemporal {
    pub fn allocation_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn pending_frame(&self) -> Option<u64> {
        self.candidate
            .map(|p| p.token.frame())
            .or(self.post.map(|p| p.token.frame))
    }
    pub fn presented_frame(&self) -> u64 {
        self.last_frame
    }
    pub(super) fn timing_sources(&self) -> [Option<render::gpu_time::TimingSource<'_>>; 3] {
        if !self.candidate.is_some_and(|c| c.encoded) {
            return [None, None, None];
        }
        [
            self.models.targets().timing_source(13),
            self.terrain.timing_source(13),
            self.far.as_ref().and_then(|f| f.timing_source(13)),
        ]
    }
    fn comparison_camera(&self, base: Camera) -> Camera {
        if self.presented_base == Some(base) {
            self.presented_draw.unwrap_or(base)
        } else {
            base
        }
    }
    fn validate_idle(&self) -> Result<(), String> {
        if self.candidate.is_some() || self.post.is_some() {
            Err("temporal presentation already pending".into())
        } else {
            Ok(())
        }
    }
}
impl ClientView {
    /// Shape of the same written timestamp sources consumed by
    /// `encode_gpu_capture`; no command encoding, allocation or query reads.
    pub fn gpu_timing_shape(&self) -> (usize, u32) {
        let mut shape = (0, 0);
        let mut add = |source: render::gpu_time::TimingSource<'_>| {
            shape.0 += 1;
            shape.1 += source.count;
        };
        if let Some(environment) = &self.environment {
            for source in environment
                .owner
                .gpu
                .timing_sources(&self.renderer)
                .into_iter()
                .flatten()
            {
                add(source);
            }
        } else if self.encoded_terrain && self.renderer.timestamp_buffer().is_some() {
            shape.0 += 1;
            shape.1 += self.renderer.frame_graph().count;
        }
        if self.encoded_shadows
            && let Some(source) = self
                .lighting
                .as_ref()
                .and_then(|l| l.owner.shadows().timing_source())
        {
            shape.0 += 1;
            shape.1 += source.count;
        }
        if let Some(temporal) = &self.temporal {
            for source in temporal.timing_sources().into_iter().flatten() {
                shape.0 += 1;
                shape.1 += source.count;
            }
        }
        shape
    }
    /// Loading only, after all actual color families and final scaled targets.
    /// The complete optional graph is validated before changing any pipeline.
    pub fn prepare_temporal(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(), String> {
        if self.temporal.is_some() {
            return Err("temporal View already prepared".into());
        }
        let environment = self
            .environment
            .as_ref()
            .ok_or("temporal reconstruction needs environment")?;
        let config = environment
            .owner
            .requested_temporal_config()
            .ok_or("no temporal loading request")?;
        if self.renderer.sample_count() != 1 || self.terrain_frames != 0 || self.props_frames != 0 {
            return Err("temporal View requires unrendered single-sample sources".into());
        }
        if self
            .entities
            .as_ref()
            .is_some_and(|e| !e.gpu.temporal_raster_preparable())
            || self
                .props
                .as_ref()
                .is_some_and(|p| !p.gpu.temporal_raster_preparable())
            || self.horizon.as_ref().is_some_and(|h| {
                !h.owner.height.temporal_raster_preparable()
                    || !h.owner.proxies.temporal_raster_preparable()
            })
        {
            return Err("temporal color sources are already populated".into());
        }
        let caps = Capacities {
            terrain_pages: self.renderer.geometry().max_pages,
            entity_instances: self.entities.as_ref().map(|e| e.gpu.instance_capacity()),
            prop_instances: self.props.as_ref().map(|p| p.gpu.instance_capacity()),
            far: self.horizon.as_ref().map(|h| {
                (
                    h.owner.height.shadow_source().max_tiles,
                    h.owner.proxies.shadow_source().max_objects,
                )
            }),
            recipe_boards: self.recipe_boards.as_ref().map(|b| b.gpu().capacity()),
            optical_mask: environment.owner.temporal_optical_mask_needed(),
        };
        let extent = self.renderer.extent();
        let timestamps = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let bytes = required_bytes(
            config,
            extent,
            device.limits().max_texture_dimension_2d,
            timestamps,
            caps,
        )?;
        let models = ViewModelMotion::prepare(
            device,
            queue,
            TargetConfig {
                extent,
                samples: 1,
                max_pixels: config.max_pixels,
                memory_budget_bytes: config.memory_budget_bytes.min(MAX_TEMPORAL_BYTES),
            },
            self.entities.as_deref_mut(),
            self.props.as_deref_mut(),
        )?;
        let terrain = TerrainMotionGpu::new(device, queue, &self.renderer, Some(bytes))
            .map_err(|e| e.to_string())?
            .ok_or("missing terrain motion")?;
        let mut pipelines = u64::from(models.startup_work().pipeline_compilations)
            + u64::from(terrain.pipeline_compilations());
        let far = if let Some(horizon) = &mut self.horizon {
            pipelines += u64::from(
                horizon
                    .owner
                    .height
                    .prepare_temporal_raster(device)
                    .map_err(|e| e.to_string())?,
            );
            pipelines += u64::from(
                horizon
                    .owner
                    .proxies
                    .prepare_temporal_raster(device)
                    .map_err(|e| e.to_string())?,
            );
            let policy = super::distant_shadows::height_policy().map_err(|e| e.to_string())?;
            let mut layers = [0; 8];
            for layer in 0..256_u16 {
                if policy.casts(layer) {
                    layers[layer as usize / 32] |= 1 << (layer % 32);
                }
            }
            FarMotionGpu::new(
                device,
                queue,
                &horizon.owner.height,
                &horizon.owner.proxies,
                Some(FarConfig {
                    memory_budget_bytes: bytes,
                    opaque_height_layers: layers,
                }),
            )
            .map_err(|e| e.to_string())?
        } else {
            None
        };
        let boards = self
            .recipe_boards
            .as_ref()
            .map(|b| RecipeBoardMotionGpu::new(device, b.gpu(), bytes))
            .transpose()
            .map_err(|e| e.to_string())?;
        pipelines += far
            .as_ref()
            .map_or(0, |f| u64::from(f.pipeline_compilations()));
        pipelines += boards
            .as_ref()
            .map_or(0, |b| u64::from(b.startup_pipeline_compilations()));
        let mut budget = UploadBudget::new(2 * 1024 * 1024);
        self.environment
            .as_mut()
            .expect("checked environment")
            .owner
            .gpu
            .prepare_temporal(
                device,
                queue,
                &self.renderer,
                TemporalInputs {
                    motion: models.targets().motion_view(),
                    surface: models.targets().surface_view(),
                },
                config,
                &mut budget,
            )
            .map_err(|e| e.to_string())?;
        pipelines += u64::from(
            self.environment
                .as_mut()
                .expect("checked environment")
                .owner
                .gpu
                .take_startup_pipeline_compilations(),
        );
        self.pipeline_compilations = self.pipeline_compilations.saturating_add(pipelines);
        self.temporal = Some(Box::new(ViewTemporal {
            models,
            terrain,
            far,
            boards,
            config,
            bytes,
            frame_id: 0,
            last_frame: 0,
            scene_revision: 0,
            geometry_epoch: 0,
            source_epoch: 0,
            geometry: None,
            source: None,
            presented_base: None,
            presented_draw: None,
            candidate: None,
            post: None,
        }));
        self.invalidate();
        Ok(())
    }
    pub fn temporal_prepared(&self) -> bool {
        self.temporal.is_some()
            && self
                .environment
                .as_ref()
                .is_some_and(|e| e.owner.gpu.temporal().is_some())
    }
    pub fn temporal_allocation_bytes(&self) -> u64 {
        self.temporal.as_ref().map_or(0, |t| t.bytes)
    }
    pub fn presentation_edit_revision(&self) -> u64 {
        self.temporal
            .as_ref()
            .and_then(|t| t.candidate)
            .map_or(self.rendered_edit_revision, |c| c.edit)
    }
    pub fn temporal_motion_views(&self) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        self.temporal.as_ref().map(|t| {
            (
                t.models.targets().motion_view(),
                t.models.targets().surface_view(),
            )
        })
    }
    /// Globally unique across View replacement; a retired View's completion
    /// cannot accidentally advance the replacement's first pose/history bank.
    pub fn set_presentation_frame_id(&mut self, frame: u64) -> Result<(), String> {
        if let Some(temporal) = &mut self.temporal {
            temporal.validate_idle()?;
            if frame == 0 || frame <= temporal.last_frame {
                return Err("presentation frame must advance".into());
            }
            temporal.frame_id = frame;
        }
        Ok(())
    }
    pub fn pending_presentation_frame(&self) -> Option<u64> {
        self.temporal.as_ref().and_then(|t| t.pending_frame())
    }
}

impl ClientView {
    pub(super) fn temporal_needs_redraw(&self, base: Camera, state: EnvironmentState) -> bool {
        let temporal = self.temporal.as_ref().expect("temporal path");
        if temporal.pending_frame().is_some() {
            return false;
        }
        let environment = &self
            .environment
            .as_ref()
            .expect("temporal environment")
            .owner;
        let camera = temporal.comparison_camera(base);
        self.force_redraw
            || self.cached_props_epoch != self.props_source_epoch()
            || self.page_links_dirty
            || temporal.presented_base != Some(base)
            || self.observed_world_version != self.scene.world().version()
            || self.observed_upload_bytes != self.upload_bytes_total
            || self
                .lighting
                .as_ref()
                .is_some_and(|l| l.owner.needs_work(self.scene.world()))
            || environment.needs_cpu_work(&self.scene)
            || environment.needs_redraw(&self.renderer, &camera, CullingMode::FrustumAndHiZ, state)
            || environment
                .gpu
                .temporal()
                .is_some_and(|taa| taa.needs_refinement(temporal.scene_revision))
    }
    /// Called after ordinary lighting/source preparation, before any temporal
    /// camera/cull or motion writes. Default MSAA/SMAA never enters this path.
    pub(super) fn encode_temporal_scene(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        base: Camera,
        state: EnvironmentState,
    ) -> Result<TerrainFrameWork, String> {
        let mut temporal = self.temporal.take().expect("prepared temporal path");
        let result = self.encode_temporal_inner(&mut temporal, queue, encoder, output, base, state);
        self.temporal = Some(temporal);
        result
    }
    fn encode_temporal_inner(
        &mut self,
        temporal: &mut ViewTemporal,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        base: Camera,
        state: EnvironmentState,
    ) -> Result<TerrainFrameWork, String> {
        temporal.validate_idle()?;
        if temporal.frame_id == 0 || temporal.frame_id <= temporal.last_frame {
            return Err("set a fresh global presentation frame before temporal encode".into());
        }
        let props_epoch = self.props_source_epoch();
        let comparison = temporal.comparison_camera(base);
        let environment = &mut self
            .environment
            .as_mut()
            .expect("prepared environment")
            .owner;
        let source_changed = self.cached_props_epoch != props_epoch
            || temporal.presented_base != Some(base)
            || environment.temporal_source_needed(
                &self.renderer,
                &comparison,
                CullingMode::FrustumAndHiZ,
                state,
            );
        let refine = environment
            .gpu
            .temporal()
            .expect("prepared resolver")
            .needs_refinement(temporal.scene_revision);
        let scene = source_changed || refine;
        let mut camera = comparison;
        let mut token = None;
        let extent = self.renderer.extent();
        let gi_started = environment.stats.gi_started;
        let mut motion_bytes = 0;
        if scene {
            let geometry = GeometryKey {
                near: self.gpu_revision,
                edit: self.edit_revision,
                far: self.horizon.as_ref().map_or([0; 2], |h| {
                    [
                        h.owner.height.shadow_source().revision,
                        h.owner.proxies.shadow_source().revision,
                    ]
                }),
                center: self.scene.config().center.page(),
            };
            let source = SourceKey {
                space: self.scene.config().center.space.0,
                material: self.renderer.material_pack_revision(),
                material_upload: self.renderer.geometry().shadow_alpha_state.revision(),
                props_epoch,
                light_ready: self
                    .lighting
                    .as_ref()
                    .is_none_or(|l| l.stats.pending_source_pages == 0),
            };
            let mut reset = ResetReasons::NONE;
            if temporal.geometry != Some(geometry) {
                temporal.geometry_epoch = temporal
                    .geometry_epoch
                    .checked_add(1)
                    .ok_or("temporal geometry epoch exhausted")?;
                temporal.geometry = Some(geometry);
                reset = reset | ResetReasons::GEOMETRY_EDIT;
            }
            if temporal.source != Some(source) {
                temporal.source_epoch = temporal
                    .source_epoch
                    .checked_add(1)
                    .ok_or("temporal source epoch exhausted")?;
                temporal.source = Some(source);
                reset = reset | ResetReasons::SOURCE_REVOKED;
            }
            if source_changed {
                temporal.scene_revision = temporal
                    .scene_revision
                    .checked_add(1)
                    .ok_or("temporal scene revision exhausted")?;
            }
            let taa = environment.gpu.temporal_mut().expect("prepared resolver");
            let jitter = taa.next_jitter();
            let current = taa
                .begin_frame(TaaFrame {
                    frame: temporal.frame_id,
                    space: source.space,
                    scene_revision: temporal.scene_revision,
                    geometry_epoch: temporal.geometry_epoch,
                    source_epoch: temporal.source_epoch,
                    jitter_pixels: jitter,
                    reset,
                })
                .map_err(|e| e.to_string())?;
            // Reserve a candidate before any fallible source staging, so the
            // caller can explicitly discard every partially encoded operation.
            temporal.candidate = Some(SceneCandidate {
                token: current,
                base,
                draw: base,
                world: self.scene.world().version(),
                uploads: self.upload_bytes_total,
                visual: self.renderer.visual_revision(),
                edit: self.edit_revision,
                props: false,
                props_epoch,
                work: TerrainFrameWork::default(),
                gi_started,
                encoded: false,
                submitted: false,
            });
            camera = render::taa::jitter_camera(base, extent, jitter).map_err(|e| e.to_string())?;
            temporal.models.stage(
                current,
                Frame {
                    space: source.space,
                    geometry_epoch: temporal.geometry_epoch,
                    source_epoch: temporal.source_epoch,
                    extent,
                    origin: [0; 3],
                    unjittered_view_projection: base.view_projection,
                    reset: reset != ResetReasons::NONE,
                },
                jitter,
                self.entities.as_deref(),
                self.props.as_deref(),
            )?;
            motion_bytes = temporal
                .models
                .required_upload_bytes()?
                .checked_add(temporal.terrain.required_upload_bytes())
                .ok_or("temporal upload overflow")?;
            if let (Some(far), Some(horizon)) = (&mut temporal.far, &self.horizon) {
                motion_bytes = motion_bytes
                    .checked_add(
                        far.pending_upload_bytes(&horizon.owner.height, &horizon.owner.proxies)
                            .map_err(|e| e.to_string())?,
                    )
                    .ok_or("far upload overflow")?;
            }
            if let Some(boards) = &temporal.boards {
                motion_bytes = motion_bytes
                    .checked_add(boards.required_upload_bytes())
                    .ok_or("recipe board motion upload overflow")?;
            }
            token = Some((current, jitter));
        } else {
            let post = PostFrameToken {
                frame: temporal.frame_id,
                serial: temporal.frame_id,
            };
            environment
                .gpu
                .prepare_post_frame(post)
                .map_err(|e| e.to_string())?;
            temporal.post = Some(PostCandidate {
                token: post,
                props_epoch,
                encoded: false,
                submitted: false,
            });
        }
        if scene {
            if let Some(entities) = &mut self.entities {
                let w = entities
                    .prepare_camera(queue, encoder, camera)
                    .map_err(|e| e.to_string())?;
                self.frame_upload_remaining = self
                    .frame_upload_remaining
                    .checked_sub(w.camera_upload_bytes)
                    .ok_or("entity camera upload credit")?;
            }
            if let Some(horizon) = &mut self.horizon {
                horizon
                    .owner
                    .prepare_camera_extent(queue, encoder, camera, state.eye, extent)
                    .map_err(|e| e.to_string())?;
                let bytes = horizon.owner.height.take_work().camera_upload_bytes
                    + horizon.owner.proxies.take_work().camera_upload_bytes;
                self.frame_upload_remaining = self
                    .frame_upload_remaining
                    .checked_sub(bytes)
                    .ok_or("far camera upload credit")?;
            }
            if let Some(props) = &mut self.props {
                let w = props.prepare_camera(queue, encoder, camera)?;
                self.frame_upload_remaining = self
                    .frame_upload_remaining
                    .checked_sub(w.camera_upload_bytes)
                    .ok_or("prop camera upload credit")?;
            }
        }
        if scene && let Some(boards) = &mut self.recipe_boards {
            let mut upload = UploadBudget::new(self.frame_upload_remaining);
            let bytes = boards.prepare_camera(queue, camera, &mut upload)?;
            self.frame_upload_remaining = upload.remaining_bytes;
            self.retained_foreground_work.record(telemetry::WorkCounts {
                upload_bytes: bytes,
                ..telemetry::WorkCounts::ZERO
            });
        }
        let entities = self.entities.as_ref();
        let horizon = self.horizon.as_ref();
        let props = self.props.as_ref();
        let boards = self.recipe_boards.as_ref();
        let props_encoded = std::cell::Cell::new(false);
        let extra = |pass: &mut wgpu::RenderPass<'_>| {
            let e = entities
                .filter(|e| e.stats.primary_records != 0)
                .map_or(0, |e| e.draw(pass));
            let h = horizon.map_or(0, |h| h.owner.draw(pass));
            let p = props.map_or(0, |p| p.draw(pass));
            props_encoded.set(p > 0 && props.is_some_and(|p| p.gpu.active_instances() > 0));
            e + h + p + boards.map_or(0, |b| b.draw(pass))
        };
        let depth = self.renderer.scene_depth_msaa_view().clone();
        let mut budget = UploadBudget::new(self.frame_upload_remaining);
        environment.gpu.request_present();
        let work = environment
            .encode_with_temporal(
                queue,
                encoder,
                &mut self.renderer,
                output,
                &camera,
                CullingMode::FrustumAndHiZ,
                state,
                &mut budget,
                extra,
                motion_bytes,
                |queue, encoder, budget| {
                    let (token, jitter) = token.ok_or(EnvironmentError::InvalidState)?;
                    temporal
                        .terrain
                        .prepare_frame(queue, token, &base, extent, jitter, budget)
                        .map_err(|_| EnvironmentError::InvalidState)?;
                    if let (Some(far), Some(horizon)) = (&mut temporal.far, horizon) {
                        far.prepare_frame(
                            queue,
                            token,
                            &base,
                            extent,
                            jitter,
                            &horizon.owner.height,
                            &horizon.owner.proxies,
                            budget,
                        )
                        .map_err(|_| EnvironmentError::InvalidState)?;
                    }
                    temporal
                        .models
                        .upload(queue, token, budget)
                        .map_err(|_| EnvironmentError::InvalidState)?;
                    if let (Some(motion), Some(boards)) = (&mut temporal.boards, boards) {
                        motion
                            .prepare_frame(
                                queue,
                                boards.gpu(),
                                boards.session(),
                                token,
                                base,
                                extent,
                                jitter,
                                budget,
                            )
                            .map_err(|_| EnvironmentError::InvalidState)?;
                    }
                    let draws = temporal.models.targets().encode(encoder, &depth, |pass| {
                        let mut draws = temporal.terrain.draw(pass).draws;
                        draws += temporal
                            .models
                            .draw_entities(pass, token)
                            .expect("prevalidated model token");
                        draws += temporal.far.as_ref().map_or(0, |f| f.draw(pass).draws);
                        draws += temporal
                            .models
                            .draw_props(pass, token)
                            .expect("prevalidated prop token");
                        draws += temporal.boards.as_ref().map_or(0, |b| {
                            b.draw(pass, token)
                                .expect("prevalidated recipe board token")
                                .draws
                        });
                        render::model_motion::Work {
                            draws,
                            ..Default::default()
                        }
                    });
                    temporal
                        .terrain
                        .encode_snapshot(encoder, token)
                        .map_err(|_| EnvironmentError::InvalidState)?;
                    if let Some(far) = &mut temporal.far {
                        far.encode_snapshot(encoder, token)
                            .map_err(|_| EnvironmentError::InvalidState)?;
                    }
                    temporal
                        .models
                        .mark_encoded(token)
                        .map_err(|_| EnvironmentError::InvalidState)?;
                    if let Some(boards) = &mut temporal.boards {
                        boards
                            .mark_encoded(token)
                            .map_err(|_| EnvironmentError::InvalidState)?;
                    }
                    Ok(draws.draws)
                },
            )
            .map_err(|e| e.to_string())?;
        self.frame_upload_remaining = budget.remaining_bytes;
        let bytes = work
            .upload_bytes
            .saturating_sub(work.terrain.camera_upload_bytes);
        self.upload_bytes_total = self.upload_bytes_total.saturating_add(bytes);
        self.retained_foreground_work.record(telemetry::WorkCounts {
            upload_bytes: bytes,
            ..telemetry::WorkCounts::ZERO
        });
        if let Some(candidate) = &mut temporal.candidate {
            candidate.draw = camera;
            candidate.uploads = self.upload_bytes_total;
            candidate.props = props_encoded.get();
            candidate.work = work.terrain;
            candidate.encoded = true;
            self.encoded_terrain = work.terrain.passes != 0;
        }
        if let Some(candidate) = &mut temporal.post {
            candidate.encoded = true;
        }
        Ok(work.terrain)
    }
    /// Call immediately after queue.submit, including restore-only/UI frames.
    pub fn mark_presentation_submitted(&mut self, frame: u64) -> Result<(), String> {
        let Some(temporal) = &mut self.temporal else {
            return Ok(());
        };
        if temporal.frame_id != frame {
            return Err("wrong global presentation submission".into());
        }
        let environment = &mut self
            .environment
            .as_mut()
            .expect("prepared environment")
            .owner
            .gpu;
        if let Some(candidate) = &mut temporal.candidate {
            if candidate.token.frame() != frame || !candidate.encoded || candidate.submitted {
                return Err("wrong temporal scene submission".into());
            }
            environment
                .post_mut()
                .mark_temporal_submitted(candidate.token.into())
                .map_err(|e| e.to_string())?;
            candidate.submitted = true;
        } else if let Some(candidate) = &mut temporal.post {
            if candidate.token.frame != frame || !candidate.encoded || candidate.submitted {
                return Err("wrong temporal Post submission".into());
            }
            environment
                .post_mut()
                .mark_temporal_submitted(candidate.token)
                .map_err(|e| e.to_string())?;
            candidate.submitted = true;
        }
        if let Some(token) = environment
            .temporal()
            .and_then(|t| t.output_restore_token())
        {
            environment
                .temporal_mut()
                .expect("resolver")
                .mark_output_restore_submitted(token)
                .map_err(|e| e.to_string())?;
        }
        if let Some(token) = environment.post().temporal_restore_token() {
            environment
                .post_mut()
                .mark_temporal_restore_submitted(token)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    /// Call after both GPU completion and actual surface presentation. Validate
    /// every owner before the first mutation so cross-owner failure is atomic.
    pub fn commit_presentation(&mut self, frame: u64) -> Result<(), String> {
        let epoch = self.props_source_epoch();
        let revoked = self.temporal.as_ref().is_some_and(|temporal| {
            temporal
                .candidate
                .is_some_and(|candidate| candidate.props_epoch != epoch)
                || temporal
                    .post
                    .is_some_and(|candidate| candidate.props_epoch != epoch)
        });
        if revoked {
            self.discard_presentation(frame)?;
            // A submitted old candidate needs its exposure rollback, but its
            // cached HDR is revoked. Clear only Post's source metadata so the
            // ordinary nonpresenting restore can precede a fresh source graph.
            self.environment
                .as_mut()
                .expect("temporal environment")
                .owner
                .gpu
                .post_mut()
                .invalidate();
            self.cached_presentation_available = false;
            self.force_redraw = true;
            return Ok(());
        }
        let Some(temporal) = &mut self.temporal else {
            return Ok(());
        };
        let environment = &mut self
            .environment
            .as_mut()
            .expect("prepared environment")
            .owner
            .gpu;
        if let Some(candidate) = temporal.candidate {
            if candidate.token.frame() != frame || !candidate.encoded || !candidate.submitted {
                return Err("wrong/unsubmitted temporal scene completion".into());
            }
            let token = candidate.token;
            environment
                .temporal()
                .expect("resolver")
                .validate_commit(token)
                .map_err(|e| e.to_string())?;
            environment
                .post()
                .validate_temporal_commit(token.into())
                .map_err(|e| e.to_string())?;
            temporal.models.validate_commit(token)?;
            temporal
                .terrain
                .validate_commit(token)
                .map_err(|e| e.to_string())?;
            if let Some(far) = &temporal.far {
                far.validate_commit(token).map_err(|e| e.to_string())?;
            }
            if let Some(boards) = &temporal.boards {
                boards.validate_commit(token).map_err(|e| e.to_string())?;
            }
            environment
                .post_mut()
                .commit_temporal_presented(token.into())
                .map_err(|e| e.to_string())?;
            environment
                .temporal_mut()
                .expect("resolver")
                .commit_presented(token)
                .map_err(|e| e.to_string())?;
            temporal.models.commit_presented(token)?;
            temporal
                .terrain
                .commit_presented(token)
                .map_err(|e| e.to_string())?;
            if let Some(far) = &mut temporal.far {
                far.commit_presented(token).map_err(|e| e.to_string())?;
            }
            if let Some(boards) = &mut temporal.boards {
                boards.commit_presented(token).map_err(|e| e.to_string())?;
            }
            temporal.presented_base = Some(candidate.base);
            temporal.presented_draw = Some(candidate.draw);
            self.cached_camera = Some(candidate.base);
            self.cached_world_version = candidate.world;
            self.cached_upload_bytes = candidate.uploads;
            self.cached_visual_revision = candidate.visual;
            self.cached_props_epoch = candidate.props_epoch;
            self.observed_world_version = candidate.world;
            self.observed_upload_bytes = candidate.uploads;
            self.cached_presentation_available = true;
            self.props_content_encoded = candidate.props;
            self.props_frames += u64::from(candidate.props);
            self.terrain_frames +=
                u64::from(candidate.work.passes > 0 && self.resident.values().any(|p| p.quads > 0));
            self.rendered_edit_revision = candidate.edit;
            self.force_redraw = self.renderer.visual_revision() != candidate.visual
                || self.scene.world().version() != candidate.world;
            temporal.candidate = None;
        } else if let Some(candidate) = temporal.post {
            if candidate.token.frame != frame || !candidate.encoded || !candidate.submitted {
                return Err("wrong/unsubmitted temporal Post completion".into());
            }
            environment
                .post()
                .validate_temporal_commit(candidate.token)
                .map_err(|e| e.to_string())?;
            environment
                .post_mut()
                .commit_temporal_presented(candidate.token)
                .map_err(|e| e.to_string())?;
            temporal.post = None;
        } else {
            return Ok(());
        }
        temporal.last_frame = frame;
        Ok(())
    }
    pub fn discard_presentation(&mut self, frame: u64) -> Result<(), String> {
        let Some(temporal) = &mut self.temporal else {
            // Default AA encodes directly into HDR. If its encoder is abandoned,
            // its CPU source stamps cannot certify the older GPU texture.
            self.cached_presentation_available = false;
            self.force_redraw = true;
            if let Some(environment) = &mut self.environment {
                environment
                    .owner
                    .gpu
                    .invalidate(&mut self.renderer, self.tick.0);
            } else {
                self.renderer.invalidate();
            }
            if self.encoded_shadows
                && let Some(lighting) = &mut self.lighting
            {
                lighting.owner.invalidate_all_shadows();
            }
            self.encoded_shadows = false;
            self.encoded_terrain = false;
            return Ok(());
        };
        let environment = &mut self
            .environment
            .as_mut()
            .expect("prepared environment")
            .owner;
        if let Some(candidate) = temporal.candidate {
            if candidate.token.frame() != frame {
                return Err("wrong temporal discard frame".into());
            }
            let token = candidate.token;
            if environment.gpu.post().temporal_frame_token() == Some(token.into()) {
                environment
                    .gpu
                    .post_mut()
                    .discard_temporal(token.into())
                    .map_err(|e| e.to_string())?;
            }
            if temporal.terrain.pending() == Some(token) {
                temporal.terrain.discard(token).map_err(|e| e.to_string())?;
            }
            if let Some(far) = &mut temporal.far
                && far.pending() == Some(token)
            {
                far.discard(token).map_err(|e| e.to_string())?;
            }
            if let Some(boards) = &mut temporal.boards
                && boards.pending() == Some(token)
            {
                boards.discard(token).map_err(|e| e.to_string())?;
            }
            // stage may have failed before a model candidate existed.
            if temporal.models.pending_token() == Some(token.into()) {
                temporal.models.discard(token)?;
            }
            environment
                .gpu
                .temporal_mut()
                .expect("resolver")
                .discard(token)
                .map_err(|e| e.to_string())?;
            if !candidate.submitted {
                environment.discard_unsubmitted_scene(
                    &mut self.renderer,
                    self.tick.0,
                    candidate.gi_started,
                );
                if let Some(entities) = &mut self.entities {
                    entities.gpu.invalidate_prepared_frame();
                }
                if let Some(props) = &mut self.props {
                    props.gpu.invalidate_prepared_frame();
                }
                if let Some(horizon) = &mut self.horizon {
                    horizon.owner.height.invalidate_prepared_frame();
                    horizon.owner.proxies.invalidate_prepared_frame();
                }
                if self.encoded_shadows
                    && let Some(lighting) = &mut self.lighting
                {
                    lighting.owner.invalidate_all_shadows();
                }
            }
            temporal.candidate = None;
        } else if let Some(candidate) = temporal.post {
            if candidate.token.frame != frame {
                return Err("wrong Post discard frame".into());
            }
            if environment.gpu.post().temporal_frame_token() == Some(candidate.token) {
                environment
                    .gpu
                    .post_mut()
                    .discard_temporal(candidate.token)
                    .map_err(|e| e.to_string())?;
            } else {
                environment
                    .gpu
                    .cancel_prepared_post_frame(candidate.token)
                    .map_err(|e| e.to_string())?;
            }
            temporal.post = None;
        }
        if let Some(token) = environment
            .gpu
            .temporal()
            .and_then(|t| t.output_restore_token())
        {
            environment
                .gpu
                .temporal_mut()
                .expect("resolver")
                .discard_output_restore(token)
                .map_err(|e| e.to_string())?;
        }
        if let Some(token) = environment.gpu.post().temporal_restore_token() {
            environment
                .gpu
                .post_mut()
                .discard_temporal_restore(token)
                .map_err(|e| e.to_string())?;
        }
        self.encoded_shadows = false;
        self.encoded_terrain = false;
        self.force_redraw = true;
        Ok(())
    }
}

impl ClientView {
    pub(super) fn temporal_restore_needed(&self) -> bool {
        self.temporal.is_some()
            && self
                .environment
                .as_ref()
                .is_some_and(|e| e.owner.gpu.post().temporal_restore_pending())
    }
    pub(super) fn encode_unpresented_temporal_restore(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), String> {
        if self.temporal_restore_needed() {
            self.environment
                .as_mut()
                .expect("temporal environment")
                .owner
                .gpu
                .post_mut()
                .encode_unpresented_temporal_restore(encoder)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn temporal_capacities(&self) -> Capacities {
        Capacities {
            terrain_pages: self.renderer.geometry().max_pages,
            entity_instances: self.entities.as_ref().map(|e| e.gpu.instance_capacity()),
            prop_instances: self.props.as_ref().map(|p| p.gpu.instance_capacity()),
            far: self.horizon.as_ref().map(|h| {
                (
                    h.owner.height.shadow_source().max_tiles,
                    h.owner.proxies.shadow_source().max_objects,
                )
            }),
            recipe_boards: self.recipe_boards.as_ref().map(|b| b.gpu().capacity()),
            optical_mask: self
                .environment
                .as_ref()
                .is_some_and(|e| e.owner.temporal_optical_mask_needed()),
        }
    }
    pub(super) fn preflight_temporal_resize(
        &self,
        device: &wgpu::Device,
        extent: [u32; 2],
    ) -> Result<(), String> {
        if let Some(temporal) = &self.temporal {
            temporal.validate_idle()?;
            required_bytes(
                temporal.config,
                extent,
                device.limits().max_texture_dimension_2d,
                device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
                self.temporal_capacities(),
            )?;
        }
        Ok(())
    }
    pub(super) fn resize_temporal(&mut self, device: &wgpu::Device) -> Result<(), String> {
        let caps = self.temporal_capacities();
        let Some(temporal) = &mut self.temporal else {
            return Ok(());
        };
        let environment = self.environment.as_mut().expect("temporal environment");
        let extent = self.renderer.extent();
        let bytes = required_bytes(
            temporal.config,
            extent,
            device.limits().max_texture_dimension_2d,
            device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            caps,
        )?;
        temporal.models.resize_targets(
            device,
            &environment.queue,
            TargetConfig {
                extent,
                samples: 1,
                max_pixels: temporal.config.max_pixels,
                memory_budget_bytes: temporal.config.memory_budget_bytes.min(MAX_TEMPORAL_BYTES),
            },
        )?;
        let mut budget = UploadBudget::new(2 * 1024 * 1024);
        environment
            .owner
            .gpu
            .resize_temporal_inputs(
                device,
                &environment.queue,
                &self.renderer,
                TemporalInputs {
                    motion: temporal.models.targets().motion_view(),
                    surface: temporal.models.targets().surface_view(),
                },
                &mut budget,
            )
            .map_err(|e| e.to_string())?;
        temporal.bytes = bytes;
        temporal.presented_base = None;
        temporal.presented_draw = None;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn caps() -> Capacities {
        Capacities {
            terrain_pages: 256,
            entity_instances: Some(1024),
            prop_instances: Some(16384),
            far: Some((4096, 512)),
            recipe_boards: Some(9),
            optical_mask: true,
        }
    }
    #[test]
    fn graph_preflight_counts_external_images_once_and_rejects_oversize() {
        let config = TaaConfig::default();
        let extent = [1920, 1080];
        let bytes = required_bytes(config, extent, 16384, true, caps()).unwrap();
        let expected = 1920 * 1080 * 88_u64
            + config.required_constant_bytes(true)
            + 256
            + (96 * 1024 + 80)
            + (96 * 16384 + 80)
            + TerrainMotionGpu::required_buffer_bytes(256, true)
            + FarMotionGpu::required_buffer_bytes(4096, 512, true)
            + RecipeBoardMotionGpu::required_buffer_bytes(9).unwrap()
            + 256
            + 16;
        assert_eq!(bytes, expected);
        assert!(bytes < MAX_TEMPORAL_BYTES);
        let large = 3440 * 1440 * 88_u64 + (expected - 1920 * 1080 * 88_u64);
        assert_eq!(
            required_bytes(config, [3440, 1440], 16384, true, caps()).unwrap_err(),
            crate::antialiasing::Error::TemporalMemory {
                required: large,
                maximum: MAX_TEMPORAL_BYTES,
            }
            .to_string(),
        );
        for extent in [[3840, 2160], [3440, 1440], [0, 1080]] {
            assert!(required_bytes(config, extent, 16384, true, caps()).is_err());
        }
        assert!(required_bytes(config, [1920, 1080], 1024, true, caps()).is_err());
    }
}
