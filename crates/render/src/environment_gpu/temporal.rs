//! Optional loading attachment. The default graph never constructs this owner.
use super::*;
use crate::taa::{TaaConfig, TaaGpu, TaaInputs};
use crate::temporal_optics_mask::TemporalOpticsMask;

#[derive(Clone, Copy)]
pub struct TemporalInputs<'a> {
    pub motion: &'a wgpu::TextureView,
    pub surface: &'a wgpu::TextureView,
}
pub(super) struct TemporalAttachment {
    pub gpu: TaaGpu,
    pub config: TaaConfig,
    pub inputs_dirty: bool,
    pub optics: Option<TemporalOpticsMask>,
    pub post_frame: Option<crate::post::PostFrameToken>,
}
impl EnvironmentGpu {
    fn temporal_hdr(&self, terrain: &TerrainGpu) -> wgpu::TextureView {
        self.clouds
            .as_ref()
            .map(CloudGpu::resolved_view)
            .or_else(|| self.aerial.as_ref().map(AerialGpu::resolved_view))
            .or_else(|| self.water.as_ref().map(WaterGpu::resolved_view))
            .or_else(|| self.contact_ao.as_ref().map(ContactAoGpu::resolved_view))
            .unwrap_or_else(|| terrain.resolved_hdr_view())
            .clone()
    }
    /// Call after color pipelines and scaled HDR targets are prepared. Physical
    /// water and cloud coverage invalidate opaque motion before reconstruction.
    pub fn prepare_temporal(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        inputs: TemporalInputs<'_>,
        config: TaaConfig,
        budget: &mut UploadBudget,
    ) -> Result<(), EnvironmentError> {
        if self.encoded || self.temporal.is_some() || self.post.smaa().is_some() {
            return Err(EnvironmentError::Terrain(TerrainError::LatePreparation));
        }
        let [width, height] = terrain.extent();
        if budget.remaining_bytes < self.post.resize_upload_bytes(width, height) {
            return Err(EnvironmentError::UploadBudget);
        }
        let hdr = self.temporal_hdr(terrain);
        let gpu = TaaGpu::new(
            device,
            queue,
            Some(config),
            TaaInputs {
                hdr: &hdr,
                depth: terrain.scene_depth_msaa_view(),
                motion: inputs.motion,
                surface: inputs.surface,
                extent: [width, height],
                samples: self.sample_count,
            },
        )?
        .expect("explicit temporal configuration");
        let optics = TemporalOpticsMask::new(
            device,
            queue,
            true,
            self.water.as_ref(),
            self.clouds.as_ref(),
            inputs.motion,
            inputs.surface,
        )?;
        let physical = self.post.output_extent();
        self.post
            .resize_input(device, queue, gpu.output_view(), width, height, budget)?;
        self.post
            .set_output_extent_with_device(device, physical[0], physical[1])?;
        self.post.prepare_temporal_rollback(device)?;
        self.startup_pipeline_compilations += gpu.pipeline_compilations();
        self.startup_pipeline_compilations += optics
            .as_ref()
            .map_or(0, TemporalOpticsMask::pipeline_compilations);
        self.temporal = Some(Box::new(TemporalAttachment {
            gpu,
            config,
            inputs_dirty: false,
            optics,
            post_frame: None,
        }));
        Ok(())
    }
    pub fn temporal(&self) -> Option<&TaaGpu> {
        self.temporal.as_ref().map(|owner| &owner.gpu)
    }
    pub fn temporal_mut(&mut self) -> Option<&mut TaaGpu> {
        self.temporal.as_mut().map(|owner| &mut owner.gpu)
    }
    pub fn temporal_config(&self) -> Option<TaaConfig> {
        self.temporal.as_ref().map(|owner| owner.config)
    }
    /// Post-only adaptive presentations share completion semantics without
    /// advancing the resolver's jitter/history or geometry-pose caches.
    pub fn prepare_post_frame(
        &mut self,
        token: crate::post::PostFrameToken,
    ) -> Result<(), EnvironmentError> {
        self.post.validate_temporal_begin(token)?;
        let owner = self
            .temporal
            .as_mut()
            .ok_or(EnvironmentError::InvalidState)?;
        if owner.post_frame.is_some() {
            return Err(EnvironmentError::InvalidState);
        }
        owner.post_frame = Some(token);
        Ok(())
    }
    pub fn cancel_prepared_post_frame(
        &mut self,
        token: crate::post::PostFrameToken,
    ) -> Result<(), EnvironmentError> {
        let owner = self
            .temporal
            .as_mut()
            .ok_or(EnvironmentError::InvalidState)?;
        if owner.post_frame != Some(token) {
            return Err(EnvironmentError::InvalidState);
        }
        owner.post_frame = None;
        Ok(())
    }
    /// Complete resource resize after MotionTargets resized to the same HDR
    /// extent. Until this succeeds the graph refuses stale temporal bindings.
    pub fn resize_temporal_inputs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        inputs: TemporalInputs<'_>,
        budget: &mut UploadBudget,
    ) -> Result<(), EnvironmentError> {
        if self.temporal.is_none() {
            return Ok(());
        }
        let [width, height] = terrain.extent();
        if budget.remaining_bytes < self.post.resize_upload_bytes(width, height) {
            return Err(EnvironmentError::UploadBudget);
        }
        let hdr = self.temporal_hdr(terrain);
        let owner = self.temporal.as_mut().expect("checked temporal owner");
        owner.gpu.resize_inputs(
            device,
            TaaInputs {
                hdr: &hdr,
                depth: terrain.scene_depth_msaa_view(),
                motion: inputs.motion,
                surface: inputs.surface,
                extent: [width, height],
                samples: self.sample_count,
            },
        )?;
        if let Some(optics) = &mut owner.optics {
            optics.resize_inputs(inputs.motion, inputs.surface)?;
        }
        let physical = self.post.output_extent();
        self.post.resize_input(
            device,
            queue,
            owner.gpu.output_view(),
            width,
            height,
            budget,
        )?;
        self.post
            .set_output_extent_with_device(device, physical[0], physical[1])?;
        owner.inputs_dirty = false;
        Ok(())
    }
}
