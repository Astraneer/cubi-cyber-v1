//! Transport-independent client authority/prediction owner. Platform sockets feed
//! bounded net frames here; only confirmed, sanitized pages enter its world view.
//! Rendering integration consumes `take_page_updates` at the scene owner boundary.
use core_types::{BlockPos, Cell, PageId, SpaceId, SpaceTransform, Tick, Version};
use net::{
    Action, ActionId, ActionReply, ActionStatus, ClientMessage, FarSummary, Hello, InputFrame,
    InputHistory, PageUpdate, PlayerId, PlayerState, PredictionOverlay, RejectReason, Replica,
    ReplicaError, ReplicaRead, SecretToken, ServerMessage, Welcome,
};
use sim::{Keys, Player, prediction::PredictionState};
use std::collections::{BTreeMap, VecDeque};
use world::CellRead;

const MAX_INPUTS: usize = 256;
const MAX_ACTIONS: usize = 128;
const MAX_OUTBOX: usize = 256;
const MAX_PAGE_UPDATES: usize = 128;
const MAX_SPACES: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct NetworkConfig {
    pub maximum_pages: usize,
    pub near_radius: u16,
    pub retain_radius: u16,
    pub input_lead_ticks: u64,
    pub simulation_steps_per_poll: u16,
}
impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            maximum_pages: 64,
            near_radius: 1,
            retain_radius: 2,
            input_lead_ticks: 6,
            simulation_steps_per_poll: 32,
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
    page_updates: VecDeque<PageUpdate>,
    counters: NetworkCounters,
}

impl Connection {
    pub fn new(config: NetworkConfig) -> Result<Self, NetworkError> {
        if config.maximum_pages == 0
            || config.maximum_pages > 512
            || config.near_radius > 3
            || config.retain_radius < config.near_radius
            || config.retain_radius > 4
            || config.input_lead_ticks == 0
            || config.input_lead_ticks > 32
            || config.simulation_steps_per_poll == 0
            || config.simulation_steps_per_poll > 64
        {
            return Err(NetworkError::Capacity);
        }
        Ok(Self {
            config,
            welcome: None,
            credential: None,
            handshake: None,
            last_hello: 0,
            replica: Replica::new(config.maximum_pages),
            overlay: PredictionOverlay::new(MAX_ACTIONS),
            player: None,
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
            page_updates: VecDeque::new(),
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
    pub fn player(&self) -> Option<&Player> {
        self.player.as_ref()
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
    pub fn remote_players(&self) -> impl Iterator<Item = &PlayerState> {
        self.remote_players.values()
    }
    pub fn inventory(&self) -> Option<&rules::survival::Inventory> {
        self.inventory.as_ref()
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
    pub fn take_reply(&mut self) -> Option<ActionReply> {
        self.replies.pop_front()
    }
    pub fn take_page_updates(&mut self, limit: usize) -> Vec<PageUpdate> {
        (0..limit.min(MAX_PAGE_UPDATES))
            .filter_map(|_| self.page_updates.pop_front())
            .collect()
    }
    pub fn take_outgoing(&mut self, limit: usize) -> Vec<ClientMessage> {
        (0..limit.min(MAX_OUTBOX))
            .filter_map(|_| self.outbox.pop_front())
            .collect()
    }
    fn queue(&mut self, message: ClientMessage) -> Result<(), NetworkError> {
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
        match message {
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
                self.input_history.acknowledge(player.input_sequence);
                self.inputs
                    .retain(|sequence, _| *sequence > player.input_sequence);
                self.predicted.retain(|old, _| *old >= tick.0);
                self.counters.authoritative_states += 1;
            }
            ServerMessage::Page(update) => {
                if self.page_updates.len() >= MAX_PAGE_UPDATES || self.outbox.len() >= MAX_OUTBOX {
                    return Err(NetworkError::Capacity);
                }
                let (page, cells) = match &update {
                    PageUpdate::Snapshot(s) => (s.page, core_types::PAGE_VOLUME),
                    PageUpdate::Delta(d) => (d.page, d.changes.len()),
                };
                // A delayed packet for an evicted AOI must not repopulate the cache.
                if !self.interest.contains_key(&page) {
                    return Ok(());
                }
                match self.replica.apply(update.clone()) {
                    Ok(net::ApplyResult::Applied) => {
                        let version = self.replica.page(page).expect("applied page").version;
                        self.queue(ClientMessage::AckPage { page, version })?;
                        self.interest
                            .get_mut(&page)
                            .expect("interest checked")
                            .received = true;
                        self.counters.page_cells += cells as u64;
                        match &update {
                            PageUpdate::Snapshot(_) => self.counters.full_snapshots += 1,
                            PageUpdate::Delta(_) => self.counters.deltas += 1,
                        }
                        self.page_updates.push_back(update);
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
                if self.replica.remove(page) {
                    self.world_changed = true;
                }
            }
            ServerMessage::Action(reply) => {
                self.overlay.reply(reply);
                self.overlay.retire_applied(&self.replica);
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
            ServerMessage::State { .. } | ServerMessage::Pong(_) => {}
        }
        Ok(())
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
            let mut player = Player::new([0.0; 3]);
            state
                .apply_authoritative(&mut player)
                .map_err(NetworkError::Simulation)?;
            self.player = Some(player);
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
            let outcome = player.tick(&query, key_state(frame.map_or(0, |f| f.keys)));
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
        mut action: Action,
        predict_edit: bool,
        now_ms: u64,
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
                },
            };
        }
        let id = ActionId(self.next_action);
        let message = ClientMessage::Action {
            id,
            action: action.clone(),
        };
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
                last_sent: now_ms,
                inventory_before: projected_inventory.map(|i| i.revision()),
            },
        );
        self.next_action = next_action;
        self.counters.peak_actions = self.counters.peak_actions.max(self.actions.len());
        Ok(id)
    }
    pub fn poll(&mut self, now_ms: u64) -> Result<(), NetworkError> {
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
            .map(|(id, a)| (*id, a.action.clone()))
            .collect();
        for (id, action) in retry {
            self.queue(ClientMessage::Action { id, action })?;
            self.actions.get_mut(&id).expect("pending retry").last_sent = now_ms;
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
        Ok(())
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
            .filter(|page| {
                !net::aoi_contains(
                    center,
                    **page,
                    self.config.near_radius,
                    self.config.retain_radius,
                    true,
                )
            })
            .copied()
            .collect();
        for page in evict {
            self.interest.remove(&page);
            self.replica.remove(page);
            self.counters.evicted_pages += 1;
        }
        let r = i32::from(self.config.near_radius);
        let mut candidates = Vec::new();
        for y in -r..=r {
            for z in -r..=r {
                for x in -r..=r {
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
        for (_, page) in candidates {
            if self.interest.len() >= self.config.maximum_pages {
                break;
            }
            self.request_page(page, now_ms)?;
        }
        Ok(())
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
