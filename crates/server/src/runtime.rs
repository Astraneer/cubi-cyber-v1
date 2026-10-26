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
    pub(super) natural: natural::State,
    pub(super) hostile_mobs: usize,
    pub(super) items: ItemRuntime,
    pub(super) machines: machines::MachineRuntime,
    pub(super) chat: chat::Ledger,
    pub(super) payloads: rules::item_payload::Ledger,
    pub(super) height: height_authority::HeightAuthority,
    pub(super) height_loaded: bool,
    pub(super) topology_bootstrap: BTreeSet<BlockPos>,
    pub(super) props_dirty: BTreeSet<PageId>,
    pub(super) packet_dirty: BTreeSet<PageId>,
    machine_activity: BTreeSet<PageId>,
    machine_activity_initialized: bool,
    machine_activity_revision: u64,
    drops: BTreeSet<ComponentId>,
    furnace_bootstrap: BTreeSet<ComponentId>,
    machine_bootstrap: BTreeSet<ComponentId>,
    farm_seed: u64,
    section_centers: BTreeMap<PlayerId, PageId>,
    active_sections: BTreeSet<PageId>,
    sections_dirty: bool,
    sections_revision: u64,
}
impl Runtimes {
    pub(super) fn new(entropy: &[u8; 32], public_seed: u32) -> Result<Self, ServerError> {
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
            hostile_mobs: 0,
            natural: natural::State::new(
                u64::from(public_seed),
                key("cubi natural spawning rng v1").max(1),
            )
            .map_err(|_| ServerError::Invalid("natural initial state"))?,
            items: ItemRuntime::new(MAX_ITEMS, key("cubi item runtime v1"))
                .map_err(|_| ServerError::Invalid("item runtime initialization"))?,
            machines: machines::MachineRuntime::new(machines::Config::default())
                .map_err(|_| ServerError::Invalid("machine runtime initialization"))?,
            chat: chat::Ledger::default(),
            payloads: rules::item_payload::Ledger::new(rules::item_payload::Config::default())
                .map_err(|_| ServerError::Invalid("item payload ledger limits"))?,
            height: height_authority::HeightAuthority::new(height_authority::Config::default())
                .map_err(|_| ServerError::Invalid("height authority limits"))?,
            height_loaded: false,
            topology_bootstrap: BTreeSet::new(),
            props_dirty: BTreeSet::new(),
            packet_dirty: BTreeSet::new(),
            machine_activity: BTreeSet::new(),
            machine_activity_initialized: false,
            machine_activity_revision: 0,
            drops: BTreeSet::new(),
            furnace_bootstrap: BTreeSet::new(),
            machine_bootstrap: BTreeSet::new(),
            farm_seed: key("cubi random section ticks v1"),
            section_centers: BTreeMap::new(),
            active_sections: BTreeSet::new(),
            sections_dirty: true,
            sections_revision: 1,
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
                    let (snapshot, natural) =
                        natural_records::restore(&record.payload).map_err(ServerError::Invalid)?;
                    let mobs = entities::MobRuntime::restore(
                        entities::Config {
                            maximum_entities: MAX_MOBS,
                            ..entities::Config::default()
                        },
                        &snapshot,
                    )
                    .map_err(|_| ServerError::Invalid("mob snapshot state"))?;
                    if let Some(natural) = natural {
                        if natural.map_seed() != self.natural.map_seed() {
                            return Err(ServerError::Invalid("natural map seed mismatch"));
                        }
                        self.natural = natural;
                    }
                    self.hostile_mobs = mobs
                        .entities()
                        .rows()
                        .iter()
                        .filter(|m| m.kind.definition().hostile && m.life.alive())
                        .count();
                    self.mobs = mobs;
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
                save::RUNTIME_HEIGHT => {
                    self.height = height_authority::HeightAuthority::restore(
                        &record.payload,
                        height_authority::Config::default(),
                    )
                    .map_err(|_| ServerError::Invalid("height snapshot codec/state"))?;
                    self.height_loaded = true;
                }
                save::RUNTIME_PAYLOADS => {
                    self.payloads = rules::item_payload::Ledger::restore(
                        rules::item_payload::Config::default(),
                        &rules::item_payload::Snapshot::decode(
                            &record.payload,
                            rules::item_payload::Config::default(),
                        )
                        .map_err(|_| ServerError::Invalid("item payload snapshot codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("item payload snapshot state"))?
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
                RUNTIME_MOBS => {
                    let (mobs, natural) = natural_records::decode_delta(&record.payload)
                        .map_err(ServerError::Invalid)?;
                    if let Some(ref mobs) = mobs {
                        self.mobs
                            .validate_owner_delta(mobs)
                            .map_err(|_| ServerError::Invalid("mob delta guard"))?;
                    }
                    if let Some(natural) = natural {
                        self.natural
                            .validate(natural)
                            .map_err(|_| ServerError::Invalid("natural delta guard"))?;
                    }
                    if let Some(mobs) = mobs {
                        let before = mobs
                            .patches
                            .iter()
                            .filter_map(|p| p.before)
                            .filter(|m| m.kind.definition().hostile && m.life.alive())
                            .count();
                        let after = mobs
                            .patches
                            .iter()
                            .filter_map(|p| p.after)
                            .filter(|m| m.kind.definition().hostile && m.life.alive())
                            .count();
                        self.hostile_mobs = self
                            .hostile_mobs
                            .checked_sub(before)
                            .and_then(|n| n.checked_add(after))
                            .ok_or(ServerError::Invalid("hostile count guard"))?;
                        self.mobs
                            .acknowledge(mobs)
                            .map_err(|_| ServerError::Invalid("mob delta state"))?;
                    }
                    if let Some(natural) = natural {
                        self.natural
                            .apply(natural)
                            .map_err(|_| ServerError::Invalid("natural delta state"))?;
                    }
                }
                RUNTIME_ITEMS => self
                    .items
                    .acknowledge(
                        ItemDelta::decode(&record.payload, MAX_ITEMS)
                            .map_err(|_| ServerError::Invalid("item delta codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("item delta state"))?,
                save::RUNTIME_MACHINES => {
                    let delta = machines::RuntimeDelta::decode(
                        &record.payload,
                        machines::Config::default(),
                    )
                    .map_err(|_| ServerError::Invalid("machine delta codec"))?;
                    if let Some(transport) = &delta.transport {
                        for patch in &transport.packets {
                            for packet in patch.before.iter().chain(patch.after.iter()) {
                                if let Some(at) = packet.at() {
                                    self.packet_dirty.insert(at.page());
                                }
                            }
                        }
                        self.packet_dirty
                            .extend(transport.clocks.iter().map(|patch| patch.page));
                    }
                    for patch in &delta.machines {
                        for meta in patch.before.iter().chain(patch.after.iter()) {
                            self.props_dirty.insert(meta.anchor.page());
                        }
                    }
                    for patch in &delta.configs {
                        for config in patch.before.iter().chain(patch.after.iter()) {
                            self.props_dirty.insert(config.position.page());
                        }
                    }
                    self.machines
                        .acknowledge(delta)
                        .map_err(|_| ServerError::Invalid("machine delta state"))?;
                }
                save::RUNTIME_HEIGHT => {
                    self.height
                        .apply_encoded_delta(&record.payload)
                        .map_err(|_| ServerError::Invalid("height delta codec/state"))?;
                }
                save::RUNTIME_PAYLOADS => self
                    .payloads
                    .acknowledge(
                        rules::item_payload::Delta::decode(&record.payload)
                            .map_err(|_| ServerError::Invalid("item payload delta codec"))?,
                    )
                    .map_err(|_| ServerError::Invalid("item payload delta state"))?,
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
                payload: natural_records::snapshot(&self.mobs, &self.natural)
                    .map_err(ServerError::Invalid)?,
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
            RuntimeRecord {
                kind: save::RUNTIME_HEIGHT,
                payload: self
                    .height
                    .snapshot()
                    .map_err(|_| ServerError::Invalid("height snapshot bounds"))?,
            },
            RuntimeRecord {
                kind: save::RUNTIME_PAYLOADS,
                payload: self
                    .payloads
                    .snapshot()
                    .encode(rules::item_payload::Config::default())
                    .map_err(|_| ServerError::Invalid("item payload snapshot bounds"))?,
            },
        ])
    }
    pub(super) fn active_sections(&self) -> impl Iterator<Item = PageId> + '_ {
        self.active_sections.iter().copied()
    }
    pub(super) fn sections_revision(&self) -> u64 {
        self.sections_revision
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
    EntityId(net::player_entity_id(id))
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
    pub(super) fn tick_items(
        &mut self,
        batch: &mut Batch,
        spawns: Vec<rules::entities::item::ItemEntity>,
    ) -> Result<(), ServerError> {
        if self.runtimes.items.entities().active_len() == 0 && spawns.is_empty() {
            return Ok(());
        }
        let pending_rows =
            if let Some(record) = batch.runtimes.iter().find(|r| r.kind == RUNTIME_ITEMS) {
                let delta = entities::items::ItemDelta::decode(&record.payload, MAX_ITEMS)
                    .map_err(|_| ServerError::Invalid("pending owned item rows"))?;
                self.runtimes
                    .items
                    .spawn_capacity(&delta)
                    .map_err(|_| ServerError::Invalid("pending owned item guards"))?;
                delta
                    .patches
                    .into_iter()
                    .map(|p| match (p.before, p.after) {
                        (None, Some(row)) => Ok(row),
                        _ => Err(ServerError::Invalid(
                            "only detached spawns precede item physics",
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
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
        let delta = self
            .runtimes
            .items
            .extend_spawn(plan.delta, pending_rows.into_iter().chain(spawns), true)
            .map_err(|_| ServerError::Invalid("machine item spawn guard/capacity"))?;
        let payload = delta
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
        let record = RuntimeRecord {
            kind: RUNTIME_ITEMS,
            payload,
        };
        if let Some(old) = batch.runtimes.iter_mut().find(|r| r.kind == RUNTIME_ITEMS) {
            *old = record;
        } else {
            batch.runtimes.push(record);
        }
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
        if self.mob_owner.due {
            return output;
        }
        let mobs = self.runtimes.mobs.entities();
        let items = self.runtimes.items.entities();
        for (connection, peer) in self.peers.iter_mut().filter(|(_, p)| p.ready) {
            if peer.entity_pending_revision.is_some() || peer.entity_attempt_tick == Some(self.tick)
            {
                continue;
            }
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
                        self.fail("publish_entities");
                        break;
                    }
                }
            }
            let history = peer.entities.as_mut().expect("initialized history");
            peer.entity_attempt_tick = Some(self.tick);
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
                        peer.entity_pending_revision = Some(frame.revision);
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
        self.component_survival_wakes(batch, &mut events);
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
        self.cell_survival_wakes(batch, &mut events)?;
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
    fn refresh_active_sections(&mut self) -> Result<(), ServerError> {
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
            self.runtimes.sections_revision = self
                .runtimes
                .sections_revision
                .checked_add(1)
                .ok_or(ServerError::Invalid("active section revision exhausted"))?;
        }
        Ok(())
    }
    fn sample_farms(
        &mut self,
        events: &mut BTreeMap<survival::Event, Tick>,
    ) -> Result<(), ServerError> {
        self.refresh_active_sections()?;
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
    pub(super) fn tick_machines(
        &mut self,
        batch: &mut Batch,
    ) -> Result<machine_owner::MachineTick, ServerError> {
        let continuing = self.runtimes.machines.owner_pending();
        // A socket may close during a held logical tick. Complete its captured
        // cohort against the same activity/topology view; apply AOI changes at
        // the next boundary instead of invalidating an in-flight ready cursor.
        if !continuing {
            self.refresh_active_sections()?;
        }
        let mut activity = Vec::new();
        if continuing {
            // No new external activity belongs to this durable cohort.
        } else if !self.runtimes.machine_activity_initialized {
            activity.extend(
                self.runtimes
                    .machines
                    .activity_pages()
                    .map(|p| (p, self.runtimes.active_sections.contains(&p))),
            );
        } else if self.runtimes.machine_activity_revision != self.runtimes.sections_revision {
            activity.extend(
                self.runtimes
                    .machine_activity
                    .symmetric_difference(&self.runtimes.active_sections)
                    .map(|p| (*p, self.runtimes.active_sections.contains(p))),
            );
        }
        let bootstrap: Vec<_> = if continuing {
            Vec::new()
        } else {
            self.runtimes
                .topology_bootstrap
                .iter()
                .take(64)
                .copied()
                .collect()
        };
        let mut topology: BTreeSet<_> = bootstrap.iter().copied().collect();
        let endpoint = |id: u16| {
            matches!(
                id,
                rules::ids::UNIVERSAL_CABLE
                    | rules::ids::MECHANICAL_PIPE
                    | rules::ids::PRESSURIZED_TUBE
                    | rules::ids::LOGISTICAL_TRANSPORTER
            ) || rules::blocks::bdef(id).be.is_some()
        };
        for cell in batch.changes.iter().flat_map(|c| &c.cells) {
            if (endpoint(cell.old.id()) || endpoint(cell.new.id()))
                && (cell.old.id() != cell.new.id() || cell.old.state() & 7 != cell.new.state() & 7)
            {
                let (x, y, z) = cell.index.xyz();
                topology.insert(BlockPos {
                    space: cell.page.space,
                    x: cell.page.x * 16 + i32::from(x),
                    y: cell.page.y * 16 + i32::from(y),
                    z: cell.page.z * 16 + i32::from(z),
                });
            }
        }
        for component in batch
            .changes
            .iter()
            .flat_map(|c| &c.components)
            .filter(|c| c.channels.intersects(core_types::ChannelMask::CONNECTIONS))
        {
            if let Some(record) = component.after.as_ref().or(component.before.as_ref()) {
                topology.insert(record.data.anchor);
            }
        }
        let topology: Vec<_> = topology.into_iter().collect();
        let mut changed: BTreeSet<_> = if continuing {
            BTreeSet::new()
        } else {
            self.runtimes.machine_bootstrap.iter().copied().collect()
        };
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
        if self.runtimes.machines.event_count() == 0
            && !self.runtimes.machines.transport_due(self.tick)
            && !self.runtimes.machines.owner_pending()
            && changed.is_empty()
            && topology.is_empty()
            && activity.is_empty()
        {
            return Ok(machine_owner::MachineTick::idle());
        }
        let pending = self
            .payload_delta(batch)
            .map_err(|_| ServerError::Invalid("pending item payload delta"))?;
        let view = pending
            .as_ref()
            .map(|delta| self.runtimes.payloads.view(delta))
            .transpose()
            .map_err(|_| ServerError::Invalid("pending payload view"))?;
        let resolver: &dyn rules::item_payload::Resolver = if let Some(view) = view.as_ref() {
            view
        } else {
            &self.runtimes.payloads
        };
        let active_sections = &self.runtimes.active_sections;
        let active = |p: BlockPos| active_sections.contains(&p.page());
        let item_capacity = batch
            .runtimes
            .iter()
            .find(|r| r.kind == RUNTIME_ITEMS)
            .map(|r| entities::items::ItemDelta::decode(&r.payload, MAX_ITEMS))
            .transpose()
            .map_err(|_| ServerError::Invalid("pending machine item capacity"))?
            .map(|delta| self.runtimes.items.spawn_capacity(&delta))
            .transpose()
            .map_err(|_| ServerError::Invalid("pending machine item guards"))?
            .unwrap_or_else(|| MAX_ITEMS.saturating_sub(self.runtimes.items.entities().len()));
        let mut plan = self
            .runtimes
            .machines
            .plan_tick_with_topology(
                &self.world,
                machines::PhaseContext {
                    payloads: Some(resolver),
                    active: Some(&active),
                    activity: &activity,
                },
                &topology,
                self.tick,
                machines::Budget {
                    item_spawns: item_capacity,
                    ..machines::Budget::default()
                },
            )
            .map_err(machine_phase_error)?;
        // Admission precedes every World write. A full item store or payload
        // ledger leaves packets and source inventories intact for a later tick.
        let spawn_delta = self
            .runtimes
            .items
            .plan_activity(&[])
            .map_err(|_| ServerError::Invalid("item spawn preflight revision"))?;
        let spawn_admission =
            self.runtimes
                .items
                .extend_spawn(spawn_delta, plan.item_spawns.iter().copied(), true);
        let payload_admission = self.plan_payloads(batch, std::mem::take(&mut plan.payloads));
        let deferred = matches!(spawn_admission, Err(EntityError::Capacity))
            || matches!(payload_admission, Err(RejectReason::Capacity));
        let payload_record = if deferred {
            // Zero work still supplies one guarded registration boundary, so
            // external machine births/edits remain recoverable with their ACK.
            plan = self
                .runtimes
                .machines
                .plan_tick_with_topology(
                    &self.world,
                    machines::PhaseContext {
                        payloads: Some(resolver),
                        active: Some(&active),
                        activity: &activity,
                    },
                    &topology,
                    self.tick,
                    machines::Budget {
                        events: 0,
                        item_spawns: 0,
                        reads: 0,
                        bytes: 0,
                        endpoints: 0,
                        route_nodes: 0,
                    },
                )
                .map_err(|_| ServerError::Invalid("deferred machine registration"))?;
            None
        } else {
            spawn_admission.map_err(|_| ServerError::Invalid("machine spawn identity/row"))?;
            payload_admission.map_err(|_| ServerError::Invalid("machine payload admission"))?
        };
        self.runtimes
            .machines
            .validate(&self.world, &plan)
            .map_err(|_| ServerError::Invalid("machine phase guard"))?;
        let complete = plan.owner_complete();
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
        Self::stage_payloads(batch, payload_record);
        self.counters.machine_events += plan.work.events;
        self.counters.machine_prepared += plan.work.prepared;
        self.counters.machine_unknown += plan.work.unknown;
        self.counters.machine_deferred += plan.work.deferred;
        self.counters.machine_copy_bytes += plan.work.bytes;
        self.counters.machine_steps += plan.work.machines;
        self.counters.machine_reads += plan.work.reads;
        self.counters.machine_transfers += plan.work.transfers;
        if !continuing {
            self.runtimes.machine_bootstrap.clear();
        }
        for p in bootstrap {
            self.runtimes.topology_bootstrap.remove(&p);
        }
        if !self.runtimes.machine_activity_initialized
            || self.runtimes.machine_activity_revision != self.runtimes.sections_revision
        {
            self.runtimes.machine_activity = self.runtimes.active_sections.clone();
            self.runtimes.machine_activity_initialized = true;
            self.runtimes.machine_activity_revision = self.runtimes.sections_revision;
        }
        Ok(machine_owner::MachineTick {
            spawns: plan.item_spawns,
            complete,
        })
    }
}

fn machine_phase_error(error: machines::MachineError) -> ServerError {
    match error {
        machines::MachineError::RouteCapacity => {
            ServerError::Invalid("machine route exceeds fixed search capacity")
        }
        _ => ServerError::Invalid("machine phase planning"),
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
    held: rules::stack::ItemStack,
    payloads: &dyn rules::item_payload::Resolver,
) -> Result<(), RejectReason> {
    let actual = tx
        .cell_updates()
        .find(|(p, _)| *p == position)
        .map(|(_, cell)| cell)
        .ok_or(RejectReason::InvalidCell)?;
    if let Some((id, data)) =
        block_loot::initial_component_with_payload(world, position, actual, held, payloads)
            .map_err(block_reason)?
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
) -> Result<
    (
        rules::survival::Inventory,
        Vec<rules::item_payload::Payload>,
    ),
    RejectReason,
> {
    let coordinates = rules::blocks::main_of(
        mining.expected.raw(),
        [mining.position.x, mining.position.y, mining.position.z],
    );
    let anchor = BlockPos {
        space: mining.position.space,
        x: coordinates[0],
        y: coordinates[1],
        z: coordinates[2],
    };
    let CellRead::Known(source) = world.read(anchor) else {
        return Err(RejectReason::Unknown);
    };
    let mut inventory = peer.inventory;
    let mut stacks = Vec::new();
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
            .plan_mine(mining.held, source, &mut rng)
            .map_err(survival_reason)?;
        stacks.extend_from_slice(plan.drops.stacks());
        inventory = *plan.inventory.after();
    }
    let block_item = rules::items::block_item(source.id());
    let has_block_drop = stacks.iter().any(|s| s.item() == block_item);
    let contents = block_loot::prepare_for_break(world, mining.position, has_block_drop)
        .map_err(block_reason)?;
    if contents.replace_block_drop {
        stacks.retain(|s| s.item() != block_item);
    }
    stacks.extend(contents.stacks);
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
    Ok((inventory, contents.payloads))
}

impl Runtimes {
    pub(super) fn validate_owned_payloads(
        &self,
        world: &World,
        players: &BTreeMap<PlayerId, SavedPlayer>,
    ) -> Result<(), ServerError> {
        let check = |stack: rules::stack::ItemStack| -> Result<(), ServerError> {
            if let Some(id) = stack.payload() {
                self.payloads
                    .resolve(id, stack.item())
                    .map_err(|_| ServerError::Invalid("owned item payload reference"))?;
            }
            Ok(())
        };
        for saved in players.values() {
            if !saved.inventory.is_empty() {
                let inventory = rules::survival::Inventory::decode(&saved.inventory)
                    .map_err(|_| ServerError::Invalid("owned inventory codec"))?;
                for stack in inventory.slots() {
                    check(*stack)?;
                }
            }
            if !saved.window.is_empty() {
                let bound = windows::BoundWindow::decode(&saved.window)
                    .map_err(|_| ServerError::Invalid("owned window codec"))?;
                check(bound.state.cursor())?;
                for stack in bound.state.craft().cells() {
                    check(*stack)?;
                }
                // Container entries are projections of World-owned state. In
                // particular, moving charge can yield a cache-only content ID;
                // only cursor/crafting are independently owned by this window.
            }
        }
        for (_, record) in world.components() {
            match record.data.kind {
                survival::CHEST_COMPONENT => {
                    let chest = rules::survival::furnace::Chest::decode(&record.data.payload)
                        .map_err(|_| ServerError::Invalid("owned chest codec"))?;
                    for stack in chest.slots() {
                        check(*stack)?;
                    }
                }
                survival::FURNACE_COMPONENT => {
                    let furnace = rules::survival::furnace::Furnace::decode(&record.data.payload)
                        .map_err(|_| ServerError::Invalid("owned furnace codec"))?;
                    for stack in furnace.slots() {
                        check(*stack)?;
                    }
                }
                machines::MACHINE_COMPONENT => {
                    let machine = rules::machines::Machine::decode(&record.data.payload)
                        .map_err(|_| ServerError::Invalid("owned machine codec"))?;
                    for slot in machine.slots {
                        check(slot.stack)?;
                    }
                }
                survival::DROP_COMPONENT => {
                    let bundle = survival::DropBundle::decode(&record.data.payload)
                        .map_err(|_| ServerError::Invalid("owned drop codec"))?;
                    for entry in bundle.entries {
                        check(entry.stack)?;
                    }
                }
                _ => {}
            }
        }
        for item in self.items.entities().rows() {
            check(item.stack)?;
        }
        Ok(())
    }
}

impl Server {
    pub(super) fn payload_delta(
        &self,
        batch: &Batch,
    ) -> Result<Option<rules::item_payload::Delta>, RejectReason> {
        batch
            .runtimes
            .iter()
            .find(|r| r.kind == save::RUNTIME_PAYLOADS)
            .map(|r| {
                rules::item_payload::Delta::decode(&r.payload)
                    .map_err(|_| RejectReason::StorageUnavailable)
            })
            .transpose()
    }
    pub(super) fn plan_payloads(
        &self,
        batch: &Batch,
        payloads: Vec<rules::item_payload::Payload>,
    ) -> Result<Option<RuntimeRecord>, RejectReason> {
        if payloads.is_empty() {
            return Ok(None);
        }
        let delta = match self.payload_delta(batch)? {
            Some(delta) => delta,
            None => self
                .runtimes
                .payloads
                .plan()
                .map_err(|_| RejectReason::StorageUnavailable)?,
        };
        let delta = self
            .runtimes
            .payloads
            .extend(delta, payloads, std::iter::empty())
            .map_err(|error| match error {
                rules::item_payload::Error::Bounds => RejectReason::Capacity,
                _ => RejectReason::InvalidCell,
            })?;
        self.runtimes
            .payloads
            .validate(&delta)
            .map_err(|_| RejectReason::InvalidCell)?;
        if delta.is_empty() {
            return Ok(None);
        }
        Ok(Some(RuntimeRecord {
            kind: save::RUNTIME_PAYLOADS,
            payload: delta.encode().map_err(|_| RejectReason::Capacity)?,
        }))
    }
    pub(super) fn stage_payloads(batch: &mut Batch, record: Option<RuntimeRecord>) {
        if let Some(record) = record {
            if let Some(existing) = batch
                .runtimes
                .iter_mut()
                .find(|r| r.kind == save::RUNTIME_PAYLOADS)
            {
                *existing = record
            } else {
                batch.runtimes.push(record)
            }
        }
    }
}
