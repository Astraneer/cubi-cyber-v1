//! Optional model portion of the temporal scene transaction. Source identities
//! come from successful GPU publications; poses commit only with presentation.
use super::{ViewEntities, ViewProps};
use render::{
    block_models::BlockInstance,
    entity_gpu::EntityInstance,
    model_motion::{ModelMotionGpu, MotionTargets, Parameters, TargetConfig},
    presented_motion::{Frame, GpuMatch, ModelPose, ObjectKey, ObjectPose, PresentedMotion, Token},
    taa::FrameToken,
    terrain_gpu::UploadBudget,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Work {
    pub records_visited: u64,
    pub identities_compared: u64,
    pub upload_bytes: u64,
    pub pipeline_compilations: u32,
    pub draws: u32,
}
struct Bank<P: ModelPose> {
    gpu: ModelMotionGpu,
    poses: PresentedMotion<P>,
    rows: Vec<ObjectPose<P>>,
    slots: Vec<usize>,
    metadata: Vec<GpuMatch>,
    parameters: Parameters,
}
impl<P: ModelPose> Bank<P> {
    fn new(gpu: ModelMotionGpu, capacity: usize) -> Result<Self, String> {
        Ok(Self {
            gpu,
            poses: PresentedMotion::new(capacity, 32.)
                .map_err(|e| format!("model pose bank: {e:?}"))?,
            rows: Vec::with_capacity(capacity),
            slots: Vec::with_capacity(capacity),
            metadata: Vec::with_capacity(capacity),
            parameters: Parameters {
                previous_unjittered_view_projection: [[0.; 4]; 4],
                inverse_extent_jitter: [0.; 4],
            },
        })
    }
    fn stage(
        &mut self,
        token: Token,
        frame: Frame,
        jitter: [f32; 2],
        records: &[P],
        mut identity: impl FnMut(usize) -> Option<(ObjectKey, u64)>,
    ) -> Result<Work, String> {
        self.rows.clear();
        self.slots.clear();
        self.metadata.clear();
        if records.len() > self.rows.capacity() {
            return Err("model motion record capacity".into());
        }
        for (slot, pose) in records.iter().enumerate() {
            self.metadata.push(GpuMatch::default());
            if let Some((key, topology)) = identity(slot) {
                self.slots.push(slot);
                self.rows.push(ObjectPose {
                    key,
                    topology,
                    pose: *pose,
                    visible: pose.primary_visible(),
                    discontinuity: false,
                });
            }
        }
        self.poses
            .stage(token, frame, &self.rows)
            .map_err(|e| format!("model motion stage: {e:?}"))?;
        for (&slot, matched) in self.slots.iter().zip(
            self.poses
                .matches(token)
                .map_err(|e| format!("model motion matches: {e:?}"))?,
        ) {
            self.metadata[slot] = *matched;
        }
        self.parameters = Parameters {
            previous_unjittered_view_projection: self
                .poses
                .presented_frame()
                .map_or(frame.unjittered_view_projection, |old| {
                    old.1.unjittered_view_projection
                }),
            inverse_extent_jitter: [
                1. / frame.extent[0] as f32,
                1. / frame.extent[1] as f32,
                jitter[0],
                jitter[1],
            ],
        };
        Ok(Work {
            records_visited: records.len() as u64,
            identities_compared: self.poses.work().identity_comparisons,
            ..Work::default()
        })
    }
}
pub struct ViewModelMotion {
    targets: MotionTargets,
    entities: Option<Bank<EntityInstance>>,
    props: Option<Bank<BlockInstance>>,
    pending: Option<(Token, bool)>,
    startup: Work,
}
impl ViewModelMotion {
    /// Pure whole-owner cost: motion24P+query plus actual96N+80 model banks.
    pub fn required_allocation_bytes(
        config: TargetConfig,
        max_dimension: u32,
        timestamps: bool,
        entity_capacity: Option<u32>,
        prop_capacity: Option<u32>,
    ) -> Result<u64, String> {
        let mut bytes = MotionTargets::required_allocation_bytes(config, max_dimension, timestamps)
            .map_err(|e| e.to_string())?;
        for capacity in [entity_capacity, prop_capacity].into_iter().flatten() {
            bytes = bytes
                .checked_add(
                    ModelMotionGpu::required_buffer_bytes(capacity).map_err(|e| e.to_string())?,
                )
                .ok_or("model motion allocation overflow")?;
        }
        if bytes > config.memory_budget_bytes {
            return Err("model motion allocation budget".into());
        }
        Ok(bytes)
    }
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: TargetConfig,
        entities: Option<&mut ViewEntities>,
        props: Option<&mut ViewProps>,
    ) -> Result<Self, String> {
        Self::required_allocation_bytes(
            config,
            device.limits().max_texture_dimension_2d,
            device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            entities.as_ref().map(|e| e.gpu.instance_capacity()),
            props.as_ref().map(|p| p.gpu.instance_capacity()),
        )?;
        // Reject every unsupported loading source before replacing any pipeline.
        if entities
            .as_ref()
            .is_some_and(|e| !e.gpu.temporal_raster_preparable())
            || props
                .as_ref()
                .is_some_and(|p| !p.gpu.temporal_raster_preparable())
        {
            return Err("model temporal raster configuration is not preparable".into());
        }
        if entities
            .as_ref()
            .is_some_and(|e| !e.gpu.instance_records().is_empty())
            || props.as_ref().is_some_and(|p| {
                p.gpu
                    .instance_records()
                    .iter()
                    .any(|i| i.flags & render::block_models::HIDDEN == 0)
            })
        {
            return Err("model temporal preparation requires unpopulated loading sources".into());
        }
        let mut startup = Work::default();
        let entity_bank = if let Some(source) = entities {
            startup.pipeline_compilations += source
                .prepare_temporal_raster(device)
                .map_err(|e| e.to_string())?
                .pipeline_compilations;
            let gpu = ModelMotionGpu::entities(device, &source.gpu).map_err(|e| e.to_string())?;
            startup.pipeline_compilations += gpu.startup_pipeline_compilations();
            Some(Bank::new(gpu, source.gpu.instance_capacity() as usize)?)
        } else {
            None
        };
        let prop_bank = if let Some(source) = props {
            startup.pipeline_compilations += source.prepare_temporal_raster(device)?;
            let gpu = ModelMotionGpu::blocks(device, &source.gpu).map_err(|e| e.to_string())?;
            startup.pipeline_compilations += gpu.startup_pipeline_compilations();
            Some(Bank::new(gpu, source.gpu.instance_capacity() as usize)?)
        } else {
            None
        };
        let targets = MotionTargets::new(device, queue, Some(config))
            .map_err(|e| e.to_string())?
            .expect("explicit motion configuration");
        Ok(Self {
            targets,
            entities: entity_bank,
            props: prop_bank,
            pending: None,
            startup,
        })
    }
    pub fn targets(&self) -> &MotionTargets {
        &self.targets
    }
    pub fn pending_token(&self) -> Option<Token> {
        self.pending.map(|p| p.0)
    }
    /// Resource phase only; pose banks remain tied to presentation. Extent
    /// compatibility rejects their old projection after resolver resize.
    pub fn resize_targets(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: TargetConfig,
    ) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("cannot resize pending model motion".into());
        }
        self.targets = MotionTargets::new(device, queue, Some(config))
            .map_err(|e| e.to_string())?
            .expect("explicit motion targets");
        Ok(())
    }
    pub fn allocation_bytes(&self) -> u64 {
        self.targets.allocation_bytes()
            + self.entities.as_ref().map_or(0, |b| b.gpu.buffer_bytes())
            + self.props.as_ref().map_or(0, |b| b.gpu.buffer_bytes())
    }
    pub fn startup_work(&self) -> Work {
        self.startup
    }
    /// No queue writes. Global Env preflight includes required_upload_bytes().
    pub fn stage(
        &mut self,
        token: FrameToken,
        frame: Frame,
        jitter: [f32; 2],
        entities: Option<&ViewEntities>,
        props: Option<&ViewProps>,
    ) -> Result<Work, String> {
        if self.pending.is_some() {
            return Err("model motion candidate already pending".into());
        }
        if jitter
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 0.5)
        {
            return Err("invalid model temporal jitter".into());
        }
        let token = Token::from(token);
        let mut work = Work::default();
        let result = (|| {
            if let Some(bank) = &mut self.entities {
                let source = entities.ok_or("prepared entity motion source absent")?;
                let identities = source
                    .published_temporal_identities()
                    .ok_or("entity identities not prepared")?;
                if identities.len() != source.gpu.instance_records().len() {
                    return Err("entity identity and GPU record count differ".into());
                }
                bank.gpu
                    .synchronize_entities(&source.gpu)
                    .map_err(|e| e.to_string())?;
                let item = bank.stage(
                    token,
                    frame,
                    jitter,
                    source.gpu.instance_records(),
                    |slot| identities[slot].map(|key| (key, 0)),
                )?;
                work.records_visited += item.records_visited;
                work.identities_compared += item.identities_compared;
            }
            if let Some(bank) = &mut self.props {
                let source = props.ok_or("prepared prop motion source absent")?;
                let identities = source
                    .published_temporal_identities()
                    .ok_or("prop identities not prepared")?;
                if identities.len() != source.gpu.instance_records().len() {
                    return Err("prop identity and GPU slot count differ".into());
                }
                bank.gpu
                    .synchronize_blocks(&source.gpu)
                    .map_err(|e| e.to_string())?;
                let item = bank.stage(
                    token,
                    frame,
                    jitter,
                    source.gpu.instance_records(),
                    |slot| {
                        identities[slot].and_then(|identity| {
                            identity.position.map(|p| {
                                (
                                    crate::model_motion_identity::prop(p)
                                        .expect("typed exact prop key"),
                                    identity.registration,
                                )
                            })
                        })
                    },
                )?;
                work.records_visited += item.records_visited;
                work.identities_compared += item.identities_compared;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(bank) = &mut self.entities
                && bank.poses.validate_commit(token).is_ok()
            {
                bank.poses
                    .discard(token)
                    .map_err(|e| format!("cancel entity stage: {e:?}"))?;
            }
            if let Some(bank) = &mut self.props
                && bank.poses.validate_commit(token).is_ok()
            {
                bank.poses
                    .discard(token)
                    .map_err(|e| format!("cancel prop stage: {e:?}"))?;
            }
            return Err(error);
        }
        self.pending = Some((token, false));
        work.upload_bytes = self.required_upload_bytes()?;
        Ok(work)
    }
    pub fn required_upload_bytes(&self) -> Result<u64, String> {
        if self.pending.is_none() {
            return Err("no staged model motion frame".into());
        }
        let mut bytes = 0;
        if let Some(bank) = &self.entities {
            bytes += bank
                .gpu
                .required_entity_upload_bytes(
                    bank.poses.presented_poses(),
                    &bank.metadata,
                    bank.parameters,
                )
                .map_err(|e| e.to_string())?;
        }
        if let Some(bank) = &self.props {
            bytes += bank
                .gpu
                .required_block_upload_bytes(
                    bank.poses.presented_poses(),
                    &bank.metadata,
                    bank.parameters,
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(bytes)
    }
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        token: FrameToken,
        budget: &mut UploadBudget,
    ) -> Result<Work, String> {
        let token = Token::from(token);
        if self.pending.map(|p| p.0) != Some(token) {
            return Err("wrong model upload token".into());
        }
        let bytes = self.required_upload_bytes()?;
        if budget.remaining_bytes < bytes {
            return Err("model motion upload budget".into());
        }
        if let Some(bank) = &mut self.entities {
            bank.gpu
                .upload_entities(
                    queue,
                    token,
                    bank.poses.presented_poses(),
                    &bank.metadata,
                    bank.parameters,
                    budget,
                )
                .map_err(|e| e.to_string())?;
        }
        if let Some(bank) = &mut self.props {
            bank.gpu
                .upload_blocks(
                    queue,
                    token,
                    bank.poses.presented_poses(),
                    &bank.metadata,
                    bank.parameters,
                    budget,
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(Work {
            upload_bytes: bytes,
            ..Work::default()
        })
    }
    /// Caller includes Terrain/Height/Proxy and optical invalidators in the
    /// same cleared target pass, then marks the complete pass encoded.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, token: FrameToken) -> Result<u32, String> {
        Ok(self.draw_entities(pass, token)? + self.draw_props(pass, token)?)
    }
    pub fn draw_entities(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        token: FrameToken,
    ) -> Result<u32, String> {
        self.draw_family(pass, token, self.entities.as_ref().map(|b| &b.gpu))
    }
    pub fn draw_props(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        token: FrameToken,
    ) -> Result<u32, String> {
        self.draw_family(pass, token, self.props.as_ref().map(|b| &b.gpu))
    }
    fn draw_family(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        token: FrameToken,
        bank: Option<&ModelMotionGpu>,
    ) -> Result<u32, String> {
        let token = Token::from(token);
        if self.pending.map(|p| p.0) != Some(token) {
            return Err("wrong model draw token".into());
        }
        bank.map_or(Ok(0), |bank| {
            bank.draw(pass, token)
                .map(|w| w.draws)
                .map_err(|e| e.to_string())
        })
    }
    pub fn mark_encoded(&mut self, token: FrameToken) -> Result<(), String> {
        let token = Token::from(token);
        let pending = self
            .pending
            .as_mut()
            .filter(|p| p.0 == token)
            .ok_or("wrong model encoded token")?;
        pending.1 = true;
        Ok(())
    }
    pub fn validate_commit(&self, token: FrameToken) -> Result<(), String> {
        let token = Token::from(token);
        if self.pending != Some((token, true)) {
            return Err("model motion was not encoded for this presentation token".into());
        }
        if let Some(bank) = &self.entities {
            bank.poses
                .validate_commit(token)
                .map_err(|e| format!("entity commit: {e:?}"))?;
        }
        if let Some(bank) = &self.props {
            bank.poses
                .validate_commit(token)
                .map_err(|e| format!("prop commit: {e:?}"))?;
        }
        Ok(())
    }
    pub fn commit_presented(&mut self, token: FrameToken) -> Result<(), String> {
        self.validate_commit(token)?;
        let token = Token::from(token);
        if let Some(bank) = &mut self.entities {
            bank.poses
                .commit_presented(token)
                .map_err(|e| format!("entity commit: {e:?}"))?;
        }
        if let Some(bank) = &mut self.props {
            bank.poses
                .commit_presented(token)
                .map_err(|e| format!("prop commit: {e:?}"))?;
        }
        self.pending = None;
        Ok(())
    }
    pub fn discard(&mut self, token: FrameToken) -> Result<(), String> {
        let token = Token::from(token);
        if self.pending.map(|p| p.0) != Some(token) {
            return Err("wrong model discard token".into());
        }
        if let Some(bank) = &self.entities {
            bank.poses
                .validate_commit(token)
                .map_err(|e| format!("entity discard: {e:?}"))?;
        }
        if let Some(bank) = &self.props {
            bank.poses
                .validate_commit(token)
                .map_err(|e| format!("prop discard: {e:?}"))?;
        }
        if let Some(bank) = &mut self.entities {
            bank.poses
                .discard(token)
                .map_err(|e| format!("entity discard: {e:?}"))?;
        }
        if let Some(bank) = &mut self.props {
            bank.poses
                .discard(token)
                .map_err(|e| format!("prop discard: {e:?}"))?;
        }
        self.pending = None;
        Ok(())
    }
}
