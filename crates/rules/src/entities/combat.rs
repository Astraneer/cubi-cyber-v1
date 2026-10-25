//! Shared PvP/mob strike rules from the approved donor, Java 1.21.1 arithmetic.
pub mod mob;
use super::{EntityError, EntityId};
use crate::{
    items::tool_attack,
    shapes::Aabb,
    stack::ItemStack,
    survival::{
        Inventory,
        health::{DamageKind, Health, Hit},
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatStatus {
    pub zone_grace: u16,
    pub combat_tag: u16,
    pub attack_ticks: u16,
}
impl CombatStatus {
    pub fn tick(&mut self, inside_spawn: bool) {
        self.combat_tag = self.combat_tag.saturating_sub(1);
        self.zone_grace = if inside_spawn {
            200
        } else {
            self.zone_grace.saturating_sub(1)
        };
        self.attack_ticks = self.attack_ticks.saturating_add(1);
    }
    pub const fn shielded(&self) -> bool {
        self.zone_grace > 0 && self.combat_tag == 0
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AttackPose {
    pub on_ground: bool,
    pub fall: f32,
    pub in_water: bool,
    pub sprinting: bool,
    pub flying: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strike {
    pub damage: f32,
    pub strong: bool,
    pub critical: bool,
    pub sprint_knockback: bool,
}
pub fn strike(held: ItemStack, ticks: u16, pose: AttackPose) -> Strike {
    let (base, speed) = tool_attack(held.item()).unwrap_or((1.0, 4.0));
    let fraction = ((f32::from(ticks) + 0.5) / (20.0 / speed)).clamp(0.0, 1.0);
    let strong = fraction > 0.9;
    let critical = strong
        && pose.fall > 0.0
        && !pose.on_ground
        && !pose.in_water
        && !pose.sprinting
        && !pose.flying;
    let mut damage = base * (0.2 + fraction * fraction * 0.8);
    if critical {
        damage *= 1.5;
    }
    Strike {
        damage,
        strong,
        critical,
        sprint_knockback: pose.sprinting && strong,
    }
}
/// LivingEntity.takeKnockback; from is target-to-attacker, not a client velocity.
pub fn knockback(velocity: [f64; 3], ground: bool, strength: f64, from: [f64; 2]) -> [f64; 3] {
    if !strength.is_finite()
        || strength <= 0.0
        || from.iter().chain(&velocity).any(|value| !value.is_finite())
    {
        return velocity;
    }
    let distance = (from[0] * from[0] + from[1] * from[1]).sqrt();
    let (x, z) = if distance < 1e-4 {
        (0.0, 0.0)
    } else {
        (from[0] / distance * strength, from[1] / distance * strength)
    };
    [
        velocity[0] / 2.0 - x,
        if ground {
            (velocity[1] / 2.0 + strength).min(0.4)
        } else {
            velocity[1]
        },
        velocity[2] / 2.0 - z,
    ]
}
/// Entity.pushAwayFrom reference vector for b; a receives its negation. The
/// living-entity phase applies it twice per pair, matching the donor tick order.
pub fn separation(a: [f64; 3], b: [f64; 3]) -> Option<[f64; 2]> {
    let mut x = b[0] - a[0];
    let mut z = b[2] - a[2];
    let maximum = x.abs().max(z.abs());
    if !maximum.is_finite() || maximum < 0.01 {
        return None;
    }
    let root = maximum.sqrt();
    x /= root;
    z /= root;
    let inverse = (1.0 / root).min(1.0);
    Some([x * inverse * 0.05, z * inverse * 0.05])
}
pub fn body_box(feet: [f64; 3], crouching: bool) -> Aabb {
    Aabb {
        lo: [feet[0] - 0.3, feet[1], feet[2] - 0.3],
        hi: [
            feet[0] + 0.3,
            feet[1] + if crouching { 1.5 } else { 1.8 },
            feet[2] + 0.3,
        ],
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Combatant {
    pub id: EntityId,
    pub inventory: Inventory,
    pub health: Health,
    pub status: CombatStatus,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub crouching: bool,
    pub pose: AttackPose,
}
#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use = "Commit both combatants, knockback and any death drops atomically"]
pub struct AttackPlan {
    before_attacker: Combatant,
    before_target: Combatant,
    pub attacker: Combatant,
    pub target: Combatant,
    pub strike: Strike,
    pub hit: Option<Hit>,
}
impl AttackPlan {
    /// Geometry and authority checks precede cooldown consumption. `visible` must
    /// query the authoritative known world; Unknown is an error, never clear air.
    pub fn prepare(
        attacker: &Combatant,
        target: &Combatant,
        eye: [f64; 3],
        look_horizontal: [f64; 2],
        mut visible: impl FnMut([f64; 3], [f64; 3]) -> Result<bool, EntityError>,
    ) -> Result<Self, EntityError> {
        if attacker.id == target.id || !attacker.health.alive() || !target.health.alive() {
            return Err(EntityError::NoEntitlement);
        }
        if eye
            .iter()
            .chain(&attacker.position)
            .chain(&target.position)
            .any(|value| !value.is_finite())
        {
            return Err(EntityError::InvalidPosition);
        }
        let bounds = body_box(target.position, target.crouching);
        let distance = (0..3)
            .map(|axis| (eye[axis] - eye[axis].clamp(bounds.lo[axis], bounds.hi[axis])).powi(2))
            .sum::<f64>();
        if distance > 36.0 {
            return Err(EntityError::NoEntitlement);
        }
        let mut seen = false;
        for height in [
            bounds.lo[1] + 0.1,
            (bounds.lo[1] + bounds.hi[1]) / 2.0,
            bounds.hi[1] - 0.1,
        ] {
            if visible(eye, [target.position[0], height, target.position[2]])? {
                seen = true;
                break;
            }
        }
        if !seen {
            return Err(EntityError::NoEntitlement);
        }
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
        if !target.status.shielded() {
            plan.attacker.status.combat_tag = 200;
            plan.hit = plan.target.health.damage(
                DamageKind::PlayerAttack,
                strike.damage,
                plan.target.inventory.armor(),
                false,
            );
        }
        if let Some(hit) = plan.hit {
            plan.target
                .inventory
                .plan_armor_wear(hit.armor_wear)
                .map_err(|_| EntityError::NoEntitlement)?
                .apply(&mut plan.target.inventory)
                .map_err(|_| EntityError::StalePlan)?;
            let tool = plan.attacker.inventory.hand();
            let wear = match crate::items::idef(tool.item()).kind {
                crate::items::ItemKind::Sword { .. } => 1,
                crate::items::ItemKind::Tool { .. } => 2,
                _ => 0,
            };
            if !tool.is_empty()
                && wear > 0
                && let Ok(wear) = plan.attacker.inventory.plan_wear_tool(tool, wear)
            {
                wear.apply(&mut plan.attacker.inventory)
                    .map_err(|_| EntityError::StalePlan)?;
            }
            if !hit.died {
                if hit.full {
                    plan.target.velocity = knockback(
                        plan.target.velocity,
                        target.pose.on_ground,
                        0.4,
                        [
                            attacker.position[0] - target.position[0],
                            attacker.position[2] - target.position[2],
                        ],
                    );
                }
                if strike.sprint_knockback {
                    plan.target.velocity = knockback(
                        plan.target.velocity,
                        target.pose.on_ground,
                        0.5,
                        [-look_horizontal[0], -look_horizontal[1]],
                    );
                }
            }
        }
        Ok(plan)
    }
    pub fn apply(
        self,
        attacker: &mut Combatant,
        target: &mut Combatant,
    ) -> Result<(), EntityError> {
        if *attacker != self.before_attacker || *target != self.before_target {
            return Err(EntityError::StalePlan);
        }
        *attacker = self.attacker;
        *target = self.target;
        Ok(())
    }
}
