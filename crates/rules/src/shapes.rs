//! Stable registry data and pure formulas from the SPEC-approved donor.
//! Source: donor/crates/voxel-core/src/registry/shapes.rs at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! Adapted to the shared rules layer; no donor world or runtime is included.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub lo: [f64; 3],
    pub hi: [f64; 3],
}

impl Aabb {
    #[inline]
    pub fn moved(&self, d: [f64; 3]) -> Aabb {
        Aabb {
            lo: [self.lo[0] + d[0], self.lo[1] + d[1], self.lo[2] + d[2]],
            hi: [self.hi[0] + d[0], self.hi[1] + d[1], self.hi[2] + d[2]],
        }
    }
    pub fn stretch(&self, d: [f64; 3]) -> Aabb {
        let mut b = *self;
        for (a, &amount) in d.iter().enumerate() {
            if amount < 0.0 {
                b.lo[a] += amount;
            } else if amount > 0.0 {
                b.hi[a] += amount;
            }
        }
        b
    }
    pub fn contract(&self, e: f64) -> Aabb {
        Aabb {
            lo: [self.lo[0] + e, self.lo[1] + e, self.lo[2] + e],
            hi: [self.hi[0] - e, self.hi[1] - e, self.hi[2] - e],
        }
    }
    #[inline]
    pub fn intersects(&self, o: &Aabb) -> bool {
        (0..3).all(|a| self.lo[a] < o.hi[a] && self.hi[a] > o.lo[a])
    }
}

pub const fn px(x0: f64, y0: f64, z0: f64, x1: f64, y1: f64, z1: f64) -> Aabb {
    Aabb {
        lo: [x0 / 16.0, y0 / 16.0, z0 / 16.0],
        hi: [x1 / 16.0, y1 / 16.0, z1 / 16.0],
    }
}

pub const CUBE: &[Aabb] = &[Aabb {
    lo: [0.0; 3],
    hi: [1.0; 3],
}];
pub const SHORT_PLANT: &[Aabb] = &[px(2.0, 0.0, 2.0, 14.0, 13.0, 14.0)];
pub const FLOWER: &[Aabb] = &[px(5.0, 0.0, 5.0, 11.0, 10.0, 11.0)];
pub const TORCH_SHAPE: &[Aabb] = &[px(6.0, 0.0, 6.0, 10.0, 10.0, 10.0)];
pub const CHEST_SHAPE: &[Aabb] = &[px(1.0, 0.0, 1.0, 15.0, 14.0, 15.0)];
pub const LADDER_SHAPE: &[Aabb] = &[px(0.0, 0.0, 13.0, 16.0, 16.0, 16.0)];
pub const SNOW_LAYER_SHAPE: &[Aabb] = &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)];
pub const CACTUS_SIDES: &[Aabb] = &[px(1.0, 0.0, 1.0, 15.0, 15.0, 15.0)];
pub const CACTUS_OUTLINE: &[Aabb] = &[px(1.0, 0.0, 1.0, 15.0, 16.0, 15.0)];

pub const LADDER_SHAPES: [&[Aabb]; 4] = [
    &[px(0.0, 0.0, 13.0, 16.0, 16.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 16.0, 3.0)],
    &[px(13.0, 0.0, 0.0, 16.0, 16.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 3.0, 16.0, 16.0)],
];
pub const WALL_TORCH_SHAPES: [&[Aabb]; 4] = [
    &[px(5.5, 3.0, 11.0, 10.5, 13.0, 16.0)],
    &[px(5.5, 3.0, 0.0, 10.5, 13.0, 5.0)],
    &[px(11.0, 3.0, 5.5, 16.0, 13.0, 10.5)],
    &[px(0.0, 3.0, 5.5, 5.0, 13.0, 10.5)],
];
pub const SNOW_SHAPES: [&[Aabb]; 9] = [
    &[],
    &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 4.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 6.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 8.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 10.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 12.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 14.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 16.0, 16.0)],
];
pub const FARMLAND_SHAPE: &[Aabb] = &[px(0.0, 0.0, 0.0, 16.0, 15.0, 16.0)];
pub const WHEAT_SHAPES: [&[Aabb]; 8] = [
    &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 4.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 6.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 8.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 10.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 12.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 14.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 16.0, 16.0)],
];
pub const ROOT_SHAPES: [&[Aabb]; 8] = [
    &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 3.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 4.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 5.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 6.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 7.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 8.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 9.0, 16.0)],
];
pub const MAST: &[Aabb] = &[px(4.0, 0.0, 4.0, 12.0, 16.0, 12.0)];
pub const CABLE_CORE: &[Aabb] = &[px(5.0, 5.0, 5.0, 11.0, 11.0, 11.0)];
pub const LARGE_CORE: &[Aabb] = &[px(4.0, 4.0, 4.0, 12.0, 12.0, 12.0)];
