//! Preallocated bridge from authoritative/predicted rule instances to the shared
//! GPU ABI. World light reads are pure; no generation or model vertices occur.
use core_types::{BlockPos, SpaceId};
use render::{entity_gpu::EntityInstance, light_volume::PackedLight};
use world::World;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemVisual {
    pub kind: u32,
    pub atlas_layer: u32,
}
pub trait ItemVisuals {
    /// Return a stable local icon/atlas mapping; it must not perform I/O.
    fn visual(&self, item: rules::items::ItemId) -> Option<ItemVisual>;
    fn block_layers(&self, _item: rules::items::ItemId) -> Option<[u16; 6]> {
        None
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityFrameError {
    Capacity,
    InvalidKind,
    InvalidPosition,
    InvalidIdentity,
    MissingItemIcon(u16),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntityCopyWork {
    pub instances: u64,
    pub light_reads: u64,
    pub unknown_light_reads: u64,
    pub cpu_vertices: u64,
}
pub struct EntityFrame {
    maximum: usize,
    instances: Vec<EntityInstance>,
    lights: Vec<PackedLight>,
    staging_instances: Vec<EntityInstance>,
    staging_lights: Vec<PackedLight>,
}
impl EntityFrame {
    pub fn new(maximum: usize) -> Self {
        Self {
            maximum,
            instances: Vec::with_capacity(maximum),
            lights: Vec::with_capacity(maximum),
            staging_instances: Vec::with_capacity(maximum),
            staging_lights: Vec::with_capacity(maximum),
        }
    }
    pub fn instances(&self) -> &[EntityInstance] {
        &self.instances
    }
    pub fn lights(&self) -> &[PackedLight] {
        &self.lights
    }
    /// Source positions already subtract the integer camera origin in f64. On
    /// failure the previous complete frame stays available to the renderer.
    pub fn prepare(
        &mut self,
        source: &[rules::entities::mob::Instance],
        world: &World,
        space: SpaceId,
        origin: [i64; 3],
        items: &impl ItemVisuals,
    ) -> Result<EntityCopyWork, EntityFrameError> {
        self.prepare_sampled(source, space, origin, items, |position| {
            world
                .page(position.page())
                .map(|page| page.light(position.local_index()))
        })
    }
    /// Client authority adapters can withhold uncertified light while retaining
    /// the same bounded instance conversion and GPU ABI.
    pub fn prepare_sampled(
        &mut self,
        source: &[rules::entities::mob::Instance],
        space: SpaceId,
        origin: [i64; 3],
        items: &impl ItemVisuals,
        mut light_at: impl FnMut(BlockPos) -> Option<world::LightSample>,
    ) -> Result<EntityCopyWork, EntityFrameError> {
        if source.len() > self.maximum {
            return Err(EntityFrameError::Capacity);
        }
        self.staging_instances.clear();
        self.staging_lights.clear();
        let mut work = EntityCopyWork::default();
        for instance in source {
            if instance.kind > 7 {
                return Err(EntityFrameError::InvalidKind);
            }
            if instance.position.iter().any(|x| !x.is_finite()) {
                return Err(EntityFrameError::InvalidPosition);
            }
            let (kind, color) = if instance.kind >= 6 {
                let id = u16::try_from(instance.color)
                    .map_err(|_| EntityFrameError::MissingItemIcon(u16::MAX))?;
                let visual = items
                    .visual(id)
                    .ok_or(EntityFrameError::MissingItemIcon(id))?;
                if !(6..=7).contains(&visual.kind) {
                    return Err(EntityFrameError::InvalidKind);
                }
                (visual.kind, visual.atlas_layer)
            } else {
                (instance.kind, instance.color)
            };
            let mut output = EntityInstance {
                position: instance.position,
                body_yaw: instance.body_yaw,
                head_yaw: instance.head_yaw,
                pitch: instance.pitch,
                limb_phase: instance.limb_phase,
                limb_amount: instance.limb_amount,
                kind,
                flags: instance.flags,
                color,
                id_low: instance.id_low,
                id_high: instance.id_high,
                death_roll: instance.death_roll,
                scale: instance.scale,
                reserved: instance.reserved,
            };
            if kind == 6 {
                let item = u16::try_from(instance.color)
                    .map_err(|_| EntityFrameError::MissingItemIcon(u16::MAX))?;
                if let Some(layers) = items.block_layers(item) {
                    output
                        .set_block_layers(layers)
                        .map_err(|_| EntityFrameError::InvalidKind)?;
                }
            }
            self.staging_instances.push(output);
            let eye = match kind {
                0 => 0.75,
                1 => 1.25,
                2 => 1.125,
                3 => 0.625,
                4 | 5 => 1.625,
                _ => 0.25,
            };
            let position = std::array::from_fn::<_, 3, _>(|axis| {
                origin[axis] as f64
                    + f64::from(instance.position[axis])
                    + if axis == 1 {
                        eye * f64::from(instance.scale)
                    } else {
                        0.
                    }
            });
            work.light_reads += 1;
            let light = if position.iter().any(|x| {
                !x.is_finite() || *x < f64::from(i32::MIN) || *x >= f64::from(i32::MAX) + 1.
            }) {
                None
            } else {
                let position = BlockPos {
                    space,
                    x: position[0].floor() as i32,
                    y: position[1].floor() as i32,
                    z: position[2].floor() as i32,
                };
                light_at(position)
            };
            self.staging_lights.push(if let Some(light) = light {
                PackedLight::new(light.sky, light.block, light.rgb)
                    .expect("world light levels valid")
            } else {
                work.unknown_light_reads += 1;
                PackedLight::default()
            });
            work.instances += 1;
        }
        std::mem::swap(&mut self.instances, &mut self.staging_instances);
        std::mem::swap(&mut self.lights, &mut self.staging_lights);
        Ok(work)
    }
}
/// Block items reuse the terrain texture array. Nonblock icons are supplied by
/// the local UI/resource atlas rather than being substituted with stone.
pub struct RegistryItemVisuals<'a> {
    pub icon_layer: &'a dyn Fn(rules::items::ItemId) -> Option<u32>,
}
impl ItemVisuals for RegistryItemVisuals<'_> {
    fn visual(&self, item: rules::items::ItemId) -> Option<ItemVisual> {
        match rules::item(item)?.kind {
            rules::items::ItemKind::Block(block) => Some(ItemVisual {
                kind: 6,
                atlas_layer: rules::blocks::layer_cell(block, 0, rules::blocks::mesh_face(1)).0,
            }),
            _ => Some(ItemVisual {
                kind: 7,
                atlas_layer: (self.icon_layer)(item)?,
            }),
        }
    }
    fn block_layers(&self, item: rules::items::ItemId) -> Option<[u16; 6]> {
        let rules::items::ItemKind::Block(block) = rules::item(item)?.kind else {
            return None;
        };
        let layers = std::array::from_fn(|face| {
            rules::blocks::layer_cell(block, 0, rules::blocks::mesh_face(face as u8)).0
        });
        Some(layers.map(|layer| layer as u16))
    }
}
/// A single 64-byte local human replaces six terrain-page avatar objects. First
/// person hides its primary view while retaining a separate dynamic shadow.
/// Positions subtract the integer render origin before narrowing to f32.
pub fn local_player_instance(
    player: &sim::Player,
    alpha: f64,
    mode: sim::CameraMode,
    origin: [i64; 3],
    id: u64,
) -> Option<EntityInstance> {
    if player.mode == sim::GameMode::Spectator
        || player
            .pos
            .iter()
            .chain(player.prev.iter())
            .any(|x| !x.is_finite() || *x < f64::from(i32::MIN) || *x > f64::from(i32::MAX))
        || !player.yaw.is_finite()
        || !player.pitch.is_finite()
    {
        return None;
    }
    let alpha = if alpha.is_finite() {
        alpha.clamp(0., 1.)
    } else {
        0.
    };
    let pose = player.self_body.pose(alpha);
    if pose.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let position = std::array::from_fn(|axis| {
        ((player.prev[axis] + (player.pos[axis] - player.prev[axis]) * alpha) - origin[axis] as f64)
            as f32
    });
    let swing_delta = (player.swing - player.last_swing).rem_euclid(1.);
    let swing = (player.last_swing + swing_delta * alpha as f32).rem_euclid(1.);
    let attack = if swing.is_finite() {
        libm::sinf(swing * std::f32::consts::PI) * 1.5
    } else {
        0.
    };
    Some(EntityInstance {
        position,
        body_yaw: pose[0],
        head_yaw: player.yaw,
        pitch: player.pitch.clamp(-90., 90.),
        limb_phase: pose[1],
        limb_amount: pose[2],
        kind: 5,
        flags: if mode == sim::CameraMode::FirstPerson {
            render::entity_gpu::PRIMARY_HIDDEN
        } else {
            0
        } | if player.crouch {
            render::entity_gpu::CROUCH
        } else {
            0
        },
        color: pose[3].to_bits(),
        id_low: id as u32,
        id_high: (id >> 32) as u32,
        death_roll: 0.,
        scale: 1.,
        reserved: attack,
    })
}
/// One known-page light read. Unknown remains None, without generation or any
/// mutation. The origin is restored in f64 before flooring the local position.
pub fn instance_light(
    world: &World,
    space: SpaceId,
    origin: [i64; 3],
    instance: &EntityInstance,
) -> Option<PackedLight> {
    instance_light_sampled(space, origin, instance, |position| {
        world
            .page(position.page())
            .map(|page| page.light(position.local_index()))
    })
}
pub fn instance_light_sampled(
    space: SpaceId,
    origin: [i64; 3],
    instance: &EntityInstance,
    mut light_at: impl FnMut(BlockPos) -> Option<world::LightSample>,
) -> Option<PackedLight> {
    let eye = match instance.kind {
        0 => 0.75,
        1 => 1.25,
        2 => 1.125,
        3 => 0.625,
        4 | 5 => {
            if instance.flags & render::entity_gpu::CROUCH != 0 {
                1.35
            } else {
                1.625
            }
        }
        _ => 0.25,
    };
    let p = std::array::from_fn::<_, 3, _>(|axis| {
        origin[axis] as f64
            + f64::from(instance.position[axis])
            + if axis == 1 {
                eye * f64::from(instance.scale)
            } else {
                0.
            }
    });
    if p.iter()
        .any(|v| !v.is_finite() || *v < f64::from(i32::MIN) || *v >= f64::from(i32::MAX) + 1.)
    {
        return None;
    }
    let p = BlockPos {
        space,
        x: p[0].floor() as i32,
        y: p[1].floor() as i32,
        z: p[2].floor() as i32,
    };
    let light = light_at(p)?;
    PackedLight::new(light.sky, light.block, light.rgb)
}
