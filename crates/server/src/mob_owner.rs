//! Canonical M7 entity phase. World edits, owned HP/tool/death changes and new
//! item rows share the same WAL record; the mob ledger ACK remains after fsync.
use super::*;
use rules::{
    entities::{
        EntityError, EntityId,
        item::ItemEntity,
        mob::{Mob, PlayerProbe},
    },
    survival::{
        Inventory,
        health::{DamageKind, Health},
    },
};
const MAX_MOBS: usize = 5000;
pub(super) struct Owner {
    pending: VecDeque<Mob>,
    active: BTreeSet<EntityId>,
    activity: VecDeque<(EntityId, bool)>,
    centers: Vec<(PlayerId, SpaceId, PageId)>,
    initialized: bool,
    pub(super) due: bool,
    remaining: usize,
    seed: u64,
    query: Vec<EntityId>,
}
impl Owner {
    pub(super) fn new(
        seed: u64,
        store: &rules::entities::mob::EntityStore,
        remaining: Option<usize>,
    ) -> Self {
        Self {
            pending: VecDeque::new(),
            active: store
                .rows()
                .iter()
                .filter(|m| store.is_active(m.id))
                .map(|m| m.id)
                .collect(),
            activity: VecDeque::new(),
            centers: Vec::with_capacity(64),
            initialized: false,
            due: remaining.map_or(!store.is_empty(), |r| r > 0),
            remaining: remaining.unwrap_or_else(|| store.active_len()),
            seed,
            query: vec![EntityId(0); MAX_MOBS],
        }
    }
    pub(super) fn remaining(&self) -> usize {
        self.remaining
    }
    pub(super) fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub(super) fn contains_pending(&self, id: EntityId) -> bool {
        self.pending.iter().any(|mob| mob.id == id)
    }
    pub(super) fn note_birth(&mut self, id: EntityId) {
        self.active.insert(id);
    }
    pub(super) fn start(&mut self, active: usize) {
        self.due = true;
        self.remaining = active;
    }
}
#[derive(Clone, Copy)]
pub(super) struct DropOrigin {
    pub(super) actor: EntityId,
    pub(super) action: u64,
    pub(super) space: SpaceId,
    pub(super) position: [f64; 3],
    pub(super) delay: u16,
}
struct Living {
    health: Health,
    inventory: Inventory,
    window: windows::BoundWindow,
    velocity: [f64; 3],
}
impl Server {
    /// Trusted owner submission; this is pending intent, never a durable ACK.
    /// Live clients have no arbitrary spawn or actor-state upload operation.
    pub fn queue_mob_spawn(&mut self, mob: Mob) -> Result<(), Box<Mob>> {
        let invalid = self.failed
            || self.mob_owner.pending.len() + self.runtimes.mobs.entities().len() >= MAX_MOBS
            || self.runtimes.mobs.entities().get(mob.id).is_some()
            || self.runtimes.items.entities().get(mob.id).is_some()
            || self.mob_owner.pending.iter().any(|m| m.id == mob.id)
            || self
                .identities
                .keys()
                .any(|p| runtime::player_entity(*p) == mob.id)
            || self.world.space(mob.space).is_none()
            || mob.encode().is_err();
        if invalid {
            return Err(Box::new(mob));
        }
        self.mob_owner.pending.push_back(mob);
        Ok(())
    }
    pub(super) fn mob_active_at(&self, space: SpaceId, position: [f64; 3]) -> bool {
        self.any_tick_actor(|p| {
            p.space == space
                && p.player.mode != GameMode::Spectator
                && (0..3).all(|a| (p.player.pos[a] - position[a]).abs() <= 128.)
        })
    }
    fn refresh_mob_activity(&mut self) -> Result<(), ServerError> {
        let actors: Vec<_> = self
            .tick_actor_refs()
            .filter(|p| p.player.mode != GameMode::Spectator)
            .map(|p| (p.identity, p.space, p.player.pos))
            .collect();
        let centers: Vec<_> = actors
            .iter()
            .map(|&(identity, space, position)| (identity, space, position_page(space, position)))
            .collect();
        if self.mob_owner.initialized && centers == self.mob_owner.centers {
            return Ok(());
        }
        let store = self.runtimes.mobs.entities();
        let mut desired = BTreeSet::new();
        for &(_, space, position) in &actors {
            let low = position.map(|v| v - 128.);
            let high = position.map(|v| v + 128.);
            let (count, work) = store
                .query_ids(
                    low,
                    high,
                    rules::entities::spatial::QueryBudget {
                        buckets: 40_000,
                        probes: 65_536,
                        candidates: MAX_MOBS as u64,
                    },
                    &mut self.mob_owner.query,
                )
                .map_err(|_| ServerError::Invalid("mob activity spatial query"))?;
            self.counters.mob_activity_buckets += work.buckets;
            if work.overflow {
                self.counters.mob_deferred += 1;
                return Ok(());
            }
            for id in &self.mob_owner.query[..count] {
                if store.get(*id).is_some_and(|m| m.space == space) {
                    desired.insert(*id);
                }
            }
        }
        self.mob_owner.activity.clear();
        for id in self.mob_owner.active.union(&desired) {
            let active = desired.contains(id);
            if store.get(*id).is_some() && store.is_active(*id) != active {
                self.mob_owner.activity.push_back((*id, active));
            }
        }
        if self
            .runtimes
            .mob_progress
            .is_none_or(|p| p.tick != self.tick)
        {
            self.mob_owner.remaining = self.mob_owner.remaining.max(desired.len());
        }
        self.mob_owner.centers = centers;
        self.mob_owner.initialized = true;
        Ok(())
    }
    pub(super) fn mob_item(
        &self,
        origin: DropOrigin,
        ordinal: usize,
        stack: rules::stack::ItemStack,
    ) -> Result<ItemEntity, ServerError> {
        let DropOrigin {
            actor,
            action,
            space,
            position,
            delay,
        } = origin;
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&actor.0.to_le_bytes());
        bytes[8..16].copy_from_slice(&action.to_le_bytes());
        bytes[16..].copy_from_slice(&(ordinal as u64).to_le_bytes());
        let hash = blake3::derive_key("cubi mob-owned item row v1", &bytes);
        let id = EntityId(u64::from_le_bytes(
            hash[..8].try_into().expect("hash prefix"),
        ));
        let mut item = ItemEntity::new(id, space, stack, position, self.mob_owner.seed, delay)
            .map_err(|_| ServerError::Invalid("mob item row"))?;
        item.last_tick = self.tick;
        Ok(item)
    }
    pub(super) fn tick_mobs(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        if !self.mob_owner.due && self.mob_owner.pending.is_empty() {
            return Ok(());
        }
        if batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_MOBS) {
            return Ok(());
        }
        if self.runtimes.mobs.entities().is_empty() && self.mob_owner.pending.is_empty() {
            self.mob_owner.due = false;
            self.mob_owner.remaining = 0;
            return Ok(());
        }
        self.refresh_mob_activity()?;
        let activity: Vec<_> = self.mob_owner.activity.iter().take(1024).copied().collect();
        let mut rows = Vec::new();
        let mut living: BTreeMap<PlayerId, Living> = BTreeMap::new();
        let mut transactions = Vec::new();
        let mut work = entities::Work::default();
        let selected = self
            .mob_owner
            .remaining
            .min(1024)
            .min(self.runtimes.mobs.entities().active_len());
        let mut delta = if !activity.is_empty() {
            self.runtimes
                .mobs
                .plan_activity(&activity)
                .map_err(|_| ServerError::Invalid("mob activity plan"))?
        } else if selected > 0 {
            let players: Vec<_> = self
                .tick_actor_refs()
                .map(|p| {
                    let feet = p.player.pos.map(|v| v.floor() as i32);
                    PlayerProbe {
                        id: runtime::player_entity(p.identity),
                        space: p.space,
                        position: p.player.pos,
                        eye: p.player.eye(1.),
                        hands: [
                            p.inventory.hand().item(),
                            p.inventory.get(40).expect("offhand").item(),
                        ],
                        alive: p.health.alive(),
                        creative: p.player.mode != GameMode::Survival || p.player.noclip,
                        spectator: p.player.mode == GameMode::Spectator,
                        protected: p.space == SpaceId(0) && self.protected(feet, Role::Player),
                    }
                })
                .collect();
            let plan = self
                .runtimes
                .mobs
                .plan_tick(
                    &self.world,
                    self.tick,
                    &players,
                    entities::Budget {
                        actors: selected,
                        sky_darkness: mob_spawn::ambient_darkness(self.tick.0 % 24000),
                        ..entities::Budget::default()
                    },
                )
                .map_err(|_| ServerError::Invalid("mob AI/physics plan"))?;
            self.runtimes
                .mobs
                .validate(&self.world, &plan)
                .map_err(|_| ServerError::Invalid("mob phase input guard"))?;
            for drops in &plan.drops {
                for (ordinal, stack) in drops
                    .stacks
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, s)| !s.is_empty())
                {
                    rows.push(self.mob_item(
                        DropOrigin {
                            actor: drops.actor,
                            action: drops.action,
                            space: drops.space,
                            position: drops.position,
                            delay: 10,
                        },
                        ordinal,
                        stack,
                    )?);
                }
            }
            for attack in &plan.attacks {
                let Some(peer) = self.tick_actor_refs().find(|p| {
                    p.space == attack.space && runtime::player_entity(p.identity) == attack.target
                }) else {
                    continue;
                };
                let patch = living.entry(peer.identity).or_insert_with(|| Living {
                    health: *peer.health,
                    inventory: *peer.inventory,
                    window: peer.window.clone(),
                    velocity: peer.player.vel,
                });
                let Some(hit) = patch.health.damage(
                    DamageKind::MobAttack,
                    attack.amount,
                    patch.inventory.armor(),
                    peer.player.mode != GameMode::Survival || peer.player.noclip,
                ) else {
                    continue;
                };
                if hit.armor_wear > 0 {
                    let plan = patch
                        .inventory
                        .plan_armor_wear(hit.armor_wear)
                        .map_err(|_| ServerError::Invalid("mob armor wear"))?;
                    patch.inventory = *plan.after();
                }
                if hit.full && !hit.died {
                    patch.velocity = rules::entities::combat::knockback(
                        patch.velocity,
                        peer.player.on_ground,
                        0.4,
                        [
                            attack.from[0] - peer.player.pos[0],
                            attack.from[2] - peer.player.pos[2],
                        ],
                    );
                }
                if hit.died {
                    let mut drops = Vec::new();
                    if !patch.window.state.closed() {
                        let close = patch
                            .window
                            .state
                            .plan_close(
                                &patch.inventory,
                                patch.window.state.guard(&patch.inventory),
                            )
                            .map_err(|_| ServerError::Invalid("death window close"))?;
                        drops.extend_from_slice(close.drops());
                        patch.inventory = *close.inventory_after();
                        patch.window.state = close.window_after().clone();
                    }
                    drops.extend(
                        patch
                            .inventory
                            .slots()
                            .iter()
                            .copied()
                            .filter(|s| !s.is_empty()),
                    );
                    let death = patch
                        .inventory
                        .plan_death()
                        .map_err(|_| ServerError::Invalid("death inventory"))?;
                    patch.inventory = *death.after();
                    for (ordinal, stack) in drops.into_iter().enumerate() {
                        rows.push(self.mob_item(
                            DropOrigin {
                                actor: attack.target,
                                action: self.tick.0,
                                space: peer.space,
                                position: peer.player.pos,
                                delay: 40,
                            },
                            ordinal,
                            stack,
                        )?);
                    }
                }
            }
            transactions = plan.transactions;
            work = plan.work;
            plan.delta
        } else {
            self.runtimes
                .mobs
                .plan_activity(&[])
                .map_err(|_| ServerError::Invalid("mob spawn boundary"))?
        };
        let mut removals = Vec::new();
        let mut active = Vec::new();
        for &(id, enabled) in &activity {
            if !enabled
                && self
                    .runtimes
                    .mobs
                    .entities()
                    .get(id)
                    .is_some_and(|m| m.kind.definition().hostile)
            {
                removals.push(id);
            }
        }
        for patch in &delta.patches {
            if let Some(after) = patch.after
                && !self.mob_active_at(after.space, after.body.position)
            {
                if after.kind.definition().hostile {
                    removals.push(after.id);
                } else {
                    active.push((after.id, false));
                }
            }
        }
        let births: Vec<_> = self
            .mob_owner
            .pending
            .iter()
            .take(64)
            .map(|m| {
                let mut m = *m;
                m.last_tick = self.tick;
                (m, self.mob_active_at(m.space, m.body.position))
            })
            .collect();
        delta = self
            .runtimes
            .mobs
            .extend_owned(delta, births.iter().copied(), &active, &removals)
            .map_err(|_| ServerError::Invalid("mob births/activity/retirement"))?;
        let item_delta = batch
            .runtimes
            .iter()
            .find(|r| r.kind == save::RUNTIME_ITEMS)
            .map(|r| entities::items::ItemDelta::decode(&r.payload, 8192))
            .transpose()
            .map_err(|_| ServerError::Invalid("pending item ledger"))?
            .map_or_else(|| self.runtimes.items.plan_activity(&[]), Ok)
            .map_err(|_| ServerError::Invalid("mob drop item boundary"))?;
        let item_delta =
            match self
                .runtimes
                .items
                .extend_spawn(item_delta, rows.iter().copied(), true)
            {
                Ok(delta) => delta,
                Err(EntityError::Capacity) => {
                    self.counters.mob_deferred += 1;
                    return Ok(());
                }
                Err(_) => return Err(ServerError::Invalid("mob owned item collision")),
            };
        let encoded = if delta.is_empty() {
            None
        } else {
            Some(natural_records::delta(Some(&delta), None).map_err(ServerError::Invalid)?)
        };
        let item_encoded = if rows.is_empty() {
            None
        } else {
            Some(
                item_delta
                    .encode()
                    .map_err(|_| ServerError::Invalid("mob item delta bounds"))?,
            )
        };
        for tx in transactions {
            batch.changes.push(
                self.world
                    .commit(tx)
                    .map_err(|_| ServerError::Invalid("mob grass transaction"))?,
            );
        }
        for (identity, patch) in living {
            let saved = {
                let actor = self
                    .tick_actor_mut(identity)
                    .ok_or(ServerError::Invalid("mob victim actor changed"))?;
                *actor.health = patch.health;
                *actor.inventory = patch.inventory;
                *actor.window = patch.window;
                actor.player.vel = patch.velocity;
                actor.player.health = actor.health.hp();
                actor.capture()?
            };
            self.record_tick_actor(saved, batch);
        }
        if let Some(payload) = item_encoded {
            let record = save::RuntimeRecord {
                kind: save::RUNTIME_ITEMS,
                payload,
            };
            if let Some(old) = batch.runtimes.iter_mut().find(|r| r.kind == record.kind) {
                *old = record
            } else {
                batch.runtimes.push(record);
            }
        }
        for patch in &delta.patches {
            let id = patch
                .id()
                .map_err(|_| ServerError::Invalid("mob patch id"))?;
            if patch.after.is_some() && patch.after_active {
                self.mob_owner.active.insert(id);
            } else {
                self.mob_owner.active.remove(&id);
            }
        }
        for _ in 0..births.len() {
            self.mob_owner.pending.pop_front();
        }
        for _ in 0..activity.len() {
            self.mob_owner.activity.pop_front();
        }
        if activity.is_empty() {
            self.mob_owner.remaining = if selected == 0 {
                0
            } else {
                self.mob_owner
                    .remaining
                    .saturating_sub(work.visited as usize)
            };
        }
        self.mob_owner.due = self.mob_owner.remaining > 0
            || !self.mob_owner.activity.is_empty()
            || !self.mob_owner.pending.is_empty();
        self.counters.mob_steps += work.actors;
        self.counters.mob_cell_reads += work.queries;
        self.counters.mob_waiting += work.waiting_world;
        self.counters.mob_drops += rows.len() as u64;
        self.counters.audio_cues_not_recorded += work.audio_cues_clipped;
        if encoded.is_some() || selected > 0 || !activity.is_empty() || !births.is_empty() {
            let progress = natural_records::Progress {
                tick: self.tick,
                remaining: self.mob_owner.remaining,
            };
            let payload =
                natural_records::with_progress(encoded.unwrap_or_default(), Some(progress))
                    .map_err(ServerError::Invalid)?;
            batch.runtimes.push(save::RuntimeRecord {
                kind: save::RUNTIME_MOBS,
                payload,
            });
        }
        Ok(())
    }
    pub(super) fn finish_mob_phase(&mut self) -> Result<bool, ServerError> {
        if !self.mob_owner.due && self.mob_owner.pending.is_empty() {
            return Ok(false);
        }
        let mut batch = Batch::empty(self.tick);
        self.tick_mobs(&mut batch)?;
        self.capture_yield_context(&mut batch)?;
        if !batch.is_empty() {
            if let Some(plan) = self
                .runtimes
                .height
                .plan_batch(&batch.changes)
                .map_err(|_| ServerError::Invalid("mob height admission"))?
            {
                batch.runtimes.push(save::RuntimeRecord {
                    kind: save::RUNTIME_HEIGHT,
                    payload: plan
                        .encode()
                        .map_err(|_| ServerError::Invalid("mob height delta"))?,
                });
            }
            self.submit_batch(batch, Vec::new());
            return Ok(true);
        }
        Ok(false)
    }
}
