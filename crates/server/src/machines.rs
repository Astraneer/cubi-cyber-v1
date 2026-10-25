//! Single-owner machine phases. World components, reservoirs, scheduling and
//! routed items are planned together; publication belongs to the WAL owner.
#[path = "machines/codec.rs"]
mod codec;
#[path = "machines/fixture.rs"]
mod fixture;
#[path = "machines/owner.rs"]
mod owner;
#[path = "machines/phase.rs"]
mod phase;
#[path = "machines/planner.rs"]
mod planner;
#[path = "machines/registration.rs"]
mod registration;
#[path = "machines/topology.rs"]
mod topology;
#[path = "machines/view.rs"]
mod view;

use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick, Version};
use rules::{
    arms,
    blocks::{self, BeKind, MekKind},
    ids::*,
    machine_recipes as recipes,
    machines::{
        self, Face, Machine, MachineWorld,
        grid::{
            Grid, GridError, GridSnapshot, GridWork, NetworkId, PhaseScratch, Reservoir, Resource,
        },
        sides,
    },
    stack::ItemStack,
    survival::furnace::Chest,
};
use std::collections::{BTreeMap, BTreeSet};
use world::{CellRead, ComponentData, Transaction, World};

pub const MACHINE_COMPONENT: u16 = 0x8201;
const CHANNELS: ChannelMask = ChannelMask(
    ChannelMask::CONTENTS.0
        | ChannelMask::COLLISION.0
        | ChannelMask::CONNECTIONS.0
        | ChannelMask::FLUID.0
        | ChannelMask::OPACITY.0
        | ChannelMask::LIGHTING.0,
);

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub machines: usize,
    pub transmitters: usize,
    pub endpoints: usize,
    pub packets: usize,
    pub events: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            machines: 16_384,
            transmitters: 65_536,
            endpoints: 4096,
            packets: 16_384,
            events: 131_072,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub events: usize,
    pub reads: u64,
    pub bytes: u64,
    pub endpoints: usize,
    pub route_nodes: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            events: 2048,
            reads: 131_072,
            bytes: 32 * 1024 * 1024,
            endpoints: 16_384,
            route_nodes: 4096,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub events: u64,
    pub machines: u64,
    pub networks: u64,
    pub reads: u64,
    pub bytes: u64,
    pub endpoint_queries: u64,
    pub transfers: u64,
    pub unknown: u64,
    pub deferred: u64,
    pub oldest_age: u64,
    pub recipe_ticks: u64,
    pub analytic_transitions: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineError {
    Bounds,
    Unknown,
    Missing,
    Component,
    Stale,
    Encoding,
    Overflow,
    Grid(GridError),
}
impl std::fmt::Display for MachineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "machine phase failed: {self:?}")
    }
}
impl std::error::Error for MachineError {}
impl From<GridError> for MachineError {
    fn from(error: GridError) -> Self {
        Self::Grid(error)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NetworkKey {
    pub space: SpaceId,
    pub network: NetworkId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Event {
    Machine(ComponentId),
    Network(NetworkKey),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    pub event: Event,
    pub due: Tick,
    pub first_due: Tick,
    pub order: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineMeta {
    pub id: ComponentId,
    pub anchor: BlockPos,
    pub order: u64,
    pub last_tick: Tick,
    pub active: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaPatch {
    pub before: Option<MachineMeta>,
    pub after: Option<MachineMeta>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuePatch {
    pub before: Option<Pending>,
    pub after: Option<Pending>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservoirPatch {
    pub key: NetworkKey,
    pub before: Reservoir,
    pub after: Reservoir,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransmitterConfig {
    pub position: BlockPos,
    pub block: u16,
    pub modes: u16,
    pub color: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigPatch {
    pub before: Option<TransmitterConfig>,
    pub after: Option<TransmitterConfig>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkMeta {
    pub key: NetworkKey,
    pub order: u64,
    pub producers: Vec<ComponentId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPatch {
    pub before: Option<NetworkMeta>,
    pub after: Option<NetworkMeta>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointPatch {
    pub key: NetworkKey,
    pub before: Vec<machines::grid::Endpoint>,
    pub after: Vec<machines::grid::Endpoint>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyPatch {
    pub space: SpaceId,
    pub records: machines::grid::TopologyRecords,
}
#[derive(Clone, Debug)]
pub struct RuntimeDelta {
    revision: u64,
    next_revision: u64,
    next_order_before: u64,
    next_order_after: u64,
    pub machines: Vec<MetaPatch>,
    pub queue: Vec<QueuePatch>,
    pub reservoirs: Vec<ReservoirPatch>,
    pub topology: Vec<TopologyPatch>,
    pub networks: Vec<NetworkPatch>,
    pub endpoints: Vec<EndpointPatch>,
    pub configs: Vec<ConfigPatch>,
}
#[must_use = "Commit transactions and RuntimeDelta in the same WAL batch before publishing"]
pub struct PhasePlan {
    pub transactions: Vec<Transaction>,
    pub delta: RuntimeDelta,
    pub inputs: Vec<jobs::InputVersion>,
    pub fluid_wakes: Vec<BlockPos>,
    pub work: Work,
}
pub struct MachineRuntime {
    config: Config,
    revision: u64,
    next_order: u64,
    machines: BTreeMap<ComponentId, MachineMeta>,
    positions: BTreeMap<BlockPos, ComponentId>,
    events: BTreeMap<(Tick, u8, u64), Pending>,
    keys: BTreeMap<Event, (Tick, u8, u64)>,
    grids: BTreeMap<SpaceId, Grid>,
    network_orders: BTreeMap<NetworkKey, u64>,
    producers: BTreeMap<NetworkKey, Vec<ComponentId>>,
    configs: BTreeMap<BlockPos, TransmitterConfig>,
    phase_scratch: PhaseScratch,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub revision: u64,
    pub next_order: u64,
    pub machines: Vec<MachineMeta>,
    pub events: Vec<Pending>,
    pub grids: Vec<(SpaceId, GridSnapshot)>,
    pub network_orders: Vec<(NetworkKey, u64)>,
    pub producers: Vec<(NetworkKey, Vec<ComponentId>)>,
    pub configs: Vec<TransmitterConfig>,
}
#[derive(Clone, Copy)]
struct ReadOwner<'a> {
    machines: &'a BTreeMap<ComponentId, MachineMeta>,
    positions: &'a BTreeMap<BlockPos, ComponentId>,
    grids: &'a BTreeMap<SpaceId, Grid>,
}
#[derive(Clone)]
struct MachineChange {
    version: Version,
    before: Machine,
    after: Machine,
}
#[derive(Clone, Copy)]
struct ChestChange {
    version: Version,
    before: Chest,
    after: Chest,
    anchor: BlockPos,
}
#[derive(Default)]
struct Overlay {
    machines: BTreeMap<ComponentId, MachineChange>,
    chests: BTreeMap<ComponentId, ChestChange>,
    cells: BTreeMap<BlockPos, (Cell, Cell)>,
    reservoirs: BTreeMap<NetworkKey, (Reservoir, Reservoir)>,
    inputs: BTreeMap<PageId, Option<Version>>,
}
pub fn machine_component(anchor: BlockPos, machine: &Machine) -> ComponentData {
    let mut bytes = [0; machines::MACHINE_BYTES];
    let used = machine.encode(&mut bytes);
    ComponentData {
        anchor,
        kind: MACHINE_COMPONENT,
        payload: bytes[..used].to_vec(),
    }
}
pub fn read_machine(world: &World, id: ComponentId) -> Result<Machine, MachineError> {
    let record = world.component(id).ok_or(MachineError::Missing)?;
    if record.data.kind != MACHINE_COMPONENT {
        return Err(MachineError::Component);
    }
    Machine::decode(&record.data.payload).map_err(|_| MachineError::Encoding)
}
fn phase(event: Event) -> u8 {
    match event {
        Event::Machine(_) => 0,
        Event::Network(_) => 1,
    }
}
fn queue_key(p: Pending) -> (Tick, u8, u64) {
    (p.due, phase(p.event), p.order)
}
fn validate_config(config: TransmitterConfig) -> Result<(), MachineError> {
    if ![
        UNIVERSAL_CABLE,
        MECHANICAL_PIPE,
        PRESSURIZED_TUBE,
        LOGISTICAL_TRANSPORTER,
    ]
    .contains(&config.block)
        || config.modes & !0xfff != 0
        || config.color > 18
    {
        return Err(MachineError::Encoding);
    }
    Ok(())
}
fn offset(p: BlockPos, face: Face) -> Result<BlockPos, MachineError> {
    let q = sides::offset([p.x, p.y, p.z], face).ok_or(MachineError::Overflow)?;
    Ok(BlockPos {
        space: p.space,
        x: q[0],
        y: q[1],
        z: q[2],
    })
}
