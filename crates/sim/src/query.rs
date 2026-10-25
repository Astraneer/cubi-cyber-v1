//! Read-only collision adapter. Unknown cells are solid; reading never generates.
use crate::{Aabb, collision::Solids};
use core_types::{BlockPos, Cell, SpaceId};
use rules::blocks::{bdef, collision_of, face_full};
use world::CellRead;

pub trait QueryWorld {
    fn cell(&self, position: [i32; 3]) -> CellRead;
    fn flow_override(&self, _position: [i32; 3]) -> Option<[f64; 3]> {
        None
    }
}
impl<F: Fn([i32; 3]) -> CellRead> QueryWorld for F {
    fn cell(&self, position: [i32; 3]) -> CellRead {
        self(position)
    }
}
pub struct WorldQuery<'a> {
    pub world: &'a world::World,
    pub space: SpaceId,
}
impl<'a> WorldQuery<'a> {
    pub const fn new(world: &'a world::World, space: SpaceId) -> Self {
        Self { world, space }
    }
}
impl QueryWorld for WorldQuery<'_> {
    fn cell(&self, p: [i32; 3]) -> CellRead {
        self.world.read(BlockPos {
            space: self.space,
            x: p[0],
            y: p[1],
            z: p[2],
        })
    }
}

pub(crate) fn body_has_unknown(world: &impl QueryWorld, body: Aabb) -> bool {
    let body = body.contract(1e-7);
    for z in body.lo[2].floor() as i32..=body.hi[2].floor() as i32 {
        for y in body.lo[1].floor() as i32..=body.hi[1].floor() as i32 {
            for x in body.lo[0].floor() as i32..=body.hi[0].floor() as i32 {
                if world.cell([x, y, z]) == CellRead::Unknown {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) struct CollisionQuery<'a, W: QueryWorld> {
    world: &'a W,
    pub unknown: bool,
}
impl<'a, W: QueryWorld> CollisionQuery<'a, W> {
    pub fn new(world: &'a W) -> Self {
        Self {
            world,
            unknown: false,
        }
    }
    fn read(&mut self, p: [i32; 3]) -> Option<Cell> {
        match self.world.cell(p) {
            CellRead::Known(cell) => Some(cell),
            CellRead::Unknown => {
                self.unknown = true;
                None
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FluidSample {
    pub kind: u8,
    pub amount: u8,
    pub falling: bool,
}
pub fn fluid_sample(cell: Cell) -> Option<FluidSample> {
    let kind = bdef(cell.id()).fluid;
    if kind == 0 {
        return None;
    }
    let level = cell.state() & 15;
    Some(FluidSample {
        kind,
        amount: if (1..=7).contains(&level) {
            8 - level as u8
        } else {
            8
        },
        falling: level >= 8,
    })
}
pub fn fluid_height(cell: Cell, above: Cell) -> f64 {
    let Some(fluid) = fluid_sample(cell) else {
        return 0.0;
    };
    if fluid_sample(above).is_some_and(|other| other.kind == fluid.kind) {
        1.0
    } else {
        (f32::from(fluid.amount) / 9.0f32) as f64
    }
}
fn solid(cell: Cell) -> bool {
    let boxes = collision_of(cell.raw());
    if boxes.is_empty() {
        return false;
    }
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for shape in boxes {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(shape.lo[axis]);
            hi[axis] = hi[axis].max(shape.hi[axis]);
        }
    }
    (hi[0] - lo[0] + hi[1] - lo[1] + hi[2] - lo[2]) / 3.0 >= 35.0 / 48.0 - 1e-9
        || hi[1] - lo[1] >= 1.0 - 1e-9
}
impl<W: QueryWorld> Solids for CollisionQuery<'_, W> {
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let offset = [f64::from(x), f64::from(y), f64::from(z)];
        match self.read([x, y, z]) {
            Some(cell) => out.extend(
                collision_of(cell.raw())
                    .iter()
                    .map(|shape| shape.moved(offset)),
            ),
            None => out.push(Aabb {
                lo: offset,
                hi: [offset[0] + 1.0, offset[1] + 1.0, offset[2] + 1.0],
            }),
        }
    }
    fn slip(&mut self, x: i32, y: i32, z: i32) -> f32 {
        self.read([x, y, z])
            .map_or(0.6, |cell| bdef(cell.id()).slip)
    }
    fn chokes(&mut self, x: i32, y: i32, z: i32) -> bool {
        self.read([x, y, z])
            .is_none_or(|cell| bdef(cell.id()).suffocates)
    }
    fn climbable(&mut self, x: i32, y: i32, z: i32) -> bool {
        self.read([x, y, z])
            .is_some_and(|cell| bdef(cell.id()).climbable)
    }
    fn fluid(&mut self, x: i32, y: i32, z: i32) -> (u8, f64) {
        let Some(cell) = self.read([x, y, z]) else {
            return (0, 0.0);
        };
        let Some(fluid) = fluid_sample(cell) else {
            return (0, 0.0);
        };
        let above = self.read([x, y + 1, z]).unwrap_or(Cell::AIR);
        (fluid.kind, fluid_height(cell, above))
    }
    fn flow(&mut self, x: i32, y: i32, z: i32) -> [f64; 3] {
        if let Some(flow) = self.world.flow_override([x, y, z]) {
            return if flow.iter().all(|x| x.is_finite()) {
                flow
            } else {
                [0.0; 3]
            };
        }
        let Some(me) = self.read([x, y, z]).and_then(fluid_sample) else {
            return [0.0; 3];
        };
        let mut v = [0.0f32; 2];
        let sides = [(0, -1, 2u8), (1, 0, 5), (0, 1, 3), (-1, 0, 4)];
        for (dx, dz, _) in sides {
            let Some(neighbor) = self.read([x + dx, y, z + dz]) else {
                continue;
            };
            let next = fluid_sample(neighbor);
            if next.is_some_and(|other| other.kind != me.kind) {
                continue;
            }
            let mut f = next.map_or(0.0, |other| f32::from(other.amount) / 9.0);
            let mut drop = 0.0;
            if f == 0.0 {
                if !solid(neighbor) {
                    let below = self.read([x + dx, y - 1, z + dz]).and_then(fluid_sample);
                    if below.is_none_or(|other| other.kind == me.kind) {
                        f = below.map_or(0.0, |other| f32::from(other.amount) / 9.0);
                        if f > 0.0 {
                            drop = f32::from(me.amount) / 9.0 - (f - 0.888_888_9);
                        }
                    }
                }
            } else {
                drop = f32::from(me.amount) / 9.0 - f;
            }
            v[0] += dx as f32 * drop;
            v[1] += dz as f32 * drop;
        }
        let mut vector = [f64::from(v[0]), 0.0, f64::from(v[1])];
        if me.falling {
            for (dx, dz, direction) in sides {
                let blocked = [y, y + 1].into_iter().any(|yy| {
                    self.read([x + dx, yy, z + dz])
                        .is_none_or(|cell| face_full(cell.raw(), direction ^ 1))
                });
                if blocked {
                    let length = (vector[0] * vector[0] + vector[2] * vector[2]).sqrt();
                    if length > 0.0 {
                        vector[0] /= length;
                        vector[2] /= length;
                    }
                    vector[1] = -6.0;
                    break;
                }
            }
        }
        let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
        if length > 0.0 {
            [vector[0] / length, vector[1] / length, vector[2] / length]
        } else {
            [0.0; 3]
        }
    }
}
