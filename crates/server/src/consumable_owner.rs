//! Canonical held-food and bucket actions. Private player changes, world cells,
//! explicit item spills, fluid wakes and causal cues share the normal WAL batch.
use super::*;
use rules::survival::{
    fluid,
    health::{EatOutcome, Eating},
};
#[derive(Default)]
pub(super) struct UseState {
    held: bool,
    eating: Option<Eating>,
    last_tick: Option<Tick>,
    start_action: ActionId,
    release_action: ActionId,
}
impl UseState {
    pub(super) fn slowing(&self) -> bool {
        self.eating.is_some()
    }
    /// An admitted newer release cancels held use immediately, even when its
    /// durable reply is waiting behind other FIFO actions. Older reordered
    /// presses cannot restart that already released lifecycle.
    pub(super) fn observe_release(&mut self, id: ActionId) {
        self.release_action = self.release_action.max(id);
        if self.start_action <= self.release_action {
            self.held = false;
            self.eating = None;
        }
    }
}
fn guard(peer: &Peer, value: net::InventoryGuard) -> Result<(), RejectReason> {
    let held = peer.inventory.hand();
    if value.revision != peer.inventory.revision()
        || usize::from(value.selected) != peer.inventory.selected()
        || value.item != held.item()
        || value.count != held.count()
        || value.damage != held.damage()
        || value.payload != held.payload().map_or(0, |id| id.0)
    {
        return Err(RejectReason::Stale);
    }
    Ok(())
}
fn survival_reason(error: rules::survival::SurvivalError) -> RejectReason {
    use rules::survival::SurvivalError;
    match error {
        SurvivalError::UnknownInput => RejectReason::Unknown,
        SurvivalError::Capacity => RejectReason::Capacity,
        _ => RejectReason::Unauthorized,
    }
}
impl Server {
    pub(super) fn use_food(
        &self,
        peer: &mut Peer,
        id: ActionId,
        held: bool,
        inventory: net::InventoryGuard,
    ) -> Result<(), RejectReason> {
        // Release cannot be stranded by an inventory revision caused by the last
        // completed bite, a slot change, damage, or death.
        if !held {
            peer.use_item.observe_release(id);
            return Ok(());
        }
        if id <= peer.use_item.release_action {
            return Ok(());
        }
        if !peer.health.alive() || peer.player.mode == GameMode::Spectator {
            return Err(RejectReason::Unauthorized);
        }
        guard(peer, inventory)?;
        if peer.mining.is_some() {
            return Err(RejectReason::RateLimited);
        }
        if peer.use_item.held && peer.use_item.eating.is_some() {
            return Ok(());
        }
        let eating =
            Eating::start(&peer.health, &peer.inventory).ok_or(RejectReason::Unauthorized)?;
        peer.use_item.held = true;
        peer.use_item.start_action = id;
        peer.use_item.eating = Some(eating);
        Ok(())
    }
    pub(super) fn advance_use_item(
        &mut self,
        peer: &mut Peer,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        if peer.use_item.last_tick == Some(self.tick) {
            return Ok(());
        }
        peer.use_item.last_tick = Some(self.tick);
        if !peer.use_item.held || !peer.health.alive() || peer.player.mode == GameMode::Spectator {
            peer.use_item.held = false;
            peer.use_item.eating = None;
            return Ok(());
        }
        if peer.use_item.eating.is_none() {
            peer.use_item.eating = Eating::start(&peer.health, &peer.inventory);
        }
        let Some(mut eating) = peer.use_item.eating else {
            return Ok(());
        };
        let before = eating.ticks_left();
        let mut inventory = peer.inventory;
        let mut health = peer.health;
        let outcome = eating
            .tick(true, &mut health, &mut inventory)
            .map_err(survival_reason)?;
        peer.inventory = inventory;
        peer.health = health;
        peer.player.health = health.hp();
        peer.use_item.eating = (outcome == EatOutcome::Eating).then_some(eating);
        if outcome == EatOutcome::Cancelled {
            peer.use_item.held = false;
        }
        let kind = if outcome == EatOutcome::Finished {
            Some(net::audio_cue::Kind::EatDone)
        } else if outcome == EatOutcome::Eating && 32 - before > 7 && before.is_multiple_of(4) {
            Some(net::audio_cue::Kind::Eat)
        } else {
            None
        };
        if let Some(kind) = kind {
            if outcome == EatOutcome::Finished {
                self.commit_owned_audio(
                    batch,
                    peer.identity,
                    net::audio_cue::Cue {
                        kind: net::audio_cue::Kind::Eat,
                        actor: runtime::player_entity(peer.identity).0,
                        sequence: self.tick.0,
                        tick: self.tick,
                        space: peer.space,
                        position: peer.player.pos,
                    },
                );
            }
            self.commit_owned_audio(
                batch,
                peer.identity,
                net::audio_cue::Cue {
                    kind,
                    actor: runtime::player_entity(peer.identity).0,
                    sequence: self.tick.0,
                    tick: self.tick,
                    space: peer.space,
                    position: peer.player.pos,
                },
            );
        }
        Ok(())
    }
    pub(super) fn use_bucket(
        &mut self,
        peer: &mut Peer,
        id: ActionId,
        position: BlockPos,
        expected: Cell,
        inventory: net::InventoryGuard,
        batch: &mut Batch,
    ) -> Result<TxId, RejectReason> {
        if !peer.health.alive() || peer.player.mode == GameMode::Spectator {
            return Err(RejectReason::Unauthorized);
        }
        if position.space != peer.space {
            return Err(RejectReason::OutOfReach);
        }
        if self.tick < peer.place_ready_at {
            return Err(RejectReason::RateLimited);
        }
        guard(peer, inventory)?;
        let rules::items::ItemKind::Bucket { fluid: kind } =
            rules::items::idef(peer.inventory.hand().item()).kind
        else {
            return Err(RejectReason::Unauthorized);
        };
        let role = self.identities[&peer.identity].role;
        let query = WorldQuery::new(&self.world, peer.space);
        let reach = if peer.player.mode == GameMode::Creative || peer.player.noclip {
            5.
        } else {
            4.5
        };
        let ray = if kind == 0 {
            sim::raycast_fluid_sources(&query, peer.player.eye(1.), peer.player.look(), reach)
        } else {
            sim::raycast(&query, peer.player.eye(1.), peer.player.look(), reach)
        };
        let hit = match ray {
            sim::RayResult::Hit(h) => h,
            sim::RayResult::Unknown(_) => return Err(RejectReason::Unknown),
            sim::RayResult::Miss => return Err(RejectReason::Occluded),
        };
        let target = if kind == 0 {
            hit.cell
        } else {
            let direction = rules::cell::DIRS[hit.face as usize];
            std::array::from_fn(|i| hit.cell[i] + direction[i])
        };
        // Establish actual reach/visibility before comparing a private cell.
        if target != [position.x, position.y, position.z] {
            return Err(RejectReason::Occluded);
        }
        if self.protected(target, role) {
            return Err(RejectReason::Protected);
        }
        let before = match self.world.read(position) {
            CellRead::Known(c) => c,
            CellRead::Unknown => return Err(RejectReason::Unknown),
        };
        if before != expected {
            return Err(RejectReason::Stale);
        }
        let plan = fluid::plan_bucket(&peer.inventory, target, before).map_err(survival_reason)?;
        let mut tx = Transaction::new(self.tick, peer.space);
        tx.expect_page(position.page(), self.world.page_version(position.page()));
        let hardening = fluid::plan_neighbor_hardening(
            |p| {
                let block = BlockPos {
                    space: peer.space,
                    x: p[0],
                    y: p[1],
                    z: p[2],
                };
                tx.expect_page(block.page(), self.world.page_version(block.page()));
                if p == target {
                    Some(plan.cell.after)
                } else {
                    match self.world.read(block) {
                        CellRead::Known(c) => Some(c),
                        CellRead::Unknown => None,
                    }
                }
            },
            target,
        )
        .map_err(survival_reason)?;
        for change in hardening.changes() {
            if self.protected(change.position, role) {
                return Err(RejectReason::Protected);
            }
        }
        let final_cell = hardening
            .changes()
            .iter()
            .find(|c| c.position == target)
            .map_or(plan.cell.after, |c| c.after);
        tx.set_cell(position, final_cell);
        for change in hardening.changes().iter().filter(|c| c.position != target) {
            tx.set_cell(
                BlockPos {
                    space: peer.space,
                    x: change.position[0],
                    y: change.position[1],
                    z: change.position[2],
                },
                change.after,
            );
        }
        let creative = peer.player.mode == GameMode::Creative && role.can_admin();
        let mut stacks = Vec::with_capacity(5);
        if kind != 0 && before.id() != rules::ids::AIR && fluid::fluid(before).is_none() {
            let mut rng = core_types::DeterministicRng::from_key(
                self.drop_seed,
                0x4255_434b,
                runtime::player_entity(peer.identity).0,
                id.0,
            );
            let drops = rules::survival::loot::block_drops(before, true, false, &mut rng)
                .map_err(survival_reason)?;
            stacks.extend_from_slice(drops.stacks());
        }
        if !creative && !plan.spill.is_empty() {
            stacks.push(plan.spill);
        }
        let rows = stacks
            .into_iter()
            .enumerate()
            .map(|(ordinal, stack)| {
                self.mob_item(
                    mob_owner::DropOrigin {
                        actor: runtime::player_entity(peer.identity),
                        action: id.0,
                        space: peer.space,
                        position: target.map(|v| f64::from(v) + 0.5),
                        delay: 10,
                    },
                    ordinal,
                    stack,
                )
                .map_err(|_| RejectReason::Capacity)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let items = self.prepare_owned_item_rows(batch, &rows)?;
        tx.event(
            audio_cue::owned_event(
                peer.identity,
                net::audio_cue::Cue {
                    kind: if kind == 0 {
                        net::audio_cue::Kind::BucketFill
                    } else {
                        net::audio_cue::Kind::BucketEmpty
                    },
                    actor: runtime::player_entity(peer.identity).0,
                    sequence: id.0,
                    tick: self.tick,
                    space: peer.space,
                    position: if kind == 0 {
                        peer.player.pos
                    } else {
                        target.map(|v| f64::from(v) + 0.5)
                    },
                },
            )
            .map_err(|_| RejectReason::Capacity)?,
        );
        let change = self.world.commit(tx).map_err(|_| RejectReason::Stale)?;
        let transaction = change.tx;
        batch.changes.push(change);
        if let Some(items) = items {
            Self::replace_runtime(batch, items);
        }
        if !creative {
            peer.inventory = *plan.inventory.after();
        }
        peer.place_ready_at = Tick(self.tick.0.saturating_add(4));
        Ok(transaction)
    }
}
#[cfg(test)]
mod tests;
