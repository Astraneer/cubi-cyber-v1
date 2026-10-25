//! Generic-AABB mob movement and bounded navigation. Entity values/rules live in
//! `rules`; this adapter shares the player's actual collision solver.
pub mod path;

use crate::{
    Aabb, QueryWorld,
    collision::{self, Bufs, Solids},
    motion::yaw_sin_cos,
    query::{self, CollisionQuery},
};
use rules::{
    blocks::{FLUID_LAVA, FLUID_WATER},
    entities::{
        EntityError, MobKind,
        mob::{Control, Mob, validate_position},
    },
    survival::health,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BodyOutcome {
    pub landed: Option<f32>,
    pub fall_damage: u32,
    pub moved: bool,
    pub horizontal_collision: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyPlan {
    pub after: Mob,
    pub outcome: BodyOutcome,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FluidEnvironment {
    pub water: bool,
    pub lava: bool,
    pub head_under_water: bool,
    pub depth: f64,
}
/// Bounded entity visibility ray against the same known collision shapes. This
/// covers the zombie follow radius as well as the shorter authoritative PvP ray.
pub fn line_of_sight(
    world: &impl QueryWorld,
    start: [f64; 3],
    end: [f64; 3],
) -> Result<bool, EntityError> {
    validate_position(start)?;
    validate_position(end)?;
    if (0..3)
        .map(|axis| (end[axis] - start[axis]).abs())
        .sum::<f64>()
        > 120.0
    {
        return Err(EntityError::Capacity);
    }
    for (position, _) in crate::interaction::ray_cells(start, end) {
        let world::CellRead::Known(cell) = world.cell(position) else {
            return Err(EntityError::UnknownInput);
        };
        for shape in rules::blocks::collision_of(cell.raw()) {
            if crate::interaction::segment_box(start, end, shape.moved(position.map(f64::from)))
                .is_some_and(|(t, _)| t < 1.0)
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
pub fn sample_environment(
    mob: &Mob,
    world: &impl QueryWorld,
) -> Result<FluidEnvironment, EntityError> {
    let mut query = CollisionQuery::new(world);
    let depth = fluid_in(&mut query, mob.bbox(), FLUID_WATER);
    let lava = fluid_in(&mut query, mob.bbox(), FLUID_LAVA).is_some();
    let eye_height = match (mob.baby, mob.kind) {
        (false, _) => mob.kind.definition().eye,
        (true, MobKind::Cow) => 0.665,
        (true, MobKind::Chicken) => 0.2975,
        (true, MobKind::Zombie) => 0.93,
        (true, _) => mob.kind.definition().eye * 0.5,
    };
    let eye = mob.body.position[1] + eye_height;
    let (kind, height) = query.fluid(
        mob.body.position[0].floor() as i32,
        eye.floor() as i32,
        mob.body.position[2].floor() as i32,
    );
    if query.unknown {
        return Err(EntityError::UnknownInput);
    }
    Ok(FluidEnvironment {
        water: depth.is_some(),
        lava,
        head_under_water: kind == FLUID_WATER && eye.floor() + height > eye,
        depth: depth.unwrap_or(0.0),
    })
}

/// Entire motion defers if any needed cell is Unknown; callers can keep the old
/// pose and ask jobs to load the dependency. No read triggers generation.
pub fn plan_body(
    mob: &Mob,
    world: &impl QueryWorld,
    control: Control,
    bufs: &mut Bufs,
) -> Result<BodyPlan, EntityError> {
    validate_position(mob.body.position)?;
    if mob
        .body
        .velocity
        .iter()
        .any(|value| !value.is_finite() || value.abs() > 64.0)
        || !control.speed_multiplier.is_finite()
        || !(0.0..=8.0).contains(&control.speed_multiplier)
    {
        return Err(EntityError::InvalidPosition);
    }
    if let Some(destination) = control.destination {
        validate_position(destination)?;
    }
    if query::body_has_unknown(world, mob.bbox()) {
        return Err(EntityError::UnknownInput);
    }
    let mut after = *mob;
    let mut query = CollisionQuery::new(world);
    let outcome = tick_solids(&mut after, &mut query, control, bufs);
    if query.unknown {
        return Err(EntityError::UnknownInput);
    }
    Ok(BodyPlan { after, outcome })
}

fn fluid_in(world: &mut impl Solids, body: Aabb, kind: u8) -> Option<f64> {
    let body = body.contract(0.001);
    let mut best: Option<f64> = None;
    for z in body.lo[2].floor() as i32..body.hi[2].ceil() as i32 {
        for y in body.lo[1].floor() as i32..body.hi[1].ceil() as i32 {
            for x in body.lo[0].floor() as i32..body.hi[0].ceil() as i32 {
                let (current, height) = world.fluid(x, y, z);
                if current == kind && f64::from(y) + height >= body.lo[1] {
                    let depth = f64::from(y) + height - body.lo[1];
                    best = Some(best.map_or(depth, |old| old.max(depth)));
                }
            }
        }
    }
    best
}
fn any_fluid(world: &mut impl Solids, body: Aabb) -> bool {
    for z in body.lo[2].floor() as i32..body.hi[2].ceil() as i32 {
        for y in body.lo[1].floor() as i32..body.hi[1].ceil() as i32 {
            for x in body.lo[0].floor() as i32..body.hi[0].ceil() as i32 {
                if world.fluid(x, y, z).0 != 0 {
                    return true;
                }
            }
        }
    }
    false
}
pub fn turn_angle(current: f32, target: f32, maximum: f32) -> f32 {
    (current + ((target - current + 180.0).rem_euclid(360.0) - 180.0).clamp(-maximum, maximum))
        .rem_euclid(360.0)
}
fn yaw_to(dx: f64, dz: f64) -> f32 {
    (libm::atan2(dz, dx).to_degrees() as f32 - 90.0).rem_euclid(360.0)
}

/// Numerical entry point for independent geometric goldens. Production owners
/// use plan_body to enforce the Unknown policy before publication.
pub fn tick_solids(
    mob: &mut Mob,
    world: &mut impl Solids,
    control: Control,
    bufs: &mut Bufs,
) -> BodyOutcome {
    let before = mob.body.position;
    mob.body.previous = before;
    let mut forward = 0.0f32;
    let mut speed = 0.0f32;
    let mut jumping = control.jump && mob.life.alive();
    let wet = fluid_in(world, mob.bbox(), FLUID_WATER);
    let fluid_depth = wet.unwrap_or(0.0);
    mob.body.in_water = wet.is_some();
    mob.body.in_lava = fluid_in(world, mob.bbox(), FLUID_LAVA).is_some();
    if mob.body.in_water {
        mob.body.fall = 0.0;
    }
    if mob.body.in_lava {
        mob.body.fall *= 0.5;
    }
    if let Some(destination) = control.destination.filter(|_| mob.life.alive()) {
        let [dx, dy, dz] = std::array::from_fn(|axis| destination[axis] - mob.body.position[axis]);
        if dx * dx + dy * dy + dz * dz >= 2.5e-7 {
            mob.body.yaw = turn_angle(mob.body.yaw, yaw_to(dx, dz), 90.0);
            let attribute = mob.kind.definition().speed
                * if mob.baby && mob.kind == MobKind::Zombie {
                    1.5
                } else {
                    1.0
                };
            speed = control.speed_multiplier * attribute;
            forward = speed;
            if dy > 0.6 && dx * dx + dz * dz < mob.dimensions().0.max(1.0) {
                jumping = true;
            }
        }
    }
    if let Some(at) = control.look_at {
        let dx = at[0] - mob.body.position[0];
        let dz = at[2] - mob.body.position[2];
        mob.body.head = turn_angle(
            mob.body.head,
            yaw_to(dx, dz),
            if mob.ai.goal == rules::entities::mob::Goal::Tempt {
                95.0
            } else {
                10.0
            },
        );
        let dy = at[1] - mob.body.position[1] - mob.kind.definition().eye;
        mob.body.pitch =
            (-(libm::atan2(dy, (dx * dx + dz * dz).sqrt()).to_degrees() as f32)).clamp(-40.0, 40.0);
    }
    mob.body.jump_cooldown = mob.body.jump_cooldown.saturating_sub(1);
    for value in &mut mob.body.velocity {
        if value.abs() < 0.003 {
            *value = 0.0;
        }
    }
    if jumping {
        let depth = if mob.body.in_lava { 0.0 } else { fluid_depth };
        let swimming = mob.body.in_water && depth > 0.0;
        let swim_height = if mob.kind.definition().eye < 0.4 {
            0.0
        } else {
            0.4
        };
        if (swimming || mob.body.in_lava) && (!mob.body.on_ground || depth > swim_height) {
            mob.body.velocity[1] += f64::from(0.04f32);
        } else if (mob.body.on_ground || (swimming && depth <= swim_height))
            && mob.body.jump_cooldown == 0
        {
            mob.body.velocity[1] = f64::from(0.42f32);
            mob.body.jump_cooldown = 10;
        }
    } else {
        mob.body.jump_cooldown = 0;
    }
    let input = forward * 0.98;
    let water_or_lava = mob.body.in_water || mob.body.in_lava;
    let y_before = mob.body.position[1];
    let slip = world.slip(
        mob.body.position[0].floor() as i32,
        (mob.body.position[1] - f64::from(0.500001f32)).floor() as i32,
        mob.body.position[2].floor() as i32,
    );
    let acceleration = if water_or_lava {
        0.02
    } else if mob.body.on_ground {
        speed * (0.21600002f32 / (slip * slip * slip))
    } else {
        0.02
    };
    let friction = if mob.body.on_ground {
        slip * 0.91
    } else {
        0.91
    };
    if f64::from(input) * f64::from(input) >= 1e-7 {
        let (sin, cos) = yaw_sin_cos(mob.body.yaw);
        let distance = f64::from(input * acceleration);
        mob.body.velocity[0] -= distance * f64::from(sin);
        mob.body.velocity[2] += distance * f64::from(cos);
    }
    let motion = mob.body.velocity;
    let body = mob.bbox();
    let movement = collision::collide(world, &body, motion, mob.body.on_ground, 0.6, bufs);
    if movement.iter().map(|value| value * value).sum::<f64>() > 1e-7 {
        for (position, delta) in mob.body.position.iter_mut().zip(movement) {
            *position += delta;
        }
    }
    let horizontal = [
        (movement[0] - motion[0]).abs() >= f64::from(1e-5f32),
        (movement[2] - motion[2]).abs() >= f64::from(1e-5f32),
    ];
    let vertical = motion[1] != movement[1];
    mob.body.horizontal_collision = horizontal[0] || horizontal[1];
    mob.body.on_ground = vertical && motion[1] < 0.0;
    if !mob.body.in_water && fluid_in(world, mob.bbox(), FLUID_WATER).is_some() {
        mob.body.in_water = true;
        mob.body.fall = 0.0;
    }
    let landed = if mob.body.on_ground {
        let fall = mob.body.fall;
        mob.body.fall = 0.0;
        (fall > 0.0).then_some(fall)
    } else {
        if movement[1] < 0.0 {
            mob.body.fall -= movement[1] as f32;
        } else if movement[1] > 0.0 {
            mob.body.fall = 0.0;
        }
        None
    };
    if horizontal[0] {
        mob.body.velocity[0] = 0.0;
    }
    if horizontal[1] {
        mob.body.velocity[2] = 0.0;
    }
    if vertical {
        mob.body.velocity[1] = 0.0;
    }
    if water_or_lava {
        let drag = if mob.body.in_water {
            f64::from(0.8f32)
        } else {
            0.5
        };
        for value in &mut mob.body.velocity {
            *value *= drag;
        }
        mob.body.velocity[1] -= 0.08 / if mob.body.in_water { 16.0 } else { 4.0 };
        if mob.body.horizontal_collision {
            let up = mob.body.velocity[1] + f64::from(0.6f32) - (mob.body.position[1] - y_before);
            let probe = mob
                .bbox()
                .moved([mob.body.velocity[0], up, mob.body.velocity[2]]);
            if collision::space_empty(world, &probe, &mut bufs.a) && !any_fluid(world, probe) {
                mob.body.velocity[1] = f64::from(0.3f32);
            }
        }
    } else {
        mob.body.velocity[0] *= f64::from(friction);
        mob.body.velocity[1] = (mob.body.velocity[1] - 0.08) * f64::from(0.98f32);
        mob.body.velocity[2] *= f64::from(friction);
        if mob.kind == MobKind::Chicken && !mob.body.on_ground && mob.body.velocity[1] < 0.0 {
            mob.body.velocity[1] *= 0.6;
        }
    }
    mob.finish_pose();
    BodyOutcome {
        landed,
        fall_damage: if mob.kind.definition().fall_immune {
            0
        } else {
            landed.map_or(0, |distance| health::fall_damage(distance, 1.0))
        },
        moved: mob.body.position != before,
        horizontal_collision: mob.body.horizontal_collision,
    }
}
