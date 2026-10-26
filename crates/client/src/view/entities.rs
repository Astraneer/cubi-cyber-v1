//! Loading-only entity setup and local body upload. The body is one64B record;
//! the same GPU catalog drives color and an independent dynamic shadow list.
use crate::entities::{
    EntityCopyWork, EntityFrame, EntityFrameError, RegistryItemVisuals, instance_light_sampled,
    local_player_instance,
};
use core_types::SpaceId;
use render::{
    entity_gpu::{
        EntityConfig, EntityError, EntityGpu, EntityInstance, EntityWork, PRIMARY_HIDDEN,
    },
    light_volume::PackedLight,
    shadows::ShadowGpu,
    terrain_gpu::{Camera, TerrainGpu, UploadBudget},
};
#[derive(Clone, Copy, Debug, Default)]
pub struct EntityStats {
    pub startup_upload_bytes: u64,
    pub startup_vertices: u32,
    pub pipeline_compilations: u32,
    pub upload_bytes: u64,
    pub instance_records: u64,
    pub light_reads: u64,
    pub unknown_light_reads: u64,
    pub primary_visible: bool,
    pub active: bool,
    pub cpu_vertices: u64,
    pub remote_records: usize,
    pub primary_records: usize,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalEntityUpdate {
    pub work: EntityWork,
    pub changed: bool,
    pub primary_visible: bool,
}
pub struct ViewEntities {
    pub gpu: EntityGpu,
    pub stats: EntityStats,
    last: Option<EntityInstance>,
    last_light: PackedLight,
    remote: EntityFrame,
    remote_space: SpaceId,
    remote_origin: [i64; 3],
    instances: Vec<EntityInstance>,
    lights: Vec<PackedLight>,
    sprites: ui::atlas::ItemSprites,
    last_count: usize,
    casts_shadows: bool,
    authoritative_light: bool,
}
impl ViewEntities {
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        shadows: Option<&ShadowGpu>,
    ) -> Result<Self, EntityError> {
        Self::prepare_with_environment(device, queue, terrain, shadows, None)
    }
    pub fn prepare_with_environment(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        shadows: Option<&ShadowGpu>,
        atmosphere: Option<&render::atmosphere::AtmosphereGpu>,
    ) -> Result<Self, EntityError> {
        Self::prepare_with_source(device, queue, terrain, shadows, atmosphere, false)
    }
    pub fn prepare_with_source(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        shadows: Option<&ShadowGpu>,
        atmosphere: Option<&render::atmosphere::AtmosphereGpu>,
        authoritative_light: bool,
    ) -> Result<Self, EntityError> {
        Self::prepare_with_source_and_gi(
            device,
            queue,
            terrain,
            shadows,
            atmosphere,
            None,
            authoritative_light,
        )
    }
    pub fn prepare_with_source_and_gi(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        shadows: Option<&ShadowGpu>,
        atmosphere: Option<&render::atmosphere::AtmosphereGpu>,
        gi: Option<&render::gi::GiGpu>,
        authoritative_light: bool,
    ) -> Result<Self, EntityError> {
        let sprites =
            ui::atlas::ItemSprites::load().map_err(|error| EntityError::Assets(error.into()))?;
        let atlas = render::entity_models::item_atlas(&sprites.rgba, sprites.layers)
            .map_err(EntityError::Assets)?;
        let mut gpu = match atmosphere {
            Some(atmosphere) => EntityGpu::new_with_lighting(
                device,
                queue,
                terrain.geometry(),
                EntityConfig::default(),
                shadows,
                Some(&atlas),
                atmosphere,
                gi,
            ),
            None => EntityGpu::new_with_items(
                device,
                queue,
                terrain.geometry(),
                EntityConfig::default(),
                shadows,
                &atlas,
            ),
        }?;
        let startup = gpu.take_work();
        Ok(Self {
            gpu,
            stats: EntityStats {
                startup_upload_bytes: startup.upload_bytes,
                startup_vertices: startup.startup_vertices,
                pipeline_compilations: startup.pipeline_compilations,
                ..EntityStats::default()
            },
            last: None,
            last_light: PackedLight::default(),
            remote: EntityFrame::new(EntityConfig::default().max_entities as usize - 1),
            remote_space: SpaceId(0),
            remote_origin: [0; 3],
            instances: Vec::with_capacity(EntityConfig::default().max_entities as usize),
            lights: Vec::with_capacity(EntityConfig::default().max_entities as usize),
            sprites,
            last_count: 0,
            casts_shadows: shadows.is_some(),
            authoritative_light,
        })
    }
    /// CPU frame conversion is bounded and atomic; original item sprites use
    /// the same stable layers as the UI, with their own GPU texture array.
    pub fn set_remote_frame(
        &mut self,
        source: &[rules::entities::mob::Instance],
        scene: &crate::ClientScene,
        space: SpaceId,
        origin: [i64; 3],
    ) -> Result<EntityCopyWork, EntityFrameError> {
        let sprites = &self.sprites;
        let visuals = RegistryItemVisuals {
            icon_layer: &|item| sprites.layer(item),
        };
        let authoritative = self.authoritative_light;
        let work = self
            .remote
            .prepare_sampled(source, space, origin, &visuals, |position| {
                if authoritative {
                    scene.read_authoritative_light(position)
                } else {
                    scene
                        .world()
                        .page(position.page())
                        .map(|page| page.light(position.local_index()))
                }
            })?;
        self.remote_space = space;
        self.remote_origin = origin;
        self.stats.remote_records = source.len();
        self.stats.light_reads += work.light_reads;
        self.stats.unknown_light_reads += work.unknown_light_reads;
        Ok(work)
    }
    #[allow(clippy::too_many_arguments)] // One frame owner's explicit immutable pose/light context.
    pub fn upload_local(
        &mut self,
        queue: &wgpu::Queue,
        player: Option<&sim::Player>,
        alpha: f64,
        mode: sim::CameraMode,
        scene: &crate::ClientScene,
        space: SpaceId,
        origin: [i64; 3],
        budget: &mut UploadBudget,
    ) -> Result<LocalEntityUpdate, EntityError> {
        // A hidden body needs a record only when it can cast a shadow. Its
        // simulation animation keeps advancing independently in either case.
        let instance = player
            .filter(|_| self.casts_shadows || mode != sim::CameraMode::FirstPerson)
            .and_then(|p| local_player_instance(p, alpha, mode, origin, 0));
        let light = instance.as_ref().and_then(|i| {
            instance_light_sampled(space, origin, i, |position| {
                if self.authoritative_light {
                    scene.read_authoritative_light(position)
                } else {
                    scene
                        .world()
                        .page(position.page())
                        .map(|page| page.light(position.local_index()))
                }
            })
        });
        let packed = light.unwrap_or_default();
        if !self.remote.instances().is_empty()
            && (self.remote_space != space || self.remote_origin != origin)
        {
            return Err(EntityError::InvalidCamera);
        }
        self.instances.clear();
        self.instances.extend_from_slice(self.remote.instances());
        self.lights.clear();
        self.lights.extend_from_slice(self.remote.lights());
        if let Some(instance) = instance {
            self.instances.push(instance);
            self.lights.push(packed);
        }
        let work = self
            .gpu
            .upload(queue, &self.instances, &self.lights, budget)?;
        let changed = self.last != instance
            || self.last_light != packed
            || self.last_count != self.instances.len()
            || work.upload_bytes != 0;
        self.gpu.take_work();
        self.last = instance;
        self.last_light = packed;
        self.last_count = self.instances.len();
        self.stats.upload_bytes += work.upload_bytes;
        self.stats.instance_records += u64::from(work.instance_records);
        self.stats.active = !self.instances.is_empty();
        self.stats.primary_records = self
            .instances
            .iter()
            .filter(|i| i.flags & PRIMARY_HIDDEN == 0)
            .count();
        self.stats.primary_visible = instance.is_some_and(|i| i.flags & PRIMARY_HIDDEN == 0);
        self.stats.light_reads += u64::from(instance.is_some());
        self.stats.unknown_light_reads += u64::from(instance.is_some() && light.is_none());
        self.stats.cpu_vertices += u64::from(work.cpu_vertices);
        Ok(LocalEntityUpdate {
            work,
            changed,
            primary_visible: self.stats.primary_visible,
        })
    }
    pub fn prepare_camera(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: Camera,
    ) -> Result<EntityWork, EntityError> {
        if self.stats.primary_records == 0 {
            return Ok(EntityWork::default());
        }
        self.gpu.prepare_frame(queue, encoder, camera)
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        self.gpu.draw(pass)
    }
}
