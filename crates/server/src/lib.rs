//! Single-owner 20 Hz authority. Durable storage completions delimit publication:
//! accepted actions, movement acknowledgements and page images cannot overtake WAL.
#![forbid(unsafe_code)]

mod actor;
pub mod entities;
pub mod lod;
pub mod survival;
pub mod transport;
mod visibility;

use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, TxId, Version};
use net::{
    Action, ActionId, ActionReply, ActionStatus, ClientMessage, Hello, InputFrame, PlayerId,
    PlayerState, RejectReason, Role, SecretToken, ServerMessage, ViewHistory, Welcome,
};
use save::{
    Batch, DedupState, Identity, SaveWorker, SavedPlayer, Snapshot, SnapshotMetadata, Store,
    WriteRequest,
};
use sim::{GameMode, InteractionController, Keys, Permissions, Player, WorldQuery};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use world::{CellRead, JournalLimits, Transaction, World, WorldChange};

pub const TICK_MILLIS: u64 = 50;
pub const MAX_CONNECTIONS: usize = 64;
const INPUT_DELAY_MILLIS: u64 = 150;
const MAX_INPUTS: usize = 256;
const MAX_INBOX: usize = 512;
const MAX_ACTIONS_PER_TICK: usize = 128;
const MAX_AOI_PAGES: usize = 64;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    pub public_seed: u32,
    pub maximum_players: usize,
    pub server_debug: bool,
    pub spawn_override: Option<[f64; 3]>,
    pub spawn_protection_radius: i32,
    pub generation_enabled: bool,
}
impl ServerConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            public_seed: 1,
            maximum_players: 32,
            server_debug: false,
            spawn_override: None,
            spawn_protection_radius: 32,
            generation_enabled: true,
        }
    }
}
#[derive(Debug)]
pub enum ServerError {
    Io(std::io::Error),
    Storage(save::SaveError),
    Invalid(&'static str),
}
impl From<std::io::Error> for ServerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<save::SaveError> for ServerError {
    fn from(e: save::SaveError) -> Self {
        Self::Storage(e)
    }
}
impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server: {self:?}")
    }
}
impl std::error::Error for ServerError {}

#[derive(Clone, Debug)]
pub struct Outgoing {
    pub connection: u64,
    pub message: ServerMessage,
    pub reliable: bool,
}
impl Outgoing {
    fn reliable(connection: u64, message: ServerMessage) -> Self {
        Self {
            connection,
            message,
            reliable: true,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ServerCounters {
    pub ticks: u64,
    pub input_steps: u64,
    pub committed_transactions: u64,
    pub durable_batches: u64,
    pub storage_stalls: u64,
    pub rejected_messages: u64,
    pub page_images: u64,
    pub generation_submitted: u64,
    pub generation_published: u64,
    pub inbox_high_water: usize,
    pub view_cells: u64,
    pub visibility_segments: u64,
    pub summary_cells: u64,
    pub rejected_transactions: u64,
}
struct MiningAction {
    id: ActionId,
    position: BlockPos,
    expected: Cell,
    held: rules::stack::ItemStack,
}
struct Peer {
    identity: PlayerId,
    credential: SecretToken,
    session: u64,
    ready: bool,
    welcome: Welcome,
    player: Player,
    inventory: rules::survival::Inventory,
    inventory_sent: Option<u64>,
    space: SpaceId,
    input_sequence: u64,
    inputs: BTreeMap<u64, InputFrame>,
    input_ready_at: Option<u64>,
    last_keys: u8,
    actions: BTreeMap<ActionId, Action>,
    next_action_to_start: u64,
    mining: Option<MiningAction>,
    interaction: InteractionController,
    place_ready_at: Tick,
    break_ready_at: Tick,
    history: ViewHistory,
    pages: BTreeSet<PageId>,
    page_requests: VecDeque<(PageId, bool)>,
    summaries: VecDeque<PageId>,
    last_view_position: [f64; 3],
}
struct Pending {
    ticket: u64,
    outgoing: Vec<Outgoing>,
    changes: Vec<WorldChange>,
    checkpoint: bool,
}
type ColumnPool = jobs::NativePool<(i32, i32), r#gen::GeneratedColumn>;
struct Generation {
    pool: ColumnPool,
    requested: BTreeMap<(i32, i32), BTreeSet<PageId>>,
    dispatched: BTreeMap<jobs::JobId, (i32, i32)>,
    next_job: u64,
    ready: VecDeque<(PageId, Box<[Cell; core_types::PAGE_VOLUME]>)>,
}

pub struct Server {
    config: ServerConfig,
    world: World,
    writer: SaveWorker,
    identities: BTreeMap<PlayerId, Identity>,
    saved_players: BTreeMap<PlayerId, SavedPlayer>,
    dedup: BTreeMap<PlayerId, DedupState>,
    peers: BTreeMap<u64, Peer>,
    inbox: VecDeque<(u64, ClientMessage, u64)>,
    pending: Option<Pending>,
    next_ticket: u64,
    tick: Tick,
    next_tick_at: u64,
    last_checkpoint_tick: u64,
    spawn: [f64; 3],
    generation: Option<Generation>,
    failed: bool,
    counters: ServerCounters,
    recent_changes: VecDeque<WorldChange>,
    drop_seed: u64,
    authoritative_transactions: VecDeque<Transaction>,
}

impl Server {
    pub fn open(config: ServerConfig) -> Result<Self, ServerError> {
        Self::open_with_world(config, World::new(rules::properties, 1024))
    }
    pub fn open_with_world(
        config: ServerConfig,
        initial_world: World,
    ) -> Result<Self, ServerError> {
        if config.maximum_players == 0
            || config.maximum_players > 32
            || config.spawn_protection_radius < 0
        {
            return Err(ServerError::Invalid("player/protection bounds"));
        }
        if config
            .spawn_override
            .is_some_and(|p| p.iter().any(|v| !v.is_finite() || v.abs() > 29_000_000.0))
        {
            return Err(ServerError::Invalid("spawn bounds"));
        }
        let (mut store, recovery) = Store::open(&config.data_dir)?;
        let entropy = store.load_or_create_private_entropy(|| {
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
            Ok(bytes)
        })?;
        let drop_seed = u64::from_le_bytes(
            blake3::derive_key("cubi survival drops v1", &entropy)[..8]
                .try_into()
                .expect("hash prefix"),
        );
        let mut identities = BTreeMap::new();
        let mut players = BTreeMap::new();
        let mut dedup = BTreeMap::new();
        let mut tick = Tick(0);
        let public_seed;
        let mut world = if let Some(snapshot) = recovery.snapshot {
            let snapshot = save::require_versions(
                snapshot,
                r#gen::GENERATOR_VERSION,
                net::RULES_VERSION,
                None,
            )?;
            public_seed = snapshot.public_seed;
            tick = snapshot.server_tick;
            identities.extend(snapshot.identities.into_iter().map(|i| (i.id, i)));
            players.extend(snapshot.players.into_iter().map(|p| (p.state.identity, p)));
            dedup.extend(snapshot.dedup);
            World::restore(rules::properties, JournalLimits::default(), snapshot.world)
                .map_err(|_| ServerError::Invalid("invalid world snapshot"))?
        } else {
            public_seed = config.public_seed;
            initial_world
        };
        for (_, batch) in recovery.batches {
            for change in batch.changes {
                world
                    .apply_recovered_change(&change)
                    .map_err(|_| ServerError::Invalid("invalid WAL world transition"))?;
            }
            for identity in batch.identities {
                identities.insert(identity.id, identity);
            }
            for player in batch.players {
                players.insert(player.state.identity, player);
            }
            for (id, reply) in batch.actions {
                remember(dedup.entry(id).or_default(), reply);
            }
            tick = batch.tick;
        }
        for saved in players.values() {
            if !saved.inventory.is_empty() {
                rules::survival::Inventory::decode(&saved.inventory)
                    .map_err(|_| ServerError::Invalid("invalid player inventory"))?;
            }
            let allow_debug = config.server_debug
                && identities
                    .get(&saved.state.identity)
                    .is_some_and(|i| i.role.can_admin());
            actor::restore(saved, allow_debug).map_err(ServerError::Invalid)?;
        }
        tick = Tick(tick.0.max(world.tick().0));
        // Fresh baselines, including fixture worlds, are saved before the first WAL
        // record. This also ensures private world bytes are never regenerated on load.
        if store.last_sequence() == 0 {
            store.checkpoint(&Snapshot {
                record_sequence: 0,
                server_tick: tick,
                public_seed,
                generator_version: r#gen::GENERATOR_VERSION,
                rules_version: net::RULES_VERSION,
                world: world.checkpoint().to_restore_state(),
                identities: identities.values().cloned().collect(),
                players: players.values().cloned().collect(),
                dedup: dedup.iter().map(|(id, d)| (*id, d.clone())).collect(),
            })?;
        }
        let generator = std::sync::Arc::new(gen_authority::ServerGenerator::new(
            r#gen::TerrainSeed(public_seed),
            gen_authority::PrivateSeed::new(entropy),
        ));
        let spawn = config.spawn_override.unwrap_or_else(|| {
            let p = generator.terrain().spawn().center;
            [
                f64::from(p[0]) + 0.5,
                f64::from(p[1]) + 1.0,
                f64::from(p[2]) + 0.5,
            ]
        });
        let generation = if config.generation_enabled {
            let worker_generator = generator.clone();
            let pool =
                jobs::NativePool::new(2, 8, move |(x, z), cancel: &jobs::CancellationToken| {
                    if cancel.is_cancelled() {
                        return Err("cancelled".into());
                    }
                    worker_generator
                        .generate_column(x, z)
                        .map_err(|_| "generation failed".into())
                })
                .map_err(|_| ServerError::Invalid("generator worker"))?;
            Some(Generation {
                pool,
                requested: BTreeMap::new(),
                dispatched: BTreeMap::new(),
                next_job: 1,
                ready: VecDeque::new(),
            })
        } else {
            None
        };
        let mut config = config;
        config.public_seed = public_seed;
        let owner = load_owner(&config.data_dir, &identities)?;
        if !identities.contains_key(&owner.identity) {
            let identity = Identity {
                id: owner.identity,
                name: owner.name.clone(),
                credential_hash: credential_hash(owner.credential.expect("owner credential")),
                role: Role::Owner,
                banned: false,
            };
            let mut batch = Batch::empty(tick);
            batch.identities.push(identity.clone());
            store.append(&batch)?;
            identities.insert(identity.id, identity);
        }
        let writer = SaveWorker::spawn(store, 2)?;
        Ok(Self {
            config,
            world,
            writer,
            identities,
            saved_players: players,
            dedup,
            peers: BTreeMap::new(),
            inbox: VecDeque::new(),
            pending: None,
            next_ticket: 1,
            tick,
            next_tick_at: 0,
            last_checkpoint_tick: tick.0,
            spawn,
            generation,
            failed: false,
            counters: ServerCounters::default(),
            recent_changes: VecDeque::new(),
            drop_seed,
            authoritative_transactions: VecDeque::new(),
        })
    }
    pub fn world(&self) -> &World {
        &self.world
    }
    pub fn current_tick(&self) -> Tick {
        self.tick
    }
    pub fn counters(&self) -> ServerCounters {
        self.counters
    }
    pub fn is_failed(&self) -> bool {
        self.failed
    }
    /// Trusted owner-side systems enqueue a logical operation here rather than
    /// mutating World outside the durable publication boundary. This is never a
    /// client wire operation; gameplay authorization precedes queueing it.
    pub fn queue_transaction(&mut self, transaction: Transaction) -> Result<(), Box<Transaction>> {
        if self.failed || self.authoritative_transactions.len() >= 2 {
            return Err(Box::new(transaction));
        }
        self.authoritative_transactions.push_back(transaction);
        Ok(())
    }
    pub fn session(&self, connection: u64) -> Option<(u64, SecretToken)> {
        self.peers
            .get(&connection)
            .filter(|p| p.ready)
            .map(|p| (p.session, p.credential))
    }
    pub fn disconnect(&mut self, connection: u64) {
        self.peers.remove(&connection);
        self.inbox.retain(|(c, _, _)| *c != connection);
    }
    pub fn handle(
        &mut self,
        connection: u64,
        message: ClientMessage,
        now_ms: u64,
    ) -> Vec<Outgoing> {
        // Decode validation is also applied to in-process callers. This boundary
        // must not admit finite/length checks bypassed by a different transport.
        if net::encode_client(&message).is_err() || self.inbox.len() >= MAX_INBOX {
            self.counters.rejected_messages += 1;
            return vec![Outgoing::reliable(
                connection,
                ServerMessage::Error(RejectReason::Protocol),
            )];
        }
        self.inbox.push_back((connection, message, now_ms));
        self.counters.inbox_high_water = self.counters.inbox_high_water.max(self.inbox.len());
        self.tick(now_ms)
    }
    pub fn tick(&mut self, now_ms: u64) -> Vec<Outgoing> {
        let mut output = self.finish_durability();
        if self.failed {
            return output;
        }
        if self.pending.is_some() {
            self.counters.storage_stalls += 1;
            return output;
        }
        if self.tick.0.saturating_sub(self.last_checkpoint_tick) >= 200 && self.request_checkpoint()
        {
            return output;
        }
        if now_ms < self.next_tick_at {
            return output;
        }
        self.next_tick_at = now_ms.saturating_add(TICK_MILLIS);
        self.tick.0 = self.tick.0.saturating_add(1);
        self.counters.ticks += 1;
        let mut batch = Batch::empty(self.tick);
        let mut deferred = Vec::new();
        for _ in 0..2 {
            let Some(transaction) = self.authoritative_transactions.pop_front() else {
                break;
            };
            match self.world.commit(transaction) {
                Ok(change) => batch.changes.push(change),
                Err(_) => self.counters.rejected_transactions += 1,
            }
        }
        for _ in 0..MAX_ACTIONS_PER_TICK {
            let Some((connection, message, received_at)) = self.inbox.pop_front() else {
                break;
            };
            self.process_message(connection, message, received_at, &mut batch, &mut deferred);
        }
        self.poll_generation(&mut batch);
        let connections: Vec<_> = self.peers.keys().copied().collect();
        for connection in connections {
            let Some(mut peer) = self.peers.remove(&connection) else {
                continue;
            };
            if peer.ready {
                if !self.identities[&peer.identity].role.can_admin()
                    && peer.player.mode != GameMode::Survival
                {
                    peer.player.set_mode(GameMode::Survival);
                }
                if !(self.config.server_debug && self.identities[&peer.identity].role.can_admin()) {
                    peer.player.revoke_debug_permission();
                }
                let wanted = peer.input_sequence.saturating_add(1);
                let frame = peer
                    .inputs
                    .get(&wanted)
                    .filter(|frame| {
                        if frame.target_tick.0 == 0 {
                            peer.input_ready_at.is_some_and(|at| now_ms >= at)
                        } else {
                            self.tick.0 >= frame.target_tick.0
                        }
                    })
                    .copied();
                if let Some(frame) = frame {
                    peer.player.yaw = frame.yaw.rem_euclid(360.0);
                    peer.player.pitch = frame.pitch;
                }
                let outcome = peer.player.tick(
                    &WorldQuery::new(&self.world, peer.space),
                    keys(frame.map_or(0, |f| f.keys)),
                );
                if !outcome.waiting_for_world {
                    peer.last_keys = frame.map_or(0, |f| f.keys);
                    if frame.is_some() {
                        peer.inputs.remove(&wanted);
                        peer.input_sequence = wanted;
                        self.counters.input_steps += 1;
                    }
                }
                // An absent sequence never becomes an invented movement input.
                // Reliable redundant input history eventually fills loss gaps.
                while self.dedup.get(&peer.identity).is_some_and(|d| {
                    d.replies
                        .iter()
                        .any(|r| r.id.0 == peer.next_action_to_start)
                }) {
                    let Some(next) = peer.next_action_to_start.checked_add(1) else {
                        break;
                    };
                    peer.next_action_to_start = next;
                }
                let id = ActionId(peer.next_action_to_start);
                if let Some(action) = peer.actions.remove(&id) {
                    peer.next_action_to_start = peer.next_action_to_start.saturating_add(1);
                    self.perform_action(
                        connection,
                        &mut peer,
                        id,
                        action,
                        &mut batch,
                        &mut deferred,
                    );
                }
                self.advance_mining(connection, &mut peer, &mut batch, &mut deferred);
                let mut saved =
                    actor::capture(peer.identity, peer.space, peer.input_sequence, &peer.player);
                saved.place_ready_at = peer.place_ready_at;
                saved.break_ready_at = peer.break_ready_at;
                saved.inventory = peer.inventory.encode().to_vec();
                self.saved_players.insert(peer.identity, saved.clone());
                batch.players.push(saved);
            }
            if !self.identities[&peer.identity].banned {
                self.peers.insert(connection, peer);
            }
        }
        if !batch.is_empty() {
            self.submit_batch(batch, deferred);
        } else {
            output.extend(deferred);
            output.extend(self.publish_pages());
        }
        output
    }

    fn finish_durability(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        while let Ok(completion) = self.writer.try_complete() {
            let Some(pending) = self.pending.take() else {
                self.failed = true;
                break;
            };
            if completion.ticket != pending.ticket || completion.result.is_err() {
                self.failed = true;
                output.extend(self.peers.keys().map(|c| {
                    Outgoing::reliable(*c, ServerMessage::Error(RejectReason::StorageUnavailable))
                }));
                break;
            }
            if pending.checkpoint {
                continue;
            }
            self.counters.durable_batches += 1;
            if !pending.changes.is_empty() {
                // A changed occluder can reveal an ore in an unchanged page.
                // Queue all bounded subscriptions after the durable frontier.
                for peer in self.peers.values_mut() {
                    for page in &peer.pages {
                        if !peer.page_requests.iter().any(|(p, _)| p == page) {
                            peer.page_requests.push_back((*page, false));
                        }
                    }
                }
            }
            for message in &pending.outgoing {
                if matches!(message.message, ServerMessage::Welcome(_))
                    && let Some(peer) = self.peers.get_mut(&message.connection)
                {
                    peer.ready = true;
                }
            }
            let mut changed_spaces = BTreeSet::new();
            for change in pending.changes {
                self.counters.committed_transactions += 1;
                if change.transform.is_some() {
                    changed_spaces.insert(change.space);
                }
                self.recent_changes.push_back(change);
                while self.recent_changes.len() > 256 {
                    self.recent_changes.pop_front();
                }
            }
            for space in changed_spaces {
                if let Some(record) = self.world.space(space) {
                    output.extend(self.peers.iter().filter(|(_, p)| p.ready).map(|(c, _)| {
                        Outgoing::reliable(
                            *c,
                            ServerMessage::Transform {
                                space,
                                version: record.version,
                                transform: record.transform,
                            },
                        )
                    }));
                }
            }
            output.extend(pending.outgoing);
            let players: Vec<_> = self
                .peers
                .values()
                .filter(|p| p.ready)
                .map(peer_state)
                .collect();
            for (connection, peer) in &mut self.peers {
                if peer.ready {
                    if peer.inventory_sent != Some(peer.inventory.revision()) {
                        output.push(Outgoing::reliable(
                            *connection,
                            ServerMessage::Inventory {
                                data: peer.inventory.encode().to_vec(),
                            },
                        ));
                        peer.inventory_sent = Some(peer.inventory.revision());
                    }
                    if let Ok(state) = sim::prediction::PredictionState::capture(&peer.player) {
                        output.push(Outgoing {
                            connection: *connection,
                            message: ServerMessage::Prediction {
                                tick: self.tick,
                                player: peer_state(peer),
                                continuation: state.as_bytes().to_vec(),
                            },
                            reliable: false,
                        });
                    }
                    output.push(Outgoing {
                        connection: *connection,
                        message: ServerMessage::State {
                            tick: self.tick,
                            player: peer_state(peer),
                        },
                        reliable: false,
                    });
                    output.push(Outgoing {
                        connection: *connection,
                        message: ServerMessage::Players {
                            tick: self.tick,
                            players: players.clone(),
                        },
                        reliable: false,
                    });
                }
            }
            output.extend(self.publish_pages());
        }
        output
    }
    fn submit_batch(&mut self, batch: Batch, outgoing: Vec<Outgoing>) {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        let changes = batch.changes.clone();
        if self
            .writer
            .try_submit(WriteRequest::Batch { ticket, batch })
            .is_err()
        {
            self.failed = true;
            return;
        }
        self.pending = Some(Pending {
            ticket,
            outgoing,
            changes,
            checkpoint: false,
        });
    }
    pub fn request_checkpoint(&mut self) -> bool {
        if self.pending.is_some() || self.failed {
            return false;
        }
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        let metadata = SnapshotMetadata {
            server_tick: self.tick,
            public_seed: self.config.public_seed,
            generator_version: r#gen::GENERATOR_VERSION,
            rules_version: net::RULES_VERSION,
            identities: self.identities.values().cloned().collect(),
            players: self.saved_players.values().cloned().collect(),
            dedup: self.dedup.iter().map(|(id, d)| (*id, d.clone())).collect(),
        };
        if self
            .writer
            .try_submit(WriteRequest::Checkpoint {
                ticket,
                checkpoint: self.world.checkpoint(),
                metadata,
            })
            .is_err()
        {
            return false;
        }
        self.pending = Some(Pending {
            ticket,
            outgoing: Vec::new(),
            changes: Vec::new(),
            checkpoint: true,
        });
        self.last_checkpoint_tick = self.tick.0;
        true
    }

    fn process_message(
        &mut self,
        connection: u64,
        message: ClientMessage,
        received_at: u64,
        batch: &mut Batch,
        out: &mut Vec<Outgoing>,
    ) {
        if let ClientMessage::Hello(hello) = message {
            self.hello(connection, hello, batch, out);
            return;
        }
        let Some(peer) = self.peers.get_mut(&connection).filter(|p| p.ready) else {
            out.push(Outgoing::reliable(
                connection,
                ServerMessage::Error(RejectReason::Unauthorized),
            ));
            return;
        };
        match message {
            ClientMessage::Inputs(frames) => {
                for frame in frames {
                    if frame.sequence > peer.input_sequence
                        && frame.sequence <= peer.input_sequence.saturating_add(MAX_INPUTS as u64)
                        && frame.target_tick.0 <= self.tick.0.saturating_add(64)
                        && peer.inputs.len() < MAX_INPUTS
                    {
                        peer.inputs.entry(frame.sequence).or_insert(frame);
                    }
                }
                if !peer.inputs.is_empty() && peer.input_ready_at.is_none() {
                    peer.input_ready_at = Some(received_at.saturating_add(INPUT_DELAY_MILLIS));
                }
            }
            ClientMessage::Action { id, action } => {
                let state = self.dedup.entry(peer.identity).or_default();
                if let Some(reply) = state.replies.iter().find(|r| r.id == id) {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Action(*reply),
                    ));
                } else if id.0 == 0 || id.0 <= state.contiguous {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Action(reply(
                            id,
                            ActionStatus::Rejected(RejectReason::AlreadyFinal),
                            None,
                            self.world.version(),
                            None,
                            None,
                        )),
                    ));
                } else if id.0 > state.contiguous.saturating_add(256) || peer.actions.len() >= 64 {
                    // Queue pressure is retryable. It is deliberately an Error,
                    // not a terminal ActionReply that might change on retry.
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Error(RejectReason::RateLimited),
                    ));
                } else if peer.mining.as_ref().is_some_and(|m| m.id == id)
                    || peer.actions.iter().any(|(queued, _)| *queued == id)
                {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Action(reply(
                            id,
                            ActionStatus::Pending,
                            None,
                            self.world.version(),
                            None,
                            None,
                        )),
                    ));
                } else {
                    peer.actions.insert(id, action);
                }
            }
            ClientMessage::RequestPage { page, acknowledged } => {
                let confirmed =
                    acknowledged.is_some_and(|version| peer.history.acknowledge(page, version));
                if peer.page_requests.len() < MAX_AOI_PAGES {
                    peer.page_requests.push_back((page, !confirmed));
                }
            }
            ClientMessage::AckPage { page, version } => {
                peer.history.acknowledge(page, version);
            }
            ClientMessage::RequestSummary { page } => {
                if peer.summaries.len() < 16 {
                    peer.summaries.push_back(page);
                }
            }
            ClientMessage::Ping(n) => {
                out.push(Outgoing::reliable(connection, ServerMessage::Pong(n)))
            }
            ClientMessage::Hello(_) => unreachable!(),
        }
    }

    fn hello(&mut self, connection: u64, hello: Hello, batch: &mut Batch, out: &mut Vec<Outgoing>) {
        if let Some(peer) = self.peers.get(&connection) {
            if peer.identity == hello.identity
                || (hello.identity == PlayerId::default() && peer.welcome.new_credential.is_some())
            {
                if peer.ready {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Welcome(peer.welcome.clone()),
                    ));
                }
                return;
            }
            out.push(Outgoing::reliable(
                connection,
                ServerMessage::Error(RejectReason::Unauthorized),
            ));
            return;
        }
        if self.peers.len() >= self.config.maximum_players {
            out.push(Outgoing::reliable(
                connection,
                ServerMessage::Error(RejectReason::Capacity),
            ));
            return;
        }
        let (identity, token, new_token) =
            if hello.identity == PlayerId::default() && hello.credential.is_none() {
                if self.identities.len() >= save::MAX_IDENTITIES
                    || self.identities.values().any(|i| i.name == hello.name)
                {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Error(RejectReason::Unauthorized),
                    ));
                    return;
                }
                let Ok(id) = random::<16>() else {
                    self.failed = true;
                    return;
                };
                let Ok(raw_token) = random::<32>() else {
                    self.failed = true;
                    return;
                };
                let token = SecretToken::new(raw_token);
                let identity = Identity {
                    id: PlayerId(id),
                    name: hello.name,
                    credential_hash: credential_hash(token),
                    role: Role::Player,
                    banned: false,
                };
                batch.identities.push(identity.clone());
                self.identities.insert(identity.id, identity.clone());
                (identity, token, Some(token))
            } else {
                let Some(identity) = self.identities.get(&hello.identity).cloned() else {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Error(RejectReason::Unauthorized),
                    ));
                    return;
                };
                let Some(token) = hello.credential else {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Error(RejectReason::Unauthorized),
                    ));
                    return;
                };
                if identity.name != hello.name
                    || !constant_equal(&identity.credential_hash, &credential_hash(token))
                {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Error(RejectReason::Unauthorized),
                    ));
                    return;
                }
                if identity.banned {
                    out.push(Outgoing::reliable(
                        connection,
                        ServerMessage::Error(RejectReason::Banned),
                    ));
                    return;
                }
                (identity, token, None)
            };
        let previous: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| p.identity == identity.id)
            .map(|(c, _)| *c)
            .collect();
        for old in previous {
            self.peers.remove(&old);
            out.push(Outgoing::reliable(
                old,
                ServerMessage::Error(RejectReason::Unauthorized),
            ));
        }
        let allow_debug = self.config.server_debug && identity.role.can_admin();
        let mut player = if let Some(saved) = self.saved_players.get(&identity.id) {
            match actor::restore(saved, allow_debug) {
                Ok(player) => player,
                Err(_) => {
                    self.failed = true;
                    return;
                }
            }
        } else {
            Player::new(self.spawn)
        };
        if !allow_debug {
            player.revoke_debug_permission();
        }
        if !identity.role.can_admin() && player.mode != GameMode::Survival {
            player.set_mode(GameMode::Survival);
        }
        let space = self
            .saved_players
            .get(&identity.id)
            .map_or(SpaceId(0), |p| p.state.space);
        let Ok(session_bytes) = random::<8>() else {
            self.failed = true;
            return;
        };
        let session = u64::from_le_bytes(session_bytes).max(1);
        let state = player_state(identity.id, space, 0, &player);
        let inventory = match self
            .saved_players
            .get(&identity.id)
            .filter(|s| !s.inventory.is_empty())
        {
            Some(saved) => match rules::survival::Inventory::decode(&saved.inventory) {
                Ok(inv) => inv,
                Err(_) => {
                    self.failed = true;
                    return;
                }
            },
            None => rules::survival::Inventory::empty(),
        };
        let welcome = Welcome {
            session,
            identity: identity.id,
            new_credential: new_token,
            role: identity.role,
            tick: self.tick,
            next_action: ActionId(
                self.dedup
                    .get(&identity.id)
                    .map_or(1, |d| d.contiguous.saturating_add(1)),
            ),
            public_seed: self.config.public_seed,
            generator_version: r#gen::GENERATOR_VERSION,
            rules_version: net::RULES_VERSION,
            player: state,
        };
        let place_ready_at = self
            .saved_players
            .get(&identity.id)
            .map_or(Tick(0), |s| s.place_ready_at);
        let break_ready_at = self
            .saved_players
            .get(&identity.id)
            .map_or(Tick(0), |s| s.break_ready_at);
        let mut saved = actor::capture(identity.id, space, 0, &player);
        saved.place_ready_at = place_ready_at;
        saved.break_ready_at = break_ready_at;
        saved.inventory = inventory.encode().to_vec();
        batch.players.push(saved.clone());
        self.saved_players.insert(identity.id, saved);
        let position = player.pos;
        self.peers.insert(
            connection,
            Peer {
                identity: identity.id,
                credential: token,
                session,
                ready: false,
                welcome: welcome.clone(),
                player,
                inventory,
                inventory_sent: None,
                space,
                input_sequence: 0,
                inputs: BTreeMap::new(),
                input_ready_at: None,
                last_keys: 0,
                actions: BTreeMap::new(),
                next_action_to_start: welcome.next_action.0,
                mining: None,
                interaction: InteractionController::default(),
                place_ready_at,
                break_ready_at,
                history: ViewHistory::new(MAX_AOI_PAGES, 8),
                pages: BTreeSet::new(),
                page_requests: VecDeque::new(),
                summaries: VecDeque::new(),
                last_view_position: position,
            },
        );
        out.push(Outgoing::reliable(
            connection,
            ServerMessage::Welcome(welcome),
        ));
        for (space, record) in self.world.spaces().take(64) {
            out.push(Outgoing::reliable(
                connection,
                ServerMessage::Transform {
                    space,
                    version: record.version,
                    transform: record.transform,
                },
            ));
        }
        let center = position_page(space, position);
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    self.request_generation(PageId {
                        x: center.x + dx,
                        y: center.y + dy,
                        z: center.z + dz,
                        ..center
                    });
                }
            }
        }
    }

    fn terminal(
        &mut self,
        connection: u64,
        identity: PlayerId,
        result: ActionReply,
        batch: &mut Batch,
        out: &mut Vec<Outgoing>,
    ) {
        remember(self.dedup.entry(identity).or_default(), result);
        batch.actions.push((identity, result));
        out.push(Outgoing::reliable(
            connection,
            ServerMessage::Action(result),
        ));
    }
    fn perform_action(
        &mut self,
        connection: u64,
        peer: &mut Peer,
        id: ActionId,
        action: Action,
        batch: &mut Batch,
        out: &mut Vec<Outgoing>,
    ) {
        if let Action::SelectSlot(slot) = action {
            let status = if peer.inventory.select(usize::from(slot)).is_ok() {
                ActionStatus::Accepted
            } else {
                ActionStatus::Rejected(RejectReason::Protocol)
            };
            self.terminal(
                connection,
                peer.identity,
                reply(id, status, None, self.world.version(), None, None),
                batch,
                out,
            );
            return;
        }
        if let Action::PlaceOwned {
            position,
            expected,
            cell,
            inventory,
        } = action
        {
            if peer.player.mode == GameMode::Creative
                && self.identities[&peer.identity].role.can_admin()
            {
                self.perform_action(
                    connection,
                    peer,
                    id,
                    Action::Place {
                        position,
                        expected,
                        cell,
                    },
                    batch,
                    out,
                );
                return;
            }
            self.owned_place(
                connection, peer, id, position, expected, cell, inventory, batch, out,
            );
            return;
        }
        let role = self.identities[&peer.identity].role;
        let mut status = ActionStatus::Accepted;
        let mut transaction = None;
        let mut position = None;
        let mut cell = None;
        match action {
            Action::SelectSlot(_) | Action::PlaceOwned { .. } => unreachable!("handled above"),
            Action::Flight(enabled) => {
                let _ = peer.player.set_flight(enabled);
            }
            Action::DebugNoclip(enabled) => {
                if peer
                    .player
                    .set_debug_noclip(
                        enabled,
                        Permissions {
                            server_debug: self.config.server_debug && role.can_admin(),
                        },
                    )
                    .is_err()
                {
                    status = ActionStatus::Rejected(RejectReason::DebugRequired);
                }
            }
            Action::Place {
                position: target,
                expected,
                cell: value,
            } => {
                position = Some(target);
                cell = Some(value);
                if peer.player.mode != GameMode::Creative || !role.can_admin() {
                    status = ActionStatus::Rejected(RejectReason::Unauthorized);
                } else if self.tick.0 < peer.place_ready_at.0 {
                    status = ActionStatus::Rejected(RejectReason::RateLimited);
                } else if target.space != peer.space {
                    status = ActionStatus::Rejected(RejectReason::OutOfReach);
                } else {
                    let prepared = sim::prepare_edit(
                        &self.world,
                        peer.space,
                        &peer.player,
                        sim::EditAction::Place {
                            position: [target.x, target.y, target.z],
                            expected,
                            cell: value,
                        },
                        self.tick,
                        |p| self.protected(p, role),
                    );
                    match prepared {
                        Ok(tx) => match self.world.commit(tx) {
                            Ok(change) => {
                                transaction = Some(change.tx);
                                if let CellRead::Known(actual) = self.world.read(target) {
                                    cell = Some(actual);
                                }
                                batch.changes.push(change);
                                peer.place_ready_at = Tick(self.tick.0.saturating_add(4));
                            }
                            Err(_) => status = ActionStatus::Rejected(RejectReason::Stale),
                        },
                        Err(error) => status = ActionStatus::Rejected(edit_reason(error)),
                    }
                }
            }
            Action::Mine {
                position: target,
                expected,
                tool,
            } => {
                position = Some(target);
                let held = peer.inventory.hand();
                if tool != 0 && tool != held.item() {
                    self.terminal(
                        connection,
                        peer.identity,
                        reply(
                            id,
                            ActionStatus::Rejected(RejectReason::Unauthorized),
                            None,
                            self.world.version(),
                            position,
                            None,
                        ),
                        batch,
                        out,
                    );
                    return;
                }
                if self.tick.0 < peer.break_ready_at.0 {
                    self.terminal(
                        connection,
                        peer.identity,
                        reply(
                            id,
                            ActionStatus::Rejected(RejectReason::RateLimited),
                            None,
                            self.world.version(),
                            position,
                            None,
                        ),
                        batch,
                        out,
                    );
                    return;
                }
                let prepared = if target.space != peer.space {
                    Err(sim::EditError::OutOfReach)
                } else {
                    sim::prepare_edit(
                        &self.world,
                        peer.space,
                        &peer.player,
                        sim::EditAction::Break {
                            position: [target.x, target.y, target.z],
                            expected,
                        },
                        self.tick,
                        |p| self.protected(p, role),
                    )
                };
                match prepared {
                    Ok(_) => {
                        if let Some(previous) = peer.mining.take() {
                            self.terminal(
                                connection,
                                peer.identity,
                                reply(
                                    previous.id,
                                    ActionStatus::Rejected(RejectReason::Canceled),
                                    None,
                                    self.world.version(),
                                    Some(previous.position),
                                    None,
                                ),
                                batch,
                                out,
                            );
                        }
                        peer.mining = Some(MiningAction {
                            id,
                            position: target,
                            expected,
                            held,
                        });
                        out.push(Outgoing::reliable(
                            connection,
                            ServerMessage::Action(reply(
                                id,
                                ActionStatus::Pending,
                                None,
                                self.world.version(),
                                position,
                                None,
                            )),
                        ));
                        return;
                    }
                    Err(error) => status = ActionStatus::Rejected(edit_reason(error)),
                }
            }
            Action::StopMining => {
                if let Some(previous) = peer.mining.take() {
                    self.terminal(
                        connection,
                        peer.identity,
                        reply(
                            previous.id,
                            ActionStatus::Rejected(RejectReason::Canceled),
                            None,
                            self.world.version(),
                            Some(previous.position),
                            None,
                        ),
                        batch,
                        out,
                    );
                }
                peer.interaction.mining.cancel();
            }
            Action::Command(command) => match self.command(peer, role, &command, batch, out) {
                Ok(tx) => transaction = tx,
                Err(reason) => status = ActionStatus::Rejected(reason),
            },
        }
        self.terminal(
            connection,
            peer.identity,
            reply(
                id,
                status,
                transaction,
                self.world.version(),
                position,
                cell,
            ),
            batch,
            out,
        );
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Detached guarded placement keeps explicit authority and inventory inputs"
    )]
    fn owned_place(
        &mut self,
        connection: u64,
        peer: &mut Peer,
        id: ActionId,
        position: BlockPos,
        expected: Cell,
        cell: Cell,
        guard: net::InventoryGuard,
        batch: &mut Batch,
        out: &mut Vec<Outgoing>,
    ) {
        let hand = peer.inventory.hand();
        let role = self.identities[&peer.identity].role;
        let result = (|| {
            if peer.player.mode != GameMode::Survival {
                return Err(RejectReason::Unauthorized);
            }
            if self.tick.0 < peer.place_ready_at.0 {
                return Err(RejectReason::RateLimited);
            }
            if guard.revision != peer.inventory.revision()
                || usize::from(guard.selected) != peer.inventory.selected()
                || guard.item != hand.item()
                || guard.count != hand.count()
                || guard.damage != hand.damage()
            {
                return Err(RejectReason::Stale);
            }
            let plan = peer
                .inventory
                .plan_place(hand, cell)
                .map_err(|_| RejectReason::Unauthorized)?;
            if position.space != peer.space {
                return Err(RejectReason::OutOfReach);
            }
            let tx = sim::prepare_edit(
                &self.world,
                peer.space,
                &peer.player,
                sim::EditAction::Place {
                    position: [position.x, position.y, position.z],
                    expected,
                    cell,
                },
                self.tick,
                |p| self.protected(p, role),
            )
            .map_err(edit_reason)?;
            let change = self.world.commit(tx).map_err(|_| RejectReason::Stale)?;
            let transaction = change.tx;
            batch.changes.push(change);
            peer.inventory = *plan.after();
            peer.place_ready_at = Tick(self.tick.0.saturating_add(4));
            let actual = match self.world.read(position) {
                CellRead::Known(actual) => actual,
                CellRead::Unknown => cell,
            };
            Ok((transaction, actual))
        })();
        let result = match result {
            Ok((tx, actual)) => reply(
                id,
                ActionStatus::Accepted,
                Some(tx),
                self.world.version(),
                Some(position),
                Some(actual),
            ),
            Err(reason) => reply(
                id,
                ActionStatus::Rejected(reason),
                None,
                self.world.version(),
                Some(position),
                None,
            ),
        };
        self.terminal(connection, peer.identity, result, batch, out);
    }
    fn advance_mining(
        &mut self,
        connection: u64,
        peer: &mut Peer,
        batch: &mut Batch,
        out: &mut Vec<Outgoing>,
    ) {
        let Some(mining) = peer.mining.as_ref() else {
            let _ = peer.interaction.tick_break(
                &self.world,
                peer.space,
                &peer.player,
                self.tick,
                0,
                0,
                false,
                |_| false,
            );
            return;
        };
        let role = self.identities[&peer.identity].role;
        let current = self.world.read(mining.position);
        let prepared = sim::prepare_edit(
            &self.world,
            peer.space,
            &peer.player,
            sim::EditAction::Break {
                position: [mining.position.x, mining.position.y, mining.position.z],
                expected: mining.expected,
            },
            self.tick,
            |p| self.protected(p, role),
        );
        let error = prepared.err();
        if current != CellRead::Known(mining.expected)
            || error.is_some()
            || peer.inventory.hand() != mining.held
        {
            let mining = peer.mining.take().expect("present");
            self.terminal(
                connection,
                peer.identity,
                reply(
                    mining.id,
                    ActionStatus::Rejected(error.map_or(RejectReason::Stale, edit_reason)),
                    None,
                    self.world.version(),
                    Some(mining.position),
                    None,
                ),
                batch,
                out,
            );
            return;
        }
        if peer.last_keys & InputFrame::ATTACK == 0 {
            return;
        }
        let result = peer.interaction.tick_break(
            &self.world,
            peer.space,
            &peer.player,
            self.tick,
            mining.held.item(),
            mining.held.damage(),
            true,
            |p| self.protected(p, role),
        );
        match result {
            Ok(Some(mut tx)) => {
                let mining = peer.mining.take().expect("present");
                // The controller ray must still be the target associated with this
                // ActionId; another ray hit cannot silently change its meaning.
                if !tx.cell_updates().any(|(p, _)| p == mining.position) {
                    self.terminal(
                        connection,
                        peer.identity,
                        reply(
                            mining.id,
                            ActionStatus::Rejected(RejectReason::Occluded),
                            None,
                            self.world.version(),
                            Some(mining.position),
                            None,
                        ),
                        batch,
                        out,
                    );
                    return;
                }
                let mut inventory_after = peer.inventory;
                if peer.player.mode == GameMode::Survival {
                    let player_key = u64::from_le_bytes(
                        blake3::hash(&peer.identity.0).as_bytes()[..8]
                            .try_into()
                            .expect("hash prefix"),
                    );
                    let mut rng = core_types::DeterministicRng::from_key(
                        self.drop_seed,
                        0x4d494e45,
                        player_key,
                        mining.id.0,
                    );
                    let plan = (|| {
                        let plan =
                            peer.inventory
                                .plan_mine(mining.held, mining.expected, &mut rng)?;
                        if !plan.drops.stacks().is_empty() {
                            survival::plan_drops(
                                &self.world,
                                mining.position,
                                peer.identity.0,
                                mining.id.0,
                                self.tick,
                                plan.drops.stacks(),
                                10,
                                None,
                            )?
                            .append_to(&mut tx);
                        }
                        Ok::<_, rules::survival::SurvivalError>(*plan.inventory.after())
                    })();
                    match plan {
                        Ok(after) => inventory_after = after,
                        Err(error) => {
                            self.terminal(
                                connection,
                                peer.identity,
                                reply(
                                    mining.id,
                                    ActionStatus::Rejected(survival_reason(error)),
                                    None,
                                    self.world.version(),
                                    Some(mining.position),
                                    None,
                                ),
                                batch,
                                out,
                            );
                            return;
                        }
                    }
                }
                match self.world.commit(tx) {
                    Ok(change) => {
                        let tx = change.tx;
                        peer.inventory = inventory_after;
                        let cooldown = u64::from(peer.interaction.mining.cooldown);
                        peer.break_ready_at = Tick(
                            self.tick
                                .0
                                .saturating_add(cooldown)
                                .saturating_add(u64::from(cooldown > 0)),
                        );
                        batch.changes.push(change);
                        self.terminal(
                            connection,
                            peer.identity,
                            reply(
                                mining.id,
                                ActionStatus::Accepted,
                                Some(tx),
                                self.world.version(),
                                Some(mining.position),
                                Some(Cell::AIR),
                            ),
                            batch,
                            out,
                        );
                    }
                    Err(_) => self.terminal(
                        connection,
                        peer.identity,
                        reply(
                            mining.id,
                            ActionStatus::Rejected(RejectReason::Stale),
                            None,
                            self.world.version(),
                            Some(mining.position),
                            None,
                        ),
                        batch,
                        out,
                    ),
                }
            }
            Err(error) => {
                let mining = peer.mining.take().expect("present");
                self.terminal(
                    connection,
                    peer.identity,
                    reply(
                        mining.id,
                        ActionStatus::Rejected(edit_reason(error)),
                        None,
                        self.world.version(),
                        Some(mining.position),
                        None,
                    ),
                    batch,
                    out,
                );
            }
            Ok(None) => {}
        }
    }
    fn protected(&self, p: [i32; 3], role: Role) -> bool {
        if role.can_admin() {
            return false;
        }
        let r = f64::from(self.config.spawn_protection_radius);
        (f64::from(p[0]) - self.spawn[0]).abs() <= r && (f64::from(p[2]) - self.spawn[2]).abs() <= r
    }
    fn command(
        &mut self,
        peer: &mut Peer,
        role: Role,
        text: &str,
        batch: &mut Batch,
        out: &mut Vec<Outgoing>,
    ) -> Result<Option<TxId>, RejectReason> {
        if !role.can_admin() {
            return Err(RejectReason::Unauthorized);
        }
        let parts: Vec<_> = text.split_ascii_whitespace().collect();
        match parts.as_slice() {
            ["/give", item, count] => {
                let item = item.parse::<u16>().map_err(|_| RejectReason::InvalidCell)?;
                let count = count.parse::<u16>().map_err(|_| RejectReason::Protocol)?;
                let stack = rules::stack::ItemStack::new(item, count)
                    .map_err(|_| RejectReason::InvalidCell)?;
                let plan = peer
                    .inventory
                    .plan_pickup(stack)
                    .map_err(|_| RejectReason::Capacity)?;
                if !plan.remainder().is_empty() {
                    return Err(RejectReason::Capacity);
                }
                peer.inventory = *plan.after();
                Ok(None)
            }
            ["/gamemode", mode] => {
                let mode = match *mode {
                    "survival" => GameMode::Survival,
                    "creative" => GameMode::Creative,
                    "spectator" => GameMode::Spectator,
                    _ => return Err(RejectReason::Protocol),
                };
                peer.player.set_mode(mode);
                Ok(None)
            }
            ["/tp", x, y, z] => {
                let p = [parse_float(x)?, parse_float(y)?, parse_float(z)?];
                peer.player
                    .teleport(p)
                    .map_err(|_| RejectReason::Protocol)?;
                Ok(None)
            }
            ["/ban", name] | ["/unban", name] | ["/op", name] | ["/deop", name] => {
                let id = self
                    .identities
                    .values()
                    .find(|i| i.name == *name)
                    .map(|i| i.id)
                    .ok_or(RejectReason::Unknown)?;
                let identity = self.identities.get_mut(&id).expect("found");
                if identity.role == Role::Owner || (parts[0] == "/op" && role != Role::Owner) {
                    return Err(RejectReason::Unauthorized);
                }
                match parts[0] {
                    "/ban" => identity.banned = true,
                    "/unban" => identity.banned = false,
                    "/op" => identity.role = Role::Admin,
                    "/deop" => identity.role = Role::Player,
                    _ => unreachable!(),
                }
                batch.identities.push(identity.clone());
                if parts[0] == "/deop" {
                    for target in self.peers.values_mut().filter(|p| p.identity == id) {
                        target.player.set_mode(GameMode::Survival);
                        target.player.revoke_debug_permission();
                        let mut saved =
                            actor::capture(id, target.space, target.input_sequence, &target.player);
                        saved.place_ready_at = target.place_ready_at;
                        saved.break_ready_at = target.break_ready_at;
                        saved.inventory = target.inventory.encode().to_vec();
                        self.saved_players.insert(id, saved.clone());
                        batch.players.retain(|p| p.state.identity != id);
                        batch.players.push(saved);
                    }
                    if peer.identity == id {
                        peer.player.set_mode(GameMode::Survival);
                        peer.player.revoke_debug_permission();
                    }
                }
                if identity.banned {
                    let connections: Vec<_> = self
                        .peers
                        .iter()
                        .filter(|(_, p)| p.identity == id)
                        .map(|(c, _)| *c)
                        .collect();
                    for connection in connections {
                        self.peers.remove(&connection);
                        out.push(Outgoing::reliable(
                            connection,
                            ServerMessage::Error(RejectReason::Banned),
                        ));
                    }
                }
                Ok(None)
            }
            ["/rollback", tx] => {
                let tx = tx.parse::<u64>().map_err(|_| RejectReason::Protocol)?;
                let old = self
                    .recent_changes
                    .iter()
                    .find(|change| change.tx.0 == tx)
                    .ok_or(RejectReason::Unknown)?;
                if old.cells.is_empty()
                    || !old.components.is_empty()
                    || !old.lights.is_empty()
                    || old.transform.is_some()
                    || old.pages.iter().any(|p| p.created || p.removed)
                {
                    return Err(RejectReason::Unsupported);
                }
                let mut transaction = Transaction::new(self.tick, old.space);
                for change in &old.cells {
                    let local = change.index.get();
                    let p = BlockPos {
                        space: change.page.space,
                        x: change.page.x * 16 + i32::from(local % 16),
                        y: change.page.y * 16 + i32::from(local / 256),
                        z: change.page.z * 16 + i32::from(local / 16 % 16),
                    };
                    if self.world.read(p) != CellRead::Known(change.new) {
                        return Err(RejectReason::Stale);
                    }
                    transaction.expect_page(change.page, self.world.page_version(change.page));
                    transaction.set_cell(p, change.old);
                }
                let change = self
                    .world
                    .commit(transaction)
                    .map_err(|_| RejectReason::Stale)?;
                let tx = change.tx;
                batch.changes.push(change);
                Ok(Some(tx))
            }
            ["/space_move", space, x, y, z] => {
                let space = SpaceId(space.parse().map_err(|_| RejectReason::Protocol)?);
                let mut transform = self
                    .world
                    .space(space)
                    .ok_or(RejectReason::Unknown)?
                    .transform;
                transform.translation = [parse_float(x)?, parse_float(y)?, parse_float(z)?];
                let mut transaction = Transaction::new(self.tick, space);
                transaction.set_transform(transform);
                let change = self
                    .world
                    .commit(transaction)
                    .map_err(|_| RejectReason::Stale)?;
                let tx = change.tx;
                batch.changes.push(change);
                Ok(Some(tx))
            }
            _ => Err(RejectReason::Unsupported),
        }
    }
    fn publish_pages(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        let connections: Vec<_> = self.peers.keys().copied().collect();
        for connection in connections {
            let mut peer = self.peers.remove(&connection).expect("listed");
            if peer.ready {
                let evicted: Vec<_> = peer
                    .pages
                    .iter()
                    .filter(|p| !self.in_aoi(&peer, **p, true))
                    .copied()
                    .collect();
                for page in evicted {
                    peer.pages.remove(&page);
                    peer.history.remove(page);
                }
                // Revisit visible pages when the eye moves. This is required for
                // visibility-only reveals even if their world versions are equal.
                if peer.last_view_position != peer.player.pos {
                    for page in &peer.pages {
                        if !peer.page_requests.iter().any(|(p, _)| p == page) {
                            peer.page_requests.push_back((*page, false));
                        }
                    }
                    peer.last_view_position = peer.player.pos;
                }
                for _ in 0..2 {
                    let Some((page, force)) = peer.page_requests.pop_front() else {
                        break;
                    };
                    if !self.in_aoi(&peer, page, peer.pages.contains(&page))
                        || peer.pages.len() >= MAX_AOI_PAGES && !peer.pages.contains(&page)
                    {
                        output.push(Outgoing::reliable(
                            connection,
                            ServerMessage::PageUnavailable(page),
                        ));
                        continue;
                    }
                    if let Some(data) = self.world.page(page) {
                        if page.space != peer.space
                            && let Some(record) = self.world.space(page.space)
                        {
                            output.push(Outgoing::reliable(
                                connection,
                                ServerMessage::Transform {
                                    space: page.space,
                                    version: record.version,
                                    transform: record.transform,
                                },
                            ));
                        }
                        let (cells, work) = visibility::sanitize(
                            &self.world,
                            page,
                            data.cells(),
                            &peer.player,
                            peer.space,
                        );
                        self.counters.view_cells += work.cells;
                        self.counters.visibility_segments += work.segments;
                        if let Ok(Some(update)) =
                            peer.history.build(page, data.version(), cells, force)
                        {
                            output
                                .push(Outgoing::reliable(connection, ServerMessage::Page(update)));
                            self.counters.page_images += 1;
                        }
                        peer.pages.insert(page);
                    } else {
                        self.request_generation(page);
                        output.push(Outgoing::reliable(
                            connection,
                            ServerMessage::PageUnavailable(page),
                        ));
                    }
                }
                if let Some(page) = peer.summaries.pop_front() {
                    if self
                        .observer_page(&peer, page.space)
                        .is_some_and(|center| net::aoi_contains(center, page, 64, 64, false))
                    {
                        if let Some(data) = self.world.page(page) {
                            let (cells, work) = visibility::sanitize(
                                &self.world,
                                page,
                                data.cells(),
                                &peer.player,
                                peer.space,
                            );
                            self.counters.view_cells += work.cells;
                            self.counters.visibility_segments += work.segments;
                            self.counters.summary_cells += core_types::PAGE_VOLUME as u64;
                            output.push(Outgoing::reliable(
                                connection,
                                ServerMessage::Summary(visibility::summary(
                                    page,
                                    data.version(),
                                    &cells,
                                )),
                            ));
                        } else {
                            output.push(Outgoing::reliable(
                                connection,
                                ServerMessage::PageUnavailable(page),
                            ));
                        }
                    } else {
                        output.push(Outgoing::reliable(
                            connection,
                            ServerMessage::PageUnavailable(page),
                        ));
                    }
                }
            }
            self.peers.insert(connection, peer);
        }
        output
    }
    fn observer_page(&self, peer: &Peer, space: SpaceId) -> Option<PageId> {
        if space == peer.space {
            return Some(position_page(space, peer.player.pos));
        }
        let from = self.world.space(peer.space)?.transform;
        let to = self.world.space(space)?.transform;
        let world = transform_point(from, peer.player.pos);
        let translated = std::array::from_fn(|axis| world[axis] - to.translation[axis]);
        let [x, y, z, w] = to.rotation;
        let local = rotate([-x, -y, -z, w], translated);
        if local
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 29_000_000.0)
        {
            return None;
        }
        Some(position_page(space, local))
    }
    fn in_aoi(&self, peer: &Peer, page: PageId, retained: bool) -> bool {
        self.observer_page(peer, page.space)
            .is_some_and(|center| net::aoi_contains(center, page, 3, 4, retained))
    }
    fn request_generation(&mut self, page: PageId) {
        if page.space != SpaceId(0) || self.world.page(page).is_some() || page.y < 0 || page.y >= 8
        {
            return;
        }
        if let Some(generation) = &mut self.generation
            && !generation.ready.iter().any(|(queued, _)| *queued == page)
            && (generation.requested.len() < 32
                || generation.requested.contains_key(&(page.x, page.z)))
        {
            generation
                .requested
                .entry((page.x, page.z))
                .or_default()
                .insert(page);
        }
    }
    fn poll_generation(&mut self, batch: &mut Batch) {
        let Some(generation) = &mut self.generation else {
            return;
        };
        for _ in 0..2 {
            // One completed profile column contributes at most8 pages. Leaving
            // completions in the bounded worker queue supplies producer pressure.
            if generation.ready.len() > 64 - 8 {
                break;
            }
            let Some(completion) = generation.pool.try_recv() else {
                break;
            };
            let Some(column) = generation.dispatched.remove(&completion.id) else {
                continue;
            };
            let requested = generation.requested.remove(&column).unwrap_or_default();
            if let jobs::JobOutcome::Success(result) = completion.outcome {
                for page in requested {
                    if self.world.page(page).is_none()
                        && let Ok(generated) = result.page(page, None)
                    {
                        generation.ready.push_back((page, generated.cells));
                    }
                }
            }
        }
        let candidates: Vec<_> = generation
            .requested
            .keys()
            .filter(|column| !generation.dispatched.values().any(|c| c == *column))
            .take(2)
            .copied()
            .collect();
        for column in candidates {
            let id = jobs::JobId(generation.next_job);
            generation.next_job += 1;
            let page = PageId {
                space: SpaceId(0),
                x: column.0,
                y: 0,
                z: column.1,
            };
            let spec = jobs::JobSpec::derived(
                jobs::JobKey { page, kind: 61 },
                jobs::Priority::Interactive,
                jobs::WorkCost {
                    cells: 18 * 18 * 128,
                    quads: 0,
                    bytes: 256 * 1024,
                    rays: 0,
                },
                256 * 1024,
                self.tick,
            );
            if generation
                .pool
                .try_submit(jobs::Dispatch {
                    id,
                    spec,
                    cancellation: jobs::CancellationToken::new(),
                    payload: column,
                })
                .is_ok()
            {
                generation.dispatched.insert(id, column);
                self.counters.generation_submitted += 1;
            }
        }
        // Page creation classification is bounded to two 4096-cell pages per tick.
        for _ in 0..2 {
            let Some((page, cells)) = generation.ready.pop_front() else {
                break;
            };
            if self.world.page(page).is_some() {
                continue;
            }
            let cells: Box<[Cell]> = cells;
            let mut tx = Transaction::new(self.tick, page.space);
            tx.expect_page(page, self.world.page_version(page));
            tx.create_page(page, cells);
            if let Ok(change) = self.world.commit(tx) {
                batch.changes.push(change);
                self.counters.generation_published += 1;
            }
        }
    }
}

fn random<const N: usize>() -> Result<[u8; N], ServerError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|e| ServerError::Io(std::io::Error::other(e)))?;
    Ok(bytes)
}
fn credential_hash(token: SecretToken) -> [u8; 32] {
    blake3::derive_key("cubi local identity credential v1", token.as_bytes())
}
fn constant_equal(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter()
        .zip(b)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
fn player_state(
    identity: PlayerId,
    space: SpaceId,
    input_sequence: u64,
    player: &Player,
) -> PlayerState {
    PlayerState {
        identity,
        space,
        input_sequence,
        position: player.pos,
        velocity: player.vel,
        yaw: player.yaw,
        pitch: player.pitch,
        health: player.health,
        mode: player.mode as u8,
        on_ground: player.on_ground,
        flying: player.flying,
        noclip: player.noclip,
    }
}
fn peer_state(peer: &Peer) -> PlayerState {
    player_state(peer.identity, peer.space, peer.input_sequence, &peer.player)
}
fn reply(
    id: ActionId,
    status: ActionStatus,
    transaction: Option<TxId>,
    source_version: Version,
    position: Option<BlockPos>,
    cell: Option<Cell>,
) -> ActionReply {
    ActionReply {
        id,
        status,
        transaction,
        source_version,
        position,
        cell,
    }
}
fn remember(state: &mut DedupState, result: ActionReply) {
    state.highest_seen = state.highest_seen.max(result.id.0);
    if !state.replies.iter().any(|old| old.id == result.id) {
        state.replies.push(result);
    }
    while let Some(next) = state.contiguous.checked_add(1) {
        if !state.replies.iter().any(|reply| reply.id.0 == next) {
            break;
        }
        state.contiguous = next;
    }
    while state.replies.len() > save::MAX_RECENT_ACTIONS {
        state.replies.remove(0);
    }
}
fn keys(bits: u8) -> Keys {
    Keys {
        fwd: bits & 1 != 0,
        back: bits & 2 != 0,
        left: bits & 4 != 0,
        right: bits & 8 != 0,
        jump: bits & 16 != 0,
        sneak: bits & 32 != 0,
        sprint: bits & 64 != 0,
    }
}
fn position_page(space: SpaceId, p: [f64; 3]) -> PageId {
    BlockPos {
        space,
        x: p[0].floor() as i32,
        y: p[1].floor() as i32,
        z: p[2].floor() as i32,
    }
    .page()
}
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let xyz = [q[0], q[1], q[2]];
    let t = cross(xyz, v).map(|x| 2.0 * x);
    let c = cross(xyz, t);
    std::array::from_fn(|axis| v[axis] + q[3] * t[axis] + c[axis])
}
fn transform_point(transform: core_types::SpaceTransform, p: [f64; 3]) -> [f64; 3] {
    let r = rotate(transform.rotation, p);
    std::array::from_fn(|axis| r[axis] + transform.translation[axis])
}
fn parse_float(text: &str) -> Result<f64, RejectReason> {
    let value = text.parse::<f64>().map_err(|_| RejectReason::Protocol)?;
    if value.is_finite() && value.abs() < 29_000_000.0 {
        Ok(value)
    } else {
        Err(RejectReason::Protocol)
    }
}
fn edit_reason(error: sim::EditError) -> RejectReason {
    match error {
        sim::EditError::Spectator => RejectReason::Unauthorized,
        sim::EditError::Unknown => RejectReason::Unknown,
        sim::EditError::OutOfReach => RejectReason::OutOfReach,
        sim::EditError::Occluded => RejectReason::Occluded,
        sim::EditError::Protected => RejectReason::Protected,
        sim::EditError::Unbreakable => RejectReason::Unbreakable,
        sim::EditError::Occupied => RejectReason::Occupied,
        sim::EditError::Stale => RejectReason::Stale,
        sim::EditError::InvalidCell => RejectReason::InvalidCell,
        sim::EditError::Unsupported => RejectReason::Unsupported,
        sim::EditError::DuplicateTick => RejectReason::RateLimited,
    }
}
fn load_owner(
    directory: &std::path::Path,
    identities: &BTreeMap<PlayerId, Identity>,
) -> Result<Hello, ServerError> {
    use std::io::{Read, Write};
    let path = directory.join("owner.identity");
    if path.exists() {
        let mut file = std::fs::File::open(path)?;
        if file.metadata()?.len() > 512 {
            return Err(ServerError::Invalid("owner identity length"));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let ClientMessage::Hello(hello) = net::decode_client(&bytes)
            .map_err(|_| ServerError::Invalid("owner identity format"))?
        else {
            return Err(ServerError::Invalid("owner identity format"));
        };
        if hello.credential.is_none() || hello.identity == PlayerId::default() {
            return Err(ServerError::Invalid("owner identity missing credential"));
        }
        if let Some(identity) = identities.get(&hello.identity)
            && (identity.role != Role::Owner
                || identity.credential_hash != credential_hash(hello.credential.expect("checked")))
        {
            return Err(ServerError::Invalid("owner identity mismatch"));
        }
        return Ok(hello);
    }
    if identities.values().any(|i| i.role == Role::Owner) {
        return Err(ServerError::Invalid("restore owner.identity backup"));
    }
    let hello = Hello {
        identity: PlayerId(random::<16>()?),
        name: "owner".into(),
        credential: Some(SecretToken::new(random::<32>()?)),
    };
    let bytes = net::encode_client(&ClientMessage::Hello(hello.clone()))
        .map_err(|_| ServerError::Invalid("owner identity encoding"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(hello)
}

fn survival_reason(error: rules::survival::SurvivalError) -> RejectReason {
    use rules::survival::SurvivalError as E;
    match error {
        E::Capacity => RejectReason::Capacity,
        E::UnknownInput => RejectReason::Unknown,
        E::NoEntitlement | E::NotDurableTool => RejectReason::Unauthorized,
        E::StalePlan => RejectReason::Stale,
        _ => RejectReason::InvalidCell,
    }
}
