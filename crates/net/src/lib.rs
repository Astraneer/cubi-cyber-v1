//! Bounded protocol, confirmed-base replication and prediction bookkeeping.
#![forbid(unsafe_code)]

mod codec;
pub mod commands;
pub mod emulator;
pub mod entities;
pub mod geometry;
pub mod height;
pub mod item_attributes;
pub mod light;
pub mod machine_packets;
pub mod presence;
pub mod props;
pub mod window;
pub use geometry::{PublicGeometry, PublicSurface, PublicSurfaceKind};
mod replication;
pub mod udp;
pub use codec::{CodecError, decode_client, decode_server, encode_client, encode_server};
pub use commands::{CommandInfo, command_catalog, commands_for};
pub use replication::*;

use core_types::{
    BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, SpaceTransform, Tick, TxId, Version,
};

pub const PROTOCOL_VERSION: u16 = 1;
pub const RULES_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 262_144;
pub const MAX_INPUT_HISTORY: usize = 8;
pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_COMMAND_BYTES: usize = 256;
pub const MAX_PREDICTION_BYTES: usize = 384;
pub const MAX_COORDINATE: i32 = 30_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SecretToken([u8; 32]);
impl SecretToken {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl std::fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretToken([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlayerId(pub [u8; 16]);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    #[default]
    Player = 0,
    Admin = 1,
    Owner = 2,
}
impl Role {
    pub const fn can_admin(self) -> bool {
        matches!(self, Self::Admin | Self::Owner)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    pub identity: PlayerId,
    pub name: String,
    pub credential: Option<SecretToken>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputFrame {
    pub sequence: u64,
    /// Explicit authority tick for deterministic prebuffered prediction. Zero
    /// requests the earliest available tick and permits corrective replay.
    pub target_tick: Tick,
    pub keys: u8,
    pub yaw: f32,
    pub pitch: f32,
}
impl InputFrame {
    pub const FORWARD: u8 = 1;
    pub const BACK: u8 = 2;
    pub const LEFT: u8 = 4;
    pub const RIGHT: u8 = 8;
    pub const JUMP: u8 = 16;
    pub const SNEAK: u8 = 32;
    pub const SPRINT: u8 = 64;
    pub const ATTACK: u8 = 128;
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerState {
    pub identity: PlayerId,
    pub space: SpaceId,
    pub input_sequence: u64,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub health: f32,
    pub mode: u8,
    pub on_ground: bool,
    pub flying: bool,
    pub noclip: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Welcome {
    pub session: u64,
    pub identity: PlayerId,
    pub new_credential: Option<SecretToken>,
    pub role: Role,
    pub tick: Tick,
    pub next_action: ActionId,
    pub public_seed: u32,
    pub generator_version: u32,
    pub rules_version: u32,
    pub player: PlayerState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryGuard {
    pub revision: u64,
    pub selected: u8,
    pub item: u16,
    pub count: u16,
    pub damage: u16,
    pub payload: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Mine {
        position: BlockPos,
        expected: Cell,
        tool: u16,
    },
    StopMining,
    Place {
        position: BlockPos,
        expected: Cell,
        cell: Cell,
    },
    Flight(bool),
    DebugNoclip(bool),
    Command(String),
    Chat(String),
    Window(window::WindowIntent),
    SelectSlot(u8),
    CycleToolMode {
        back: bool,
        inventory: InventoryGuard,
    },
    PlaceOwned {
        position: BlockPos,
        expected: Cell,
        cell: Cell,
        inventory: InventoryGuard,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    Hello(Hello),
    Inputs(Vec<InputFrame>),
    Action {
        id: ActionId,
        action: Action,
    },
    /// Camera orientation attached to the same idempotent action. Position and
    /// velocity remain server-owned; delayed movement frames cannot stale aim.
    AimedAction {
        id: ActionId,
        action: Action,
        yaw: f32,
        pitch: f32,
    },
    RequestPage {
        page: PageId,
        acknowledged: Option<u64>,
    },
    AckPage {
        page: PageId,
        version: u64,
    },
    RequestSummary {
        page: PageId,
    },
    Ping(u64),
    RequestGeometry {
        page: PageId,
        known: Option<Version>,
    },
    ForgetGeometry(PageId),
    RequestHeight {
        key: height::HeightKey,
        known: Option<u64>,
    },
    /// Sticky exact public-geometry representation for a level-zero leaf.
    RequestExactHeight {
        key: height::HeightKey,
        known: Option<u64>,
    },
    ForgetHeight(height::HeightKey),
    RequestProps {
        page: PageId,
        cell_view: u64,
        known: Option<u64>,
    },
    MachinePackets(machine_packets::Request),
    RequestLight(light::Request),
    ForgetLight(PageId),
    Presence(presence::PresenceClientEvent),
    Entities(entities::EntityRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RejectReason {
    Protocol = 0,
    Unauthorized = 1,
    Banned = 2,
    Capacity = 3,
    Unknown = 4,
    OutOfReach = 5,
    Occluded = 6,
    Protected = 7,
    Unbreakable = 8,
    Occupied = 9,
    Stale = 10,
    InvalidCell = 11,
    RateLimited = 12,
    DebugRequired = 13,
    Unsupported = 14,
    Canceled = 15,
    AlreadyFinal = 16,
    StorageUnavailable = 17,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionStatus {
    Pending,
    Accepted,
    Rejected(RejectReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionReply {
    pub id: ActionId,
    pub status: ActionStatus,
    pub transaction: Option<TxId>,
    pub source_version: Version,
    pub position: Option<BlockPos>,
    pub cell: Option<Cell>,
}

/// View revision is per peer, while source_version identifies authoritative
/// inputs. Revealing an ore can change a peer's view without modifying that ore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageSnapshot {
    pub page: PageId,
    pub version: u64,
    pub source_version: Version,
    pub cells: Box<[Cell; PAGE_VOLUME]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageDelta {
    pub page: PageId,
    pub base: u64,
    pub version: u64,
    pub source_version: Version,
    pub changes: Vec<(u16, Cell)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageUpdate {
    Snapshot(PageSnapshot),
    Delta(PageDelta),
}

/// A 4³ occupancy/material summary of the actual sanitized world, not a fresh
/// generator approximation. Each representative corresponds to a 4³ cell group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FarSummary {
    pub page: PageId,
    pub source_version: Version,
    pub occupied: u64,
    pub materials: [u16; 64],
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerMessage {
    Welcome(Welcome),
    State {
        tick: Tick,
        player: PlayerState,
    },
    /// Exact public simulation continuation for this owner. It excludes private
    /// authority grants, credentials, inventory and generation metadata.
    Prediction {
        tick: Tick,
        player: PlayerState,
        continuation: Vec<u8>,
    },
    Inventory {
        data: Vec<u8>,
    },
    ItemAttributes(Box<item_attributes::InventoryAttributes>),
    Geometry(Box<PublicGeometry>),
    GeometryUnavailable(PageId),
    Presence(presence::PresenceServerEvent),
    Entities(Box<entities::EntityFrame>),
    Window(Box<window::WindowSnapshot>),
    Height(Box<height::HeightTile>),
    HeightUnavailable(height::HeightKey),
    Props(Box<props::Frame>),
    MachinePackets(Box<machine_packets::Frame>),
    Light(Box<light::Update>),
    LightUnavailable(PageId),
    Players {
        tick: Tick,
        players: Vec<PlayerState>,
    },
    Page(PageUpdate),
    PageUnavailable(PageId),
    Action(ActionReply),
    Transform {
        space: SpaceId,
        version: Version,
        transform: SpaceTransform,
    },
    Summary(FarSummary),
    Pong(u64),
    Error(RejectReason),
}

pub fn valid_position(position: [f64; 3]) -> bool {
    position
        .iter()
        .all(|value| value.is_finite() && value.abs() < f64::from(MAX_COORDINATE))
}
