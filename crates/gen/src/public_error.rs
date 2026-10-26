//! Server-worker bounds for the canonical public terrain surface. These are
//! interval enclosures, not estimates from a few sampled peaks. Branches whose
//! guards overlap an interval are joined; protected edits remain the caller's
//! separate replacement geometry.
use crate::{GenerationError, Generator, PROFILE_HEIGHT, SEA_LEVEL, v4};
use std::ops::{Add, Div, Mul, Sub};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PublicHeightError {
    pub max_error_blocks: f64,
    pub min_height: i32,
    pub max_height: i32,
    pub intervals: u64,
    pub unit_cells: u64,
    /// Point and gradient-cell interval Perlin evaluations, excluding the
    /// explicitly counted integer columns (at most five per noise query).
    pub noise_evaluations: u64,
    pub sampled_columns: u64,
}

#[derive(Clone, Copy, Debug)]
struct I {
    lo: f32,
    hi: f32,
}
impl From<f32> for I {
    fn from(v: f32) -> Self {
        Self { lo: v, hi: v }
    }
}
impl I {
    fn range(lo: f32, hi: f32) -> Self {
        debug_assert!(lo <= hi);
        Self { lo, hi }
    }
    fn outward(mut lo: f32, mut hi: f32) -> Self {
        // Widen rounded arithmetic endpoints; the enclosure follows the scalar
        // kernel's operand order and never relies on fused instructions.
        for _ in 0..4 {
            lo = lo.next_down();
            hi = hi.next_up();
        }
        Self::range(lo, hi)
    }
    fn hull(self, b: Self) -> Self {
        Self::range(self.lo.min(b.lo), self.hi.max(b.hi))
    }
    fn intersection(self, b: Self) -> Self {
        Self::range(self.lo.max(b.lo), self.hi.min(b.hi))
    }
    fn clamp(self, lo: f32, hi: f32) -> Self {
        Self::range(self.lo.clamp(lo, hi), self.hi.clamp(lo, hi))
    }
    fn abs(self) -> Self {
        if self.lo >= 0. {
            self
        } else if self.hi <= 0. {
            Self::range(-self.hi, -self.lo)
        } else {
            Self::range(0., self.lo.abs().max(self.hi.abs()))
        }
    }
    fn square(self) -> Self {
        let a = self.abs();
        Self::outward(a.lo * a.lo, a.hi * a.hi).clamp(0., f32::MAX)
    }
    fn sqrt(self) -> Self {
        Self::outward(self.lo.max(0.).sqrt(), self.hi.max(0.).sqrt()).clamp(0., f32::MAX)
    }
    fn floor(self) -> Self {
        Self::range(self.lo.floor(), self.hi.floor())
    }
    fn min(self, b: Self) -> Self {
        Self::range(self.lo.min(b.lo), self.hi.min(b.hi))
    }
    fn max(self, b: Self) -> Self {
        Self::range(self.lo.max(b.lo), self.hi.max(b.hi))
    }
    fn sm(self) -> Self {
        let t = self.clamp(0., 1.);
        let generic = (t * t * (I::from(3.) - t * 2.)).intersection(Self::range(0., 1.00001));
        let endpoint = |t: f32| {
            let t = f64::from(t);
            t * t * (3. - 2. * t)
        };
        // Hermite is monotone on[0,1]. The scalar cubic's absolute roundoff is
        // <=9u(1+u)^3+8eta;2^-20 also covers the<16 f64 endpoint operations.
        let padding = 2f64.powi(-20);
        let precise = Self::outward(
            (endpoint(t.lo) - padding) as f32,
            (endpoint(t.hi) + padding) as f32,
        )
        .clamp(0., 1.00001);
        precise.intersection(generic)
    }
    fn lerp(self, b: Self, t: Self) -> Self {
        let value = self + (b - self) * t;
        // Both evaluations enclose the result; their intersection is tighter.
        // Include rounding of the final f32 arithmetic around the convex hull.
        if t.lo >= 0. && t.hi <= 1. {
            let hull = self.hull(b);
            let padding = 8. * f32::EPSILON * (1. + hull.lo.abs().max(hull.hi.abs()));
            value.intersection(Self::range(hull.lo - padding, hull.hi + padding))
        } else {
            value
        }
    }
}
impl<T: Into<I>> Add<T> for I {
    type Output = Self;
    fn add(self, b: T) -> Self {
        let b = b.into();
        Self::outward(self.lo + b.lo, self.hi + b.hi)
    }
}
impl<T: Into<I>> Sub<T> for I {
    type Output = Self;
    fn sub(self, b: T) -> Self {
        let b = b.into();
        Self::outward(self.lo - b.hi, self.hi - b.lo)
    }
}
impl<T: Into<I>> Mul<T> for I {
    type Output = Self;
    fn mul(self, b: T) -> Self {
        let b = b.into();
        let p = [
            self.lo * b.lo,
            self.lo * b.hi,
            self.hi * b.lo,
            self.hi * b.hi,
        ];
        Self::outward(
            p.into_iter().fold(f32::INFINITY, f32::min),
            p.into_iter().fold(f32::NEG_INFINITY, f32::max),
        )
    }
}
impl<T: Into<I>> Div<T> for I {
    type Output = Self;
    fn div(self, b: T) -> Self {
        let b = b.into();
        if b.lo <= 0. && b.hi >= 0. {
            return Self::range(-f32::MAX, f32::MAX);
        }
        let p = [
            self.lo / b.lo,
            self.lo / b.hi,
            self.hi / b.lo,
            self.hi / b.hi,
        ];
        Self::outward(
            p.into_iter().fold(f32::INFINITY, f32::min),
            p.into_iter().fold(f32::NEG_INFINITY, f32::max),
        )
    }
}
fn perlin(noise: &v4::noise::Noise, x: I, z: I, work: &mut u64) -> I {
    *work += 1;
    let cx = (x.lo * 0.5 + x.hi * 0.5).clamp(x.lo, x.hi);
    let cz = (z.lo * 0.5 + z.hi * 0.5).clamp(z.lo, z.hi);
    let center = noise.perlin(cx, cz);
    // Gradients have components<=1. For two horizontal corners, the x terms
    // contribute <=(1-x)+x=1 and the common z terms<=2, hence dot difference<=3.
    // Fade'<=1.875, so each axis derivative<=0.7*(1+3*1.875)=4.6375.
    //4.638 rounds that UP; the separate endpoint guard covers scalar roundoff.
    // The f32 quintic/lerps use local fractions in[0,1], not world coordinates;
    // 1e-4 covers their rounding plus the center evaluation's rounding.
    let radius = 4.638
        * (f64::from((cx - x.lo).max(x.hi - cx)) + f64::from((cz - z.lo).max(z.hi - cz)))
        + 0.0001;
    let global = I::outward(
        (f64::from(center) - radius) as f32,
        (f64::from(center) + radius) as f32,
    )
    .intersection(I::range(-1.4001, 1.4001));
    let Some(local) = perlin_cells(noise, x, z, work) else {
        return global;
    };
    let lo = global.lo.max(local.lo);
    let hi = global.hi.min(local.hi);
    // Both are enclosures; retain the independently proven global bound if
    // an unexpected numerical disagreement makes their intersection empty.
    if lo <= hi { I::range(lo, hi) } else { global }
}

/// A small noise footprint intersects at most four gradient lattice cells.
/// No sample grid or allocation grows with the world tile's surface area.
fn perlin_cells(noise: &v4::noise::Noise, x: I, z: I, work: &mut u64) -> Option<I> {
    if x.hi - x.lo > 1. || z.hi - z.lo > 1. {
        return None;
    }
    let floors = [x.lo.floor(), x.hi.floor(), z.lo.floor(), z.hi.floor()];
    if floors.iter().any(|value| {
        !value.is_finite()
            || f64::from(*value) < f64::from(i32::MIN)
            || f64::from(*value) >= f64::from(i32::MAX)
    }) {
        return None;
    }
    let [x0, x1, z0, z1] = floors.map(|value| value as i32);
    if x1 - x0 > 1 || z1 - z0 > 1 {
        return None;
    }
    let mut result: Option<I> = None;
    for zi in z0..=z1 {
        for xi in x0..=x1 {
            let lo_x = x.lo.max(xi as f32);
            let hi_x = x.hi.min((xi + 1) as f32);
            let lo_z = z.lo.max(zi as f32);
            let hi_z = z.hi.min((zi + 1) as f32);
            if lo_x > hi_x || lo_z > hi_z {
                continue;
            }
            *work += 1;
            let fx = (I::range(lo_x, hi_x) - xi as f32).clamp(0., 1.);
            let fz = (I::range(lo_z, hi_z) - zi as f32).clamp(0., 1.);
            let u = fade(fx);
            let v = fade(fz);
            let px = (xi & 255) as usize;
            let pz = (zi & 255) as usize;
            let a = usize::from(noise.perm[px]) + pz;
            let b = usize::from(noise.perm[px + 1]) + pz;
            let gradient = |hash: u8, x: I, z: I| {
                let (gx, gz) = v4::noise::GRAD[usize::from(hash & 7)];
                I::from(gx) * x + I::from(gz) * z
            };
            let row0 =
                gradient(noise.perm[a], fx, fz).lerp(gradient(noise.perm[b], fx - 1., fz), u);
            let row1 = gradient(noise.perm[a + 1], fx, fz - 1.)
                .lerp(gradient(noise.perm[b + 1], fx - 1., fz - 1.), u);
            let value = row0.lerp(row1, v) * 0.7;
            let value = I::outward(value.lo - 0.0001, value.hi + 0.0001);
            result = Some(result.map_or(value, |old| old.hull(value)));
        }
    }
    result
}
fn fade(t: I) -> I {
    // Quintic fade is monotone on[0,1]. Its ten rounded operations have absolute
    // error below gamma(10)*31 <2e-5;1e-4 also covers endpoint evaluation.
    let eval = |x: f32| {
        let x = f64::from(x);
        x * x * x * (x * (x * 6. - 15.) + 10.)
    };
    I::outward((eval(t.lo) - 0.0001) as f32, (eval(t.hi) + 0.0001) as f32).clamp(0., 1.0001)
}
fn fbm(noise: &v4::noise::Noise, x: I, z: I, octaves: u32, work: &mut u64) -> I {
    let (mut sum, mut amp, mut frequency, mut norm) = (I::from(0.), 1., 1., 0.);
    for o in 0..octaves {
        sum = sum
            + perlin(
                noise,
                x * frequency + o as f32 * 17.13,
                z * frequency - o as f32 * 9.71,
                work,
            ) * amp;
        norm += amp;
        amp *= 0.5;
        frequency *= 2.03;
    }
    sum / norm
}
fn spline(x: I) -> I {
    let mut out = None;
    let mut add = |v: I| {
        out = Some(out.map_or(v, |old: I| old.hull(v)));
    };
    let points = &v4::SPLINE;
    if x.lo <= points[0].0 {
        add(I::from(points[0].1));
    }
    for pair in points.windows(2) {
        let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
        if x.hi >= x0 && x.lo <= x1 {
            let v = x.intersection(I::range(x0, x1));
            add(I::from(y0) + (v - x0) * (y1 - y0) / (x1 - x0));
        }
    }
    if x.hi >= points[points.len() - 1].0 {
        add(I::from(points[points.len() - 1].1));
    }
    out.expect("finite interval intersects a spline branch")
}
fn terrace(value: I, step: I, sharp: f32) -> I {
    let k = value / step;
    let fl = k.floor();
    let t = ((k - fl - (1. - sharp)) / sharp).clamp(0., 1.);
    let generic = (fl + t * t * (I::from(3.) - t * 2.)) * step;
    if step.lo <= 0. || !(0.0..=1.0).contains(&sharp) || sharp == 0. {
        return generic;
    }
    // T(v,s)=s*G(v/s), G is continuous at integer joins and monotone.
    // |G'|<=1.5/a and |dT/ds|<=1+K*(1+1.5/a), K=max|v/s|.
    let a = f64::from(sharp);
    let k = f64::from(value.lo.abs().max(value.hi.abs())) / f64::from(step.lo);
    // The fractional-subtraction bound below assumes every relevant integer
    // floor is exactly representable. Terrain stays far inside this range;
    // retain the generic enclosure for a broader caller instead of assuming it.
    if !k.is_finite() || k >= 2f64.powi(24) {
        return generic;
    }
    let derivative = 1. + k * (1. + 1.5 / a);
    let midpoint = ((step.lo + step.hi) * 0.5).clamp(step.lo, step.hi);
    let radius =
        (f64::from(midpoint) - f64::from(step.lo)).max(f64::from(step.hi) - f64::from(midpoint));
    // Global f32-vs-real bound, including division crossing a floor join:
    // round(k) moves G by <=max|G'|*Ek; frac subtraction has error<=u;
    // clamp is1-Lipschitz; cubic rounding <=9u(1+u)^3; then sum+product.
    let u = 2f64.powi(-24);
    let tiny = 2f64.powi(-149);
    let ek = u * k + tiny;
    let n = k + ek + 1.;
    let et = (4. * u + 16. * u * u + 8. * tiny) / a + tiny;
    let eh = 1.5 * et + 9. * u * (1. + u).powi(3) + 8. * tiny;
    let sum = u * (n + 1. + eh) + tiny;
    let product = f64::from(step.hi) * u * (n + 1. + eh + sum) + tiny;
    let scalar_error = f64::from(step.hi) * (1.5 / a * ek + eh + sum) + product;
    // Two endpoint errors enclose both the scalar endpoint and any interior
    // scalar evaluation. 2^-40 exceeds the roundoff of these<100 f64 ops.
    let padding = (derivative * radius + 2. * scalar_error) * (1. + 2f64.powi(-40));
    let low = f64::from(v4::terrace(value.lo, midpoint, sharp)) - padding;
    let high = f64::from(v4::terrace(value.hi, midpoint, sharp)) + padding;
    let precise = I::outward(low as f32, high as f32);
    if precise.lo > generic.hi || precise.hi < generic.lo {
        generic
    } else {
        precise.intersection(generic)
    }
}

fn raw_height(g: &v4::Gen, x: I, z: I, work: &mut u64) -> I {
    let s = SEA_LEVEL as f32;
    let d0 = (x.square() + z.square()).sqrt();
    let center = (I::from(1.) - d0 / 1600.).sm();
    let near0 = (I::from(1.) - d0 / 400.).sm();
    let c = fbm(&g.cont, x / 2400., z / 2400., 5, work) * 3.2 + 0.02 + center * 0.45;
    let e = fbm(&g.ero, x / 1100., z / 1100., 2, work) * 2.6;
    let temp = fbm(&g.temp, x / 1700., z / 1700., 2, work) * 2.6 * (I::from(1.) - center * 0.5);
    let hum = fbm(&g.hum, x / 1500., z / 1500., 2, work) * 2.6;
    let det = perlin(&g.det, x / 23., z / 23., work) + perlin(&g.det, x / 9., z / 9., work) * 0.5;
    let mut h = spline(c) + s;
    if h.lo < s - 5. {
        let lowered = I::from(s - 5.) + terrace(h - s + 5., I::from(4.), 0.35);
        h = if h.hi < s - 5. {
            lowered
        } else {
            h.hull(lowered)
        };
    }
    if c.hi > -0.2 {
        let cliffy = ((I::from(-0.05) - e) / 0.3).sm();
        let land_k = ((c + 0.1) / I::from(0.14).lerp(I::from(0.015), cliffy)).sm();
        let inland = ((c + 0.05) / 0.4).sm();
        let hilly = ((I::from(0.4) - e) / 0.75).sm();
        let hn = fbm(&g.hill, x / 210., z / 210., 2, work) * 2.6;
        let rel = (I::from(2.) + I::from(22.) * hilly * hilly) * (hn * 0.5 + 0.5).sm();
        let mut chain = (I::from(1.)
            - perlin(&g.ridge, x / 2200. + 77., z / 2200. - 41., work).abs() / 0.15)
            .sm();
        if e.lo >= 0.3 {
            chain = I::from(0.);
        } else if e.hi >= 0.3 {
            chain = chain.hull(I::from(0.));
        }
        let mtn = ((chain - 0.15) / 0.5)
            .sm()
            .mul(((I::from(0.3) - e) / 0.45).sm())
            .max(((I::from(-0.4) - e) / 0.3).sm())
            * land_k
            * (inland * 0.55 + 0.45)
            * (I::from(1.) - near0 * 0.85);
        let mut mh = I::from(0.);
        if mtn.hi > 0.01 {
            let n1 = perlin(&g.ridge, x / 420., z / 420., work);
            let n2 = perlin(&g.ridge, x / 170. + 31., z / 170. - 17., work);
            let r1 = (I::from(1.) - n1.abs() * 2.4).max(I::from(0.));
            let r2 = (I::from(1.) - n2.abs() * 2.4).max(I::from(0.));
            let peaks = r1 * r1 * (I::from(0.6) + I::from(0.4) * r2 * r2);
            mh = mtn * (peaks * 66. * (chain * 0.45 + 0.55) + 14. + det * 3.);
            if mtn.lo <= 0.01 {
                mh = mh.hull(I::from(0.));
            }
        }
        let mut v = (rel * (I::from(1.) - mtn * 0.6) + mh) * land_k;
        if v.hi > 2. {
            let pl = perlin(&g.hill, x / 640. + 91., z / 640. - 33., work) * 2.;
            let warm = ((temp - 0.05) / 0.25).sm();
            let tk = ((pl - 0.05) / 0.3).sm()
                * (warm * 0.7 + 0.3)
                * (hilly * 0.75 + 0.25)
                * (I::from(1.) - mtn * 0.5);
            if tk.hi > 0. {
                let terraced = v.lerp(terrace(v + det * 1.5, pl * 2. + 6.5, 0.3), tk);
                v = if v.lo > 2. && tk.lo > 0. {
                    terraced
                } else {
                    v.hull(terraced)
                };
            }
        }
        let mut land = h + v;
        let desert = ((temp - 0.35) / 0.1).sm()
            * ((I::from(-0.1) - hum) / 0.1).sm()
            * land_k
            * (I::from(1.) - mtn);
        if desert.hi > 0. {
            let dunes = I::from(1.) - perlin(&g.det, x / 34., z / 46., work).abs();
            let added = land + desert * dunes.square() * 5.;
            land = if desert.lo > 0. {
                added
            } else {
                land.hull(added)
            };
        }
        let swamp = ((e - 0.12) / 0.2).sm()
            * ((hum - 0.05) / 0.15).sm()
            * ((temp + 0.1) / 0.2).sm()
            * ((I::from(0.35) - c) / 0.2).sm()
            * land_k
            * (I::from(1.) - mtn);
        if swamp.hi > 0. {
            let changed = land.lerp(I::from(s + 0.4) + det * 2.2, swamp);
            land = if swamp.lo > 0. {
                changed
            } else {
                land.hull(changed)
            };
        }
        let wx = x + perlin(&g.warp, x / 280., z / 280., work) * 70.;
        let wz = z + perlin(&g.warp, x / 280. + 50., z / 280., work) * 70.;
        if mtn.lo < 0.3 {
            let lx = wx / 420. - 400. + g.roff.1;
            let lz = wz / 420. + 170. + g.roff.0;
            let p1 = perlin(&g.vary, lx, lz, work);
            let early = (p1 + 0.35) / 1.5 * 2.7;
            let mut lake =
                (p1 + perlin(&g.vary, lx * 2.03 + 17.13, lz * 2.03 - 9.71, work) * 0.5) / 1.5 * 2.7;
            if p1.hi < -0.07 {
                lake = early;
            } else if p1.lo < -0.07 {
                lake = lake.hull(early);
            }
            let ln = lake + det * 0.03;
            let basin = ((ln - 0.55) / 0.3).sm() * land_k * (I::from(1.) - mtn / 0.3);
            if basin.hi > 0. {
                let mut changed = land.lerp(I::from(s + 1.), basin);
                let core = ((ln - 0.88) / 0.08).sm();
                if core.hi > 0. {
                    let deep = changed.lerp(I::from(s - 1.) - core * 5., core.min(I::from(0.999)));
                    changed = if core.lo > 0. {
                        deep
                    } else {
                        changed.hull(deep)
                    };
                }
                land = if mtn.hi < 0.3 && basin.lo > 0. {
                    changed
                } else {
                    land.hull(changed)
                };
            }
        }
        if land_k.hi > 0. {
            let t = fbm(
                &g.river,
                wx / 1150. + g.roff.0,
                wz / 1150. + g.roff.1,
                2,
                work,
            )
            .abs();
            let core = (I::from(1.) + (I::from(1.) - inland) * 0.8) * 0.0045;
            let valley = core * 3.5 + mtn * 0.012;
            if t.lo < valley.hi {
                let river_k = ((valley - t) / (valley - core)).sm();
                let bank = I::from(s + 1.5)
                    + (land - s - 1.5).max(I::from(0.)) * 0.1 * (I::from(1.) - river_k);
                let mut changed = land.lerp(bank, river_k * land_k);
                let core_k = ((core - t) / core * 2.).sm() * land_k;
                changed = changed.lerp(I::from(s - 4.) + t / core * 4., core_k);
                let mid = core * 0.75;
                if t.lo < mid.hi && land_k.hi > 0.3 {
                    let cut = changed.min(I::from(s - 1.) - (I::from(1.) - t / mid) * 3.);
                    changed = if t.hi < mid.lo && land_k.lo > 0.3 {
                        cut
                    } else {
                        changed.hull(cut)
                    };
                }
                land = if land_k.lo > 0. && t.hi < valley.lo {
                    changed
                } else {
                    land.hull(changed)
                };
            }
        }
        h = if c.lo > -0.2 { land } else { h.hull(land) };
    }
    if h.hi > 108. {
        let high = I::from(108.) + (h - 108.) * 0.5;
        h = if h.lo > 108. { high } else { h.hull(high) };
    }
    h.clamp(4., (PROFILE_HEIGHT - 3) as f32)
}

impl Generator {
    /// Bounds the actual public voxel stairs against the coarse triangles.
    /// Levels0..2 use at most4225 exact integer columns; larger cold areas use
    /// 256intervals+289corners, never a scan proportional to their area.
    pub fn public_height_error(
        &self,
        origin: [i32; 2],
        level: u8,
    ) -> Result<PublicHeightError, GenerationError> {
        if level > 10 {
            return Err(GenerationError::CoordinateOutOfRange);
        }
        let step = 1i32 << level;
        let end = [
            origin[0].checked_add(16 * step),
            origin[1].checked_add(16 * step),
        ];
        let [Some(end_x), Some(end_z)] = end else {
            return Err(GenerationError::CoordinateOutOfRange);
        };
        if level <= 2 {
            return Ok(self.exact_small_height_error(origin, level));
        }
        let mut heights = [0i32; 289];
        for z in 0..17 {
            for x in 0..17 {
                heights[x + 17 * z] = self
                    .inner
                    .column(origin[0] + x as i32 * step, origin[1] + z as i32 * step)
                    .h
                    .max(SEA_LEVEL)
                    + 1;
            }
        }
        let mut result = PublicHeightError {
            max_error_blocks: 0.,
            min_height: i32::MAX,
            max_height: i32::MIN,
            intervals: 0,
            unit_cells: 0,
            noise_evaluations: 0,
            sampled_columns: 289,
        };
        for z in 0..16 {
            for x in 0..16 {
                let x0 = origin[0] + x * step;
                let z0 = origin[1] + z * step;
                let bounds = raw_height(
                    &self.inner,
                    I::range(x0 as f32, (x0 + step) as f32),
                    I::range(z0 as f32, (z0 + step) as f32),
                    &mut result.noise_evaluations,
                );
                let mut low = (bounds.lo as i32).max(SEA_LEVEL) + 1;
                let mut high = (bounds.hi as i32).max(SEA_LEVEL) + 1;
                // shape() is a rounded convex blend with the spawn's floor inside
                // R_BLEND. Its square is conservative even for inactive sites.
                let site = &self.inner.site;
                let radius = f64::from(v4::spawn::R_BLEND);
                if f64::from(x0) <= f64::from(site.x) + radius
                    && f64::from(x0 + step) >= f64::from(site.x) - radius
                    && f64::from(z0) <= f64::from(site.z) + radius
                    && f64::from(z0 + step) >= f64::from(site.z) - radius
                {
                    low = low.min(site.y0.max(SEA_LEVEL));
                    high = high.max(site.y0.max(SEA_LEVEL) + 2);
                }
                result.min_height = result.min_height.min(low);
                result.max_height = result.max_height.max(high);
                let i = (x + 17 * z) as usize;
                let corners = [heights[i], heights[i + 1], heights[i + 17], heights[i + 18]];
                let min = *corners.iter().min().unwrap();
                let max = *corners.iter().max().unwrap();
                // The raster's two triangles are convex combinations of corners.
                // This also bounds voxel stair/floor displacement between vertices.
                result.max_error_blocks = result
                    .max_error_blocks
                    .max(f64::from((high - min).max(max - low)));
                result.intervals += 1;
            }
        }
        debug_assert!(end_x > origin[0] && end_z > origin[1]);
        Ok(result)
    }
    fn exact_small_height_error(&self, origin: [i32; 2], level: u8) -> PublicHeightError {
        let step = 1usize << level;
        let span = 16 * step;
        let width = span + 1;
        let mut heights = [0i32; 65 * 65];
        let mut result = PublicHeightError {
            max_error_blocks: 0.,
            min_height: i32::MAX,
            max_height: i32::MIN,
            intervals: 0,
            unit_cells: 0,
            noise_evaluations: 0,
            sampled_columns: (width * width) as u64,
        };
        for z in 0..width {
            for x in 0..width {
                let h = self
                    .inner
                    .column(origin[0] + x as i32, origin[1] + z as i32)
                    .h
                    .max(SEA_LEVEL)
                    + 1;
                heights[x + z * width] = h;
                result.min_height = result.min_height.min(h);
                result.max_height = result.max_height.max(h);
            }
        }
        let plane = |x: usize, z: usize| {
            let cx = (x / step).min(15) * step;
            let cz = (z / step).min(15) * step;
            let fx = (x - cx) as f64 / step as f64;
            let fz = (z - cz) as f64 / step as f64;
            let h00 = f64::from(heights[cx + cz * width]);
            let h10 = f64::from(heights[cx + step + cz * width]);
            let h01 = f64::from(heights[cx + (cz + step) * width]);
            let h11 = f64::from(heights[cx + step + (cz + step) * width]);
            if fx + fz <= 1. {
                h00 + (h10 - h00) * fx + (h01 - h00) * fz
            } else {
                h11 + (h01 - h11) * (1. - fx) + (h10 - h11) * (1. - fz)
            }
        };
        // Coarse diagonals have integer45-degree edges. Their intersections
        // with a unit footprint are its corners, so affine extrema occur here.
        // Include closed grid endpoints separately for the adjacent voxel side.
        for z in 0..width {
            for x in 0..width {
                result.max_error_blocks = result
                    .max_error_blocks
                    .max((f64::from(heights[x + z * width]) - plane(x, z)).abs());
                if x < span && z < span {
                    let h = f64::from(heights[x + z * width]);
                    for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        result.max_error_blocks = result
                            .max_error_blocks
                            .max((h - plane(x + dx, z + dz)).abs());
                    }
                    result.unit_cells += 1;
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monotone_smoothstep_enclosure_covers_scalar_clamps_and_tightens_high_end() {
        for low in [-0.01f32, 0., 0.1, 0.4, 0.6, 0.8, 0.95, 1., 1.01] {
            for width in [0.000001f32, 0.01, 0.2, 1.] {
                let high = low + width;
                let bound = I::range(low, high).sm();
                for index in 0..=1000 {
                    let x = low + (high - low) * index as f32 / 1000.;
                    let actual = v4::noise::sm(x);
                    assert!(
                        actual >= bound.lo && actual <= bound.hi,
                        "x{x}actual{actual}bound{bound:?}"
                    );
                }
            }
        }
        let tight = I::range(0.8, 1.).sm();
        assert!(tight.hi - tight.lo < 0.105);
    }
    #[test]
    fn terrace_enclosures_cover_variable_step_negative_values_and_floor_joins() {
        for sharp in [0.3f32, 0.35, 1.] {
            for center in [-40f32, -4.000001, -3.9, -0.000001, 0., 3.9, 4., 39.9, 100.] {
                for width in [0.00001f32, 0.1, 1., 10.] {
                    for step in [0.9f32, 4., 7.] {
                        let v = I::range(center - width * 0.5, center + width * 0.5);
                        let s = I::range(step - 0.1, step + 0.1);
                        let bound = terrace(v, s, sharp);
                        for iz in 0..=20 {
                            for ix in 0..=20 {
                                let v = v.lo + (v.hi - v.lo) * ix as f32 / 20.;
                                let s = s.lo + (s.hi - s.lo) * iz as f32 / 20.;
                                let actual = v4::terrace(v, s, sharp);
                                assert!(
                                    actual >= bound.lo && actual <= bound.hi,
                                    "v{v}s{s}a{sharp}:actual{actual}bound{bound:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
        let tight = terrace(I::range(3.99, 4.01), I::from(4.), 0.3);
        assert!(
            tight.hi - tight.lo < 0.01,
            "continuous floor join must not invent an8block gap"
        );
    }
    #[test]
    fn gradient_cell_enclosures_cover_scalar_noise_across_negative_and_periodic_boundaries() {
        for seed in [1, 1337, 0x7f23ac15] {
            let noise = v4::noise::Noise::new(seed);
            for origin in [-257.001_f32, -1.001, 0.001, 255.999] {
                for width in [0.0005_f32, 0.05, 0.5, 1.] {
                    let x = I::range(origin, origin + width);
                    let z = I::range(origin * 0.5, origin * 0.5 + width);
                    let mut work = 0;
                    let enclosure = perlin(&noise, x, z, &mut work);
                    assert!((1..=5).contains(&work));
                    for iz in 0..=100 {
                        for ix in 0..=100 {
                            let value = noise.perlin(
                                x.lo + (x.hi - x.lo) * ix as f32 / 100.,
                                z.lo + (z.hi - z.lo) * iz as f32 / 100.,
                            );
                            assert!(
                                value >= enclosure.lo && value <= enclosure.hi,
                                "seed{seed} origin{origin} width{width} value{value} enclosure{enclosure:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn narrow_gradient_cells_improve_the_global_lipschitz_envelope() {
        let noise = v4::noise::Noise::new(1337);
        let x = I::range(0.12, 0.125);
        let z = I::range(-0.4, -0.395);
        let mut work = 0;
        let bound = perlin(&noise, x, z, &mut work);
        let previous_width = 16. * f64::from((x.hi - x.lo + z.hi - z.lo) * 0.5) + 0.0002;
        assert!(f64::from(bound.hi - bound.lo) < previous_width * 0.5);
        assert_eq!(work, 2);
    }
}
