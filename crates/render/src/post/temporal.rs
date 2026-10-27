//! Optional presentation transaction. GPU submission is explicit: an abandoned
//! encoder never makes its unwritten checkpoint eligible for restoration.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostFrameToken {
    pub frame: u64,
    pub serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostRestoreToken {
    frame: PostFrameToken,
    serial: u64,
}
impl From<crate::taa::FrameToken> for PostFrameToken {
    fn from(token: crate::taa::FrameToken) -> Self {
        Self {
            frame: token.frame(),
            serial: token.serial(),
        }
    }
}
#[derive(Clone, Copy)]
struct Snapshot {
    source: Option<u64>,
    adaptation_remaining: f32,
    output_requested: bool,
}
#[derive(Clone, Copy)]
pub(super) struct Candidate {
    token: PostFrameToken,
    before: Snapshot,
    submitted: bool,
}
pub(super) struct TemporalPost {
    exposure: wgpu::Buffer,
    pub(super) candidate: Option<Candidate>,
    restore: bool,
    restore_encoded: bool,
    restore_frame: Option<PostFrameToken>,
    restore_token: Option<PostRestoreToken>,
    restore_serial: u64,
}
impl PostGpu {
    /// Loading only. The ordinary graph keeps its original buffers and binds.
    pub fn prepare_temporal_rollback(&mut self, device: &wgpu::Device) -> Result<(), PostError> {
        if self.temporal.is_some() || self.last_source.is_some() || self.smaa.is_some() {
            return Err(PostError::InvalidPresentation);
        }
        self.temporal = Some(TemporalPost {
            exposure: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("last presented exposure checkpoint"),
                size: 16,
                usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            candidate: None,
            restore: false,
            restore_encoded: false,
            restore_frame: None,
            restore_token: None,
            restore_serial: 0,
        });
        Ok(())
    }
    pub fn temporal_rollback_bytes(&self) -> u64 {
        self.temporal
            .as_ref()
            .map_or(0, |owner| owner.exposure.size())
    }
    pub fn temporal_restore_pending(&self) -> bool {
        self.temporal.as_ref().is_some_and(|owner| owner.restore)
    }
    pub fn temporal_frame_token(&self) -> Option<PostFrameToken> {
        self.temporal
            .as_ref()
            .and_then(|owner| owner.candidate.map(|candidate| candidate.token))
    }
    pub fn validate_temporal_begin(&self, token: PostFrameToken) -> Result<(), PostError> {
        let owner = self
            .temporal
            .as_ref()
            .ok_or(PostError::InvalidPresentation)?;
        if token.frame == 0 || token.serial == 0 || owner.candidate.is_some() || owner.restore {
            return Err(PostError::InvalidPresentation);
        }
        Ok(())
    }
    /// Preflight the complete Post plan before encoding its checkpoint. A
    /// post-only adaptive frame may use its own shared presentation token;
    /// cached UI frames never call this method or advance adaptation.
    pub fn encode_temporal(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        source_revision: u64,
        remaining_pixels: &mut u64,
        token: PostFrameToken,
    ) -> Result<PostWork, PostError> {
        self.validate_temporal_begin(token)?;
        if self.pending_work(source_revision).pixels() > *remaining_pixels {
            return Err(PostError::WorkBudget);
        }
        let before = Snapshot {
            source: self.last_source,
            adaptation_remaining: self.adaptation_remaining,
            output_requested: self.output_requested,
        };
        encoder.copy_buffer_to_buffer(
            &self.state,
            0,
            &self.temporal.as_ref().unwrap().exposure,
            0,
            16,
        );
        let mut work = self.encode_inner(encoder, output, source_revision, remaining_pixels)?;
        self.temporal.as_mut().unwrap().candidate = Some(Candidate {
            token,
            before,
            submitted: false,
        });
        work.state_copy_bytes = 16;
        Ok(work)
    }
    pub fn mark_temporal_submitted(&mut self, token: PostFrameToken) -> Result<(), PostError> {
        let candidate = self
            .temporal
            .as_mut()
            .and_then(|owner| owner.candidate.as_mut())
            .ok_or(PostError::InvalidPresentation)?;
        if candidate.token != token || candidate.submitted {
            return Err(PostError::InvalidPresentation);
        }
        candidate.submitted = true;
        Ok(())
    }
    pub fn validate_temporal_commit(&self, token: PostFrameToken) -> Result<(), PostError> {
        let candidate = self
            .temporal
            .as_ref()
            .and_then(|owner| owner.candidate)
            .ok_or(PostError::InvalidPresentation)?;
        if candidate.token != token || !candidate.submitted {
            return Err(PostError::InvalidPresentation);
        }
        Ok(())
    }
    pub fn commit_temporal_presented(&mut self, token: PostFrameToken) -> Result<(), PostError> {
        self.validate_temporal_commit(token)?;
        self.temporal.as_mut().unwrap().candidate = None;
        Ok(())
    }
    pub fn discard_temporal(&mut self, token: PostFrameToken) -> Result<(), PostError> {
        let owner = self
            .temporal
            .as_mut()
            .ok_or(PostError::InvalidPresentation)?;
        let candidate = owner.candidate.ok_or(PostError::InvalidPresentation)?;
        if candidate.token != token {
            return Err(PostError::InvalidPresentation);
        }
        self.last_source = candidate.before.source;
        self.adaptation_remaining = candidate.before.adaptation_remaining;
        // Preserve hot/current user compensation and output requests.
        self.output_requested |= candidate.before.output_requested;
        owner.candidate = None;
        owner.restore = candidate.submitted;
        owner.restore_encoded = false;
        owner.restore_frame = candidate.submitted.then_some(candidate.token);
        owner.restore_token = None;
        Ok(())
    }
    pub(super) fn temporal_restore_work(&self) -> PostWork {
        if !self.temporal_restore_pending() {
            return PostWork::default();
        }
        let histogram_samples = self.auto.as_ref().map_or(0, |_| {
            u64::from(self.parameters.image[0].div_ceil(self.config.histogram_stride))
                * u64::from(self.parameters.image[1].div_ceil(self.config.histogram_stride))
        });
        PostWork {
            state_copy_bytes: 16,
            histogram_samples,
            bloom_texels: self.bloom.as_ref().map_or(0, |bloom| {
                bloom
                    .levels
                    .iter()
                    .map(|level| u64::from(level.width) * u64::from(level.height))
                    .sum()
            }),
            passes: u32::from(self.auto.is_some()) * 2
                + self
                    .bloom
                    .as_ref()
                    .map_or(0, |bloom| bloom.levels.len() as u32),
            ..PostWork::default()
        }
    }
    fn begin_temporal_restore(&mut self) -> Result<(), PostError> {
        let owner = self
            .temporal
            .as_mut()
            .ok_or(PostError::InvalidPresentation)?;
        if !owner.restore || owner.restore_token.is_some() {
            return Err(PostError::InvalidPresentation);
        }
        owner.restore_serial = owner
            .restore_serial
            .checked_add(1)
            .ok_or(PostError::InvalidPresentation)?;
        owner.restore_token = Some(PostRestoreToken {
            frame: owner.restore_frame.ok_or(PostError::InvalidPresentation)?,
            serial: owner.restore_serial,
        });
        Ok(())
    }
    pub fn temporal_restore_token(&self) -> Option<PostRestoreToken> {
        self.temporal.as_ref().and_then(|owner| owner.restore_token)
    }
    pub(super) fn encode_temporal_restore(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), PostError> {
        if !self.temporal_restore_pending() {
            return Ok(());
        }
        self.begin_temporal_restore()?;
        let owner = self.temporal.as_ref().unwrap();
        encoder.copy_buffer_to_buffer(&owner.exposure, 0, &self.state, 0, 16);
        self.encode_histogram(encoder);
        self.encode_bloom(encoder);
        self.temporal.as_mut().unwrap().restore_encoded = true;
        Ok(())
    }
    /// After a submitted first candidate was discarded there is no old HDR to
    /// tone. This copy can accompany Loading clear; the next scene remeasures
    /// its own histogram/Bloom. Submission must still be acknowledged.
    pub fn encode_unpresented_temporal_restore(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<u64, PostError> {
        if self.has_cached_source() || !self.temporal_restore_pending() {
            return Err(PostError::InvalidPresentation);
        }
        self.begin_temporal_restore()?;
        let owner = self.temporal.as_mut().unwrap();
        encoder.copy_buffer_to_buffer(&owner.exposure, 0, &self.state, 0, 16);
        owner.restore_encoded = true;
        Ok(16)
    }
    pub fn mark_temporal_restore_submitted(
        &mut self,
        token: PostRestoreToken,
    ) -> Result<(), PostError> {
        let owner = self
            .temporal
            .as_mut()
            .ok_or(PostError::InvalidPresentation)?;
        if !owner.restore || !owner.restore_encoded || owner.restore_token != Some(token) {
            return Err(PostError::InvalidPresentation);
        }
        owner.restore = false;
        owner.restore_encoded = false;
        owner.restore_frame = None;
        owner.restore_token = None;
        Ok(())
    }
    pub fn discard_temporal_restore(&mut self, token: PostRestoreToken) -> Result<(), PostError> {
        let owner = self
            .temporal
            .as_mut()
            .ok_or(PostError::InvalidPresentation)?;
        if owner.restore_token != Some(token) {
            return Err(PostError::InvalidPresentation);
        }
        owner.restore_encoded = false;
        owner.restore_token = None;
        Ok(())
    }
}
