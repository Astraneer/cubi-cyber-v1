//! Checksummed snapshots and a durable append-only transaction/action journal.
#![deny(unsafe_op_in_unsafe_fn)]

mod codec;
mod disk;
mod worker;
pub use codec::{decode_batch, decode_snapshot, encode_batch, encode_snapshot};
pub use disk::{Recovery, Store};
pub use worker::{Completion, Durability, SaveWorker, SnapshotMetadata, WriteRequest};

use core_types::Tick;
use net::{ActionReply, PlayerId, PlayerState, Role};
use world::{WorldChange, persistence::RestoreState};

pub const FORMAT_VERSION: u16 = 1;
pub const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_WAL_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_IDENTITIES: usize = 10_000;
pub const MAX_RECENT_ACTIONS: usize = 256;

#[derive(Clone, PartialEq)]
pub struct Identity {
    pub id: PlayerId,
    pub name: String,
    pub credential_hash: [u8; 32],
    pub role: Role,
    pub banned: bool,
}
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("credential_hash", &"[redacted]")
            .field("role", &self.role)
            .field("banned", &self.banned)
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
pub struct DedupState {
    pub contiguous: u64,
    pub highest_seen: u64,
    pub replies: Vec<ActionReply>,
}

#[derive(Clone, Debug)]
pub struct SavedPlayer {
    pub state: PlayerState,
    pub place_ready_at: Tick,
    pub break_ready_at: Tick,
    /// Bounded optional extension. An absent M6 payload means an empty inventory.
    pub inventory: Vec<u8>,
    /// Versioned server-only simulation continuation, never put into net frames.
    pub simulation: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Batch {
    pub tick: Tick,
    pub changes: Vec<WorldChange>,
    pub identities: Vec<Identity>,
    pub players: Vec<SavedPlayer>,
    pub actions: Vec<(PlayerId, ActionReply)>,
}
impl Batch {
    pub fn empty(tick: Tick) -> Self {
        Self {
            tick,
            changes: Vec::new(),
            identities: Vec::new(),
            players: Vec::new(),
            actions: Vec::new(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.identities.is_empty()
            && self.players.is_empty()
            && self.actions.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub record_sequence: u64,
    pub server_tick: Tick,
    pub public_seed: u32,
    pub generator_version: u32,
    pub rules_version: u32,
    pub world: RestoreState,
    pub identities: Vec<Identity>,
    pub players: Vec<SavedPlayer>,
    pub dedup: Vec<(PlayerId, DedupState)>,
}

#[derive(Debug)]
pub enum SaveError {
    Io(std::io::Error),
    Invalid(&'static str),
    Bounds(&'static str),
    UnsupportedVersion(u16),
    Checksum,
    AlreadyOpen,
    Poisoned,
    MissingBase,
}
impl From<std::io::Error> for SaveError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "save operation failed: {self:?}")
    }
}
impl std::error::Error for SaveError {}

/// Rule/generator changes require an explicit caller-supplied migration. The
/// current first schema never silently opens an incompatible save as a new world.
pub trait Migration {
    fn migrate(
        &self,
        snapshot: Snapshot,
        generator_version: u32,
        rules_version: u32,
    ) -> Result<Snapshot, SaveError>;
}

pub fn require_versions(
    snapshot: Snapshot,
    generator_version: u32,
    rules_version: u32,
    migration: Option<&dyn Migration>,
) -> Result<Snapshot, SaveError> {
    if snapshot.generator_version == generator_version && snapshot.rules_version == rules_version {
        return Ok(snapshot);
    }
    let migrated = migration
        .ok_or(SaveError::Invalid(
            "explicit rules/generator migration required",
        ))?
        .migrate(snapshot, generator_version, rules_version)?;
    if migrated.generator_version != generator_version || migrated.rules_version != rules_version {
        return Err(SaveError::Invalid(
            "migration did not produce requested versions",
        ));
    }
    Ok(migrated)
}
