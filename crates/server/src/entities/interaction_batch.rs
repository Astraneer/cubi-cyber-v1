//! Compose owned interactions at one mob revision without cloning the mob store.
use super::interactions::Request;
use super::*;
use rules::entities::combat::{
    Combatant,
    mob::{MobAttackPlan, reachable},
};
#[must_use = "Persist the complete mob/item/private-player plan before ACK"]
pub struct InteractionBatch {
    before_actor: Combatant,
    pub actor: Combatant,
    pub delta: MobDelta,
    pub drops: Vec<DeathDrops>,
    pub inputs: Vec<jobs::InputVersion>,
    /// A sound is warranted only when authoritative damage actually applied.
    pub strike: Option<rules::entities::combat::Strike>,
}
impl MobRuntime {
    /// Pending changes come from earlier accepted actions in the same bounded
    /// owner batch. Each original before value remains guarded until fsync.
    pub fn plan_interaction_after(
        &self,
        world: &World,
        request: Request,
        pending: Option<&MobDelta>,
        use_entity: bool,
    ) -> Result<InteractionBatch, EntityError> {
        if let Some(pending) = pending {
            self.validate_owner_delta(pending)?;
            if pending.patches.len() > 128 {
                return Err(EntityError::Capacity);
            }
        }
        let original = *self
            .store
            .get(request.target)
            .ok_or(EntityError::UnknownId)?;
        let prior = pending.and_then(|p| p.patches.iter().find(|p| p.id() == Ok(request.target)));
        let before = prior
            .map_or(Some(original), |p| p.after)
            .ok_or(EntityError::UnknownId)?;
        if before.space != request.context.space
            || !request.actor.health.alive()
            || request.actor.inventory.hand() != request.context.expected_held
        {
            return Err(EntityError::NoEntitlement);
        }
        let reads = Counter::new(0);
        let exhausted = Counter::new(false);
        let inputs = RefCell::new(BTreeMap::new());
        let query = Tracked {
            world,
            space: before.space,
            reads: &reads,
            maximum: 256,
            exhausted: &exhausted,
            inputs: &inputs,
        };
        let (actor, after, drops, strike) = if use_entity {
            reachable(before.bbox(), request.context.eye, |a, b| {
                movement::line_of_sight(&query, a, b)
            })?;
            let shear = mob::ShearPlan::prepare(
                &before,
                &request.actor.inventory,
                request.context.expected_held,
                self.seed,
                request.action,
            )?;
            let mut actor = request.actor;
            shear
                .inventory
                .apply(&mut actor.inventory)
                .map_err(|_| EntityError::StalePlan)?;
            (
                actor,
                shear.after,
                vec![DeathDrops {
                    actor: before.id,
                    space: before.space,
                    position: before.body.position,
                    action: request.action,
                    stacks: [shear.wool, ItemStack::EMPTY, ItemStack::EMPTY],
                }],
                None,
            )
        } else {
            let hit = MobAttackPlan::prepare(&request.actor, &before, request.context, |a, b| {
                movement::line_of_sight(&query, a, b)
            })?;
            let mut after = hit.target;
            let drops = if hit.hit.is_some_and(|h| h.died) {
                let mut rng = core_types::DeterministicRng::from_key(
                    self.seed,
                    0x4d4f42414354494f,
                    before.id.0,
                    request.action,
                );
                after.loot_emitted = true;
                vec![DeathDrops {
                    actor: before.id,
                    space: before.space,
                    position: before.body.position,
                    action: request.action,
                    stacks: mob::mob_loot(&after, true, &mut rng),
                }]
            } else {
                Vec::new()
            };
            (hit.attacker, after, drops, hit.hit.map(|_| hit.strike))
        };
        if exhausted.get() {
            return Err(EntityError::Capacity);
        }
        let mut delta = match pending {
            Some(pending) => pending.clone(),
            None => self.delta(Vec::new(), self.store.active_cursor())?,
        };
        if let Some(patch) = delta
            .patches
            .iter_mut()
            .find(|p| p.id() == Ok(request.target))
        {
            patch.after = Some(after);
        } else {
            delta.patches.push(EntityPatch {
                before: Some(original),
                after: Some(after),
                before_active: self.store.is_active(original.id),
                after_active: self.store.is_active(original.id),
            });
            delta
                .patches
                .sort_unstable_by_key(|p| p.id().expect("validated ID"));
        }
        self.validate_owner_delta(&delta)?;
        Ok(InteractionBatch {
            before_actor: request.actor,
            actor,
            delta,
            drops,
            strike,
            inputs: inputs
                .into_inner()
                .into_iter()
                .map(|(page, version)| jobs::InputVersion { page, version })
                .collect(),
        })
    }
    pub fn validate_interaction_batch(
        &self,
        world: &World,
        actor: &Combatant,
        plan: &InteractionBatch,
    ) -> Result<(), EntityError> {
        if *actor != plan.before_actor
            || plan.inputs.iter().any(|input| {
                world
                    .page(input.page)
                    .map(|_| world.input_version(input.page, CHANNELS))
                    != input.version
            })
        {
            return Err(EntityError::StalePlan);
        }
        self.validate_owner_delta(&plan.delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rules::entities::combat::{AttackPose, CombatStatus, mob::AttackContext};
    use rules::survival::{Inventory, health::Health};
    fn setup(kind: MobKind) -> (World, MobRuntime) {
        let mut world = World::new(rules::properties, 64);
        let mut cells = vec![Cell::AIR; 4096];
        cells[..256].fill(Cell::from_parts(STONE, 0));
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(
            PageId {
                space: SpaceId(0),
                x: 0,
                y: 0,
                z: 0,
            },
            cells,
        );
        world.commit(tx).unwrap();
        let mut runtime = MobRuntime::new(Config::default()).unwrap();
        runtime
            .insert_initial(Mob::new(EntityId(90), kind, [8.5, 1., 11.5]).unwrap(), true)
            .unwrap();
        (world, runtime)
    }
    fn request(item: u16, id: u64) -> Request {
        let mut inventory = Inventory::empty();
        if item != 0 {
            inventory
                .set_trusted(0, ItemStack::new(item, 1).unwrap())
                .unwrap();
        }
        let actor = Combatant {
            id: EntityId(id),
            inventory,
            health: Health::new(),
            status: CombatStatus {
                attack_ticks: 20,
                ..CombatStatus::default()
            },
            position: [8.5, 1., 8.5],
            velocity: [0.; 3],
            crouching: false,
            pose: AttackPose {
                on_ground: true,
                ..AttackPose::default()
            },
        };
        Request {
            actor,
            context: AttackContext {
                space: SpaceId(0),
                eye: [8.5, 2.62, 8.5],
                look_horizontal: [0., 1.],
                expected_held: inventory.hand(),
                now: Tick(1),
            },
            target: EntityId(90),
            action: id,
        }
    }
    #[test]
    fn multiple_owned_attacks_share_one_before_guard_and_compose_without_publishing() {
        let (world, mut runtime) = setup(MobKind::Pig);
        let first = runtime
            .plan_interaction_after(&world, request(0, 1), None, false)
            .unwrap();
        assert_eq!(first.delta.patches[0].after.unwrap().life.hp(), 9.);
        let second = runtime
            .plan_interaction_after(&world, request(285, 2), Some(&first.delta), false)
            .unwrap();
        assert_eq!(second.delta.patches.len(), 1);
        assert_eq!(second.delta.patches[0].before.unwrap().life.hp(), 10.);
        assert_eq!(second.delta.patches[0].after.unwrap().life.hp(), 4.);
        assert_eq!(second.actor.inventory.hand().damage(), 1);
        assert_eq!(runtime.entities().get(EntityId(90)).unwrap().life.hp(), 10.);
        runtime
            .validate_interaction_batch(&world, &request(285, 2).actor, &second)
            .unwrap();
        runtime.acknowledge(second.delta).unwrap();
        assert_eq!(runtime.entities().get(EntityId(90)).unwrap().life.hp(), 4.);
        assert!(runtime.acknowledge(first.delta).is_err());
    }
    #[test]
    fn pending_shearing_is_not_issued_twice_and_world_change_rejects_the_entire_plan() {
        let (mut world, runtime) = setup(MobKind::Sheep);
        let req = request(rules::items::I_SHEARS, 1);
        let plan = runtime
            .plan_interaction_after(&world, req, None, true)
            .unwrap();
        assert!(plan.delta.patches[0].after.unwrap().sheared);
        assert_eq!(plan.actor.inventory.hand().damage(), 1);
        assert_eq!(plan.drops.len(), 1);
        assert!(
            runtime
                .plan_interaction_after(&world, req, Some(&plan.delta), true)
                .is_err()
        );
        assert!(!runtime.entities().get(EntityId(90)).unwrap().sheared);
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(
            BlockPos {
                space: SpaceId(0),
                x: 8,
                y: 2,
                z: 10,
            },
            Cell::from_parts(STONE, 0),
        );
        world.commit(tx).unwrap();
        assert_eq!(
            runtime.validate_interaction_batch(&world, &req.actor, &plan),
            Err(EntityError::StalePlan)
        );
        assert!(!runtime.entities().get(EntityId(90)).unwrap().sheared);
    }
}
