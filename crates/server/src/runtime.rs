//! Runtime ledgers share the same durability frontier as world changes and
//! private player inventories. No item import or pickup publishes before fsync.
use super::*;
use crate::entities::items::{ItemDelta, ItemRuntime, PickupPlayer};
use core_types::ComponentId;
use rules::entities::{EntityError, EntityId};
use save::{RUNTIME_ITEMS, RUNTIME_MOBS, RUNTIME_SURVIVAL, RuntimeRecord};
const MAX_ITEMS: usize = 8192;
const MAX_MOBS: usize = 5000;
const MAX_EVENTS: usize = 100_000;
const MAX_DROP_SOURCES: usize = 16_384;
pub(super) struct Runtimes {
    pub(super) survival: survival::SurvivalRuntime,
    pub(super) mobs: entities::MobRuntime,
    pub(super) items: ItemRuntime,
    pub(super) machines: machines::MachineRuntime,
    pub(super) chat: chat::Ledger,
    drops: BTreeSet<ComponentId>,
    furnace_bootstrap: BTreeSet<ComponentId>,
    machine_bootstrap: BTreeSet<ComponentId>,
    farm_seed: u64,
    section_centers: BTreeMap<PlayerId, PageId>,
    active_sections: BTreeSet<PageId>,
    sections_dirty: bool,
}
impl Runtimes {
    pub(super) fn new(entropy: &[u8; 32]) -> Result<Self, ServerError> {
        let key = |domain: &str| {
            let hash = blake3::derive_key(domain, entropy);
            u64::from_le_bytes(hash[..8].try_into().expect("hash prefix"))
        };
        Ok(Self {
            survival: survival::SurvivalRuntime::with_seed(
                MAX_EVENTS,
                key("cubi survival runtime v1"),
            ),
            mobs: entities::MobRuntime::new(entities::Config {
                maximum_entities: MAX_MOBS,
                seed: key("cubi mob runtime v1"),
                ..entities::Config::default()
            })
            .map_err(|_| ServerError::Invalid("mob runtime initialization"))?,
            items: ItemRuntime::new(MAX_ITEMS, key("cubi item runtime v1"))
                .map_err(|_| ServerError::Invalid("item runtime initialization"))?,
            machines: machines::MachineRuntime::new(machines::Config::default())
                .map_err(|_| ServerError::Invalid("machine runtime initialization"))?,
            chat: chat::Ledger::default(),
            drops: BTreeSet::new(),
            furnace_bootstrap: BTreeSet::new(),
            machine_bootstrap: BTreeSet::new(),
            farm_seed: key("cubi random section ticks v1"),
            section_centers: BTreeMap::new(),
            active_sections: BTreeSet::new(),
            sections_dirty: true,
        })
    }
    pub(super) fn restore(&mut self, records: &[RuntimeRecord]) -> Result<(), ServerError> {
        for record in records {
            match record.kind {
                RUNTIME_SURVIVAL => {
                    self.survival = survival::SurvivalRuntime::restore(
                        MAX_EVENTS,
                        &survival::RuntimeSnapshot::decode(&record.payload, MAX_EVENTS)
                            .map_err(|_| ServerError::Invalid("survival snapshot codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("survival snapshot state"))?
                }
                RUNTIME_MOBS => {
                    self.mobs = entities::MobRuntime::restore(
                        entities::Config {
                            maximum_entities: MAX_MOBS,
                            ..entities::Config::default()
                        },
                        &entities::Snapshot::decode(&record.payload, MAX_MOBS)
                            .map_err(|_| ServerError::Invalid("mob snapshot codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("mob snapshot state"))?
                }
                RUNTIME_ITEMS => {
                    self.items = ItemRuntime::restore(
                        MAX_ITEMS,
                        &entities::items::Snapshot::decode(&record.payload, MAX_ITEMS)
                            .map_err(|_| ServerError::Invalid("item snapshot codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("item snapshot state"))?
                }
                save::RUNTIME_MACHINES => {
                    self.machines = machines::MachineRuntime::restore(
                        machines::Config::default(),
                        &machines::RuntimeSnapshot::decode(
                            &record.payload,
                            machines::Config::default(),
                        )
                        .map_err(|_| ServerError::Invalid("machine snapshot codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("machine snapshot state"))?
                }
                save::RUNTIME_CHAT => {
                    self.chat =
                        chat::Ledger::restore(&record.payload).map_err(ServerError::Invalid)?
                }
                _ => {
                    return Err(ServerError::Invalid(
                        "runtime kind has no authoritative owner",
                    ));
                }
            }
        }
        Ok(())
    }
    pub(super) fn replay(&mut self, records: &[RuntimeRecord]) -> Result<(), ServerError> {
        for record in records {
            match record.kind {
                RUNTIME_SURVIVAL => self
                    .survival
                    .acknowledge(
                        survival::RuntimeDelta::decode(&record.payload, MAX_EVENTS)
                            .map_err(|_| ServerError::Invalid("survival delta codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("survival delta state"))?,
                RUNTIME_MOBS => self
                    .mobs
                    .acknowledge(
                        entities::MobDelta::decode(&record.payload, MAX_MOBS)
                            .map_err(|_| ServerError::Invalid("mob delta codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("mob delta state"))?,
                RUNTIME_ITEMS => self
                    .items
                    .acknowledge(
                        ItemDelta::decode(&record.payload, MAX_ITEMS)
                            .map_err(|_| ServerError::Invalid("item delta codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("item delta state"))?,
                save::RUNTIME_MACHINES => self
                    .machines
                    .acknowledge(
                        machines::RuntimeDelta::decode(
                            &record.payload,
                            machines::Config::default(),
                        )
                        .map_err(|_| ServerError::Invalid("machine delta codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("machine delta state"))?,
                save::RUNTIME_CHAT => self
                    .chat
                    .apply(&chat::decode_delta(&record.payload).map_err(ServerError::Invalid)?)
                    .map_err(ServerError::Invalid)?,
                _ => {
                    return Err(ServerError::Invalid(
                        "runtime delta has no authoritative owner",
                    ));
                }
            }
        }
        Ok(())
    }
    pub(super) fn snapshots(&self) -> Result<Vec<RuntimeRecord>, ServerError> {
        Ok(vec![
            RuntimeRecord {
                kind: RUNTIME_SURVIVAL,
                payload: self.survival.snapshot().encode(),
            },
            RuntimeRecord {
                kind: RUNTIME_MOBS,
                payload: self
                    .mobs
                    .snapshot()
                    .encode()
                    .map_err(|_| ServerError::Invalid("mob snapshot bounds"))?,
            },
            RuntimeRecord {
                kind: RUNTIME_ITEMS,
                payload: self
                    .items
                    .snapshot()
                    .encode()
                    .map_err(|_| ServerError::Invalid("item snapshot bounds"))?,
            },
            RuntimeRecord {
                kind: save::RUNTIME_MACHINES,
                payload: self
                    .machines
                    .snapshot()
                    .encode()
                    .map_err(|_| ServerError::Invalid("machine snapshot bounds"))?,
            },
            RuntimeRecord {
                kind: save::RUNTIME_CHAT,
                payload: self.chat.snapshot().map_err(ServerError::Invalid)?,
            },
        ])
    }
    pub(super) fn bootstrap_drops(&mut self, world: &World) -> Result<(), ServerError> {
        // Startup migration scans component metadata once, never4096 cells or
        // the cold component table on each owner tick.
        for (id, record) in world.components() {
            if record.data.kind == survival::DROP_COMPONENT {
                self.queue_drop(id)?;
            }
            if record.data.kind == machines::MACHINE_COMPONENT
                && self.machines.machine_meta(id).is_none()
            {
                self.machine_bootstrap.insert(id);
            }
            if record.data.kind == survival::FURNACE_COMPONENT {
                let furnace = rules::survival::furnace::Furnace::decode(&record.data.payload)
                    .map_err(|_| ServerError::Invalid("legacy furnace state"))?;
                if furnace.burn() > 0
                    || (!furnace.slots()[0].is_empty() && !furnace.slots()[1].is_empty())
                {
                    self.furnace_bootstrap.insert(id);
                }
            }
        }
        Ok(())
    }
    fn queue_drop(&mut self, id: ComponentId) -> Result<(), ServerError> {
        if self.drops.len() >= MAX_DROP_SOURCES && !self.drops.contains(&id) {
            return Err(ServerError::Invalid("durable drop source backlog capacity"));
        }
        self.drops.insert(id);
        Ok(())
    }
    pub(super) fn committed_changes(&mut self, changes: &[WorldChange]) -> Result<(), ServerError> {
        for change in changes {
            if change.pages.iter().any(|p| p.created || p.removed) {
                self.sections_dirty = true;
            }
            for component in &change.components {
                if component
                    .after
                    .as_ref()
                    .is_some_and(|r| r.data.kind == survival::DROP_COMPONENT)
                {
                    self.queue_drop(component.id)?;
                }
            }
        }
        Ok(())
    }
}
pub(crate) fn player_entity(id: PlayerId) -> EntityId {
    let hash = blake3::derive_key("cubi authoritative player entity v1", &id.0);
    EntityId(u64::from_le_bytes(hash[..8].try_into().expect("hash prefix")).max(1))
}
impl Server {
    /// Import is a durable microbatch at the current tick, before the20Hz gate.
    /// This avoids cloning an entire item store or halving physics to10Hz when
    /// a drop import and a physics plan would share the same ledger revision.
    pub(super) fn import_items(&mut self) -> Result<bool, ServerError> {
        let sources: Vec<_> = self
            .runtimes
            .drops
            .iter()
            .copied()
            .filter(|id| {
                self.world
                    .component(*id)
                    .is_some_and(|r| r.data.kind == survival::DROP_COMPONENT)
            })
            .take(64)
            .collect();
        if sources.is_empty() {
            self.runtimes.drops.clear();
            return Ok(false);
        }
        let plan = match self
            .runtimes
            .items
            .plan_import(&self.world, &sources, self.tick, true)
        {
            Ok(plan) => plan,
            Err(EntityError::Capacity) => return Ok(false),
            Err(_) => return Err(ServerError::Invalid("item import plan")),
        };
        self.runtimes
            .items
            .validate_import(&self.world, &plan)
            .map_err(|_| ServerError::Invalid("item import stale"))?;
        let payload = plan
            .delta
            .encode()
            .map_err(|_| ServerError::Invalid("item import delta bounds"))?;
        let imported = plan.delta.patches.len() as u64;
        let mut batch = Batch::empty(self.tick);
        for tx in plan.transactions {
            batch.changes.push(
                self.world
                    .commit(tx)
                    .map_err(|_| ServerError::Invalid("item import transaction"))?,
            );
        }
        batch.runtimes.push(RuntimeRecord {
            kind: RUNTIME_ITEMS,
            payload,
        });
        for id in sources {
            self.runtimes.drops.remove(&id);
        }
        self.counters.items_imported += imported;
        self.submit_batch(batch, Vec::new());
        Ok(true)
    }
    pub(super) fn tick_items(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        if self.runtimes.items.entities().active_len() == 0 {
            return Ok(());
        }
        let mut identities = BTreeMap::new();
        let mut players = Vec::new();
        for peer in self.peers.values().filter(|p| p.ready) {
            let id = player_entity(peer.identity);
            if identities.insert(id, peer.identity).is_some() {
                return Err(ServerError::Invalid("player entity identity collision"));
            }
            players.push(PickupPlayer {
                id,
                space: peer.space,
                position: peer.player.pos,
                crouching: peer.player.crouch,
                alive: peer.player.health > 0.0,
                inventory: peer.inventory,
            });
        }
        let plan = self
            .runtimes
            .items
            .plan_tick(
                &self.world,
                self.tick,
                &players,
                entities::items::Budget::default(),
            )
            .map_err(|_| ServerError::Invalid("item physics plan"))?;
        self.runtimes
            .items
            .validate(&self.world, &players, &plan)
            .map_err(|_| ServerError::Invalid("item pickup guard"))?;
        for patch in &plan.inventories {
            let identity = identities
                .get(&patch.player)
                .ok_or(ServerError::Invalid("item pickup identity"))?;
            let peer = self
                .peers
                .values()
                .find(|p| p.identity == *identity)
                .ok_or(ServerError::Invalid("item pickup peer"))?;
            if peer.inventory != patch.before {
                return Err(ServerError::Invalid("item pickup inventory changed"));
            }
        }
        let payload = plan
            .delta
            .encode()
            .map_err(|_| ServerError::Invalid("item physics delta bounds"))?;
        for patch in plan.inventories {
            let identity = identities[&patch.player];
            let peer = self
                .peers
                .values_mut()
                .find(|p| p.identity == identity)
                .expect("validated peer");
            peer.inventory = patch.after;
            let saved = capture_peer(peer).map_err(ServerError::Invalid)?;
            self.saved_players.insert(identity, saved.clone());
            if let Some(existing) = batch
                .players
                .iter_mut()
                .find(|p| p.state.identity == identity)
            {
                *existing = saved
            } else {
                batch.players.push(saved)
            }
        }
        self.counters.item_steps += plan.work.entities;
        self.counters.item_pickups += plan.work.picked_items;
        batch.runtimes.push(RuntimeRecord {
            kind: RUNTIME_ITEMS,
            payload,
        });
        Ok(())
    }
}

pub(super) fn capture_peer(peer: &Peer) -> Result<SavedPlayer, &'static str> {
    let mut saved = actor::capture_living(
        peer.identity,
        peer.space,
        peer.input_sequence,
        &peer.player,
        &peer.health,
        &peer.combat,
    )?;
    saved.place_ready_at = peer.place_ready_at;
    saved.break_ready_at = peer.break_ready_at;
    saved.inventory = peer.inventory.encode().to_vec();
    saved.window = peer.window.encode();
    Ok(saved)
}
fn damage(
    peer: &mut Peer,
    kind: rules::survival::health::DamageKind,
    amount: f32,
) -> Result<(), ServerError> {
    if let Some(hit) = peer.health.damage(
        kind,
        amount,
        peer.inventory.armor(),
        peer.player.mode != GameMode::Survival,
    ) && hit.armor_wear > 0
    {
        let plan = peer
            .inventory
            .plan_armor_wear(hit.armor_wear)
            .map_err(|_| ServerError::Invalid("armor wear revision"))?;
        peer.inventory = *plan.after();
    }
    Ok(())
}
pub(super) fn tick_health(
    peer: &mut Peer,
    fall: u32,
    inside_spawn: bool,
) -> Result<(), ServerError> {
    use rules::survival::health::DamageKind;
    peer.health.begin_tick();
    peer.combat.tick(inside_spawn);
    if fall > 0 {
        damage(peer, DamageKind::Fall, fall as f32)?;
    }
    if let Some(amount) = peer.health.tick_air(peer.player.eye_water) {
        damage(peer, DamageKind::Drown, amount)?;
    }
    if peer.player.in_lava {
        peer.health.ignite(600);
        if peer.health.now().is_multiple_of(10) {
            damage(peer, DamageKind::Lava, 4.0)?;
        }
    }
    if let Some(amount) = peer
        .health
        .tick_fire(peer.player.in_water, peer.player.in_lava)
    {
        damage(peer, DamageKind::OnFire, amount)?;
    }
    if peer.player.pos[1] < -64.0 {
        damage(peer, DamageKind::OutOfWorld, 4.0)?;
    }
    peer.health.end_tick();
    peer.player.health = peer.health.hp();
    Ok(())
}

impl Server {
    pub(super) fn publish_entities(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        let mobs = self.runtimes.mobs.entities();
        let items = self.runtimes.items.entities();
        for (connection, peer) in self.peers.iter_mut().filter(|(_, p)| p.ready) {
            if peer.entities.is_none() {
                if mobs.is_empty() && items.is_empty() {
                    continue;
                }
                match entities::replication::EntityHistory::new(
                    net::entities::MAX_ENTITIES,
                    4096,
                    8,
                ) {
                    Ok(history) => peer.entities = Some(history),
                    Err(_) => {
                        self.failed = true;
                        break;
                    }
                }
            }
            let history = peer.entities.as_mut().expect("initialized history");
            let interest = entities::replication::Interest {
                space: peer.space,
                center: peer.player.pos,
                enter: 64.0,
                retain: 80.0,
            };
            let budget = rules::entities::spatial::QueryBudget {
                buckets: 24_000,
                probes: 65_536,
                candidates: 4096,
            };
            match history.plan(mobs, items, interest, self.tick, budget) {
                Ok((frame, work)) => {
                    self.counters.entity_buckets += work.buckets;
                    self.counters.entity_poses += work.poses;
                    if let Some(frame) = frame {
                        self.counters.entity_frames += 1;
                        output.push(Outgoing::reliable(
                            *connection,
                            ServerMessage::Entities(Box::new(frame)),
                        ));
                    }
                }
                Err(_) => self.counters.entity_overflows += 1,
            }
        }
        output
    }
}

impl Server {
    pub(super) fn tick_survival(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        use survival::Event;
        let mut events = BTreeMap::new();
        self.sample_farms(&mut events)?;
        for id in &self.runtimes.furnace_bootstrap {
            events.insert(Event::Furnace(*id), self.tick);
        }
        // Only external writes wake an idle furnace. Its own last burn/component
        // update must not turn a deliberately sleeping furnace into a full scan.
        for component in batch.changes.iter().flat_map(|c| &c.components) {
            if component
                .after
                .as_ref()
                .is_some_and(|r| r.data.kind == survival::FURNACE_COMPONENT)
            {
                events.insert(
                    Event::Furnace(component.id),
                    Tick(self.tick.0.saturating_add(1)),
                );
            }
        }
        let cell_edits = batch.changes.iter().any(|c| !c.cells.is_empty());
        if self.runtimes.survival.is_empty() && events.is_empty() && !cell_edits {
            return Ok(());
        }
        let plan = self
            .runtimes
            .survival
            .plan_tick(&self.world, self.tick, survival::Budget::default())
            .map_err(|_| ServerError::Invalid("survival phase planning"))?;
        self.runtimes
            .survival
            .validate_delta(&plan.delta)
            .map_err(|_| ServerError::Invalid("survival phase guard"))?;
        for tx in plan.transactions {
            batch.changes.push(
                self.world
                    .commit(tx)
                    .map_err(|_| ServerError::Invalid("survival phase transaction"))?,
            );
        }
        let mut positions = BTreeSet::new();
        for cell in batch.changes.iter().flat_map(|c| &c.cells) {
            let (x, y, z) = cell.index.xyz();
            let p = BlockPos {
                space: cell.page.space,
                x: cell
                    .page
                    .x
                    .checked_mul(16)
                    .and_then(|v| v.checked_add(i32::from(x)))
                    .ok_or(ServerError::Invalid("cell x bound"))?,
                y: cell
                    .page
                    .y
                    .checked_mul(16)
                    .and_then(|v| v.checked_add(i32::from(y)))
                    .ok_or(ServerError::Invalid("cell y bound"))?,
                z: cell
                    .page
                    .z
                    .checked_mul(16)
                    .and_then(|v| v.checked_add(i32::from(z)))
                    .ok_or(ServerError::Invalid("cell z bound"))?,
            };
            for d in [
                [0, 0, 0],
                [-1, 0, 0],
                [1, 0, 0],
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
            ] {
                if let (Some(x), Some(y), Some(z)) = (
                    p.x.checked_add(d[0]),
                    p.y.checked_add(d[1]),
                    p.z.checked_add(d[2]),
                ) {
                    positions.insert(BlockPos {
                        space: p.space,
                        x,
                        y,
                        z,
                    });
                }
            }
        }
        for position in positions {
            self.counters.survival_wake_reads += 1;
            if let CellRead::Known(cell) = self.world.read(position)
                && let Some(fluid) = rules::survival::fluid::fluid(cell)
            {
                events.insert(
                    Event::Fluid(position, fluid.liquid),
                    Tick(self.tick.0.saturating_add(fluid.liquid.delay())),
                );
            }
        }
        let delta = self
            .runtimes
            .survival
            .extend_registration(plan.delta, events)
            .map_err(|_| ServerError::Invalid("survival wake capacity/guard"))?;
        if !delta.is_empty() {
            batch.runtimes.push(RuntimeRecord {
                kind: RUNTIME_SURVIVAL,
                payload: delta.encode(),
            });
        }
        self.counters.survival_events += plan.work.events;
        self.counters.survival_reads += plan.work.reads;
        self.counters.survival_writes += plan.work.writes;
        self.runtimes.furnace_bootstrap.clear();
        Ok(())
    }
}

impl Server {
    fn sample_farms(
        &mut self,
        events: &mut BTreeMap<survival::Event, Tick>,
    ) -> Result<(), ServerError> {
        fn center(peer: &Peer) -> PageId {
            BlockPos {
                space: peer.space,
                x: peer.player.pos[0].floor() as i32,
                y: peer.player.pos[1].floor() as i32,
                z: peer.player.pos[2].floor() as i32,
            }
            .page()
        }
        let active = |p: &&Peer| p.ready && p.player.mode != GameMode::Spectator;
        let count = self.peers.values().filter(active).count();
        let changed = self.runtimes.sections_dirty
            || self.runtimes.section_centers.len() != count
            || self.peers.values().filter(active).any(|p| {
                self.runtimes.section_centers.get(&p.identity).copied() != Some(center(p))
            });
        if changed {
            self.runtimes.section_centers.clear();
            self.runtimes.active_sections.clear();
            for peer in self.peers.values().filter(active) {
                let c = center(peer);
                self.runtimes.section_centers.insert(peer.identity, c);
                for dy in -2..=2 {
                    for dz in -2..=2 {
                        for dx in -2..=2 {
                            let page = PageId {
                                space: c.space,
                                x: c.x + dx,
                                y: c.y + dy,
                                z: c.z + dz,
                            };
                            if self.world.page(page).is_some() {
                                self.runtimes.active_sections.insert(page);
                            }
                        }
                    }
                }
            }
            self.runtimes.sections_dirty = false;
        }
        let mut samples = [core_types::LocalIdx::default(); 3];
        for page in &self.runtimes.active_sections {
            let Some(image) = self.world.page(*page) else {
                continue;
            };
            rules::survival::farm::random_tick_indices(
                self.runtimes.farm_seed,
                *page,
                self.tick,
                3,
                &mut samples,
            )
            .map_err(|_| ServerError::Invalid("section random tick indices"))?;
            self.counters.random_tick_sections += 1;
            for (ordinal, index) in samples.into_iter().enumerate() {
                self.counters.random_tick_reads += 1;
                let cell = image.cell(index);
                if !matches!(
                    rules::blocks::bdef(cell.id()).random,
                    rules::blocks::RandomTick::Farmland | rules::blocks::RandomTick::Crop
                ) {
                    continue;
                }
                let (x, y, z) = index.xyz();
                let position = BlockPos {
                    space: page.space,
                    x: page.x * 16 + i32::from(x),
                    y: page.y * 16 + i32::from(y),
                    z: page.z * 16 + i32::from(z),
                };
                let token = survival::farm_sample_token(self.tick, ordinal as u16)
                    .map_err(|_| ServerError::Invalid("section random tick clock"))?;
                events.insert(survival::Event::FarmSample(position, token), self.tick);
            }
        }
        Ok(())
    }
    pub(super) fn tick_machines(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        let mut changed: BTreeSet<_> = self.runtimes.machine_bootstrap.iter().copied().collect();
        for c in batch.changes.iter().flat_map(|c| &c.components) {
            if c.before
                .as_ref()
                .is_some_and(|r| r.data.kind == machines::MACHINE_COMPONENT)
                || c.after
                    .as_ref()
                    .is_some_and(|r| r.data.kind == machines::MACHINE_COMPONENT)
            {
                changed.insert(c.id);
            }
        }
        if self.runtimes.machines.event_count() == 0 && changed.is_empty() {
            return Ok(());
        }
        let plan = self
            .runtimes
            .machines
            .plan_tick(&self.world, self.tick, machines::Budget::default())
            .map_err(|_| ServerError::Invalid("machine phase planning"))?;
        self.runtimes
            .machines
            .validate(&self.world, &plan)
            .map_err(|_| ServerError::Invalid("machine phase guard"))?;
        for tx in plan.transactions {
            batch.changes.push(
                self.world
                    .commit(tx)
                    .map_err(|_| ServerError::Invalid("machine phase transaction"))?,
            );
        }
        let delta = self
            .runtimes
            .machines
            .extend_registration(plan.delta, &self.world, changed, self.tick)
            .map_err(|_| ServerError::Invalid("machine registration guard/capacity"))?;
        if !delta.is_empty() {
            batch.runtimes.push(RuntimeRecord {
                kind: save::RUNTIME_MACHINES,
                payload: delta
                    .encode()
                    .map_err(|_| ServerError::Invalid("machine delta bounds"))?,
            });
        }
        self.counters.machine_events += plan.work.events;
        self.counters.machine_steps += plan.work.machines;
        self.counters.machine_reads += plan.work.reads;
        self.counters.machine_transfers += plan.work.transfers;
        self.runtimes.machine_bootstrap.clear();
        Ok(())
    }
}

fn block_reason(error: block_loot::Error) -> RejectReason {
    match error {
        block_loot::Error::Unknown => RejectReason::Unknown,
        block_loot::Error::UnsupportedTypedItem => RejectReason::Unsupported,
        block_loot::Error::Capacity => RejectReason::Capacity,
        _ => RejectReason::InvalidCell,
    }
}
pub(super) fn component_birth(
    world: &World,
    tx: &mut Transaction,
    position: BlockPos,
) -> Result<(), RejectReason> {
    let actual = tx
        .cell_updates()
        .find(|(p, _)| *p == position)
        .map(|(_, cell)| cell)
        .ok_or(RejectReason::InvalidCell)?;
    if let Some((id, data)) =
        block_loot::initial_component(world, position, actual).map_err(block_reason)?
    {
        tx.set_component(
            id,
            data,
            core_types::ChannelMask::CONTENTS | core_types::ChannelMask::CONNECTIONS,
        );
    }
    Ok(())
}
pub(super) fn prepare_break(
    world: &World,
    peer: &Peer,
    mining: &MiningAction,
    seed: u64,
    tx: &mut Transaction,
    now: Tick,
) -> Result<rules::survival::Inventory, RejectReason> {
    let contents = block_loot::prepare_contents(world, mining.position).map_err(block_reason)?;
    let mut stacks = contents.stacks;
    let mut inventory = peer.inventory;
    if peer.player.mode == GameMode::Survival {
        let player_key = u64::from_le_bytes(
            blake3::hash(&peer.identity.0).as_bytes()[..8]
                .try_into()
                .expect("hash prefix"),
        );
        let mut rng =
            core_types::DeterministicRng::from_key(seed, 0x4d494e45, player_key, mining.id.0);
        let plan = peer
            .inventory
            .plan_mine(mining.held, contents.cell, &mut rng)
            .map_err(survival_reason)?;
        stacks.extend_from_slice(plan.drops.stacks());
        inventory = *plan.inventory.after();
    }
    if !stacks.is_empty() {
        survival::plan_drops(
            world,
            contents.anchor,
            peer.identity.0,
            mining.id.0,
            now,
            &stacks,
            10,
            contents.remove,
        )
        .map_err(survival_reason)?
        .append_to(tx);
    } else if let Some(id) = contents.remove {
        tx.remove_component(
            id,
            core_types::ChannelMask::CONTENTS | core_types::ChannelMask::CONNECTIONS,
        );
    }
    Ok(inventory)
}
