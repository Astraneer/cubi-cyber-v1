//! Owned player/entity interactions staged at the same durable frontier.
use super::*;
use rules::entities::{
    EntityError, EntityId,
    combat::{AttackPose, Combatant},
};
#[derive(Clone, Copy)]
pub(super) struct EntityAction {
    pub id: ActionId,
    pub target: u64,
    pub inventory: net::InventoryGuard,
    pub use_entity: bool,
}
fn reason(error: EntityError) -> RejectReason {
    match error {
        EntityError::UnknownId | EntityError::UnknownInput => RejectReason::Unknown,
        EntityError::StalePlan => RejectReason::Stale,
        EntityError::Capacity => RejectReason::Capacity,
        EntityError::NoEntitlement => RejectReason::OutOfReach,
        _ => RejectReason::InvalidCell,
    }
}
fn combatant(peer: &Peer) -> Combatant {
    Combatant {
        id: runtime::player_entity(peer.identity),
        inventory: peer.inventory,
        health: peer.health,
        status: peer.combat,
        position: peer.player.pos,
        velocity: peer.player.vel,
        crouching: peer.player.crouch,
        pose: AttackPose {
            on_ground: peer.player.on_ground,
            fall: peer.player.fall,
            in_water: peer.player.in_water,
            sprinting: peer.player.sprinting,
            flying: peer.player.flying,
        },
    }
}
fn validate_guard(peer: &Peer, guard: net::InventoryGuard) -> Result<(), RejectReason> {
    let stack = peer.inventory.hand();
    if guard.revision != peer.inventory.revision()
        || usize::from(guard.selected) != peer.inventory.selected()
        || guard.item != stack.item()
        || guard.count != stack.count()
        || guard.damage != stack.damage()
        || guard.payload != stack.payload().map_or(0, |id| id.0)
    {
        return Err(RejectReason::Stale);
    }
    Ok(())
}
impl Server {
    pub(super) fn entity_action(
        &mut self,
        peer: &mut Peer,
        request: EntityAction,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        if !peer.health.alive() || peer.player.mode == GameMode::Spectator {
            return Err(RejectReason::Unauthorized);
        }
        validate_guard(peer, request.inventory)?;
        let target = EntityId(request.target);
        if self
            .peers
            .values()
            .any(|p| runtime::player_entity(p.identity) == target)
        {
            if request.use_entity {
                return Err(RejectReason::Unsupported);
            }
            return self.player_attack(peer, request, batch);
        }
        let mob = self
            .runtimes
            .mobs
            .entities()
            .get(target)
            .ok_or(RejectReason::Unknown)?;
        if mob.space != peer.space {
            return Err(RejectReason::OutOfReach);
        }
        let point = mob.body.position.map(|v| v.floor() as i32);
        if peer.space == SpaceId(0) && self.protected(point, self.identities[&peer.identity].role) {
            return Err(RejectReason::Protected);
        }
        let pending = batch
            .runtimes
            .iter()
            .find(|r| r.kind == save::RUNTIME_MOBS)
            .map(|r| natural_records::decode_delta(&r.payload))
            .transpose()
            .map_err(|_| RejectReason::Capacity)?;
        if pending.as_ref().is_some_and(|(_, meta)| meta.is_some()) {
            return Err(RejectReason::Capacity);
        }
        let pending = pending.as_ref().and_then(|(mobs, _)| mobs.as_ref());
        let actor = combatant(peer);
        let mut identity = [0; 32];
        identity[..16].copy_from_slice(&peer.identity.0);
        identity[16..24].copy_from_slice(&request.id.0.to_le_bytes());
        identity[24..].copy_from_slice(&request.target.to_le_bytes());
        let hash = blake3::derive_key("cubi owned entity interaction v1", &identity);
        let event = u64::from_le_bytes(hash[..8].try_into().expect("hash prefix"));
        let look = peer.player.look();
        let mut plan = self
            .runtimes
            .mobs
            .plan_interaction_after(
                &self.world,
                entities::interactions::Request {
                    actor,
                    context: rules::entities::combat::mob::AttackContext {
                        space: peer.space,
                        eye: peer.player.eye(1.),
                        look_horizontal: [look[0], look[2]],
                        expected_held: peer.inventory.hand(),
                        now: self.tick,
                    },
                    target,
                    action: event,
                },
                pending,
                request.use_entity,
            )
            .map_err(reason)?;
        self.runtimes
            .mobs
            .validate_interaction_batch(&self.world, &actor, &plan)
            .map_err(reason)?;
        let mut rows = Vec::new();
        for drops in &plan.drops {
            for (ordinal, stack) in drops
                .stacks
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, s)| !s.is_empty())
            {
                rows.push(
                    self.mob_item(
                        mob_owner::DropOrigin {
                            actor: target,
                            action: event,
                            space: drops.space,
                            position: drops.position,
                            delay: 10,
                        },
                        ordinal,
                        stack,
                    )
                    .map_err(|_| RejectReason::Capacity)?,
                );
            }
        }
        let item_record = self.prepare_owned_item_rows(batch, &rows)?;
        plan.delta = self
            .runtimes
            .mobs
            .extend_owned(plan.delta, [], &[(target, true)], &[])
            .map_err(reason)?;
        let payload = plan.delta.encode().map_err(reason)?;
        // All quotas, ownership, before values and codecs pass before mutation.
        peer.health = plan.actor.health;
        peer.combat = plan.actor.status;
        peer.inventory = plan.actor.inventory;
        peer.player.vel = plan.actor.velocity;
        peer.player.health = peer.health.hp();
        peer.player.swing_hand();
        Self::replace_runtime(
            batch,
            save::RuntimeRecord {
                kind: save::RUNTIME_MOBS,
                payload,
            },
        );
        if let Some(record) = item_record {
            Self::replace_runtime(batch, record);
        }
        self.mob_owner.note_birth(target);
        Ok(())
    }
    fn prepare_owned_item_rows(
        &self,
        batch: &Batch,
        rows: &[rules::entities::item::ItemEntity],
    ) -> Result<Option<save::RuntimeRecord>, RejectReason> {
        if rows.is_empty() {
            return Ok(None);
        }
        let pending = batch
            .runtimes
            .iter()
            .find(|r| r.kind == save::RUNTIME_ITEMS)
            .map(|r| entities::items::ItemDelta::decode(&r.payload, 8192))
            .transpose()
            .map_err(reason)?;
        let delta = pending
            .map_or_else(|| self.runtimes.items.plan_activity(&[]), Ok)
            .map_err(reason)?;
        let delta = self
            .runtimes
            .items
            .extend_spawn(delta, rows.iter().copied(), true)
            .map_err(reason)?;
        Ok(Some(save::RuntimeRecord {
            kind: save::RUNTIME_ITEMS,
            payload: delta.encode().map_err(reason)?,
        }))
    }
    fn replace_runtime(batch: &mut Batch, record: save::RuntimeRecord) {
        if let Some(old) = batch.runtimes.iter_mut().find(|r| r.kind == record.kind) {
            *old = record;
        } else {
            batch.runtimes.push(record);
        }
    }
    fn player_attack(
        &mut self,
        peer: &mut Peer,
        request: EntityAction,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        let (connection, before, window, space, mode) = self
            .peers
            .iter()
            .find(|(_, p)| runtime::player_entity(p.identity).0 == request.target)
            .map(|(c, p)| (*c, combatant(p), p.window.clone(), p.space, p.player.mode))
            .ok_or(RejectReason::Unknown)?;
        if space != peer.space {
            return Err(RejectReason::OutOfReach);
        }
        if peer.player.mode != GameMode::Survival || mode != GameMode::Survival {
            return Err(RejectReason::Unauthorized);
        }
        let actor = combatant(peer);
        let look = peer.player.look();
        let plan = rules::entities::combat::AttackPlan::prepare(
            &actor,
            &before,
            peer.player.eye(1.),
            [look[0], look[2]],
            |a, b| sim::entities::line_of_sight(&WorldQuery::new(&self.world, space), a, b),
        )
        .map_err(reason)?;
        let (inventory, window, item_record) = if plan.hit.is_some_and(|h| h.died) {
            let identity = self.peers[&connection].identity;
            let death = self.prepare_player_death(
                DeathOwner {
                    identity,
                    space,
                    position: before.position,
                    inventory: plan.target.inventory,
                    window,
                },
                batch,
            )?;
            (death.inventory, death.window, death.items)
        } else {
            (plan.target.inventory, window, None)
        };
        // Check encoding before either owner is changed. The world is single-
        // threaded here; no unvalidated external callback runs during planning.
        let target = self.peers.get_mut(&connection).expect("guarded player");
        let rollback = (
            target.inventory,
            target.window.clone(),
            target.health,
            target.combat,
            target.player.vel,
            target.player.health,
        );
        target.inventory = inventory;
        target.window = window;
        target.health = plan.target.health;
        target.combat = plan.target.status;
        target.player.vel = plan.target.velocity;
        target.player.health = target.health.hp();
        let saved = match runtime::capture_peer(target) {
            Ok(saved) => saved,
            Err(_) => {
                (
                    target.inventory,
                    target.window,
                    target.health,
                    target.combat,
                    target.player.vel,
                    target.player.health,
                ) = rollback;
                return Err(RejectReason::Capacity);
            }
        };
        self.saved_players.insert(target.identity, saved.clone());
        if let Some(old) = batch
            .players
            .iter_mut()
            .find(|p| p.state.identity == target.identity)
        {
            *old = saved;
        } else {
            batch.players.push(saved);
        }
        peer.inventory = plan.attacker.inventory;
        peer.health = plan.attacker.health;
        peer.combat = plan.attacker.status;
        peer.player.vel = plan.attacker.velocity;
        peer.player.health = peer.health.hp();
        peer.player.swing_hand();
        if let Some(record) = item_record {
            Self::replace_runtime(batch, record);
        }
        Ok(())
    }
    fn prepare_player_death(
        &self,
        mut owned: DeathOwner,
        batch: &Batch,
    ) -> Result<DeathPlan, RejectReason> {
        let mut drops = Vec::new();
        if owned.window.state.closed()
            && (!owned.window.state.cursor().is_empty()
                || owned
                    .window
                    .state
                    .craft()
                    .cells()
                    .iter()
                    .any(|s| !s.is_empty()))
        {
            return Err(RejectReason::Stale);
        }
        if !owned.window.state.closed() {
            let close = owned
                .window
                .state
                .plan_close(&owned.inventory, owned.window.state.guard(&owned.inventory))
                .map_err(|_| RejectReason::Capacity)?;
            drops.extend_from_slice(close.drops());
            owned.inventory = *close.inventory_after();
            owned.window.state = close.window_after().clone();
        }
        drops.extend(
            owned
                .inventory
                .slots()
                .iter()
                .copied()
                .filter(|s| !s.is_empty()),
        );
        let mut rows = Vec::with_capacity(drops.len());
        for (ordinal, stack) in drops.into_iter().enumerate() {
            rows.push(
                self.mob_item(
                    mob_owner::DropOrigin {
                        actor: runtime::player_entity(owned.identity),
                        action: self.tick.0,
                        space: owned.space,
                        position: owned.position,
                        delay: 40,
                    },
                    ordinal,
                    stack,
                )
                .map_err(|_| RejectReason::Capacity)?,
            );
        }
        let items = self.prepare_owned_item_rows(batch, &rows)?;
        owned.inventory = *owned
            .inventory
            .plan_death()
            .map_err(|_| RejectReason::Capacity)?
            .after();
        Ok(DeathPlan {
            inventory: owned.inventory,
            window: owned.window,
            items,
        })
    }
    pub(super) fn clear_owned_death(
        &self,
        peer: &mut Peer,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        if peer.health.alive()
            || (peer.window.state.closed()
                && peer.window.state.cursor().is_empty()
                && peer
                    .window
                    .state
                    .craft()
                    .cells()
                    .iter()
                    .all(|s| s.is_empty())
                && peer.inventory.slots().iter().all(|s| s.is_empty()))
        {
            return Ok(());
        }
        let plan = self.prepare_player_death(
            DeathOwner {
                identity: peer.identity,
                space: peer.space,
                position: peer.player.pos,
                inventory: peer.inventory,
                window: peer.window.clone(),
            },
            batch,
        )?;
        peer.inventory = plan.inventory;
        peer.window = plan.window;
        if let Some(record) = plan.items {
            Self::replace_runtime(batch, record);
        }
        Ok(())
    }
    pub(super) fn respawn(
        &mut self,
        peer: &mut Peer,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        if !peer.health.may_respawn() {
            return Err(RejectReason::Unauthorized);
        }
        // Search a bounded local ring. Every candidate needs known clear body
        // cells and actual support; missing pages are requested, never invented.
        let mut selected = None;
        'search: for radius in 0i32..=4 {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if dx.abs().max(dz.abs()) != radius {
                        continue;
                    }
                    for dy in 0..=4 {
                        let position = [
                            self.spawn[0] + f64::from(dx),
                            self.spawn[1] + f64::from(dy),
                            self.spawn[2] + f64::from(dz),
                        ];
                        let candidate = Player::new(position);
                        let bounds = candidate.bbox().contract(1e-7);
                        let mut clear = true;
                        for z in bounds.lo[2].floor() as i32..=bounds.hi[2].floor() as i32 {
                            for y in bounds.lo[1].floor() as i32..=bounds.hi[1].floor() as i32 {
                                for x in bounds.lo[0].floor() as i32..=bounds.hi[0].floor() as i32 {
                                    let p = BlockPos {
                                        space: SpaceId(0),
                                        x,
                                        y,
                                        z,
                                    };
                                    match self.world.read(p) {
                                        CellRead::Unknown => {
                                            self.request_generation(p.page());
                                            clear = false;
                                        }
                                        CellRead::Known(cell) => {
                                            if rules::properties(cell).fluid
                                                || rules::blocks::collision_of(cell.raw())
                                                    .iter()
                                                    .any(|shape| {
                                                        bounds.intersects(&shape.moved([
                                                            f64::from(x),
                                                            f64::from(y),
                                                            f64::from(z),
                                                        ]))
                                                    })
                                            {
                                                clear = false;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let below = BlockPos {
                            space: SpaceId(0),
                            x: position[0].floor() as i32,
                            y: (position[1] - 0.01).floor() as i32,
                            z: position[2].floor() as i32,
                        };
                        if clear
                            && matches!(self.world.read(below),CellRead::Known(c)if rules::blocks::face_full(c.raw(),1))
                        {
                            selected = Some(candidate);
                            break 'search;
                        }
                    }
                }
            }
        }
        let player = selected.ok_or(RejectReason::Unknown)?;
        self.clear_owned_death(peer, batch)?;
        peer.player = player;
        peer.raw_view.invalidate_observer();
        peer.space = SpaceId(0);
        peer.health = rules::survival::health::Health::new();
        peer.combat = rules::entities::combat::CombatStatus::default();
        peer.mining = None;
        peer.interaction.mining.cancel();
        peer.last_keys = 0;
        Ok(())
    }
}

struct DeathOwner {
    identity: PlayerId,
    space: SpaceId,
    position: [f64; 3],
    inventory: rules::survival::Inventory,
    window: windows::BoundWindow,
}
struct DeathPlan {
    inventory: rules::survival::Inventory,
    window: windows::BoundWindow,
    items: Option<save::RuntimeRecord>,
}
