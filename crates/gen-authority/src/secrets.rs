//! Pure version-4 numerical kernels and procedural structures from the approved donor.
//! Source: donor/crates/voxel-core/src/gen at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! World storage, async ownership, publication and client secrecy are implemented separately.
use super::{CS, Entropy, H, PAD, Region, W};
use rules::cell::BlockId;
use rules::ids::*;
use terrain_gen::compatibility::{Rng, hash};

pub(crate) struct OreFeature {
    pub(crate) block: BlockId,
    pub(crate) count: f32,
    pub(crate) size: i32,
    pub(crate) lo: i32,
    pub(crate) hi: i32,
    pub(crate) tri: bool,
    pub(crate) discard: f32,
    pub(crate) plat: i32,
}

const fn ore_f(
    block: BlockId,
    count: f32,
    size: i32,
    lo: i32,
    hi: i32,
    tri: bool,
    discard: f32,
) -> OreFeature {
    OreFeature {
        block,
        count,
        size,
        lo,
        hi,
        tri,
        discard,
        plat: 0,
    }
}

pub(crate) const ORES: [OreFeature; 14] = [
    ore_f(COAL_ORE, 7.3, 17, 82, 127, false, 0.0),
    ore_f(COAL_ORE, 7.5, 17, 24, 96, true, 0.5),
    ore_f(IRON_ORE, 22.2, 9, 68, 143, true, 0.0),
    ore_f(IRON_ORE, 3.75, 9, 15, 45, true, 0.0),
    ore_f(IRON_ORE, 4.26, 4, 0, 58, false, 0.0),
    ore_f(GOLD_ORE, 1.5, 9, 0, 36, true, 0.5),
    ore_f(GOLD_ORE, 0.19, 9, 0, 6, false, 0.5),
    ore_f(REDSTONE_ORE, 1.52, 8, 0, 30, false, 0.0),
    ore_f(REDSTONE_ORE, 3.0, 8, -12, 12, true, 0.0),
    ore_f(COPPER_ORE, 7.25, 10, 18, 76, true, 0.0),
    OreFeature {
        block: OSMIUM_ORE,
        count: 17.75,
        size: 7,
        lo: 59,
        hi: 133,
        tri: true,
        discard: 0.0,
        plat: 2,
    },
    ore_f(OSMIUM_ORE, 2.25, 9, 12, 45, true, 0.0),
    ore_f(OSMIUM_ORE, 3.06, 4, 0, 49, false, 0.0),
    ore_f(TIN_ORE, 5.42, 9, 12, 59, true, 0.0),
];

pub(crate) const ORE_SALT: u32 = 0x0E5E_0000;

pub(crate) fn ore_height(f: &OreFeature, h: u32) -> i32 {
    let span = f.hi - f.lo;
    if !f.tri {
        return f.lo + (((h & 0xFFFF) as i64 * (span as i64 + 1)) >> 16) as i32;
    }
    let l = (span - f.plat).max(0) / 2;
    let m = span - l;
    f.lo + (((h & 0xFFFF) as i64 * (m as i64 + 1)) >> 16) as i32
        + (((h >> 16) as i64 * (l as i64 + 1)) >> 16) as i32
}

pub(crate) const VEIN_SHAPES: usize = 512;

thread_local! {
    static VEINS: Vec<Vec<Box<[u32]>>> = ORE_SIZES.iter().map(|&s| (0..VEIN_SHAPES as u32).map(|k| vein_shape(s, hash(0x0E5E_5A9E, k, s, 0)).into_boxed_slice()).collect()).collect();
}
pub(crate) const ORE_SIZES: [i32; 6] = [17, 9, 4, 8, 10, 7];

pub(crate) fn place_ores(reg: &mut Region, seed: &Entropy<'_>, cx: i32, cz: i32, tops: &[i32]) {
    let (kx0, kz0) = (cx * CS, cz * CS);
    let (kx1, kz1) = (kx0 + CS - 1, kz0 + CS - 1);
    let mut grid = [0i32; 9];
    for lz in 0..W {
        for lx in 0..W {
            let g = &mut grid[(lz / 6 * 3 + lx / 6) as usize];
            *g = (*g).max(tops[(lz * W + lx) as usize]);
        }
    }
    let (rx0, rz0) = (reg.x0, reg.z0);
    let local_max = |x0: i32, x1: i32, z0: i32, z1: i32| -> i32 {
        let (a, b) = (
            (x0 - rx0).clamp(0, W - 1) / 6,
            (x1 - rx0).clamp(0, W - 1) / 6,
        );
        let (c, d) = (
            (z0 - rz0).clamp(0, W - 1) / 6,
            (z1 - rz0).clamp(0, W - 1) / 6,
        );
        let mut m = 0;
        for gz in c..=d {
            for gx in a..=b {
                m = m.max(grid[(gz * 3 + gx) as usize]);
            }
        }
        m
    };
    VEINS.with(|shapes| {
        for (fi, f) in ORES.iter().enumerate() {
            let si = ORE_SIZES.iter().position(|&s| s == f.size).unwrap_or(0);
            let reach =
                (f.size as f64 / 8.0 + (2.0 * f.size as f64 / 16.0 + 1.0) / 2.0).ceil() as i32;
            let n = f.count.ceil() as u32;
            let frac = f.count - f.count.floor();
            for ccz in cz - 1..=cz + 1 {
                for ccx in cx - 1..=cx + 1 {
                    for k in 0..n {
                        let id = (fi as u32) << 8 | k;
                        let h = seed.sample(ORE_SALT, id, ccx, ccz);
                        if k as f32 + 1.0 > f.count && (h >> 24) as f32 / 256.0 >= frac {
                            continue;
                        }
                        let (ox, oz) =
                            (ccx * CS + (h & 15) as i32, ccz * CS + (h >> 4 & 15) as i32);
                        if ox + reach < kx0
                            || ox - reach > kx1
                            || oz + reach < kz0
                            || oz - reach > kz1
                        {
                            continue;
                        }
                        let hy = seed.sample(ORE_SALT ^ 0x1111, id, ccx, ccz);
                        let y = ore_height(f, hy);
                        if y + reach < 1
                            || y - 2 - reach >= H
                            || y - 2 - reach
                                > local_max(ox - reach, ox + reach, oz - reach, oz + reach)
                        {
                            continue;
                        }
                        let vs = seed.sample(ORE_SALT ^ 0x2222, id, ccx, ccz);
                        for &c in shapes[si][(vs as usize) % VEIN_SHAPES].iter() {
                            let (x, z) =
                                (ox + (c & 31) as i32 - 16, oz + (c >> 5 & 31) as i32 - 16);
                            let yy = y + (c >> 10 & 31) as i32 - 16;
                            if x < kx0 || x > kx1 || z < kz0 || z > kz1 || !(1..H).contains(&yy) {
                                continue;
                            }
                            let (lx, lz) = (x - reg.x0, z - reg.z0);
                            if yy > tops[(lz * W + lx) as usize] {
                                continue;
                            }
                            let at = Region::idx(lx, yy, lz);
                            if reg.blocks[at] != STONE as u8 {
                                continue;
                            }
                            if f.discard > 0.0
                                && (hash(vs, yy as u32, x, z) >> 8) as f32 / 16777216.0 < f.discard
                                && ore_exposed(reg, lx, yy, lz)
                            {
                                continue;
                            }
                            reg.blocks[at] = f.block as u8;
                        }
                    }
                }
            }
        }
    });
}

pub(crate) fn vein_shape(size: i32, vs: u32) -> Vec<u32> {
    let mut r = Rng(vs);
    let ang = r.next_f32() * std::f32::consts::PI;
    let g = size as f32 / 8.0;
    let (sn, cs) = (
        libm::sinf(ang) as f64 * g as f64,
        libm::cosf(ang) as f64 * g as f64,
    );
    let y0 = ((r.next_f32() * 3.0) as i32 - 2) as f64;
    let y1 = ((r.next_f32() * 3.0) as i32 - 2) as f64;
    let mut out = Vec::with_capacity(64);
    for k in 0..size {
        let t = k as f64 / size as f64;
        let hh = r.next_f32() as f64 * size as f64 / 16.0;
        let sin = libm::sinf(std::f32::consts::PI * (k as f32 / size as f32)) as f64;
        let d = ((sin + 1.0) * hh + 1.0) / 2.0;
        let (cx, cy, cz) = (sn - t * 2.0 * sn, y0 + t * (y1 - y0), cs - t * 2.0 * cs);
        let d2 = d * d;
        for x in ifloor(cx - d)..=ifloor(cx + d) {
            let dx = x as f64 + 0.5 - cx;
            for z in ifloor(cz - d)..=ifloor(cz + d) {
                let dz = z as f64 + 0.5 - cz;
                for y in ifloor(cy - d)..=ifloor(cy + d) {
                    let dy = y as f64 + 0.5 - cy;
                    if dx * dx + dy * dy + dz * dz < d2 {
                        out.push(
                            (x + 16) as u32 | ((z + 16) as u32) << 5 | ((y + 16) as u32) << 10,
                        );
                    }
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[inline]
pub(crate) fn ifloor(x: f64) -> i32 {
    let i = x as i32;
    if (i as f64) > x { i - 1 } else { i }
}

pub(crate) const LAKE_SALT: u32 = 0x1A4E_0000;

pub(crate) fn place_lava_lake(
    reg: &mut Region,
    seed: &Entropy<'_>,
    cx: i32,
    cz: i32,
    tops: &[i32],
) {
    let h = seed.sample(LAKE_SALT, 0, cx, cz);
    if !h.is_multiple_of(9) {
        return;
    }
    let mut r = Rng(seed.sample(LAKE_SALT, 1, cx, cz));
    let y0 = 6 + (r.next_f32() * 34.0) as i32;
    let mut top = i32::MAX;
    for lz in 1..15 {
        for lx in 1..15 {
            top = top.min(tops[((lz + PAD) * W + lx + PAD) as usize]);
        }
    }
    if y0 + 8 + 4 > top {
        return;
    }
    let mut mark = [false; 14 * 14 * 8];
    let n = 4 + (r.next_f32() * 4.0) as i32;
    for _ in 0..n {
        let d = r.next_f32() as f64 * 5.0 + 3.0;
        let e = r.next_f32() as f64 * 4.0 + 2.0;
        let f = r.next_f32() as f64 * 5.0 + 3.0;
        let g = r.next_f32() as f64 * (14.0 - d - 2.0) + 1.0 + d / 2.0;
        let hh = r.next_f32() as f64 * (8.0 - e - 4.0) + 2.0 + e / 2.0;
        let k = r.next_f32() as f64 * (14.0 - f - 2.0) + 1.0 + f / 2.0;
        for x in 1..13 {
            for z in 1..13 {
                for y in 1..7 {
                    let (a, b, c) = (
                        (x as f64 - g) / (d / 2.0),
                        (y as f64 - hh) / (e / 2.0),
                        (z as f64 - k) / (f / 2.0),
                    );
                    if a * a + b * b + c * c < 1.0 {
                        mark[((x * 14 + z) * 8 + y) as usize] = true;
                    }
                }
            }
        }
    }
    for x in 0..14 {
        for z in 0..14 {
            for y in 0..8 {
                if mark[((x * 14 + z) * 8 + y) as usize] {
                    let b = if y < 4 { LAVA } else { AIR };
                    reg.blocks[Region::idx(x + 1 + PAD, y0 + y, z + 1 + PAD)] = b as u8;
                }
            }
        }
    }
}

pub(crate) fn ore_exposed(reg: &Region, lx: i32, y: i32, lz: i32) -> bool {
    [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ]
    .iter()
    .any(|&(dx, dy, dz)| {
        let (x, yy, z) = (lx + dx, y + dy, lz + dz);
        x >= 0
            && z >= 0
            && x < W
            && z < W
            && (0..H).contains(&yy)
            && reg.blocks[Region::idx(x, yy, z)] == AIR as u8
    })
}
