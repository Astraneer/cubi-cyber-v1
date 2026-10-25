//! Player-to-mob action planning with real mob dimensions and owned tools.
use super::*;
use crate::entities::{MobKind, mob::Mob};
use core_types::{SpaceId, Tick};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttackContext {
    pub space: SpaceId,
    pub eye: [f64; 3],
    pub look_horizontal: [f64; 2],
    pub expected_held: ItemStack,
    pub now: Tick,
}
pub fn reachable(
    bounds: Aabb,
    eye: [f64; 3],
    mut visible: impl FnMut([f64; 3], [f64; 3]) -> Result<bool, EntityError>,
) -> Result<(), EntityError> {
    if eye
        .iter()
        .chain(&bounds.lo)
        .chain(&bounds.hi)
        .any(|v| !v.is_finite())
    {
        return Err(EntityError::InvalidPosition);
    }
    let distance = (0..3)
        .map(|axis| (eye[axis] - eye[axis].clamp(bounds.lo[axis], bounds.hi[axis])).powi(2))
        .sum::<f64>();
    if distance > 36.0 {
        return Err(EntityError::NoEntitlement);
    }
    for y in [
        bounds.lo[1] + 0.1,
        (bounds.lo[1] + bounds.hi[1]) / 2.0,
        bounds.hi[1] - 0.1,
    ] {
        if visible(
            eye,
            [
                (bounds.lo[0] + bounds.hi[0]) / 2.0,
                y,
                (bounds.lo[2] + bounds.hi[2]) / 2.0,
            ],
        )? {
            return Ok(());
        }
    }
    Err(EntityError::NoEntitlement)
}
#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use = "Persist player/tool, mob state and death drops together"]
pub struct MobAttackPlan {
    before_attacker: Combatant,
    before_target: Mob,
    pub attacker: Combatant,
    pub target: Mob,
    pub strike: Strike,
    pub hit: Option<Hit>,
}
impl MobAttackPlan {
    pub fn prepare(
        attacker: &Combatant,
        target: &Mob,
        context: AttackContext,
        visible: impl FnMut([f64; 3], [f64; 3]) -> Result<bool, EntityError>,
    ) -> Result<Self, EntityError> {
        if attacker.id == target.id
            || !attacker.health.alive()
            || !target.life.alive()
            || context.space != target.space
            || context.expected_held != attacker.inventory.hand()
        {
            return Err(EntityError::NoEntitlement);
        }
        if attacker
            .position
            .iter()
            .chain(&context.look_horizontal)
            .any(|v| !v.is_finite())
        {
            return Err(EntityError::InvalidPosition);
        }
        reachable(target.bbox(), context.eye, visible)?;
        let strike = strike(
            attacker.inventory.hand(),
            attacker.status.attack_ticks,
            attacker.pose,
        );
        let mut plan = Self {
            before_attacker: *attacker,
            before_target: *target,
            attacker: *attacker,
            target: *target,
            strike,
            hit: None,
        };
        plan.attacker.status.attack_ticks = 0;
        plan.hit = plan.target.life.damage(
            DamageKind::PlayerAttack,
            strike.damage,
            (target.kind.definition().armor, 0.0),
            false,
        );
        if let Some(hit) = plan.hit {
            plan.target.last_player_hit = Some((attacker.id, context.now));
            if target.kind != MobKind::Zombie {
                plan.target.ai.panic_until = Tick(context.now.0.saturating_add(40));
            }
            let tool = plan.attacker.inventory.hand();
            let wear = match crate::items::idef(tool.item()).kind {
                crate::items::ItemKind::Sword { .. } => 1,
                crate::items::ItemKind::Tool { .. } => 2,
                _ => 0,
            };
            if wear > 0 {
                plan.attacker
                    .inventory
                    .plan_wear_tool(tool, wear)
                    .map_err(|_| EntityError::NoEntitlement)?
                    .apply(&mut plan.attacker.inventory)
                    .map_err(|_| EntityError::StalePlan)?;
            }
            if hit.full {
                plan.target.body.velocity = knockback(
                    plan.target.body.velocity,
                    target.body.on_ground,
                    0.4,
                    [
                        attacker.position[0] - target.body.position[0],
                        attacker.position[2] - target.body.position[2],
                    ],
                );
                if strike.sprint_knockback {
                    plan.target.body.velocity = knockback(
                        plan.target.body.velocity,
                        target.body.on_ground,
                        0.5,
                        [-context.look_horizontal[0], -context.look_horizontal[1]],
                    );
                }
            }
        }
        Ok(plan)
    }
    pub fn apply(self, attacker: &mut Combatant, target: &mut Mob) -> Result<(), EntityError> {
        if *attacker != self.before_attacker || *target != self.before_target {
            return Err(EntityError::StalePlan);
        }
        *attacker = self.attacker;
        *target = self.target;
        Ok(())
    }
}
