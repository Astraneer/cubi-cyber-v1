//! Preallocated bridge from an explicit immutable visible-model snapshot to the
//! shared static GPU catalog. No World scan, vertices, I/O or generation occurs.
use core_types::{BlockPos, Cell};
use render::block_models::{ACTIVE, BlockInstance, kind};
use rules::{
    Model, arms,
    blocks::{self, Render, ST_LIT},
    ids::*,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Animation {
    pub chest_open: f32,
    pub rotor_radians: f32,
    pub active: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockSample {
    pub position: BlockPos,
    pub cell: Cell,
    /// Already sampled RGB/sky light, using the shared M4 nibble layout.
    pub light: u32,
    /// RGBA8, low byte red; normally u32::MAX except biome-tinted plants.
    pub tint: u32,
    pub animation: Animation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelError {
    Capacity,
    UnknownBlock(u16),
    InvalidAnimation,
    InvalidPosition,
    Layer,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub sources: u64,
    pub instances: u64,
    pub skipped: u64,
    pub cpu_vertices: u64,
}
pub struct BlockModelFrame {
    instances: Vec<BlockInstance>,
    staging: Vec<BlockInstance>,
    maximum: usize,
}
impl BlockModelFrame {
    pub fn new(maximum: usize) -> Self {
        Self {
            instances: Vec::with_capacity(maximum),
            staging: Vec::with_capacity(maximum),
            maximum,
        }
    }
    pub fn instances(&self) -> &[BlockInstance] {
        &self.instances
    }
    /// Errors preserve the previous complete frame; both output buffers remain
    /// allocated. Inputs are selected/updated by the scene owner outside here.
    pub fn prepare(
        &mut self,
        samples: &[BlockSample],
        origin: [i64; 3],
    ) -> Result<Work, ModelError> {
        if samples.len() > self.maximum {
            return Err(ModelError::Capacity);
        }
        self.staging.clear();
        let mut work = Work::default();
        for sample in samples {
            work.sources += 1;
            if let Some(instance) = instance(*sample, origin)? {
                self.staging.push(instance);
                work.instances += 1;
            } else {
                work.skipped += 1;
            }
        }
        std::mem::swap(&mut self.instances, &mut self.staging);
        Ok(work)
    }
}
pub fn instance(
    sample: BlockSample,
    origin: [i64; 3],
) -> Result<Option<BlockInstance>, ModelError> {
    build(sample, origin, false)
}
/// Explicit catalog takeover for cube machines. The scene must exclude the same
/// cells from its terrain mesh before drawing these records. The default bridge
/// preserves the registry's existing cube draw path.
pub fn cube_machine_instance(
    sample: BlockSample,
    origin: [i64; 3],
) -> Result<Option<BlockInstance>, ModelError> {
    build(sample, origin, true)
}
fn build(
    sample: BlockSample,
    origin: [i64; 3],
    cube_override: bool,
) -> Result<Option<BlockInstance>, ModelError> {
    if origin
        .iter()
        .any(|value| !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(value))
    {
        return Err(ModelError::InvalidPosition);
    }
    let cell = sample.cell;
    let definition = rules::block(cell.id()).ok_or(ModelError::UnknownBlock(cell.id()))?;
    if !sample.animation.chest_open.is_finite()
        || !(0.0..=1.0).contains(&sample.animation.chest_open)
        || !sample.animation.rotor_radians.is_finite()
    {
        return Err(ModelError::InvalidAnimation);
    }
    let model = if cube_override {
        match cell.id() {
            ENRICHMENT_CHAMBER | ENERGIZED_SMELTER | CRUSHER | PURIFICATION_CHAMBER => {
                kind::CUBE_MACHINE
            }
            ELECTRIC_PUMP => kind::PUMP,
            _ => return Ok(None),
        }
    } else {
        match definition.render {
            Render::Cross => kind::CROSS,
            Render::Model(Model::Chest) => kind::CHEST,
            Render::Model(Model::Ladder) => kind::LADDER,
            Render::Model(Model::Snow) => kind::SNOW,
            Render::Model(Model::WallTorch) => kind::WALL_TORCH,
            Render::Model(Model::Cable) => match cell.id() {
                UNIVERSAL_CABLE => kind::CABLE,
                PRESSURIZED_TUBE => kind::TUBE,
                MECHANICAL_PIPE => kind::PIPE,
                LOGISTICAL_TRANSPORTER => kind::TRANSPORTER,
                _ => return Err(ModelError::UnknownBlock(cell.id())),
            },
            Render::Model(Model::Mek) => match cell.id() {
                METALLURGIC_INFUSER => kind::INFUSER,
                ELECTROLYTIC_SEPARATOR => kind::SEPARATOR,
                HEAT_GENERATOR => kind::HEAT_GENERATOR,
                WIND_GENERATOR => kind::WIND_GENERATOR,
                ENERGY_CUBE => kind::ENERGY_CUBE,
                _ => kind::CUBE_MACHINE,
            },
            _ => return Ok(None),
        }
    };
    let [x, y, z] = [sample.position.x, sample.position.y, sample.position.z];
    let offset = blocks::plant_offset(cell.id(), x, z);
    let position = std::array::from_fn::<_, 3, _>(|axis| {
        ([x, y, z][axis] as f64 - origin[axis] as f64
            + if axis == 1 { 0.0 } else { 0.5 }
            + offset[axis]) as f32
    });
    if position.iter().any(|value| !value.is_finite()) {
        return Err(ModelError::InvalidPosition);
    }
    let directional = cube_override
        || matches!(
            definition.render,
            Render::Model(Model::Chest | Model::Ladder | Model::WallTorch | Model::Mek)
        );
    let yaw = if directional {
        match blocks::facing_of(cell.raw()) {
            3 => 0.0,
            4 => std::f32::consts::FRAC_PI_2,
            2 => std::f32::consts::PI,
            _ => 3.0 * std::f32::consts::FRAC_PI_2,
        }
    } else {
        0.0
    };
    let mut reserved = [0; 4];
    if model == kind::CUBE_MACHINE {
        // Texture directions refer to the unrotated static mesh, whose front
        // is South. Cell state still supplies the authoritative lit bit.
        let state = cell.state() & !7 | 3;
        for (pair, packed) in reserved[..3].iter_mut().enumerate() {
            let low = u16::try_from(
                blocks::layer_cell(cell.id(), state, blocks::mesh_face((pair * 2) as u8)).0,
            )
            .map_err(|_| ModelError::Layer)?;
            let high = u16::try_from(
                blocks::layer_cell(cell.id(), state, blocks::mesh_face((pair * 2 + 1) as u8)).0,
            )
            .map_err(|_| ModelError::Layer)?;
            *packed = u32::from(low) | u32::from(high) << 16;
        }
    }
    Ok(Some(BlockInstance {
        position,
        yaw,
        kind: model,
        flags: u32::from(sample.animation.active || cell.state() & ST_LIT != 0) * ACTIVE,
        connections: if matches!(definition.render, Render::Model(Model::Cable)) {
            u32::from(arms::arms_of(cell.state()))
        } else {
            0
        },
        light: sample.light,
        phase: if model == kind::CHEST {
            sample.animation.chest_open
        } else if model == kind::WIND_GENERATOR {
            sample.animation.rotor_radians
        } else {
            0.0
        },
        amount: if model == kind::SNOW {
            f32::from(blocks::snow_layers(cell.raw())) / 8.0
        } else {
            1.0
        },
        tint: if definition.tint != 0 {
            sample.tint
        } else {
            u32::MAX
        },
        layer: blocks::layer_cell(cell.id(), cell.state(), 0).0,
        reserved,
    }))
}
