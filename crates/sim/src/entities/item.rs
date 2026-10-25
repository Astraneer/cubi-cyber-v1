//! Java ItemEntity motion, using the shared swept AABB solver. No vertices or
//! inventory/world mutation occurs in this detached transition.
use crate::{
    Aabb, QueryWorld,
    collision::{self, Bufs, Solids},
    query::{self, CollisionQuery},
};
use core_types::DeterministicRng;
use rules::{
    blocks::FLUID_WATER,
    entities::{EntityError, item::ItemEntity},
    survival::loot::unit,
};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemPlan {
    pub after: Option<ItemEntity>,
    pub merge_due: bool,
}
pub fn plan_item(
    item: &ItemEntity,
    world: &impl QueryWorld,
    seed: u64,
    bufs: &mut Bufs,
) -> Result<ItemPlan, EntityError> {
    item.validate()?;
    if query::body_has_unknown(world, item.bbox()) {
        return Err(EntityError::UnknownInput);
    }
    let mut query = CollisionQuery::new(world);
    let plan = tick_solids(*item, &mut query, seed, bufs);
    if query.unknown {
        return Err(EntityError::UnknownInput);
    }
    Ok(plan)
}
fn water(item: &mut ItemEntity, world: &mut impl Solids) -> f64 {
    let depth = super::fluid_in(world, item.bbox(), FLUID_WATER);
    item.in_water = depth.is_some();
    depth.unwrap_or(0.0)
}
fn full_cube(world: &mut impl Solids, position: [i32; 3], bufs: &mut Bufs) -> bool {
    bufs.b.clear();
    world.shapes_at(position[0], position[1], position[2], &mut bufs.b);
    let low = position.map(f64::from);
    bufs.b.len() == 1
        && bufs.b[0]
            == Aabb {
                lo: low,
                hi: low.map(|value| value + 1.0),
            }
}
fn push_out(item: &mut ItemEntity, world: &mut impl Solids, seed: u64, bufs: &mut Bufs) {
    let midpoint = [item.position[0], item.position[1] + 0.125, item.position[2]];
    let cell = midpoint.map(|value| value.floor() as i32);
    let local = std::array::from_fn::<_, 3, _>(|axis| midpoint[axis] - f64::from(cell[axis]));
    let mut best = (1usize, 1.0f64, f64::MAX);
    for (axis, sign) in [(2usize, -1.0f64), (2, 1.0), (0, -1.0), (0, 1.0), (1, 1.0)] {
        let mut neighbor = cell;
        neighbor[axis] += sign as i32;
        if full_cube(world, neighbor, bufs) {
            continue;
        }
        let distance = if sign > 0.0 {
            1.0 - local[axis]
        } else {
            local[axis]
        };
        if distance < best.2 {
            best = (axis, sign, distance);
        }
    }
    let mut rng =
        DeterministicRng::from_key(seed, 0x4954454d50555348, item.id.0, u64::from(item.age));
    for value in &mut item.velocity {
        *value *= 0.75;
    }
    let impulse = unit(&mut rng) * 0.2 + 0.1;
    item.velocity[best.0] = f64::from(best.1 as f32 * impulse);
}
pub fn tick_solids(
    mut item: ItemEntity,
    world: &mut impl Solids,
    seed: u64,
    bufs: &mut Bufs,
) -> ItemPlan {
    item.previous = item.position;
    item.age = item.age.wrapping_add(1);
    let depth = water(&mut item, world);
    if item.position[1] < -64.0 {
        return ItemPlan {
            after: None,
            merge_due: false,
        };
    }
    if item.pickup_delay > 0 && item.pickup_delay != 32767 {
        item.pickup_delay -= 1;
    }
    if item.in_water && depth > f64::from(0.1f32) {
        item.velocity[0] *= f64::from(0.99f32);
        item.velocity[2] *= f64::from(0.99f32);
        if item.velocity[1] < f64::from(0.06f32) {
            item.velocity[1] += f64::from(5e-4f32);
        }
    } else {
        item.velocity[1] -= 0.04;
    }
    item.no_clip = !collision::space_empty(world, &item.bbox().contract(1e-7), &mut bufs.a);
    if item.no_clip {
        push_out(&mut item, world, seed, bufs);
    }
    let horizontal = item.velocity[0] * item.velocity[0] + item.velocity[2] * item.velocity[2];
    if !item.on_ground
        || horizontal > f64::from(1e-5f32)
        || (u64::from(item.age).wrapping_add(item.id.0)).is_multiple_of(4)
    {
        let requested = item.velocity;
        if item.no_clip {
            for (position, delta) in item.position.iter_mut().zip(requested) {
                *position += delta;
            }
        } else {
            let movement =
                collision::collide(world, &item.bbox(), requested, item.on_ground, 0.0, bufs);
            if movement.iter().map(|value| value * value).sum::<f64>() > 1e-7 {
                for (position, delta) in item.position.iter_mut().zip(movement) {
                    *position += delta;
                }
            }
            let vertical = requested[1] != movement[1];
            item.on_ground = vertical && requested[1] < 0.0;
            if (requested[0] - movement[0]).abs() >= f64::from(1e-5f32) {
                item.velocity[0] = 0.0;
            }
            if (requested[2] - movement[2]).abs() >= f64::from(1e-5f32) {
                item.velocity[2] = 0.0;
            }
            if vertical {
                item.velocity[1] = 0.0;
            }
        }
        let drag = if item.on_ground {
            f64::from(
                world.slip(
                    item.position[0].floor() as i32,
                    (item.position[1] - f64::from(0.999999f32)).floor() as i32,
                    item.position[2].floor() as i32,
                ) * 0.98f32,
            )
        } else {
            f64::from(0.98f32)
        };
        item.velocity[0] *= drag;
        item.velocity[1] *= 0.98;
        item.velocity[2] *= drag;
        if item.on_ground && item.velocity[1] < 0.0 {
            item.velocity[1] *= -0.5;
        }
    }
    let moved = (0..3).any(|axis| item.previous[axis].floor() != item.position[axis].floor());
    let merge_due = item.merge_debt || item.age.is_multiple_of(if moved { 2 } else { 40 });
    item.merge_debt = merge_due;
    item.item_age = item.item_age.saturating_add(1);
    item.spin_age = item.spin_age.wrapping_add(1);
    water(&mut item, world);
    ItemPlan {
        after: (item.item_age < 6000).then_some(item),
        merge_due,
    }
}
