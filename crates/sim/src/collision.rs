//! Deterministic swept AABB collision with the Java 1.21.1 axis and step rules.
//!
//! Numerical reference: the allowlisted donor `game/physics.rs`. The solver owns
//! no world and imposes no world-height convention. Callers supply loaded-cell
//! shapes; unknown-cell policy belongs to that adapter.

use rules::shapes::Aabb;

pub const EPS: f64 = 1e-7;
pub const MAX_QUERY_CELLS: usize = 4096;
pub const MAX_QUERY_SHAPES: usize = 16_384;
pub const MAX_AXIS_MOVEMENT: f64 = 64.0;

/// Cell-local queries append shapes in world coordinates. No generation occurs
/// in this interface. Shapes must be finite and confined to their source cell.
pub trait Solids {
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>);
    fn slip(&mut self, x: i32, y: i32, z: i32) -> f32;
    fn chokes(&mut self, x: i32, y: i32, z: i32) -> bool;
    fn climbable(&mut self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    fn fluid(&mut self, _x: i32, _y: i32, _z: i32) -> (u8, f64) {
        (0, 0.0)
    }
    fn flow(&mut self, _x: i32, _y: i32, _z: i32) -> [f64; 3] {
        [0.0; 3]
    }
}

/// Reused by one owner. Both broad phases and step candidates are retained.
#[derive(Debug)]
pub struct Bufs {
    pub a: Vec<Aabb>,
    pub b: Vec<Aabb>,
    pub candidates: Vec<f32>,
}

impl Default for Bufs {
    fn default() -> Self {
        Self {
            a: Vec::with_capacity(256),
            b: Vec::with_capacity(256),
            candidates: Vec::with_capacity(32),
        }
    }
}

fn finite_box(area: &Aabb) -> bool {
    (0..3).all(|axis| {
        area.lo[axis].is_finite() && area.hi[axis].is_finite() && area.lo[axis] <= area.hi[axis]
    })
}

fn valid_movement(movement: [f64; 3]) -> bool {
    movement
        .iter()
        .all(|v| v.is_finite() && v.abs() <= MAX_AXIS_MOVEMENT)
}

fn query_bounds(area: &Aabb) -> Option<([i32; 3], [i32; 3])> {
    if !finite_box(area) {
        return None;
    }
    let mut low = [0; 3];
    let mut high = [0; 3];
    let mut volume = 1usize;
    for axis in 0..3 {
        let lo = (area.lo[axis] - EPS).floor();
        let hi = (area.hi[axis] + EPS).floor();
        if lo < f64::from(i32::MIN) || hi > f64::from(i32::MAX) {
            return None;
        }
        low[axis] = lo as i32;
        high[axis] = hi as i32;
        let length = i64::from(high[axis]) - i64::from(low[axis]) + 1;
        let length = usize::try_from(length).ok()?;
        volume = volume.checked_mul(length)?;
        if volume > MAX_QUERY_CELLS {
            return None;
        }
    }
    Some((low, high))
}

/// Collect intersecting shapes in deterministic Z/Y/X order. `false` means an
/// invalid or over-budget query; movement callers must treat it as blocked.
pub fn collisions(world: &mut impl Solids, area: &Aabb, out: &mut Vec<Aabb>) -> bool {
    out.clear();
    let Some((low, high)) = query_bounds(area) else {
        return false;
    };
    for z in low[2]..=high[2] {
        for y in low[1]..=high[1] {
            for x in low[0]..=high[0] {
                let old_len = out.len();
                world.shapes_at(x, y, z, out);
                if out.len() < old_len || out.len() > MAX_QUERY_SHAPES {
                    out.clear();
                    return false;
                }
                let mut retained = old_len;
                for index in old_len..out.len() {
                    if !finite_box(&out[index]) {
                        out.clear();
                        return false;
                    }
                    if out[index].intersects(area) {
                        out[retained] = out[index];
                        retained += 1;
                    }
                }
                out.truncate(retained);
            }
        }
    }
    true
}

/// Maximum signed travel on one axis before a shape is entered. Existing deep
/// interpenetration is left to the player's separate push-out policy.
pub fn max_dist(axis: usize, shape: &Aabb, body: &Aabb, distance: f64) -> f64 {
    if axis >= 3 || !distance.is_finite() || !finite_box(shape) || !finite_box(body) {
        return 0.0;
    }
    if distance.abs() < EPS {
        return 0.0;
    }
    for other in 0..3 {
        if other != axis
            && !(body.hi[other] - EPS >= shape.lo[other] && body.lo[other] + EPS < shape.hi[other])
        {
            return distance;
        }
    }
    if distance > 0.0 && body.hi[axis] - EPS < shape.lo[axis] {
        let gap = shape.lo[axis] - body.hi[axis];
        if gap >= -EPS {
            return distance.min(gap);
        }
    } else if distance < 0.0 && body.lo[axis] + EPS >= shape.hi[axis] {
        let gap = shape.hi[axis] - body.lo[axis];
        if gap <= EPS {
            return distance.max(gap);
        }
    }
    distance
}

pub fn max_offset(axis: usize, body: &Aabb, shapes: &[Aabb], mut distance: f64) -> f64 {
    for shape in shapes {
        if distance.abs() < EPS {
            return 0.0;
        }
        distance = max_dist(axis, shape, body, distance);
    }
    distance
}

/// Resolve Y first, then the larger horizontal component. Equal X/Z uses X.
pub fn sweep(movement: [f64; 3], mut body: Aabb, shapes: &[Aabb]) -> [f64; 3] {
    if !valid_movement(movement) || !finite_box(&body) {
        return [0.0; 3];
    }
    let [mut x, mut y, mut z] = movement;
    if y != 0.0 {
        y = max_offset(1, &body, shapes, y);
        body = body.moved([0.0, y, 0.0]);
    }
    let z_first = x.abs() < z.abs();
    if z_first && z != 0.0 {
        z = max_offset(2, &body, shapes, z);
        body = body.moved([0.0, 0.0, z]);
    }
    if x != 0.0 {
        x = max_offset(0, &body, shapes, x);
        if !z_first {
            body = body.moved([x, 0.0, 0.0]);
        }
    }
    if !z_first && z != 0.0 {
        z = max_offset(2, &body, shapes, z);
    }
    [x, y, z]
}

pub fn hlen2(vector: [f64; 3]) -> f64 {
    vector[0] * vector[0] + vector[2] * vector[2]
}

/// A 1.21.1 step tries ascending shape-boundary heights, selecting the first
/// candidate which increases horizontal travel over the ordinary collision.
pub fn collide(
    world: &mut impl Solids,
    body: &Aabb,
    movement: [f64; 3],
    on_ground: bool,
    step: f32,
    bufs: &mut Bufs,
) -> [f64; 3] {
    if !finite_box(body) || !valid_movement(movement) || !step.is_finite() || step < 0.0 {
        return [0.0; 3];
    }
    if movement == [0.0; 3] {
        return movement;
    }
    if !collisions(world, &body.stretch(movement), &mut bufs.a) {
        return [0.0; 3];
    }
    let ordinary = sweep(movement, *body, &bufs.a);
    let landing = movement[1] != ordinary[1] && movement[1] < 0.0;
    let horizontal_hit = movement[0] != ordinary[0] || movement[2] != ordinary[2];
    if step == 0.0 || !(on_ground || landing) || !horizontal_hit {
        return ordinary;
    }
    let origin = if landing {
        body.moved([0.0, ordinary[1], 0.0])
    } else {
        *body
    };
    let mut step_area = origin.stretch([movement[0], f64::from(step), movement[2]]);
    if !landing {
        step_area = step_area.stretch([0.0, -f64::from(1e-5f32), 0.0]);
    }
    if !collisions(world, &step_area, &mut bufs.b) {
        return [0.0; 3];
    }
    bufs.candidates.clear();
    let ordinary_y = ordinary[1] as f32;
    for shape in &bufs.b {
        for boundary in [shape.lo[1], shape.hi[1]] {
            let candidate = (boundary - origin.lo[1]) as f32;
            if candidate >= 0.0
                && candidate != ordinary_y
                && candidate <= step
                && !bufs.candidates.contains(&candidate)
            {
                bufs.candidates.push(candidate);
            }
        }
    }
    bufs.candidates.sort_unstable_by(f32::total_cmp);
    for &height in &bufs.candidates {
        let alternative = sweep(
            [movement[0], f64::from(height), movement[2]],
            origin,
            &bufs.b,
        );
        if hlen2(alternative) > hlen2(ordinary) {
            return [
                alternative[0],
                alternative[1] - (body.lo[1] - origin.lo[1]),
                alternative[2],
            ];
        }
    }
    ordinary
}

pub fn space_below_empty(
    world: &mut impl Solids,
    body: &Aabb,
    dx: f64,
    dz: f64,
    depth: f32,
    buffer: &mut Vec<Aabb>,
) -> bool {
    if !dx.is_finite() || !dz.is_finite() || !depth.is_finite() || depth < 0.0 {
        return false;
    }
    let area = Aabb {
        lo: [
            body.lo[0] + dx,
            body.lo[1] - f64::from(depth) - f64::from(1e-5f32),
            body.lo[2] + dz,
        ],
        hi: [body.hi[0] + dx, body.lo[1], body.hi[2] + dz],
    };
    collisions(world, &area, buffer) && buffer.is_empty()
}

fn shorter(value: f64) -> f64 {
    if value.abs() <= 0.05 {
        0.0
    } else {
        value - 0.05f64.copysign(value)
    }
}

/// The three sneak passes are X, Z, then the external corner, in 0.05 steps.
pub fn edge_guard(
    world: &mut impl Solids,
    body: &Aabb,
    movement: [f64; 3],
    step: f32,
    buffer: &mut Vec<Aabb>,
) -> [f64; 3] {
    if !valid_movement(movement) || !finite_box(body) || !step.is_finite() || step < 0.0 {
        return [0.0; 3];
    }
    let area = body
        .stretch(movement)
        .stretch([0.0, -f64::from(step) - f64::from(1e-5f32), 0.0]);
    if query_bounds(&area).is_none() {
        return [0.0; 3];
    }
    let (mut x, mut z) = (movement[0], movement[2]);
    while x != 0.0 && space_below_empty(world, body, x, 0.0, step, buffer) {
        x = shorter(x);
    }
    while z != 0.0 && space_below_empty(world, body, 0.0, z, step, buffer) {
        z = shorter(z);
    }
    while x != 0.0 && z != 0.0 && space_below_empty(world, body, x, z, step, buffer) {
        x = shorter(x);
        z = shorter(z);
    }
    [x, movement[1], z]
}

pub fn space_empty(world: &mut impl Solids, body: &Aabb, buffer: &mut Vec<Aabb>) -> bool {
    collisions(world, body, buffer) && buffer.is_empty()
}
