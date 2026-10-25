//! Trusted action adapters: geometry, owned tools, private before guards and
//! mob/drop changes are returned for one enclosing durable player action.
use super::*;
use rules::entities::combat::{
    Combatant,
    mob::{AttackContext, MobAttackPlan, reachable},
};

#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub actor: Combatant,
    pub context: AttackContext,
    pub target: EntityId,
    pub action: u64,
}
#[must_use = "Persist player, mob delta and every drop in one WAL batch before ACK"]
pub struct ActionPlan {
    before_actor: Combatant,
    pub actor: Combatant,
    pub delta: MobDelta,
    pub drops: Vec<DeathDrops>,
    pub inputs: Vec<jobs::InputVersion>,
}
impl MobRuntime {
    pub fn plan_attack(&self, world: &World, request: Request) -> Result<ActionPlan, EntityError> {
        let before = *self
            .store
            .get(request.target)
            .ok_or(EntityError::UnknownId)?;
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
        let hit = MobAttackPlan::prepare(&request.actor, &before, request.context, |a, b| {
            movement::line_of_sight(&query, a, b)
        });
        if exhausted.get() {
            return Err(EntityError::Capacity);
        }
        let hit = hit?;
        let mut after = hit.target;
        let mut drops = Vec::new();
        if hit.hit.is_some_and(|hit| hit.died) {
            let mut rng = core_types::DeterministicRng::from_key(
                self.seed,
                0x4d4f42414354494f,
                before.id.0,
                request.action,
            );
            drops.push(DeathDrops {
                actor: before.id,
                space: after.space,
                position: after.body.position,
                action: request.action,
                stacks: mob::mob_loot(&after, true, &mut rng),
            });
            after.loot_emitted = true;
        }
        self.interaction_plan(
            request.actor,
            hit.attacker,
            before,
            after,
            drops,
            inputs.into_inner(),
        )
    }
    pub fn plan_shear(&self, world: &World, request: Request) -> Result<ActionPlan, EntityError> {
        let before = *self
            .store
            .get(request.target)
            .ok_or(EntityError::UnknownId)?;
        if before.space != request.context.space || !request.actor.health.alive() {
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
        let reach = reachable(before.bbox(), request.context.eye, |a, b| {
            movement::line_of_sight(&query, a, b)
        });
        if exhausted.get() {
            return Err(EntityError::Capacity);
        }
        reach?;
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
        let drops = vec![DeathDrops {
            actor: before.id,
            space: before.space,
            position: before.body.position,
            action: request.action,
            stacks: [shear.wool, ItemStack::EMPTY, ItemStack::EMPTY],
        }];
        self.interaction_plan(
            request.actor,
            actor,
            before,
            shear.after,
            drops,
            inputs.into_inner(),
        )
    }
    fn interaction_plan(
        &self,
        before_actor: Combatant,
        actor: Combatant,
        before: Mob,
        after: Mob,
        drops: Vec<DeathDrops>,
        inputs: BTreeMap<PageId, Option<Version>>,
    ) -> Result<ActionPlan, EntityError> {
        let active = self.store.is_active(before.id);
        let patches = vec![EntityPatch {
            before: Some(before),
            after: Some(after),
            before_active: active,
            after_active: active,
        }];
        self.store.validate_patches(&patches)?;
        Ok(ActionPlan {
            before_actor,
            actor,
            delta: self.delta(patches, self.store.active_cursor())?,
            drops,
            inputs: inputs
                .into_iter()
                .map(|(page, version)| jobs::InputVersion { page, version })
                .collect(),
        })
    }
    pub fn validate_interaction(
        &self,
        world: &World,
        actor: &Combatant,
        plan: &ActionPlan,
    ) -> Result<(), EntityError> {
        if *actor != plan.before_actor
            || plan.delta.revision != self.revision
            || plan.delta.cursor_before != self.store.active_cursor()
            || plan.inputs.iter().any(|input| {
                world
                    .page(input.page)
                    .map(|_| world.input_version(input.page, CHANNELS))
                    != input.version
            })
        {
            return Err(EntityError::StalePlan);
        }
        self.store.validate_patches(&plan.delta.patches)
    }
}
