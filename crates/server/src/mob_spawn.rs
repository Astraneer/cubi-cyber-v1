//! Natural-spawn planners over immutable recovered pages. Background column
//! formulas never generate cells; Unknown and budget exhaustion defer the whole
//! request without publishing candidates or advancing the owner's private RNG.
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use rules::{
    blocks,
    entities::{EntityId, MobKind, mob::Mob},
    shapes::Aabb,
};
#[path = "mob_spawn/snapshot.rs"]
mod snapshot;
pub use snapshot::{Snapshot, Task, Workers};
#[path = "mob_spawn/plan.rs"]
mod plan;
pub const HERD_COLUMNS_PER_TICK: usize = 6;
pub const HERD_RADIUS: i32 = 8;
pub const MAX_CANDIDATES: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Biome {
    Plains,
    Forest,
    Mountain,
    Birch,
    Taiga,
    Snowy,
    Savanna,
    Other,
}
impl Biome {
    pub fn from_public_name(name: &str) -> Self {
        match name {
            "Plains" | "Flower meadow" => Self::Plains,
            "Forest" | "Swamp" | "Sparse woodland" | "Old growth forest" => Self::Forest,
            "Mountains" => Self::Mountain,
            "Birch forest" | "Old growth birch forest" => Self::Birch,
            "Taiga" | "Sparse taiga" | "Old growth taiga" => Self::Taiga,
            "Snowy plains" => Self::Snowy,
            "Savanna" => Self::Savanna,
            _ => Self::Other,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HerdTable {
    pub weights: [u16; 4],
    pub empty: u16,
    pub chance: f32,
}
pub fn herd_table(biome: Biome) -> Option<HerdTable> {
    let empty = match biome {
        Biome::Plains => 6,
        Biome::Forest | Biome::Mountain => 5,
        Biome::Birch => 0,
        Biome::Taiga | Biome::Snowy => 20,
        Biome::Savanna => 12,
        Biome::Other => return None,
    };
    Some(HerdTable {
        weights: [12, 10, 10, 8],
        empty,
        chance: 0.1,
    })
}
pub fn monster_cap(zone_columns: usize) -> usize {
    zone_columns.saturating_mul(70) / 289
}
pub fn ambient_darkness(day: u64) -> u8 {
    let d = (day as f64 / 24000. - 0.25).rem_euclid(1.);
    let e = 0.5 - libm::cos(d * std::f64::consts::PI) / 2.;
    let angle = ((d * 2. + e) / 3.) as f32;
    let c = f64::from(libm::cosf(angle * std::f32::consts::TAU));
    ((1. - (0.5 + 2. * c.clamp(-0.25, 0.25))) * 11.) as u8
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub height: i32,
    pub biome: Biome,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Light {
    pub sky: u8,
    pub block: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Unknown,
    Budget,
    Encoding,
}
pub trait SpawnWorld {
    fn cell(&mut self, p: BlockPos) -> Result<Cell, Error>;
    fn column(&mut self, x: i32, z: i32) -> Result<Column, Error>;
    fn light(&mut self, p: BlockPos) -> Result<Light, Error>;
    fn protected(&self, p: BlockPos) -> bool;
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Player {
    pub space: SpaceId,
    pub position: [f64; 3],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Herd,
    Monster,
}
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub kind: Kind,
    pub space: SpaceId,
    pub column: [i32; 2],
    pub tick: Tick,
    pub map_seed: u64,
    pub rng_before: u64,
    pub available: usize,
    pub day: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub cells: u64,
    pub columns: u64,
    pub lights: u64,
    pub min_y: i32,
    pub max_y_exclusive: i32,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            cells: 8192,
            columns: 512,
            lights: 64,
            min_y: 1,
            max_y_exclusive: r#gen::PROFILE_HEIGHT - 2,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub cell_reads: u64,
    pub column_queries: u64,
    pub light_reads: u64,
    pub attempts: u64,
    pub candidates: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Complete,
    DeferredUnknown,
    DeferredBudget,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub kind: MobKind,
    pub position: [f64; 3],
    pub baby: bool,
    pub yaw: f32,
    pub color: u8,
}
impl Candidate {
    pub fn instantiate(
        self,
        id: EntityId,
        space: SpaceId,
        entity_seed: u64,
        now: Tick,
    ) -> Result<Mob, rules::entities::EntityError> {
        let mut mob = Mob::spawned(id, self.kind, space, self.position, entity_seed)?;
        mob.baby = self.baby;
        mob.color = self.color;
        mob.body.yaw = self.yaw;
        mob.body.head = self.yaw;
        mob.render_yaw = self.yaw;
        mob.body_last_head = self.yaw;
        mob.last_tick = now;
        Ok(mob)
    }
}
pub struct Plan {
    pub kind: Kind,
    pub space: SpaceId,
    pub column: [i32; 2],
    pub tick: Tick,
    pub rng_before: u64,
    pub rng_after: u64,
    pub status: Status,
    pub candidates: Vec<Candidate>,
    pub inputs: Vec<jobs::InputVersion>,
    pub work: Work,
}
pub fn plan(
    world: &mut impl SpawnWorld,
    request: Request,
    players: &[Player],
    budget: Budget,
) -> Result<Plan, Error> {
    if request.available > 5000
        || players.len() > 32
        || request
            .column
            .iter()
            .any(|&v| v.checked_mul(16).and_then(|x| x.checked_add(32)).is_none())
        || budget.min_y >= budget.max_y_exclusive
        || budget.columns > 4096
        || budget.cells > 131072
        || budget.lights > 1024
        || players.iter().any(|p| {
            p.position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 30_000_000.)
        })
    {
        return Err(Error::Bounds);
    }
    let mut ctx = plan::Context {
        world,
        request,
        budget,
        work: Work::default(),
    };
    let mut rng = plan::Rng(request.rng_before);
    let mut candidates = Vec::new();
    let result = match request.kind {
        Kind::Herd => ctx.herd(&mut rng, &mut candidates),
        Kind::Monster => ctx.monsters(&mut rng, &mut candidates, players),
    };
    let status = match result {
        Ok(()) => Status::Complete,
        Err(Error::Unknown) => Status::DeferredUnknown,
        Err(Error::Budget) => Status::DeferredBudget,
        Err(error) => return Err(error),
    };
    if status != Status::Complete {
        candidates.clear();
        rng.0 = request.rng_before;
    }
    Ok(Plan {
        kind: request.kind,
        space: request.space,
        column: request.column,
        tick: request.tick,
        rng_before: request.rng_before,
        rng_after: rng.0,
        status,
        candidates,
        inputs: Vec::new(),
        work: ctx.work,
    })
}
