//! Transport-independent client authority/prediction owner. Platform sockets feed
//! bounded net frames here; only confirmed, sanitized pages enter its world view.
//! Rendering integration consumes `take_page_updates` at the scene owner boundary.
pub mod audio;
pub mod audio_cues;
pub mod entities;
pub mod machine_packets;
pub mod presence;
pub mod water;
pub mod window;
use core_types::{BlockPos, Cell, PageId, SpaceId, SpaceTransform, Tick, Version};
use net::{
    Action, ActionId, ActionReply, ActionStatus, ClientMessage, FarSummary, Hello, InputFrame,
    InputHistory, PageUpdate, PlayerId, PlayerState, PredictionOverlay, RejectReason, Replica,
    ReplicaError, ReplicaRead, SecretToken, ServerMessage, Welcome,
};
use sim::{Keys, Player, prediction::PredictionState};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use world::CellRead;

const MAX_INPUTS: usize = 256;
const MAX_ACTIONS: usize = 128;
const MAX_OUTBOX: usize = 256;
const MAX_PAGE_UPDATES_PER_DRAIN: usize = 128;
const MAX_PAGE_REMOVALS: usize = 512;
const MAX_SPACES: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct NetworkConfig {
    pub maximum_pages: usize,
    pub near_radius: u16,
    pub retain_radius: u16,
    /// Optional vertical bounds. None preserves the original cubic interest.
    pub near_vertical_radius: Option<u16>,
    pub retain_vertical_radius: Option<u16>,
    pub input_lead_ticks: u64,
    pub simulation_steps_per_poll: u16,
}
impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            maximum_pages: 64,
            near_radius: 1,
            retain_radius: 2,
            near_vertical_radius: None,
            retain_vertical_radius: None,
            input_lead_ticks: 6,
            simulation_steps_per_poll: 32,
        }
    }
}
impl NetworkConfig {
    pub fn validate(self) -> Result<(), NetworkError> {
        let config = self;
        if config.maximum_pages == 0
            || config.maximum_pages > 256
            || config.near_radius > 3
            || config.retain_radius < config.near_radius
            || config.retain_radius > 4
            || config.interest_radii(false)[1] > 3
            || config.interest_radii(false)[1] > config.interest_radii(true)[1]
            || config.interest_radii(true)[1] > 4
            || config.input_lead_ticks == 0
            || config.input_lead_ticks > 32
            || config.simulation_steps_per_poll == 0
            || config.simulation_steps_per_poll > 64
            || config.requested_pages() > config.maximum_pages
        {
            return Err(NetworkError::Capacity);
        }
        Ok(())
    }
    pub fn interest_radii(self, retained: bool) -> [u16; 3] {
        let horizontal = if retained {
            self.retain_radius
        } else {
            self.near_radius
        };
        let vertical = if retained {
            self.retain_vertical_radius
        } else {
            self.near_vertical_radius
        }
        .unwrap_or(horizontal);
        [horizontal, vertical, horizontal]
    }
    pub fn requested_pages(self) -> usize {
        self.interest_radii(false)
            .into_iter()
            .map(|radius| 2 * usize::from(radius) + 1)
            .fold(1, usize::saturating_mul)
    }
    pub(crate) fn contains(self, center: PageId, page: PageId, retained: bool) -> bool {
        center.space == page.space
            && [center.x, center.y, center.z]
                .into_iter()
                .zip([page.x, page.y, page.z])
                .zip(self.interest_radii(retained))
                .all(|((center, page), radius)| {
                    (i64::from(center) - i64::from(page)).abs() <= i64::from(radius)
                })
    }
    /// 3³ visible pages plus the one-page meshing halo require5³ confirmed pages.
    pub fn remote_scene() -> Self {
        Self {
            maximum_pages: 256,
            near_radius: 2,
            retain_radius: 3,
            ..Self::default()
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkError {
    Protocol,
    Capacity,
    NotConnected,
    Unknown,
    Simulation(&'static str),
    Rejected(RejectReason),
}
#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkCounters {
    pub authoritative_states: u64,
    pub corrections: u64,
    pub prediction_steps: u64,
    pub replay_steps: u64,
    pub unknown_steps: u64,
    pub page_cells: u64,
    pub full_snapshots: u64,
    pub deltas: u64,
    pub missing_bases: u64,
    pub rejected_actions: u64,
    pub accepted_actions: u64,
    pub evicted_pages: u64,
    pub peak_inputs: usize,
    pub peak_actions: usize,
    pub peak_outbox: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueDepths {
    pub cells: usize,
    pub light: usize,
    pub geometry: usize,
    pub height: usize,
    pub outgoing: usize,
    pub props: usize,
}
#[derive(Clone, Copy)]
struct PredictedTick {
    state: PredictionState,
    input_sequence: u64,
}
struct PageInterest {
    received: bool,
    last_request: u64,
}
#[derive(Clone)]
struct PendingAction {
    action: Action,
    aim: Option<[f32; 2]>,
    last_sent: u64,
    inventory_before: Option<u64>,
}

pub struct Connection {
    config: NetworkConfig,
    welcome: Option<Welcome>,
    credential: Option<SecretToken>,
    handshake: Option<Hello>,
    last_hello: u64,
    replica: Replica,
    overlay: PredictionOverlay,
    player: Option<Player>,
    using_food: Option<ActionId>,
    movement_replay: Box<sim::replay::Recorder>,
    replay_time_ns: u64,
    input_history: InputHistory,
    inputs: BTreeMap<u64, InputFrame>,
    next_input: u64,
    predicted_input: u64,
    predicted_tick: Tick,
    last_input_tick: Tick,
    predicted: BTreeMap<u64, PredictedTick>,
    last_authority: Option<(Tick, u64, PredictionState)>,
    pending_authority: Option<(Tick, u64, PredictionState)>,
    latest_tick: Tick,
    last_received_at: u64,
    visual_tick: Tick,
    visual_received_at: u64,
    replay_until: Tick,
    world_changed: bool,
    actions: BTreeMap<ActionId, PendingAction>,
    next_action: u64,
    replies: VecDeque<ActionReply>,
    outbox: VecDeque<ClientMessage>,
    last_input_send: u64,
    interest: BTreeMap<PageId, PageInterest>,
    summaries: BTreeMap<PageId, FarSummary>,
    transforms: BTreeMap<SpaceId, (Version, SpaceTransform)>,
    remote_players: BTreeMap<PlayerId, PlayerState>,
    remote_tick: Tick,
    inventory: Option<rules::survival::Inventory>,
    item_attributes: Option<net::item_attributes::InventoryAttributes>,
    audio_cues: Box<audio_cues::AudioCues>,
    presence: presence::PresenceCache,
    window: window::WindowCache,
    entities: entities::EntityReplica,
    page_updates: VecDeque<PageUpdate>,
    page_removals: VecDeque<PageId>,
    geometry_interest: BTreeMap<PageId, PageInterest>,
    height_geometry_interest: BTreeSet<PageId>,
    geometries: BTreeMap<PageId, net::PublicGeometry>,
    geometry_updates: VecDeque<net::PublicGeometry>,
    geometry_removals: VecDeque<PageId>,
    height_interest: BTreeMap<net::height::HeightKey, (Option<u64>, u64, bool)>,
    height_exact: BTreeSet<net::height::HeightKey>,
    height_updates: VecDeque<Box<net::height::HeightTile>>,
    water: water::WaterState,
    lights: net::light::Replica,
    light_updates: VecDeque<net::light::Update>,
    props: BTreeMap<PageId, std::sync::Arc<net::props::Frame>>,
    machine_packets: machine_packets::PacketScene,
    props_updates: VecDeque<std::sync::Arc<net::props::Frame>>,
    counters: NetworkCounters,
}

impl Connection {
    pub fn new(config: NetworkConfig) -> Result<Self, NetworkError> {
        config.validate()?;
        Ok(Self {
            config,
            welcome: None,
            credential: None,
            handshake: None,
            last_hello: 0,
            replica: Replica::new(config.maximum_pages),
            overlay: PredictionOverlay::new(MAX_ACTIONS),
            player: None,
            using_food: None,
            movement_replay: Box::new(
                sim::replay::Recorder::new(sim::replay::DEFAULT_STEPS)
                    .map_err(NetworkError::Simulation)?,
            ),
            replay_time_ns: 0,
            input_history: InputHistory::new(MAX_INPUTS),
            inputs: BTreeMap::new(),
            next_input: 1,
            predicted_input: 0,
            predicted_tick: Tick(0),
            last_input_tick: Tick(0),
            predicted: BTreeMap::new(),
            last_authority: None,
            pending_authority: None,
            latest_tick: Tick(0),
            last_received_at: 0,
            visual_tick: Tick(0),
            visual_received_at: 0,
            replay_until: Tick(0),
            world_changed: false,
            actions: BTreeMap::new(),
            next_action: 1,
            replies: VecDeque::new(),
            outbox: VecDeque::new(),
            last_input_send: 0,
            interest: BTreeMap::new(),
            summaries: BTreeMap::new(),
            transforms: BTreeMap::new(),
            remote_players: BTreeMap::new(),
            remote_tick: Tick(0),
            inventory: None,
            item_attributes: None,
            audio_cues: Box::new(audio_cues::AudioCues::default()),
            presence: presence::PresenceCache::new(),
            window: window::WindowCache::default(),
            entities: entities::EntityReplica::new(net::entities::MAX_ENTITIES)
                .map_err(|_| NetworkError::Capacity)?,
            page_updates: VecDeque::new(),
            page_removals: VecDeque::new(),
            geometry_interest: BTreeMap::new(),
            height_geometry_interest: BTreeSet::new(),
            geometries: BTreeMap::new(),
            geometry_updates: VecDeque::new(),
            geometry_removals: VecDeque::new(),
            height_interest: BTreeMap::new(),
            height_exact: BTreeSet::new(),
            height_updates: VecDeque::new(),
            water: water::WaterState::default(),
            lights: net::light::Replica::new(config.maximum_pages)
                .map_err(|_| NetworkError::Capacity)?,
            light_updates: VecDeque::new(),
            props: BTreeMap::new(),
            machine_packets: machine_packets::PacketScene::new(config.maximum_pages, 2048)
                .map_err(|_| NetworkError::Capacity)?,
            props_updates: VecDeque::new(),
            counters: NetworkCounters::default(),
        })
    }
    pub fn hello(
        &mut self,
        name: String,
        identity: PlayerId,
        credential: Option<SecretToken>,
    ) -> Result<(), NetworkError> {
        let hello = Hello {
            name,
            identity,
            credential,
        };
        let message = ClientMessage::Hello(hello.clone());
        net::encode_client(&message).map_err(|_| NetworkError::Protocol)?;
        self.credential = credential;
        self.handshake = Some(hello);
        self.queue(message)
    }
    pub fn welcome(&self) -> Option<&Welcome> {
        self.welcome.as_ref()
    }
    pub fn credential(&self) -> Option<SecretToken> {
        self.credential
    }
    pub fn queue_depths(&self) -> QueueDepths {
        QueueDepths {
            cells: self.page_updates.len(),
            light: self.light_updates.len(),
            geometry: self.geometry_updates.len(),
            height: self.height_updates.len(),
            outgoing: self.outbox.len(),
            props: self.props_updates.len(),
        }
    }
    pub fn movement_replay(&self) -> &sim::replay::Recorder {
        &self.movement_replay
    }
    fn note_replay_time(&mut self, now_ms: u64) {
        self.replay_time_ns = self.replay_time_ns.max(now_ms.saturating_mul(1_000_000));
        self.movement_replay
            .set_time(self.replay_time_ns)
            .expect("clamped replay wall clock");
    }
    pub fn player(&self) -> Option<&Player> {
        self.player.as_ref()
    }
    /// Last decoded authority checkpoint, without collision scratch allocation
    /// or local prediction. Diagnostic consumers must not label player() as
    /// confirmed movement/health after it has advanced beyond this tick.
    pub fn confirmed_motion(&self) -> Option<(Tick, sim::prediction::AudioMotion)> {
        let (tick, _, state) = self.last_authority.as_ref()?;
        Some((*tick, state.audio_motion().ok()?))
    }
    pub fn replica(&self) -> &Replica {
        &self.replica
    }
    pub fn read(&self, position: BlockPos) -> ReplicaRead {
        self.overlay.read(position, &self.replica)
    }
    pub fn predicted_tick(&self) -> Tick {
        self.predicted_tick
    }
    pub fn acknowledged_tick(&self) -> Tick {
        self.latest_tick
    }
    pub fn pending_inputs(&self) -> usize {
        self.inputs.len()
    }
    pub fn pending_actions(&self) -> usize {
        self.actions.len()
    }
    pub fn predicted_edits(&self) -> usize {
        self.overlay.len()
    }
    pub fn counters(&self) -> NetworkCounters {
        self.counters
    }
    pub fn transform(&self, space: SpaceId) -> Option<(Version, SpaceTransform)> {
        self.transforms.get(&space).copied()
    }
    pub fn summary(&self, page: PageId) -> Option<&FarSummary> {
        self.summaries.get(&page)
    }
    pub fn remote_players_tick(&self) -> Tick {
        self.remote_tick
    }
    pub fn remote_players(&self) -> impl Iterator<Item = &PlayerState> {
        self.remote_players.values()
    }
    pub fn window(&self) -> Option<&net::window::WindowSnapshot> {
        self.inventory
            .as_ref()
            .and_then(|i| self.window.matching_inventory(i.revision()))
    }
    pub fn presence(&self) -> &presence::PresenceCache {
        &self.presence
    }
    pub fn current_role(&self) -> Option<net::Role> {
        self.presence.own_role()
    }
    pub fn entity_replica(&self) -> &entities::EntityReplica {
        &self.entities
    }
    pub fn inventory(&self) -> Option<&rules::survival::Inventory> {
        self.inventory.as_ref()
    }
    pub fn item_attributes(&self) -> Option<&net::item_attributes::InventoryAttributes> {
        self.item_attributes.as_ref().filter(|a| {
            self.inventory
                .as_ref()
                .is_some_and(|i| i.revision() == a.inventory_revision)
        })
    }
    pub fn audio_cues(&self) -> &audio_cues::AudioCues {
        &self.audio_cues
    }
    pub fn predicted_inventory(&self) -> Option<rules::survival::Inventory> {
        let mut inventory = self.inventory?;
        let creative = self
            .player
            .as_ref()
            .is_some_and(|p| p.mode == sim::GameMode::Creative)
            && self.welcome.as_ref().is_some_and(|w| w.role.can_admin());
        for action in self.actions.values() {
            if action.inventory_before != Some(inventory.revision()) {
                continue;
            }
            match &action.action {
                Action::SelectSlot(slot) => {
                    inventory.select(usize::from(*slot)).ok()?;
                }
                Action::PlaceOwned {
                    cell,
                    inventory: guard,
                    ..
                } if !creative && guard.revision == inventory.revision() => {
                    if let Ok(plan) = inventory.plan_place(inventory.hand(), *cell) {
                        inventory = *plan.after();
                    }
                }
                _ => {}
            }
        }
        Some(inventory)
    }
    pub fn geometry(&self, page: PageId) -> Option<&net::PublicGeometry> {
        self.geometries.get(&page)
    }
    pub fn take_geometry_updates(&mut self, limit: usize) -> Vec<net::PublicGeometry> {
        (0..limit.min(32))
            .filter_map(|_| self.geometry_updates.pop_front())
            .collect()
    }
    pub fn take_geometry_removal(&mut self) -> Option<PageId> {
        self.geometry_removals.pop_front()
    }
    pub fn request_geometry(&mut self, page: PageId, now_ms: u64) -> Result<(), NetworkError> {
        if !self.geometry_interest.contains_key(&page)
            && self.geometry_interest.len() >= net::MAX_PUBLIC_GEOMETRY_PAGES
        {
            return Err(NetworkError::Capacity);
        }
        self.height_geometry_interest.remove(&page);
        let known = self.geometries.get(&page).map(|g| g.source_version);
        self.queue(ClientMessage::RequestGeometry { page, known })?;
        self.geometry_interest.insert(
            page,
            PageInterest {
                received: known.is_some(),
                last_request: now_ms,
            },
        );
        Ok(())
    }
    pub fn release_geometry(&mut self, page: PageId) -> Result<(), NetworkError> {
        if self.outbox.len() >= MAX_OUTBOX || self.geometry_removals.len() >= 32 {
            return Err(NetworkError::Capacity);
        }
        self.queue(ClientMessage::ForgetGeometry(page))?;
        self.geometry_interest.remove(&page);
        self.height_geometry_interest.remove(&page);
        self.geometry_updates.retain(|g| g.page != page);
        if self.geometries.remove(&page).is_some() {
            self.geometry_removals.push_back(page);
        }
        Ok(())
    }
    pub fn request_height(
        &mut self,
        key: net::height::HeightKey,
        now_ms: u64,
    ) -> Result<(), NetworkError> {
        if self.height_exact.contains(&key) {
            return self.request_height_exact(key, now_ms);
        }
        key.validate().map_err(|_| NetworkError::Protocol)?;
        if !self.height_interest.contains_key(&key)
            && self.height_interest.len() >= net::height::MAX_TILES
        {
            return Err(NetworkError::Capacity);
        }
        let known = self.height_interest.get(&key).and_then(|i| i.0);
        self.queue(ClientMessage::RequestHeight { key, known })?;
        self.height_interest.insert(key, (known, now_ms, false));
        Ok(())
    }
    pub fn height_is_exact(&self, key: net::height::HeightKey) -> bool {
        self.height_exact.contains(&key)
    }
    pub fn request_height_exact(
        &mut self,
        key: net::height::HeightKey,
        now_ms: u64,
    ) -> Result<(), NetworkError> {
        key.validate().map_err(|_| NetworkError::Protocol)?;
        if key.level != 0 {
            return Err(NetworkError::Protocol);
        }
        if !self.height_interest.contains_key(&key)
            && self.height_interest.len() >= net::height::MAX_TILES
        {
            return Err(NetworkError::Capacity);
        }
        // Representation promotion keeps the source stamp, so a source-only
        // conditional request cannot distinguish it from the prior height mesh.
        self.queue(ClientMessage::RequestExactHeight { key, known: None })?;
        self.height_exact.insert(key);
        let known = self.height_interest.get(&key).and_then(|i| i.0);
        self.height_interest.insert(key, (known, now_ms, false));
        Ok(())
    }
    pub fn release_height(&mut self, key: net::height::HeightKey) -> Result<(), NetworkError> {
        let pages: Vec<_> = self
            .height_geometry_interest
            .iter()
            .filter(|p| key.level == 0 && p.space.0 == key.space && p.x == key.x && p.z == key.z)
            .copied()
            .collect();
        if self.outbox.len() + pages.len() + 1 > MAX_OUTBOX
            || self.geometry_removals.len() + pages.len() > 32
        {
            return Err(NetworkError::Capacity);
        }
        self.queue(ClientMessage::ForgetHeight(key))?;
        for page in pages {
            self.release_geometry(page)?;
        }
        self.height_interest.remove(&key);
        self.height_exact.remove(&key);
        self.height_updates.retain(|t| t.key != key);
        Ok(())
    }
    pub fn take_height_update(&mut self) -> Option<Box<net::height::HeightTile>> {
        self.height_updates.pop_front()
    }
    fn queue_light_update(&mut self, update: net::light::Update) -> Result<(), NetworkError> {
        let page = update.header().page;
        if let Some(old) = self
            .light_updates
            .iter_mut()
            .find(|u| u.header().page == page)
        {
            match (old, &update) {
                (net::light::Update::Snapshot(a), net::light::Update::Delta(b))
                    if a.header.version == b.base =>
                {
                    for &(index, value) in &b.changes {
                        a.values[usize::from(index)] = value;
                    }
                    a.header = b.header;
                }
                (net::light::Update::Delta(a), net::light::Update::Delta(b))
                    if a.header.version == b.base =>
                {
                    let mut changes: BTreeMap<_, _> = a.changes.iter().copied().collect();
                    changes.extend(b.changes.iter().copied());
                    a.changes = changes.into_iter().collect();
                    a.header = b.header;
                }
                (old, _) => *old = update,
            }
            return Ok(());
        }
        if self.light_updates.len() >= self.config.maximum_pages {
            return Err(NetworkError::Capacity);
        }
        self.light_updates.push_back(update);
        Ok(())
    }
    fn queue_page_update(&mut self, update: PageUpdate) -> Result<(), NetworkError> {
        let page = match &update {
            PageUpdate::Snapshot(p) => p.page,
            PageUpdate::Delta(p) => p.page,
        };
        if let Some(old) = self.page_updates.iter_mut().find(|u| match u {
            PageUpdate::Snapshot(p) => p.page == page,
            PageUpdate::Delta(p) => p.page == page,
        }) {
            match (old, &update) {
                (PageUpdate::Snapshot(a), PageUpdate::Delta(b)) if a.version == b.base => {
                    for &(index, value) in &b.changes {
                        a.cells[usize::from(index)] = value;
                    }
                    a.version = b.version;
                    a.source_version = b.source_version;
                }
                (PageUpdate::Delta(a), PageUpdate::Delta(b)) if a.version == b.base => {
                    let mut changes: BTreeMap<_, _> = a.changes.iter().copied().collect();
                    changes.extend(b.changes.iter().copied());
                    a.changes = changes.into_iter().collect();
                    a.version = b.version;
                    a.source_version = b.source_version;
                }
                (old, _) => *old = update,
            }
            return Ok(());
        }
        if self.page_updates.len() >= self.config.maximum_pages {
            return Err(NetworkError::Capacity);
        }
        self.page_updates.push_back(update);
        Ok(())
    }
    pub fn request_light_snapshot(&mut self, page: PageId) -> Result<(), NetworkError> {
        let cell_view = self
            .replica
            .page(page)
            .ok_or(NetworkError::Protocol)?
            .version;
        if self.outbox.len() >= MAX_OUTBOX {
            return Err(NetworkError::Capacity);
        }
        self.queue(ClientMessage::RequestLight(net::light::Request {
            page,
            cell_view,
            acknowledged: None,
        }))?;
        self.lights.remove(page);
        self.light_updates.retain(|u| u.header().page != page);
        Ok(())
    }
    pub fn light_replica(&self) -> &net::light::Replica {
        &self.lights
    }
    pub fn packet_scene(&self) -> &machine_packets::PacketScene {
        &self.machine_packets
    }
    pub fn props_frame(&self, page: PageId) -> Option<&net::props::Frame> {
        self.props
            .get(&page)
            .filter(|f| {
                self.replica
                    .page(page)
                    .is_some_and(|p| p.version == f.cell_view)
            })
            .map(|f| f.as_ref())
    }
    pub fn props_frames(&self) -> impl Iterator<Item = &std::sync::Arc<net::props::Frame>> {
        self.props.values().filter(|f| {
            self.replica
                .page(f.page)
                .is_some_and(|p| p.version == f.cell_view)
        })
    }
    pub fn take_props_update(&mut self) -> Option<std::sync::Arc<net::props::Frame>> {
        self.props_updates.pop_front()
    }
    fn queue_props_update(&mut self, frame: std::sync::Arc<net::props::Frame>) {
        if let Some(old) = self.props_updates.iter_mut().find(|f| f.page == frame.page) {
            *old = frame;
        } else if self.props_updates.len() < self.config.maximum_pages {
            self.props_updates.push_back(frame);
        }
    }
    pub fn take_light_update(&mut self) -> Option<net::light::Update> {
        self.light_updates.pop_front()
    }
    pub fn take_reply(&mut self) -> Option<ActionReply> {
        self.replies.pop_front()
    }
    pub fn take_page_updates(&mut self, limit: usize) -> Vec<PageUpdate> {
        (0..limit.min(MAX_PAGE_UPDATES_PER_DRAIN))
            .filter_map(|_| self.page_updates.pop_front())
            .collect()
    }
    pub fn take_page_removal(&mut self) -> Option<PageId> {
        self.page_removals.pop_front()
    }
    pub fn take_page_removals(&mut self, limit: usize) -> Vec<PageId> {
        (0..limit.min(MAX_PAGE_REMOVALS))
            .filter_map(|_| self.page_removals.pop_front())
            .collect()
    }
    pub fn predicted_positions(&self) -> impl Iterator<Item = BlockPos> + '_ {
        self.overlay.positions()
    }
    pub fn take_outgoing(&mut self, limit: usize) -> Vec<ClientMessage> {
        (0..limit.min(MAX_OUTBOX))
            .filter_map(|_| self.outbox.pop_front())
            .collect()
    }
    fn queue(&mut self, message: ClientMessage) -> Result<(), NetworkError> {
        // ACKs describe retained public state, so a newer ACK supersedes an
        // unsent older one. A slow render/loading frame never builds an ACK log.
        let same = |old: &ClientMessage| match (old, &message) {
            (
                ClientMessage::Action { id: a, .. } | ClientMessage::AimedAction { id: a, .. },
                ClientMessage::Action { id: b, .. } | ClientMessage::AimedAction { id: b, .. },
            ) => a == b,
            (ClientMessage::AckPage { page: a, .. }, ClientMessage::AckPage { page: b, .. }) => {
                a == b
            }
            (ClientMessage::RequestLight(a), ClientMessage::RequestLight(b)) => a.page == b.page,
            (ClientMessage::RequestWater(a), ClientMessage::RequestWater(b)) => a.page == b.page,
            (ClientMessage::RequestWater(a), ClientMessage::ForgetWater(b)) => a.page == *b,
            (ClientMessage::ForgetWater(a), ClientMessage::RequestWater(b)) => *a == b.page,
            (ClientMessage::ForgetWater(a), ClientMessage::ForgetWater(b)) => a == b,
            (ClientMessage::MachinePackets(a), ClientMessage::MachinePackets(b)) => {
                a.page() == b.page()
            }
            (
                ClientMessage::RequestPage { page: a, .. },
                ClientMessage::RequestPage { page: b, .. },
            ) => a == b,
            (ClientMessage::ForgetPage(a), ClientMessage::ForgetPage(b)) => a == b,
            (ClientMessage::ForgetPage(a), ClientMessage::RequestPage { page: b, .. })
            | (ClientMessage::RequestPage { page: b, .. }, ClientMessage::ForgetPage(a)) => a == b,
            (ClientMessage::Entities(_), ClientMessage::Entities(_)) => true,
            (
                ClientMessage::RequestHeight { key: a, .. }
                | ClientMessage::RequestExactHeight { key: a, .. },
                ClientMessage::RequestHeight { key: b, .. }
                | ClientMessage::RequestExactHeight { key: b, .. },
            ) => a == b,
            _ => false,
        };
        if let Some(old) = self.outbox.iter_mut().find(|old| same(old)) {
            let replace = match (&*old, &message) {
                (
                    ClientMessage::Entities(net::entities::EntityRequest::Full { .. }),
                    ClientMessage::Entities(net::entities::EntityRequest::Ack { .. }),
                ) => false,
                (
                    ClientMessage::Entities(net::entities::EntityRequest::Ack { revision: a }),
                    ClientMessage::Entities(net::entities::EntityRequest::Ack { revision: b }),
                ) => b >= a,
                (ClientMessage::RequestExactHeight { .. }, ClientMessage::RequestHeight { .. }) => {
                    false
                }
                (
                    ClientMessage::AckPage { version: a, .. },
                    ClientMessage::AckPage { version: b, .. },
                ) => b >= a,
                (ClientMessage::MachinePackets(a), ClientMessage::MachinePackets(b)) => {
                    use net::machine_packets::Request;
                    b.view_version() > a.view_version()
                        || (b.view_version() == a.view_version()
                            && match (a, b) {
                                (Request::Full { .. }, Request::Ack { .. }) => false,
                                (
                                    Request::Ack { revision: a, .. },
                                    Request::Ack { revision: b, .. },
                                ) => b >= a,
                                _ => true,
                            })
                }
                (ClientMessage::RequestLight(a), ClientMessage::RequestLight(b)) => {
                    b.cell_view >= a.cell_view
                }
                (ClientMessage::RequestWater(a), ClientMessage::RequestWater(b)) => {
                    if a.geometry_source == b.geometry_source
                        && a.geometry_view == b.geometry_view
                        && a.column_source == b.column_source
                    {
                        match (a.acknowledged, b.acknowledged) {
                            (None, Some(_)) => false,
                            (Some(a), Some(b)) => b >= a,
                            _ => true,
                        }
                    } else {
                        b.geometry_view >= a.geometry_view && b.column_source >= a.column_source
                    }
                }
                _ => true,
            };
            if replace {
                *old = message;
            }
            return Ok(());
        }
        if self.outbox.len() >= MAX_OUTBOX {
            return Err(NetworkError::Capacity);
        }
        self.outbox.push_back(message);
        self.counters.peak_outbox = self.counters.peak_outbox.max(self.outbox.len());
        Ok(())
    }
    pub fn request_page(&mut self, page: PageId, now_ms: u64) -> Result<(), NetworkError> {
        if !self.interest.contains_key(&page) && self.interest.len() >= self.config.maximum_pages {
            return Err(NetworkError::Capacity);
        }
        let acknowledged = self.replica.page(page).map(|p| p.version);
        self.queue(ClientMessage::RequestPage { page, acknowledged })?;
        self.interest
            .entry(page)
            .and_modify(|i| i.last_request = now_ms)
            .or_insert(PageInterest {
                received: false,
                last_request: now_ms,
            });
        Ok(())
    }
    pub fn request_summary(&mut self, page: PageId) -> Result<(), NetworkError> {
        self.queue(ClientMessage::RequestSummary { page })
    }
    pub fn request_snapshot(&mut self, page: PageId, now_ms: u64) -> Result<(), NetworkError> {
        if !self.interest.contains_key(&page) || self.outbox.len() >= MAX_OUTBOX {
            return Err(NetworkError::Capacity);
        }
        self.machine_packets.remove_page(page);
        self.props.remove(&page);
        self.props_updates.retain(|f| f.page != page);
        if self.replica.remove(page) {
            self.world_changed = true;
        }
        self.interest.insert(
            page,
            PageInterest {
                received: false,
                last_request: now_ms,
            },
        );
        self.queue(ClientMessage::RequestPage {
            page,
            acknowledged: None,
        })
    }

    pub fn receive(&mut self, message: ServerMessage, now_ms: u64) -> Result<(), NetworkError> {
        self.note_replay_time(now_ms);
        match message {
            ServerMessage::AudioCue(cue) => {
                self.audio_cues
                    .push(cue)
                    .map_err(|_| NetworkError::Protocol)?;
            }
            ServerMessage::ItemAttributes(attributes) => {
                attributes.validate().map_err(|_| NetworkError::Protocol)?;
                if self
                    .item_attributes
                    .as_ref()
                    .is_some_and(|old| old.inventory_revision > attributes.inventory_revision)
                {
                    return Ok(());
                }
                if self.item_attributes.as_ref().is_some_and(|old| {
                    old.inventory_revision == attributes.inventory_revision
                        && old != attributes.as_ref()
                }) {
                    return Err(NetworkError::Protocol);
                }
                self.item_attributes = Some(*attributes);
            }
            ServerMessage::Inventory { data } => {
                let inventory = rules::survival::Inventory::decode(&data)
                    .map_err(|_| NetworkError::Protocol)?;
                if self
                    .inventory
                    .as_ref()
                    .is_some_and(|old| old.revision() > inventory.revision())
                {
                    return Ok(());
                }
                if self
                    .inventory
                    .as_ref()
                    .is_some_and(|old| old.revision() == inventory.revision() && *old != inventory)
                {
                    return Err(NetworkError::Protocol);
                }
                self.inventory = Some(inventory);
            }
            ServerMessage::Welcome(welcome) => {
                if welcome.rules_version != net::RULES_VERSION
                    || welcome.generator_version != terrain_gen::GENERATOR_VERSION
                {
                    return Err(NetworkError::Protocol);
                }
                if self.welcome.as_ref().is_some_and(|old| {
                    old.identity != welcome.identity || old.session != welcome.session
                }) {
                    return Err(NetworkError::Protocol);
                }
                if self.welcome.is_some() {
                    return Ok(());
                }
                self.credential = welcome.new_credential.or(self.credential);
                self.next_action = welcome.next_action.0;
                self.latest_tick = welcome.tick;
                self.last_received_at = now_ms;
                self.visual_tick = welcome.tick;
                self.visual_received_at = now_ms;
                self.presence.reset(welcome.identity);
                *self.audio_cues = audio_cues::AudioCues::default();
                self.welcome = Some(welcome);
                self.handshake = None;
            }
            ServerMessage::Prediction {
                tick,
                player,
                continuation,
            } => {
                let welcome = self.welcome.as_ref().ok_or(NetworkError::NotConnected)?;
                if player.identity != welcome.identity || player.space != welcome.player.space {
                    return Err(NetworkError::Protocol);
                }
                let state =
                    PredictionState::decode(&continuation).map_err(NetworkError::Simulation)?;
                let decoded = state.to_player().map_err(NetworkError::Simulation)?;
                if !matches_public(&decoded, player) {
                    return Err(NetworkError::Protocol);
                }
                if self.last_authority.is_some_and(|(old, _, _)| tick <= old) {
                    return Ok(());
                }
                let equal = self.predicted.get(&tick.0).is_some_and(|old| {
                    old.state == state && old.input_sequence == player.input_sequence
                });
                if !equal {
                    if self.predicted.contains_key(&tick.0) {
                        self.counters.corrections += 1;
                    }
                    self.replay_until = Tick(self.replay_until.0.max(self.predicted_tick.0));
                    self.pending_authority = Some((tick, player.input_sequence, state));
                }
                self.last_authority = Some((tick, player.input_sequence, state));
                self.latest_tick = tick;
                self.last_received_at = now_ms;
                self.observe_visual_tick(tick, now_ms);
                self.input_history.acknowledge(player.input_sequence);
                self.inputs
                    .retain(|sequence, _| *sequence > player.input_sequence);
                self.predicted.retain(|old, _| *old >= tick.0);
                self.counters.authoritative_states += 1;
            }
            ServerMessage::Page(update) => {
                let (page, cells) = match &update {
                    PageUpdate::Snapshot(s) => (s.page, core_types::PAGE_VOLUME),
                    PageUpdate::Delta(d) => (d.page, d.changes.len()),
                };
                // A delayed packet for an evicted AOI cannot consume queue
                // credit or repopulate the confirmed cache.
                if !self.interest.contains_key(&page) {
                    return Ok(());
                }
                let already_queued = self.page_updates.iter().any(|update| match update {
                    PageUpdate::Snapshot(image) => image.page == page,
                    PageUpdate::Delta(image) => image.page == page,
                });
                if !already_queued && self.page_updates.len() >= self.config.maximum_pages {
                    return Err(NetworkError::Capacity);
                }
                let needed = usize::from(
                    !self
                        .outbox
                        .iter()
                        .any(|m| matches!(m,ClientMessage::AckPage{page:p,..} if *p==page)),
                ) + usize::from(
                    !self
                        .outbox
                        .iter()
                        .any(|m| matches!(m,ClientMessage::RequestLight(r) if r.page==page)),
                );
                if self.outbox.len() + needed > MAX_OUTBOX {
                    return Err(NetworkError::Capacity);
                }
                match self.replica.apply(update.clone()) {
                    Ok(net::ApplyResult::Applied) => {
                        let version = self.replica.page(page).expect("applied page").version;
                        self.queue(ClientMessage::AckPage { page, version })?;
                        // A packet for a future binding may already have been
                        // rejected before this cell image arrived. Request a
                        // binding fallback rather than ACK an older binding
                        // whose credit cannot acknowledge that discarded image.
                        let acknowledged = self
                            .lights
                            .page(page)
                            .filter(|s| s.header.cell_view == version)
                            .map(|s| s.header.version);
                        self.queue(ClientMessage::RequestLight(net::light::Request {
                            page,
                            cell_view: version,
                            acknowledged,
                        }))?;
                        self.interest
                            .get_mut(&page)
                            .expect("interest checked")
                            .received = true;
                        self.counters.page_cells += cells as u64;
                        match &update {
                            PageUpdate::Snapshot(_) => self.counters.full_snapshots += 1,
                            PageUpdate::Delta(_) => self.counters.deltas += 1,
                        }
                        self.queue_page_update(update)?;
                        if let Some(frame) = self
                            .props
                            .get(&page)
                            .filter(|f| f.cell_view == version)
                            .cloned()
                        {
                            self.queue_props_update(frame);
                        }
                        self.overlay.retire_applied(&self.replica);
                        self.world_changed = true;
                    }
                    Ok(net::ApplyResult::Stale) => {
                        if let Some(image) = self.replica.page(page) {
                            self.queue(ClientMessage::AckPage {
                                page,
                                version: image.version,
                            })?;
                        }
                    }
                    Err(ReplicaError::MissingBase { .. }) => {
                        self.counters.missing_bases += 1;
                        self.queue(ClientMessage::RequestPage {
                            page,
                            acknowledged: None,
                        })?;
                    }
                    Err(_) => return Err(NetworkError::Protocol),
                }
            }
            ServerMessage::PageUnavailable(page) => {
                if let Some(interest) = self.interest.get_mut(&page) {
                    interest.received = false;
                    interest.last_request = now_ms;
                }
                if self.replica.page(page).is_some()
                    && self.page_removals.len() >= MAX_PAGE_REMOVALS
                {
                    return Err(NetworkError::Capacity);
                }
                self.machine_packets.remove_page(page);
                self.props.remove(&page);
                self.props_updates.retain(|f| f.page != page);
                self.lights.remove(page);
                self.light_updates.retain(|u| u.header().page != page);
                self.page_updates.retain(|u| match u {
                    PageUpdate::Snapshot(p) => p.page != page,
                    PageUpdate::Delta(p) => p.page != page,
                });
                self.outbox.retain(|message| {
                    !matches!(message,
                    ClientMessage::AckPage { page: p, .. } if *p == page)
                        && !matches!(message, ClientMessage::RequestLight(r) if r.page == page)
                });
                if self.replica.remove(page) {
                    self.page_removals.push_back(page);
                    self.world_changed = true;
                }
            }
            ServerMessage::Action(reply) => {
                self.overlay.reply(reply);
                self.overlay.retire_applied(&self.replica);
                // An authoritative terminal reply makes every unsent retry
                // redundant. Retire only this ID; other actions keep FIFO order.
                if !matches!(reply.status, ActionStatus::Pending) {
                    self.outbox.retain(|message| !matches!(message,ClientMessage::Action{id,..}|ClientMessage::AimedAction{id,..} if *id==reply.id));
                }
                match reply.status {
                    ActionStatus::Pending => {}
                    ActionStatus::Accepted => {
                        if self.actions.remove(&reply.id).is_some() {
                            self.counters.accepted_actions += 1;
                        }
                    }
                    ActionStatus::Rejected(_) => {
                        if self.actions.remove(&reply.id).is_some() {
                            self.counters.rejected_actions += 1;
                            // Only this still-pending start owns the local
                            // slowdown. Old or duplicate replies cannot cancel
                            // a newer start (or an already accepted one).
                            if self.using_food == Some(reply.id) {
                                self.using_food = None;
                            }
                        }
                        self.world_changed = true;
                    }
                }
                if self.replies.len() >= MAX_ACTIONS {
                    self.replies.pop_front();
                }
                self.replies.push_back(reply);
            }
            ServerMessage::Transform {
                space,
                version,
                transform,
            } => {
                if !transform.is_valid() {
                    return Err(NetworkError::Protocol);
                }
                if self
                    .transforms
                    .get(&space)
                    .is_some_and(|(v, _)| *v > version)
                {
                    return Ok(());
                }
                if self
                    .transforms
                    .get(&space)
                    .is_some_and(|(v, t)| *v == version && *t != transform)
                {
                    return Err(NetworkError::Protocol);
                }
                if !self.transforms.contains_key(&space) && self.transforms.len() >= MAX_SPACES {
                    return Err(NetworkError::Capacity);
                }
                self.transforms.insert(space, (version, transform));
            }
            ServerMessage::Geometry(geometry) => {
                let geometry = *geometry;
                if !self.geometry_interest.contains_key(&geometry.page) {
                    let page = geometry.page;
                    let height_column = (0..32).contains(&page.y)
                        && self.height_interest.keys().any(|key| {
                            key.level == 0
                                && key.space == page.space.0
                                && key.x == page.x
                                && key.z == page.z
                        });
                    if !height_column {
                        return Ok(());
                    }
                    if self.geometry_interest.len() >= net::MAX_PUBLIC_GEOMETRY_PAGES {
                        return Err(NetworkError::Capacity);
                    }
                    self.geometry_interest.insert(
                        page,
                        PageInterest {
                            received: false,
                            last_request: now_ms,
                        },
                    );
                    self.height_geometry_interest.insert(page);
                }
                geometry.validate().map_err(|_| NetworkError::Protocol)?;
                if let Some(old) = self.geometries.get(&geometry.page) {
                    if old.view_version == geometry.view_version {
                        if *old != geometry {
                            return Err(NetworkError::Protocol);
                        }
                        return Ok(());
                    }
                    if old.view_version > geometry.view_version
                        || old.source_version > geometry.source_version
                    {
                        return Ok(());
                    }
                }
                if self.geometry_updates.len() >= net::MAX_PUBLIC_GEOMETRY_PAGES
                    && !self
                        .geometry_updates
                        .iter()
                        .any(|g| g.page == geometry.page)
                {
                    return Err(NetworkError::Capacity);
                }
                self.geometry_interest
                    .get_mut(&geometry.page)
                    .expect("interest checked")
                    .received = true;
                self.geometries.insert(geometry.page, geometry.clone());
                if let Some(old) = self
                    .geometry_updates
                    .iter_mut()
                    .find(|g| g.page == geometry.page)
                {
                    *old = geometry;
                } else {
                    self.geometry_updates.push_back(geometry);
                }
            }
            ServerMessage::GeometryUnavailable(page) => {
                if let Some(interest) = self.geometry_interest.get_mut(&page) {
                    interest.received = false;
                    interest.last_request = now_ms;
                }
                if self.geometries.contains_key(&page) {
                    if self.geometry_removals.len() >= 32 {
                        return Err(NetworkError::Capacity);
                    }
                    self.geometries.remove(&page);
                    self.geometry_removals.push_back(page);
                }
            }
            ServerMessage::Summary(summary) => {
                if self
                    .summaries
                    .get(&summary.page)
                    .is_some_and(|old| old.source_version > summary.source_version)
                {
                    return Ok(());
                }
                if !self.summaries.contains_key(&summary.page) && self.summaries.len() >= 64 {
                    return Err(NetworkError::Capacity);
                }
                self.summaries.insert(summary.page, summary);
            }
            ServerMessage::Error(RejectReason::RateLimited) => {} // retry pending IDs unchanged
            ServerMessage::Error(reason) => return Err(NetworkError::Rejected(reason)),
            ServerMessage::Players { tick, players } => {
                if players.len() > 64 {
                    return Err(NetworkError::Capacity);
                }
                if tick < self.remote_tick {
                    return Ok(());
                }
                self.remote_tick = tick;
                self.remote_players.clear();
                let own = self.welcome.as_ref().map(|w| w.identity);
                for player in players {
                    if Some(player.identity) != own {
                        self.remote_players.insert(player.identity, player);
                    }
                }
            }
            ServerMessage::Presence(event) => {
                if let Some(reply) = self
                    .presence
                    .receive(event)
                    .map_err(|_| NetworkError::Protocol)?
                {
                    self.queue(ClientMessage::Presence(reply))?;
                }
            }
            ServerMessage::Light(update) => {
                let header = update.header();
                if self.outbox.len() >= MAX_OUTBOX
                    && !self
                        .outbox
                        .iter()
                        .any(|m| matches!(m,ClientMessage::RequestLight(r) if r.page==header.page))
                {
                    return Err(NetworkError::Capacity);
                }
                let cell_view = self.replica.page(header.page).map(|p| p.version);
                match self.lights.apply((*update).clone(), cell_view) {
                    Ok(result) => {
                        if let Some(image) = self.lights.page(header.page) {
                            self.queue(ClientMessage::RequestLight(net::light::Request {
                                page: header.page,
                                cell_view: image.header.cell_view,
                                acknowledged: Some(image.header.version),
                            }))?;
                        }
                        if result == net::light::Apply::Applied {
                            self.queue_light_update(*update)?;
                        }
                    }
                    Err(net::light::Error::MissingBase { .. }) => {
                        if let Some(cell_view) = cell_view {
                            self.queue(ClientMessage::RequestLight(net::light::Request {
                                page: header.page,
                                cell_view,
                                acknowledged: None,
                            }))?;
                        }
                    }
                    Err(net::light::Error::MissingCells { .. }) => {
                        self.queue(ClientMessage::RequestPage {
                            page: header.page,
                            acknowledged: None,
                        })?;
                    }
                    Err(net::light::Error::Capacity) => return Err(NetworkError::Capacity),
                    Err(_) => return Err(NetworkError::Protocol),
                }
            }
            ServerMessage::LightUnavailable(page) => {
                self.lights.remove(page);
                self.light_updates.retain(|u| u.header().page != page);
            }
            ServerMessage::Height(tile) => {
                tile.validate().map_err(|_| NetworkError::Protocol)?;
                if self.height_exact.contains(&tile.key)
                    && !matches!(&tile.replacement, net::height::Replacement::Column { .. })
                {
                    // A normal worker result may have crossed the promotion
                    // request. It cannot downgrade a sticky exact representation.
                    return Ok(());
                }
                if let Some(interest) = self.height_interest.get_mut(&tile.key)
                    && interest.0.is_none_or(|old| tile.source >= old)
                {
                    if self.height_updates.len() >= net::height::MAX_TILES
                        && !self.height_updates.iter().any(|t| t.key == tile.key)
                    {
                        return Err(NetworkError::Capacity);
                    }
                    let exact_ready = !self.height_exact.contains(&tile.key)
                        || matches!(&tile.replacement, net::height::Replacement::Column { .. });
                    *interest = (Some(tile.source), now_ms, exact_ready);
                    if let Some(old) = self.height_updates.iter_mut().find(|t| t.key == tile.key) {
                        *old = tile;
                    } else {
                        self.height_updates.push_back(tile);
                    }
                }
            }
            ServerMessage::HeightUnavailable(key) => {
                if let Some(interest) = self.height_interest.get_mut(&key) {
                    interest.1 = now_ms;
                    interest.2 = false;
                }
            }
            ServerMessage::WaterHalo(halo) => self.receive_water(halo, now_ms)?,
            ServerMessage::WaterUnavailable(page) => self.water_unavailable(page, now_ms),
            ServerMessage::MachinePackets(frame) => {
                let page = frame.page;
                if !self.interest.contains_key(&page) {
                    return Ok(());
                }
                let Some(raw) = self.replica.page(page) else {
                    return Ok(());
                };
                let certified = Version(raw.version);
                if self.outbox.len() == MAX_OUTBOX
                    && !self
                        .outbox
                        .iter()
                        .any(|m| matches!(m,ClientMessage::MachinePackets(r)if r.page()==page))
                {
                    return Err(NetworkError::Capacity);
                }
                let request = match self
                    .machine_packets
                    .apply(*frame, certified)
                    .map_err(|_| NetworkError::Protocol)?
                {
                    machine_packets::Apply::Applied(r)
                    | machine_packets::Apply::Stale(r)
                    | machine_packets::Apply::MissingBase(r)
                    | machine_packets::Apply::WrongView(r) => r,
                };
                self.queue(ClientMessage::MachinePackets(request))?;
            }
            ServerMessage::Props(frame) => {
                frame.validate().map_err(|_| NetworkError::Protocol)?;
                let page = frame.page;
                if !self.interest.contains_key(&page) {
                    return Ok(());
                }
                if self
                    .replica
                    .page(page)
                    .is_some_and(|p| frame.cell_view < p.version)
                {
                    return Ok(());
                }
                if let Some(old) = self.props.get(&page) {
                    if old.view > frame.view {
                        return Ok(());
                    }
                    if old.view < frame.view
                        && (frame.source < old.source || frame.cell_view < old.cell_view)
                    {
                        return Err(NetworkError::Protocol);
                    }
                    if old.view == frame.view {
                        return if old.as_ref() == frame.as_ref() {
                            Ok(())
                        } else {
                            Err(NetworkError::Protocol)
                        };
                    }
                } else if self.props.len() >= self.config.maximum_pages {
                    return Err(NetworkError::Capacity);
                }
                let frame: std::sync::Arc<net::props::Frame> = std::sync::Arc::from(frame);
                self.props.insert(page, frame.clone());
                if self
                    .replica
                    .page(page)
                    .is_some_and(|p| p.version == frame.cell_view)
                {
                    self.queue_props_update(frame);
                }
            }
            ServerMessage::Window(state) => {
                self.window
                    .receive(*state)
                    .map_err(|_| NetworkError::Protocol)?;
            }
            ServerMessage::Entities(frame) => {
                if self.outbox.len() == MAX_OUTBOX
                    && !self
                        .outbox
                        .iter()
                        .any(|m| matches!(m, ClientMessage::Entities(_)))
                {
                    return Err(NetworkError::Capacity);
                }
                let request = match self
                    .entities
                    .apply(*frame)
                    .map_err(|_| NetworkError::Protocol)?
                {
                    entities::Apply::Applied(r)
                    | entities::Apply::Stale(r)
                    | entities::Apply::MissingBase(r) => r,
                };
                self.queue(ClientMessage::Entities(request))?;
            }
            ServerMessage::State { tick, player } => {
                if self
                    .welcome
                    .as_ref()
                    .is_some_and(|w| w.identity == player.identity)
                {
                    self.observe_visual_tick(tick, now_ms);
                }
            }
            ServerMessage::Pong(_) => {}
        }
        Ok(())
    }

    pub fn server_tick_time(&self, now_ms: u64) -> f64 {
        self.latest_tick.0 as f64 + now_ms.saturating_sub(self.last_received_at) as f64 / 50.0
    }
    /// Presentation clock from confirmed authority, with at most two ticks of
    /// extrapolation. Prediction keeps its separate input-lead clock.
    pub fn visual_server_tick_time(&self, now_ms: u64) -> f64 {
        let (tick, sub_tick) = self.visual_clock_sample(now_ms);
        tick.0 as f64 + sub_tick
    }
    pub fn visual_clock_sample(&self, now_ms: u64) -> (Tick, f64) {
        if self.welcome.is_none() {
            return (Tick(0), 0.0);
        }
        let elapsed = now_ms.saturating_sub(self.visual_received_at).min(100);
        (
            Tick(self.visual_tick.0.saturating_add(elapsed / 50)),
            (elapsed % 50) as f64 / 50.0,
        )
    }
    fn observe_visual_tick(&mut self, tick: Tick, now_ms: u64) {
        if tick > self.visual_tick {
            self.visual_tick = tick;
            self.visual_received_at = now_ms;
        }
    }
    pub fn estimated_server_tick(&self, now_ms: u64) -> Tick {
        Tick(
            self.latest_tick
                .0
                .saturating_add(now_ms.saturating_sub(self.last_received_at) / 50),
        )
    }
    pub fn sample_input(
        &mut self,
        now_ms: u64,
        keys: u8,
        yaw: f32,
        pitch: f32,
    ) -> Result<InputFrame, NetworkError> {
        let target = Tick(
            self.estimated_server_tick(now_ms)
                .0
                .saturating_add(self.config.input_lead_ticks)
                .max(self.last_input_tick.0.saturating_add(1)),
        );
        self.schedule_input(target, keys, yaw, pitch)
    }
    pub fn schedule_input(
        &mut self,
        target_tick: Tick,
        keys: u8,
        yaw: f32,
        pitch: f32,
    ) -> Result<InputFrame, NetworkError> {
        if self.player.is_none() {
            return Err(NetworkError::NotConnected);
        }
        if target_tick.0 <= self.last_input_tick.0
            || target_tick.0 > self.latest_tick.0.saturating_add(64)
            || self.inputs.len() >= MAX_INPUTS
        {
            return Err(NetworkError::Capacity);
        }
        let next_input = self
            .next_input
            .checked_add(1)
            .ok_or(NetworkError::Capacity)?;
        let frame = InputFrame {
            sequence: self.next_input,
            target_tick,
            keys,
            yaw: yaw.rem_euclid(360.0),
            pitch,
        };
        net::encode_client(&ClientMessage::Inputs(vec![frame]))
            .map_err(|_| NetworkError::Protocol)?;
        self.input_history
            .push(frame)
            .map_err(|_| NetworkError::Capacity)?;
        self.inputs.insert(frame.sequence, frame);
        self.next_input = next_input;
        self.last_input_tick = target_tick;
        self.counters.peak_inputs = self.counters.peak_inputs.max(self.inputs.len());
        Ok(frame)
    }
    /// Advance/replay at most the configured logical step budget. Returns true
    /// once the requested render prediction tick is reached.
    pub fn advance_to(&mut self, target: Tick) -> Result<bool, NetworkError> {
        if target.0 > self.latest_tick.0.saturating_add(64) {
            return Err(NetworkError::Capacity);
        }
        if self.world_changed {
            if let Some(state) = self.last_authority {
                self.pending_authority = Some(state);
                self.replay_until = Tick(self.replay_until.0.max(self.predicted_tick.0));
            }
            self.world_changed = false;
        }
        if let Some((tick, sequence, state)) = self.pending_authority.take() {
            if let Some(player) = self.player.as_mut() {
                state
                    .apply_authoritative(player)
                    .map_err(NetworkError::Simulation)?;
            } else {
                self.player = Some(state.to_player().map_err(NetworkError::Simulation)?);
            }
            self.predicted_tick = tick;
            self.predicted_input = sequence;
            self.predicted.retain(|old, _| *old <= tick.0);
            self.predicted.insert(
                tick.0,
                PredictedTick {
                    state,
                    input_sequence: sequence,
                },
            );
        }
        let Some(player) = &mut self.player else {
            return Ok(false);
        };
        let space = self
            .welcome
            .as_ref()
            .ok_or(NetworkError::NotConnected)?
            .player
            .space;
        let desired = target.0.max(self.replay_until.0);
        for _ in 0..self.config.simulation_steps_per_poll {
            if self.predicted_tick.0 >= desired {
                break;
            }
            let tick = self.predicted_tick.0 + 1;
            let frame = self
                .inputs
                .get(&self.predicted_input.saturating_add(1))
                .filter(|f| f.target_tick.0 <= tick)
                .copied();
            if let Some(frame) = frame {
                player.yaw = frame.yaw;
                player.pitch = frame.pitch;
            }
            let query = |p: [i32; 3]| match self.overlay.read(
                BlockPos {
                    space,
                    x: p[0],
                    y: p[1],
                    z: p[2],
                },
                &self.replica,
            ) {
                ReplicaRead::Unknown => CellRead::Unknown,
                ReplicaRead::Known(cell) => CellRead::Known(cell),
            };
            let outcome = self.movement_replay.tick(
                player,
                &query,
                key_state(frame.map_or(0, |f| f.keys)),
                self.using_food.is_some(),
            );
            if outcome.waiting_for_world {
                self.counters.unknown_steps += 1;
            } else if let Some(frame) = frame {
                self.predicted_input = frame.sequence;
            }
            self.predicted_tick = Tick(tick);
            self.counters.prediction_steps += 1;
            if tick <= self.replay_until.0 {
                self.counters.replay_steps += 1;
            }
            let state = PredictionState::capture(player).map_err(NetworkError::Simulation)?;
            self.predicted.insert(
                tick,
                PredictedTick {
                    state,
                    input_sequence: self.predicted_input,
                },
            );
            while self.predicted.len() > MAX_INPUTS {
                let first = *self.predicted.first_key_value().expect("nonempty").0;
                self.predicted.remove(&first);
            }
        }
        Ok(self.predicted_tick.0 >= desired)
    }
    pub fn action(
        &mut self,
        action: Action,
        predict_edit: bool,
        now_ms: u64,
    ) -> Result<ActionId, NetworkError> {
        self.queue_action(action, predict_edit, now_ms, None)
    }
    pub fn aimed_action(
        &mut self,
        action: Action,
        predict_edit: bool,
        now_ms: u64,
        yaw: f32,
        pitch: f32,
    ) -> Result<ActionId, NetworkError> {
        self.queue_action(action, predict_edit, now_ms, Some([yaw, pitch]))
    }
    pub fn cycle_tool_mode(&mut self, back: bool, now_ms: u64) -> Result<ActionId, NetworkError> {
        let inventory = self
            .predicted_inventory()
            .ok_or(NetworkError::NotConnected)?;
        let held = inventory.hand();
        self.action(
            Action::CycleToolMode {
                back,
                inventory: net::InventoryGuard {
                    revision: inventory.revision(),
                    selected: inventory.selected() as u8,
                    item: held.item(),
                    count: held.count(),
                    damage: held.damage(),
                    payload: held.payload().map_or(0, |id| id.0),
                },
            },
            false,
            now_ms,
        )
    }
    pub fn interact_entity(
        &mut self,
        target: u64,
        use_entity: bool,
        now_ms: u64,
        yaw: f32,
        pitch: f32,
    ) -> Result<ActionId, NetworkError> {
        let inventory = self
            .predicted_inventory()
            .ok_or(NetworkError::NotConnected)?;
        let held = inventory.hand();
        let inventory = net::InventoryGuard {
            revision: inventory.revision(),
            selected: inventory.selected() as u8,
            item: held.item(),
            count: held.count(),
            damage: held.damage(),
            payload: held.payload().map_or(0, |id| id.0),
        };
        let action = if use_entity {
            Action::UseEntity { target, inventory }
        } else {
            Action::AttackEntity { target, inventory }
        };
        self.aimed_action(action, false, now_ms, yaw, pitch)
    }
    pub fn use_block_tool(
        &mut self,
        hit: sim::BlockHit,
        sneaking: bool,
        now_ms: u64,
        aim: [f32; 2],
    ) -> Result<ActionId, NetworkError> {
        let inventory = self
            .predicted_inventory()
            .ok_or(NetworkError::NotConnected)?;
        let held = inventory.hand();
        let space = self
            .welcome
            .as_ref()
            .ok_or(NetworkError::NotConnected)?
            .player
            .space;
        self.aimed_action(
            Action::UseBlockTool {
                position: BlockPos {
                    space,
                    x: hit.cell[0],
                    y: hit.cell[1],
                    z: hit.cell[2],
                },
                expected: hit.block,
                sneaking,
                inventory: net::InventoryGuard {
                    revision: inventory.revision(),
                    selected: inventory.selected() as u8,
                    item: held.item(),
                    count: held.count(),
                    damage: held.damage(),
                    payload: held.payload().map_or(0, |id| id.0),
                },
            },
            false,
            now_ms,
            aim[0],
            aim[1],
        )
    }
    fn held_inventory_guard(&self) -> Result<net::InventoryGuard, NetworkError> {
        let inventory = self
            .predicted_inventory()
            .ok_or(NetworkError::NotConnected)?;
        let held = inventory.hand();
        Ok(net::InventoryGuard {
            revision: inventory.revision(),
            selected: inventory.selected() as u8,
            item: held.item(),
            count: held.count(),
            damage: held.damage(),
            payload: held.payload().map_or(0, |p| p.0),
        })
    }
    pub fn use_food(&mut self, held: bool, now_ms: u64) -> Result<ActionId, NetworkError> {
        let inventory = self.held_inventory_guard()?;
        let id = self.action(Action::UseFood { held, inventory }, false, now_ms)?;
        self.using_food = held.then_some(id);
        Ok(id)
    }
    pub(crate) fn using_food(&self) -> bool {
        self.using_food.is_some()
    }
    pub fn use_bucket(
        &mut self,
        position: BlockPos,
        expected: Cell,
        now_ms: u64,
        aim: [f32; 2],
    ) -> Result<ActionId, NetworkError> {
        let inventory = self.held_inventory_guard()?;
        self.aimed_action(
            Action::UseBucket {
                position,
                expected,
                inventory,
            },
            false,
            now_ms,
            aim[0],
            aim[1],
        )
    }
    /// Read-only admission for a retained UI intent. The action itself still
    /// validates and reserves the same queues atomically when it is enqueued.
    pub fn can_queue_action(&self) -> bool {
        self.welcome.is_some()
            && self.actions.len() < MAX_ACTIONS
            && self.outbox.len() < MAX_OUTBOX
            && self.next_action < u64::MAX
    }
    pub fn action_pending(&self, id: ActionId) -> bool {
        self.actions.contains_key(&id)
    }
    fn queue_action(
        &mut self,
        mut action: Action,
        predict_edit: bool,
        now_ms: u64,
        aim: Option<[f32; 2]>,
    ) -> Result<ActionId, NetworkError> {
        if self.welcome.is_none() {
            return Err(NetworkError::NotConnected);
        }
        if self.actions.len() >= MAX_ACTIONS || self.outbox.len() >= MAX_OUTBOX {
            return Err(NetworkError::Capacity);
        }
        let next_action = self
            .next_action
            .checked_add(1)
            .ok_or(NetworkError::Capacity)?;
        let projected_inventory = self.predicted_inventory();
        if let Action::Place {
            position,
            expected,
            cell,
        } = action
            && let Some(inventory) = projected_inventory
        {
            let held = inventory.hand();
            action = Action::PlaceOwned {
                position,
                expected,
                cell,
                inventory: net::InventoryGuard {
                    revision: inventory.revision(),
                    selected: inventory.selected() as u8,
                    item: held.item(),
                    count: held.count(),
                    damage: held.damage(),
                    payload: held.payload().map_or(0, |id| id.0),
                },
            };
        }
        let id = ActionId(self.next_action);
        let message = action_message(id, action.clone(), aim);
        net::encode_client(&message).map_err(|_| NetworkError::Protocol)?;
        if predict_edit {
            let edit = match &action {
                Action::Mine {
                    position, expected, ..
                } => (*position, *expected, Cell::AIR),
                Action::Place {
                    position,
                    expected,
                    cell,
                } => (*position, *expected, *cell),
                Action::PlaceOwned {
                    position,
                    expected,
                    cell,
                    ..
                } => (*position, *expected, *cell),
                _ => return Err(NetworkError::Protocol),
            };
            if self.read(edit.0) != ReplicaRead::Known(edit.1) {
                return Err(NetworkError::Unknown);
            }
            self.overlay
                .predict(id, edit.0, edit.2)
                .map_err(|_| NetworkError::Capacity)?;
            self.world_changed = true;
        }
        self.queue(message)?;
        self.actions.insert(
            id,
            PendingAction {
                action,
                aim,
                last_sent: now_ms,
                inventory_before: projected_inventory.map(|i| i.revision()),
            },
        );
        self.next_action = next_action;
        self.counters.peak_actions = self.counters.peak_actions.max(self.actions.len());
        Ok(id)
    }
    pub fn poll(&mut self, now_ms: u64) -> Result<(), NetworkError> {
        self.poll_water(now_ms)?;
        self.note_replay_time(now_ms);
        if let Some(hello) = &self.handshake
            && now_ms.saturating_sub(self.last_hello) >= 250
        {
            self.queue(ClientMessage::Hello(hello.clone()))?;
            self.last_hello = now_ms;
        }
        if now_ms.saturating_sub(self.last_input_send) >= 50 {
            let frames: Vec<_> = self
                .input_history
                .pending()
                .take(net::MAX_INPUT_HISTORY)
                .copied()
                .collect();
            if !frames.is_empty() {
                self.queue(ClientMessage::Inputs(frames))?;
            }
            self.last_input_send = now_ms;
        }
        let retry: Vec<_> = self
            .actions
            .iter()
            .filter(|(_, a)| now_ms.saturating_sub(a.last_sent) >= 250)
            .take(8)
            .map(|(id, a)| (*id, a.action.clone(), a.aim))
            .collect();
        for (id, action, aim) in retry {
            self.queue(action_message(id, action, aim))?;
            self.actions.get_mut(&id).expect("pending retry").last_sent = now_ms;
        }
        let geometry: Vec<_> = self
            .geometry_interest
            .iter()
            .filter(|(_, i)| !i.received && now_ms.saturating_sub(i.last_request) >= 500)
            .take(2)
            .map(|(page, _)| *page)
            .collect();
        for page in geometry {
            self.request_geometry(page, now_ms)?;
        }
        let heights: Vec<_> = self
            .height_interest
            .iter()
            .filter(|(_, i)| !i.2 && now_ms.saturating_sub(i.1) >= 1000)
            .take(2)
            .map(|(key, _)| *key)
            .collect();
        for key in heights {
            self.request_height(key, now_ms)?;
        }
        let pages: Vec<_> = self
            .interest
            .iter()
            .filter(|(_, i)| !i.received && now_ms.saturating_sub(i.last_request) >= 500)
            .take(4)
            .map(|(page, _)| *page)
            .collect();
        for page in pages {
            self.request_page(page, now_ms)?;
        }
        // An accepted no-op or coalesced ABA may have no new cell body to send.
        // Ask for its causal watermark using the retained base; never turn a
        // queued full repair into an ACK, or fail admission at a full outbox.
        let mut causal_pages = [None; 4];
        let mut causal_count = 0;
        for (id, position) in self.overlay.awaiting_actions() {
            let page = position.page();
            let Some(interest) = self.interest.get(&page) else {
                continue;
            };
            let Some(image) = self.replica.page(page) else {
                continue;
            };
            if image.applied_action >= id
                || now_ms.saturating_sub(interest.last_request) < 500
                || causal_pages[..causal_count].contains(&Some(page))
            {
                continue;
            }
            causal_pages[causal_count] = Some(page);
            causal_count += 1;
            if causal_count == causal_pages.len() {
                break;
            }
        }
        for page in causal_pages.into_iter().flatten() {
            if self
                .outbox
                .iter()
                .any(|message| matches!(message,ClientMessage::RequestPage{page:p,..} if *p==page))
            {
                continue;
            }
            if self.outbox.len() == MAX_OUTBOX
                && !self
                    .outbox
                    .iter()
                    .any(|message| matches!(message,ClientMessage::ForgetPage(p) if *p==page))
            {
                continue;
            }
            let acknowledged = self.replica.page(page).map(|image| image.version);
            self.queue(ClientMessage::RequestPage { page, acknowledged })?;
            self.interest
                .get_mut(&page)
                .expect("causal page has current interest")
                .last_request = now_ms;
        }
        Ok(())
    }
    /// Change only the bounded raw interest; retain identity, prediction,
    /// actions and the socket. The next I/O poll contracts/expands its leases.
    pub fn set_near_profile(
        &mut self,
        profile: crate::near_profile::NearProfile,
    ) -> Result<bool, NetworkError> {
        let mut config = self.config;
        profile.apply_network(&mut config);
        config.validate()?;
        let changed = self.config.interest_radii(false) != config.interest_radii(false)
            || self.config.interest_radii(true) != config.interest_radii(true);
        self.config = config;
        Ok(changed)
    }
    pub fn near_profile_applied(&self, profile: crate::near_profile::NearProfile) -> bool {
        profile.matches_network(self.config)
    }
    pub fn update_interest(&mut self, now_ms: u64) -> Result<(), NetworkError> {
        let Some(player) = self.player.as_ref() else {
            return Ok(());
        };
        let space = self
            .welcome
            .as_ref()
            .ok_or(NetworkError::NotConnected)?
            .player
            .space;
        let center = BlockPos {
            space,
            x: player.pos[0].floor() as i32,
            y: player.pos[1].floor() as i32,
            z: player.pos[2].floor() as i32,
        }
        .page();
        let evict: Vec<_> = self
            .interest
            .keys()
            .filter(|page| !self.config.contains(center, **page, true))
            .copied()
            .collect();
        let removals = evict
            .iter()
            .filter(|p| self.replica.page(**p).is_some())
            .count();
        if removals > MAX_PAGE_REMOVALS - self.page_removals.len() {
            return Ok(());
        }
        for page in evict {
            if !self.retire_page_interest(page)? {
                return Ok(());
            }
        }
        let [horizontal, vertical, _] = self.config.interest_radii(false).map(i32::from);
        let mut candidates = Vec::new();
        for y in -vertical..=vertical {
            for z in -horizontal..=horizontal {
                for x in -horizontal..=horizontal {
                    let page = PageId {
                        x: center.x + x,
                        y: center.y + y,
                        z: center.z + z,
                        ..center
                    };
                    if !self.interest.contains_key(&page) {
                        candidates.push((x * x + y * y + z * z, page));
                    }
                }
            }
        }
        candidates.sort_unstable();
        let needed =
            (self.interest.len() + candidates.len()).saturating_sub(self.config.maximum_pages);
        if needed > 0 {
            let mut retained: Vec<_> = self
                .interest
                .keys()
                .filter(|page| !self.config.contains(center, **page, false))
                .copied()
                .collect();
            retained.sort_by_key(|page| {
                std::cmp::Reverse(
                    (i64::from(page.x) - i64::from(center.x)).pow(2)
                        + (i64::from(page.y) - i64::from(center.y)).pow(2)
                        + (i64::from(page.z) - i64::from(center.z)).pow(2),
                )
            });
            let selected: Vec<_> = retained.into_iter().take(needed).collect();
            let removals = selected
                .iter()
                .filter(|p| self.replica.page(**p).is_some())
                .count();
            if removals > MAX_PAGE_REMOVALS - self.page_removals.len() {
                return Ok(());
            }
            for page in selected {
                if !self.retire_page_interest(page)? {
                    return Ok(());
                }
            }
        }
        for (_, page) in candidates {
            if self.interest.len() >= self.config.maximum_pages {
                break;
            }
            if self.outbox.len() == MAX_OUTBOX && !self.outbox.iter().any(|message| matches!(message,ClientMessage::RequestPage {page:p,..}|ClientMessage::ForgetPage(p) if *p == page)) { break; }
            self.request_page(page, now_ms)?;
        }
        Ok(())
    }
    fn retire_page_interest(&mut self, page: PageId) -> Result<bool, NetworkError> {
        let obsolete = |message: &ClientMessage| match message {
            ClientMessage::RequestPage { page: p, .. }
            | ClientMessage::AckPage { page: p, .. }
            | ClientMessage::RequestProps { page: p, .. }
            | ClientMessage::ForgetPage(p) => *p == page,
            ClientMessage::RequestLight(request) => request.page == page,
            ClientMessage::MachinePackets(request) => request.page() == page,
            _ => false,
        };
        if self.outbox.len()
            - self
                .outbox
                .iter()
                .filter(|message| obsolete(message))
                .count()
            >= MAX_OUTBOX
        {
            return Ok(false);
        }
        self.outbox.retain(|message| !obsolete(message));
        self.queue(ClientMessage::ForgetPage(page))?;
        self.interest.remove(&page);
        self.machine_packets.remove_page(page);
        self.props.remove(&page);
        self.props_updates.retain(|frame| frame.page != page);
        self.lights.remove(page);
        self.light_updates
            .retain(|update| update.header().page != page);
        self.page_updates.retain(|update| match update {
            PageUpdate::Snapshot(snapshot) => snapshot.page != page,
            PageUpdate::Delta(delta) => delta.page != page,
        });
        if self.replica.remove(page) {
            self.page_removals.push_back(page);
            self.world_changed = true;
        }
        self.counters.evicted_pages += 1;
        Ok(true)
    }
}
fn key_state(bits: u8) -> Keys {
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
fn matches_public(player: &Player, state: PlayerState) -> bool {
    player.pos == state.position
        && player.vel == state.velocity
        && player.yaw == state.yaw
        && player.pitch == state.pitch
        && player.health == state.health
        && player.mode as u8 == state.mode
        && player.on_ground == state.on_ground
        && player.flying == state.flying
        && player.noclip == state.noclip
}

fn action_message(id: ActionId, action: Action, aim: Option<[f32; 2]>) -> ClientMessage {
    if let Some([yaw, pitch]) = aim {
        ClientMessage::AimedAction {
            id,
            action,
            yaw,
            pitch,
        }
    } else {
        ClientMessage::Action { id, action }
    }
}

#[cfg(test)]
mod entity_repair_credit_tests {
    use super::*;
    #[test]
    fn late_ack_never_overwrites_an_unsent_full_entity_repair() {
        let mut c = Connection::new(NetworkConfig::default()).unwrap();
        c.queue(ClientMessage::Entities(
            net::entities::EntityRequest::Full { have: Some(1) },
        ))
        .unwrap();
        c.queue(ClientMessage::Entities(net::entities::EntityRequest::Ack {
            revision: 2,
        }))
        .unwrap();
        let outgoing = c.take_outgoing(256);
        assert!(matches!(
            outgoing.as_slice(),
            [ClientMessage::Entities(
                net::entities::EntityRequest::Full { have: Some(1) }
            )]
        ));
    }
}
