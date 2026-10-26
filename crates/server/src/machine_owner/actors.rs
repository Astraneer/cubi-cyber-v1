//! Exact simulation actors for a captured tick. No transport or authority state
//! is constructed when a durable tick resumes before its players reconnect.
use crate::{Peer, ServerError, actor, window_owner, windows::BoundWindow};
use core_types::{SpaceId, Tick};
use net::PlayerId;
use rules::{
    entities::combat::CombatStatus,
    survival::{Inventory, health::Health},
};
use save::SavedPlayer;
use sim::Player;

pub(crate) struct TickActor {
    pub(crate) identity: PlayerId,
    pub(crate) space: SpaceId,
    pub(crate) input_sequence: u64,
    pub(crate) player: Player,
    pub(crate) health: Health,
    pub(crate) combat: CombatStatus,
    pub(crate) inventory: Inventory,
    pub(crate) window: BoundWindow,
    pub(crate) place_ready_at: Tick,
    pub(crate) break_ready_at: Tick,
}
impl TickActor {
    pub(crate) fn restore(saved: &SavedPlayer, allow_debug: bool) -> Result<Self, ServerError> {
        let (player, health, combat) =
            actor::restore_living(saved, allow_debug).map_err(ServerError::Invalid)?;
        let inventory = if saved.inventory.is_empty() {
            Inventory::empty()
        } else {
            Inventory::decode(&saved.inventory)
                .map_err(|_| ServerError::Invalid("captured actor inventory codec"))?
        };
        let window = window_owner::initial(&saved.window, &inventory)?;
        Ok(Self {
            identity: saved.state.identity,
            space: saved.state.space,
            input_sequence: saved.state.input_sequence,
            player,
            health,
            combat,
            inventory,
            window,
            place_ready_at: saved.place_ready_at,
            break_ready_at: saved.break_ready_at,
        })
    }
    #[cfg(test)]
    pub(crate) fn capture(&self) -> Result<SavedPlayer, ServerError> {
        self.as_ref().capture()
    }
    pub(crate) fn as_ref(&self) -> ActorRef<'_> {
        self.into()
    }
    pub(crate) fn as_mut(&mut self) -> ActorMut<'_> {
        self.into()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ActorRef<'a> {
    pub(crate) identity: PlayerId,
    pub(crate) space: SpaceId,
    pub(crate) input_sequence: u64,
    pub(crate) player: &'a Player,
    pub(crate) health: &'a Health,
    pub(crate) combat: &'a CombatStatus,
    pub(crate) inventory: &'a Inventory,
    pub(crate) window: &'a BoundWindow,
    pub(crate) place_ready_at: Tick,
    pub(crate) break_ready_at: Tick,
}
impl ActorRef<'_> {
    pub(crate) fn capture(self) -> Result<SavedPlayer, ServerError> {
        let mut saved = actor::capture_living(
            self.identity,
            self.space,
            self.input_sequence,
            self.player,
            self.health,
            self.combat,
        )
        .map_err(ServerError::Invalid)?;
        saved.place_ready_at = self.place_ready_at;
        saved.break_ready_at = self.break_ready_at;
        saved.inventory = self.inventory.encode().to_vec();
        saved.window = self.window.encode();
        Ok(saved)
    }
}
impl<'a> From<&'a Peer> for ActorRef<'a> {
    fn from(value: &'a Peer) -> Self {
        Self {
            identity: value.identity,
            space: value.space,
            input_sequence: value.input_sequence,
            player: &value.player,
            health: &value.health,
            combat: &value.combat,
            inventory: &value.inventory,
            window: &value.window,
            place_ready_at: value.place_ready_at,
            break_ready_at: value.break_ready_at,
        }
    }
}
impl<'a> From<&'a TickActor> for ActorRef<'a> {
    fn from(value: &'a TickActor) -> Self {
        Self {
            identity: value.identity,
            space: value.space,
            input_sequence: value.input_sequence,
            player: &value.player,
            health: &value.health,
            combat: &value.combat,
            inventory: &value.inventory,
            window: &value.window,
            place_ready_at: value.place_ready_at,
            break_ready_at: value.break_ready_at,
        }
    }
}

pub(crate) struct ActorMut<'a> {
    pub(crate) identity: PlayerId,
    pub(crate) space: SpaceId,
    pub(crate) input_sequence: u64,
    pub(crate) player: &'a mut Player,
    pub(crate) health: &'a mut Health,
    pub(crate) combat: &'a mut CombatStatus,
    pub(crate) inventory: &'a mut Inventory,
    pub(crate) window: &'a mut BoundWindow,
    pub(crate) place_ready_at: &'a mut Tick,
    pub(crate) break_ready_at: &'a mut Tick,
}
impl ActorMut<'_> {
    pub(crate) fn as_ref(&self) -> ActorRef<'_> {
        ActorRef {
            identity: self.identity,
            space: self.space,
            input_sequence: self.input_sequence,
            player: &*self.player,
            health: &*self.health,
            combat: &*self.combat,
            inventory: &*self.inventory,
            window: &*self.window,
            place_ready_at: *self.place_ready_at,
            break_ready_at: *self.break_ready_at,
        }
    }
    pub(crate) fn capture(&self) -> Result<SavedPlayer, ServerError> {
        self.as_ref().capture()
    }
}
impl<'a> From<&'a mut Peer> for ActorMut<'a> {
    fn from(value: &'a mut Peer) -> Self {
        Self {
            identity: value.identity,
            space: value.space,
            input_sequence: value.input_sequence,
            player: &mut value.player,
            health: &mut value.health,
            combat: &mut value.combat,
            inventory: &mut value.inventory,
            window: &mut value.window,
            place_ready_at: &mut value.place_ready_at,
            break_ready_at: &mut value.break_ready_at,
        }
    }
}
impl<'a> From<&'a mut TickActor> for ActorMut<'a> {
    fn from(value: &'a mut TickActor) -> Self {
        Self {
            identity: value.identity,
            space: value.space,
            input_sequence: value.input_sequence,
            player: &mut value.player,
            health: &mut value.health,
            combat: &mut value.combat,
            inventory: &mut value.inventory,
            window: &mut value.window,
            place_ready_at: &mut value.place_ready_at,
            break_ready_at: &mut value.break_ready_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_actor_retains_exact_living_inventory_window_and_mutable_updates() {
        let mut player = Player::new([8.5, 7., 9.5]);
        player.vel = [0.125, -0.25, 0.375];
        player.ticks = 119;
        player.fall = 3.5;
        let mut health = Health::new();
        health.set_spawn_protection(0);
        health.ignite(80);
        health.damage(
            rules::survival::health::DamageKind::MobAttack,
            3.,
            (0, 0.),
            false,
        );
        player.health = health.hp();
        let combat = CombatStatus {
            zone_grace: 7,
            combat_tag: 13,
            attack_ticks: 19,
        };
        let mut inventory = Inventory::empty();
        inventory
            .set_trusted(
                0,
                rules::stack::ItemStack::new(rules::ids::STONE, 11).unwrap(),
            )
            .unwrap();
        let mut window = BoundWindow::inventory(7).unwrap();
        let plan = window
            .state
            .plan_click(
                &inventory,
                window.state.guard(&inventory),
                crate::ui::Click {
                    slot: 0,
                    right: true,
                    shift: false,
                },
            )
            .unwrap();
        plan.apply(&mut window.state, &mut inventory).unwrap();
        assert_eq!(window.state.cursor().count(), 6);
        assert_eq!(inventory.get(0).unwrap().count(), 5);
        let mut saved = actor::capture_living(
            PlayerId([19; 16]),
            SpaceId(3),
            47,
            &player,
            &health,
            &combat,
        )
        .unwrap();
        saved.inventory = inventory.encode().to_vec();
        saved.window = window.encode();
        saved.place_ready_at = Tick(123);
        saved.break_ready_at = Tick(456);
        let mut recovered = TickActor::restore(&saved, false).unwrap();
        let same = recovered.capture().unwrap();
        assert_eq!(same.state, saved.state);
        assert_eq!(same.simulation, saved.simulation);
        assert_eq!(same.inventory, saved.inventory);
        assert_eq!(same.window, saved.window);
        assert_eq!(
            (same.place_ready_at, same.break_ready_at),
            (Tick(123), Tick(456))
        );
        assert_eq!(recovered.player.state_hash(), player.state_hash());
        assert_eq!(recovered.window.state.cursor().count(), 6);
        assert_eq!(recovered.inventory.get(0).unwrap().count(), 5);
        let mut broken = saved.clone();
        broken.inventory = vec![255];
        assert!(TickActor::restore(&broken, false).is_err());
        broken = saved.clone();
        broken.window = vec![255];
        assert!(TickActor::restore(&broken, false).is_err());
        {
            let actor = recovered.as_mut();
            actor.player.vel[0] = 0.75;
            actor.health.damage(
                rules::survival::health::DamageKind::Generic,
                1.,
                (0, 0.),
                false,
            );
            actor.health.ignite(140);
            actor
                .inventory
                .set_trusted(
                    1,
                    rules::stack::ItemStack::new(rules::ids::STONE, 2).unwrap(),
                )
                .unwrap();
            actor.player.health = actor.health.hp();
            *actor.place_ready_at = Tick(789);
            let changed = actor.capture().unwrap();
            let again = TickActor::restore(&changed, false).unwrap();
            assert_eq!(again.player.state_hash(), actor.player.state_hash());
            assert_eq!(again.health.encode(), actor.health.encode());
            assert_eq!(again.place_ready_at, Tick(789));
            assert_eq!(again.inventory.get(1).unwrap().count(), 2);
            assert_eq!(again.window.state.cursor().count(), 6);
        }
        recovered.player.health = recovered.health.hp() - 1.;
        assert!(
            recovered.capture().is_err(),
            "inconsistent public HP must not be saved"
        );
    }
}
