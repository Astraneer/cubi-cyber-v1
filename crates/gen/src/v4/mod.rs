//! Pure version-4 numerical kernels and procedural structures from the approved donor.
//! Source: donor/crates/voxel-core/src/generator at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! World storage, async ownership, publication and client secrecy are implemented separately.

pub(crate) mod noise;
mod region;
pub(crate) mod spawn;

use self::cell::{BlockId, CS, H, SEA};
use noise::{Noise, Rng, hash, lerp, sm};
use rules::blocks::is_grass;
use rules::ids::*;
mod cell;
pub(crate) use region::{PAD, Region, W};

pub(crate) const GEN_VERSION: u32 = 4;

pub(crate) const PROBE_SEED: u32 = 1337;
pub(crate) const PROBE_CHUNKS: [(i32, i32); 4] = [(0, 0), (7, -3), (-21, 12), (55, 38)];

pub(crate) fn region_hash(r: &Region) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |b: u8| h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    r.blocks.iter().for_each(|&b| eat(b));
    r.tint.iter().for_each(|&t| {
        eat(t as u8);
        eat((t >> 8) as u8);
    });
    h
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Biome {
    Ocean,
    Beach,
    River,
    Plains,
    Forest,
    Birch,
    Taiga,
    Snowy,
    Savanna,
    Desert,
    Mountain,
    Peaks,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Var {
    None,
    Meadow,    // цветочный луг (равнины)
    Sparse,    // редколесье: поляны, крупные деревья поодиночке
    OldGrowth, // старый лес: крупные деревья
    Swamp,     // болотистая низина: заводи, тёмная трава
    Stony,     // каменистый берег
}

pub(crate) struct Column {
    pub(crate) h: i32,
    pub(crate) biome: Biome,
    pub(crate) var: Var,
    pub(crate) det: f32,
    pub(crate) tt: f32,    // температура с поправкой на высоту
    pub(crate) hum: f32,   // влажность
    pub(crate) vary: f32,  // шум разнообразия (σ ≈ 0.37): поляны, опушки, луга
    pub(crate) swamp: f32, // 0…1 — болотистость (тёмная трава плавно)
}

pub(crate) struct Gen {
    pub(crate) seed: u32,
    pub(crate) cont: Noise,
    pub(crate) ero: Noise,
    pub(crate) ridge: Noise,
    pub(crate) hill: Noise,
    pub(crate) temp: Noise,
    pub(crate) hum: Noise,
    pub(crate) river: Noise,
    pub(crate) warp: Noise,
    pub(crate) det: Noise,
    pub(crate) vary: Noise,
    pub(crate) roff: (f32, f32), // сдвиг поля рек и озёр: подобран по сиду так, чтобы у точки появления их не было
    pub(crate) site: spawn::SpawnSite,
}

pub(crate) const SPLINE: [(f32, f32); 12] = [
    (-1.2, -28.0),
    (-0.55, -24.0),
    (-0.35, -17.0),
    (-0.2, -9.0),
    (-0.13, -4.0),
    (-0.1, -1.5),
    (-0.07, 1.5),
    (0.0, 4.5),
    (0.15, 8.0),
    (0.4, 11.0),
    (0.8, 14.0),
    (1.2, 16.0),
];

pub(crate) fn spline(x: f32) -> f32 {
    if x <= SPLINE[0].0 {
        return SPLINE[0].1;
    }
    for i in 1..SPLINE.len() {
        if x <= SPLINE[i].0 {
            let (x0, y0) = SPLINE[i - 1];
            let (x1, y1) = SPLINE[i];
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    SPLINE[SPLINE.len() - 1].1
}

pub(crate) fn terrace(v: f32, step: f32, sharp: f32) -> f32 {
    let k = v / step;
    let fl = k.floor();
    let t = ((k - fl - (1.0 - sharp)) / sharp).clamp(0.0, 1.0);
    (fl + t * t * (3.0 - 2.0 * t)) * step
}

impl Gen {
    pub(crate) fn new(seed: u32) -> Self {
        let mut g = Gen {
            seed,
            cont: Noise::new(seed ^ 0x11),
            ero: Noise::new(seed ^ 0x22),
            ridge: Noise::new(seed ^ 0x33),
            hill: Noise::new(seed ^ 0x44),
            temp: Noise::new(seed ^ 0x55),
            hum: Noise::new(seed ^ 0x66),
            river: Noise::new(seed ^ 0x77),
            warp: Noise::new(seed ^ 0x88),
            det: Noise::new(seed ^ 0x99),
            vary: Noise::new(seed ^ 0xAA),
            roff: (0.0, 0.0),
            site: spawn::SpawnSite::placeholder(),
        };
        for k in 0..48 {
            g.roff = (k as f32 * 0.37, k as f32 * -0.23);
            let dry = [
                (0, 0),
                (64, 0),
                (-64, 0),
                (0, 64),
                (0, -64),
                (45, 45),
                (-45, -45),
                (45, -45),
                (-45, 45),
            ]
            .iter()
            .all(|&(x, z)| {
                let (wx, wz) = g.warp(x as f32, z as f32);
                g.river_t(wx, wz) > 0.035 && g.lake_n(wx, wz) < 0.45
            });
            if dry {
                break;
            }
        }
        g.site = spawn::SpawnSite::find(&g);
        g
    }

    #[inline]
    pub(crate) fn warp(&self, fx: f32, fz: f32) -> (f32, f32) {
        (
            fx + 70.0 * self.warp.perlin(fx / 280.0, fz / 280.0),
            fz + 70.0 * self.warp.perlin(fx / 280.0 + 50.0, fz / 280.0),
        )
    }
    #[inline]
    pub(crate) fn river_t(&self, wx: f32, wz: f32) -> f32 {
        self.river
            .fbm(wx / 1150.0 + self.roff.0, wz / 1150.0 + self.roff.1, 2)
            .abs()
    }
    #[inline]
    pub(crate) fn lake_n(&self, wx: f32, wz: f32) -> f32 {
        let (x, z) = (
            wx / 420.0 - 400.0 + self.roff.1,
            wz / 420.0 + 170.0 + self.roff.0,
        );
        let p1 = self.vary.perlin(x, z);
        if p1 < -0.07 {
            return (p1 + 0.35) / 1.5 * 2.7;
        }
        let p2 = self.vary.perlin(x * 2.03 + 17.13, z * 2.03 - 9.71);
        (p1 + 0.5 * p2) / 1.5 * 2.7
    }

    #[inline]
    pub(crate) fn column(&self, x: i32, z: i32) -> Column {
        let mut c = self.column_raw(x, z);
        self.site.shape(x, z, &mut c);
        c
    }

    pub(crate) fn column_raw(&self, x: i32, z: i32) -> Column {
        let (fx, fz) = (x as f32, z as f32);
        let s = SEA as f32;
        let d0 = (fx * fx + fz * fz).sqrt();
        let center = sm(1.0 - d0 / 1600.0);
        let near0 = sm(1.0 - d0 / 400.0);
        let c = self.cont.fbm(fx / 2400.0, fz / 2400.0, 5) * 3.2 + 0.02 + 0.45 * center;
        let e = self.ero.fbm(fx / 1100.0, fz / 1100.0, 2) * 2.6;
        let temp = self.temp.fbm(fx / 1700.0, fz / 1700.0, 2) * 2.6 * (1.0 - 0.5 * center);
        let hum = self.hum.fbm(fx / 1500.0, fz / 1500.0, 2) * 2.6;
        let det = self.det.perlin(fx / 23.0, fz / 23.0) + 0.5 * self.det.perlin(fx / 9.0, fz / 9.0);

        let mut h = s + spline(c);
        if h < s - 5.0 {
            h = s - 5.0 + terrace(h - s + 5.0, 4.0, 0.35);
        }
        let (mut vary, mut mtn, mut cliffy, mut swamp, mut river_k) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let mut lake = false;
        if c > -0.2 {
            vary = self.vary.perlin(fx / 260.0, fz / 260.0) * 2.0;
            cliffy = sm((-0.05 - e) / 0.3);
            let land_k = sm((c + 0.1) / lerp(0.14, 0.015, cliffy));
            let inland = sm((c + 0.05) / 0.4);
            let hilly = sm((0.4 - e) / 0.75);
            let hn = self.hill.fbm(fx / 210.0, fz / 210.0, 2) * 2.6;
            let rel = (2.0 + 22.0 * hilly * hilly) * sm(hn * 0.5 + 0.5);
            let chain = if e < 0.3 {
                sm(1.0
                    - self
                        .ridge
                        .perlin(fx / 2200.0 + 77.0, fz / 2200.0 - 41.0)
                        .abs()
                        / 0.15)
            } else {
                0.0
            };
            mtn = (sm((chain - 0.15) / 0.5) * sm((0.3 - e) / 0.45)).max(sm((-0.4 - e) / 0.3))
                * land_k
                * (0.45 + 0.55 * inland)
                * (1.0 - 0.85 * near0);
            let mut mh = 0.0;
            if mtn > 0.01 {
                let n1 = self.ridge.perlin(fx / 420.0, fz / 420.0);
                let n2 = self.ridge.perlin(fx / 170.0 + 31.0, fz / 170.0 - 17.0);
                let r1 = (1.0 - n1.abs() * 2.4).max(0.0);
                let r2 = (1.0 - n2.abs() * 2.4).max(0.0);
                let peaks = r1 * r1 * (0.6 + 0.4 * r2 * r2);
                mh = mtn * (14.0 + 66.0 * peaks * (0.55 + 0.45 * chain) + 3.0 * det);
            }
            let mut v = (rel * (1.0 - 0.6 * mtn) + mh) * land_k;
            if v > 2.0 {
                let pl = self.hill.perlin(fx / 640.0 + 91.0, fz / 640.0 - 33.0) * 2.0;
                let warm = sm((temp - 0.05) / 0.25);
                let tk = sm((pl - 0.05) / 0.3)
                    * (0.3 + 0.7 * warm)
                    * (0.25 + 0.75 * hilly)
                    * (1.0 - 0.5 * mtn);
                if tk > 0.0 {
                    v = lerp(v, terrace(v + det * 1.5, 6.5 + 2.0 * pl, 0.3), tk);
                }
            }
            h += v;
            let desert = sm((temp - 0.35) / 0.1) * sm((-0.1 - hum) / 0.1) * land_k * (1.0 - mtn);
            if desert > 0.0 {
                h += desert * (1.0 - self.det.perlin(fx / 34.0, fz / 46.0).abs()).powi(2) * 5.0;
            }
            swamp = sm((e - 0.12) / 0.2)
                * sm((hum - 0.05) / 0.15)
                * sm((temp + 0.1) / 0.2)
                * sm((0.35 - c) / 0.2)
                * land_k
                * (1.0 - mtn);
            if swamp > 0.0 {
                h = lerp(h, s + 0.4 + det * 2.2, swamp);
            }
            let (wx, wz) = self.warp(fx, fz);
            if mtn < 0.3 {
                let ln = self.lake_n(wx, wz) + det * 0.03;
                let basin = sm((ln - 0.55) / 0.3) * land_k * (1.0 - mtn / 0.3);
                if basin > 0.0 {
                    h = lerp(h, s + 1.0, basin);
                    let core = sm((ln - 0.88) / 0.08);
                    if core > 0.0 {
                        h = lerp(h, s - 1.0 - 5.0 * core, core.min(0.999));
                        lake = h < s;
                    }
                }
            }
            if land_k > 0.0 {
                let t = self.river_t(wx, wz);
                let core = 0.0045 * (1.0 + 0.8 * (1.0 - inland));
                let valley = core * 3.5 + 0.012 * mtn;
                if t < valley {
                    river_k = sm((valley - t) / (valley - core));
                    let bank = s + 1.5 + (h - s - 1.5).max(0.0) * 0.1 * (1.0 - river_k);
                    h = lerp(h, bank, river_k * land_k);
                    let core_k = sm((core - t) / core * 2.0) * land_k;
                    h = lerp(h, s - 4.0 + 4.0 * (t / core), core_k);
                    let mid = core * 0.75;
                    if t < mid && land_k > 0.3 {
                        h = h.min(s - 1.0 - 3.0 * (1.0 - t / mid));
                    }
                }
            }
        }
        if h > 108.0 {
            h = 108.0 + (h - 108.0) * 0.5;
        }
        let hi = (h.clamp(4.0, (H - 3) as f32)) as i32;
        let tt = temp - (hi - SEA - 12).max(0) as f32 / 75.0;
        let (tj, hj) = (tt + det * 0.035, hum - det * 0.035);
        let mut var = Var::None;
        let biome = if hi < SEA {
            if lake || river_k > 0.7 {
                Biome::River
            } else if swamp > 0.5 {
                var = Var::Swamp;
                Biome::Forest
            } else {
                Biome::Ocean
            }
        } else if hi <= SEA + 2 && c + det * 0.03 < -0.04 && mtn < 0.3 && swamp < 0.5 {
            if cliffy > 0.5 {
                var = Var::Stony;
            }
            Biome::Beach
        } else if hi > SEA + 48 + (det * 5.0) as i32 - (temp * 14.0) as i32 {
            Biome::Peaks
        } else if mtn > 0.4 && hi > SEA + 26 {
            Biome::Mountain
        } else if tj > 0.38 && hj < -0.12 {
            Biome::Desert
        } else if tj > 0.22 && hj < 0.12 {
            Biome::Savanna
        } else if tj < -0.5 {
            Biome::Snowy
        } else if tj < -0.2 {
            if vary > 0.35 {
                var = Var::OldGrowth;
            } else if vary < -0.3 {
                var = Var::Sparse;
            }
            Biome::Taiga
        } else if swamp > 0.5 {
            var = Var::Swamp;
            Biome::Forest
        } else if tj < 0.02 && hj > -0.05 {
            if vary > 0.4 {
                var = Var::OldGrowth;
            }
            Biome::Birch
        } else if hj > 0.02 {
            if vary > 0.35 {
                var = Var::OldGrowth;
            } else if vary < -0.25 {
                var = Var::Sparse;
            }
            Biome::Forest
        } else {
            if vary > 0.4 {
                var = Var::Meadow;
            }
            Biome::Plains
        };
        Column {
            h: hi,
            biome,
            var,
            det,
            tt,
            hum,
            vary,
            swamp,
        }
    }
}

#[allow(dead_code)]
pub(crate) fn biome_name(c: &Column) -> &'static str {
    match (c.biome, c.var) {
        (Biome::Plains, Var::Meadow) => "Flower meadow",
        (Biome::Forest, Var::Swamp) => "Swamp",
        (Biome::Forest, Var::Sparse) => "Sparse woodland",
        (Biome::Forest, Var::OldGrowth) => "Old growth forest",
        (Biome::Birch, Var::OldGrowth) => "Old growth birch forest",
        (Biome::Taiga, Var::OldGrowth) => "Old growth taiga",
        (Biome::Taiga, Var::Sparse) => "Sparse taiga",
        (Biome::Beach, Var::Stony) => "Stony shore",
        (Biome::Ocean, _) => "Ocean",
        (Biome::Beach, _) => "Beach",
        (Biome::River, _) => "River",
        (Biome::Plains, _) => "Plains",
        (Biome::Forest, _) => "Forest",
        (Biome::Birch, _) => "Birch forest",
        (Biome::Taiga, _) => "Taiga",
        (Biome::Snowy, _) => "Snowy plains",
        (Biome::Savanna, _) => "Savanna",
        (Biome::Desert, _) => "Desert",
        (Biome::Mountain, _) => "Mountains",
        (Biome::Peaks, _) => "Snowy peaks",
    }
}

pub(crate) const T_PLAINS: [f32; 3] = [0.4516, 0.6667, 0.2581];
pub(crate) const T_FOREST: [f32; 3] = [0.3548, 0.5873, 0.2258];
pub(crate) const T_BIRCH: [f32; 3] = [0.4516, 0.6667, 0.2903];
pub(crate) const T_TAIGA: [f32; 3] = [0.3548, 0.5397, 0.3226];
pub(crate) const T_SNOWY: [f32; 3] = [0.4516, 0.6190, 0.4839];
pub(crate) const T_SAVANNA: [f32; 3] = [0.7419, 0.6825, 0.3226];
pub(crate) const T_MOUNTAIN: [f32; 3] = [0.4194, 0.5714, 0.3871];
pub(crate) const FRINGE_TINTS: [[f32; 3]; 6] = [
    [0.4516, 0.6667, 0.2581],
    [0.3548, 0.5873, 0.2258],
    [0.3548, 0.5397, 0.3226],
    [0.7419, 0.6825, 0.3226],
    [0.5806, 0.6667, 0.2903],
    [0.4194, 0.5714, 0.3871],
];

pub(crate) fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

pub(crate) fn grass_tint(c: &Column) -> [f32; 3] {
    let (tt, hum) = (c.tt, c.hum);
    let lush = sm((hum + 0.22) / 0.3);
    let mut t = mix3(T_PLAINS, T_FOREST, lush);
    let birch = sm((tt + 0.38) / 0.1) * sm((0.02 - tt) / 0.1) * sm((hum + 0.18) / 0.12);
    t = mix3(t, T_BIRCH, birch * 0.8);
    t = mix3(t, T_TAIGA, sm((-0.2 - tt) / 0.16));
    t = mix3(t, T_SNOWY, sm((-0.56 - tt) / 0.14));
    t = mix3(
        t,
        T_SAVANNA,
        sm((tt - 0.15) / 0.16) * sm((0.3 - hum) / 0.16),
    );
    t = mix3(t, T_MOUNTAIN, sm((c.h - SEA - 22) as f32 / 18.0));
    let sw = c.swamp * 0.85;
    t = mix3(
        t,
        [T_TAIGA[0] * 0.9, T_FOREST[1] * 0.82, T_TAIGA[2] * 0.62],
        sw,
    );
    let k = 1.0 + 0.035 * c.det;
    [t[0] * k, t[1] * k, t[2] * k]
}

pub(crate) fn rgb565(t: [f32; 3]) -> u16 {
    let r = (t[0].clamp(0.0, 1.0) * 31.0 + 0.5) as u16;
    let g = (t[1].clamp(0.0, 1.0) * 63.0 + 0.5) as u16;
    let b = (t[2].clamp(0.0, 1.0) * 31.0 + 0.5) as u16;
    (r << 11) | (g << 5) | b
}

pub(crate) fn fringe_variant(t: [f32; 3]) -> u8 {
    let mut best = (f32::MAX, 0u8);
    for (i, f) in FRINGE_TINTS.iter().enumerate() {
        let d = (t[0] - f[0]).powi(2) + (t[1] - f[1]).powi(2) + (t[2] - f[2]).powi(2);
        if d < best.0 {
            best = (d, i as u8);
        }
    }
    best.1
}

pub(crate) fn tree_density(c: &Column) -> f32 {
    let open = sm((c.vary + 0.55) / 0.4); // 0 — поляна, 1 — лес
    match c.biome {
        Biome::Forest if c.var == Var::Swamp => 0.2,
        Biome::Forest | Biome::Plains => {
            let wood = 0.02 + 0.53 * sm((c.hum + 0.2) / 0.22);
            let meadow = if c.biome == Biome::Plains {
                sm((c.vary - 0.28) / 0.15)
            } else {
                0.0
            };
            wood * (0.2 + 0.8 * open) * (1.0 - 0.97 * meadow)
        }
        Biome::Birch => 0.06 + 0.44 * open,
        Biome::Taiga => 0.05 + 0.5 * open,
        Biome::Snowy => 0.1,
        Biome::Savanna => 0.06,
        Biome::Mountain => 0.07,
        Biome::Desert => 0.1,
        _ => 0.0,
    }
}

pub(crate) const TREE_REACH: i32 = 6;
pub(crate) const TREE_MAX: f32 = 0.72;

pub(crate) fn base(generator: &Gen, cx: i32, cz: i32) -> (Region, Vec<Column>) {
    let x0 = cx * CS - PAD;
    let z0 = cz * CS - PAD;
    let mut reg = Region {
        x0,
        z0,
        blocks: crate::DraftBlocks::new((W * W * H) as usize),
        tint: vec![0xFFFF; (W * W) as usize],
    };
    let cw = W + 2;
    let mut cols: Vec<Column> = Vec::with_capacity((cw * cw) as usize);
    for lz in -1..W + 1 {
        for lx in -1..W + 1 {
            cols.push(generator.column(x0 + lx, z0 + lz));
        }
    }
    let col = |lx: i32, lz: i32| -> &Column { &cols[((lz + 1) * cw + lx + 1) as usize] };

    for lz in 0..W {
        for lx in 0..W {
            let c = col(lx, lz);
            let h = c.h;
            let low = [
                col(lx - 1, lz).h,
                col(lx + 1, lz).h,
                col(lx, lz - 1).h,
                col(lx, lz + 1).h,
            ]
            .iter()
            .fold(h, |a, &b| a.min(b));
            let drop = h - low;
            let (top, fill, depth) = match c.biome {
                Biome::Ocean => (
                    if SEA - h > 9 {
                        GRAVEL
                    } else if c.det > 0.35 {
                        CLAY
                    } else {
                        SAND
                    },
                    SAND,
                    3,
                ),
                Biome::River => (
                    if c.det > 0.2 {
                        GRAVEL
                    } else if c.det < -0.35 {
                        CLAY
                    } else {
                        SAND
                    },
                    SAND,
                    2,
                ),
                Biome::Beach if c.var == Var::Stony => {
                    (if c.det > 0.15 { GRAVEL } else { STONE }, STONE, 1)
                }
                Biome::Beach => (SAND, SAND, 4),
                Biome::Desert => (if drop >= 3 { SANDSTONE } else { SAND }, SANDSTONE, 5),
                Biome::Peaks => (if drop >= 5 { STONE } else { SNOW }, STONE, 2),
                Biome::Mountain => {
                    if drop >= 3 {
                        (STONE, STONE, 0)
                    } else if c.det > 0.35 {
                        (GRAVEL, STONE, 1)
                    } else {
                        (GRASS, DIRT, 2)
                    }
                }
                Biome::Savanna | Biome::Taiga => (GRASS, DIRT, 3),
                Biome::Snowy => (SNOWY_GRASS, DIRT, 3),
                _ => (GRASS, DIRT, 3),
            };
            let t = grass_tint(c);
            reg.tint[(lz * W + lx) as usize] = rgb565(t);
            let mut top = top;
            if top == GRASS {
                if h <= SEA + 1 && low <= SEA && c.det > -0.15 && c.swamp < 0.5 {
                    top = SAND;
                } else {
                    top = if drop == 1 {
                        GRASS_FULL
                    } else {
                        GRASS_EDGE + fringe_variant(t) as BlockId
                    };
                }
            }
            let base = Region::idx(lx, 0, lz);
            reg.blocks[base] = BEDROCK as u8;
            for y in 1..=h {
                let d = h - y;
                let b = if d == 0 {
                    if y <= SEA && (is_grass(top) || top == SNOWY_GRASS) {
                        if c.swamp >= 0.5 {
                            if c.det > 0.3 { CLAY } else { DIRT }
                        } else if c.det > 0.0 {
                            SAND
                        } else {
                            DIRT
                        }
                    } else {
                        top
                    }
                } else if d <= depth {
                    if drop >= 4 && y > low {
                        STONE
                    } else if top == SAND && c.biome == Biome::Desert && d > 3 {
                        SANDSTONE
                    } else {
                        fill
                    }
                } else {
                    STONE
                };
                reg.blocks[base + y as usize] = b as u8;
            }
            for y in h + 1..=SEA {
                reg.blocks[base + y as usize] = WATER as u8;
            }
        }
    }

    (reg, cols)
}

pub(crate) fn decorate(generator: &Gen, mut reg: Region, cols: Vec<Column>) -> Region {
    let (x0, z0) = (reg.x0, reg.z0);
    let cw = W + 2;
    let col = |lx: i32, lz: i32| -> &Column { &cols[((lz + 1) * cw + lx + 1) as usize] };
    let seed = generator.seed;
    let reach = TREE_REACH;
    let tg = 5;
    for gz in (z0 - reach).div_euclid(tg)..=(z0 + W + reach).div_euclid(tg) {
        for gx in (x0 - reach).div_euclid(tg)..=(x0 + W + reach).div_euclid(tg) {
            let mut r = Rng(hash(seed, 1, gx, gz));
            let roll = r.next_f32();
            let tx = gx * tg + (r.next_f32() * tg as f32) as i32;
            let tz = gz * tg + (r.next_f32() * tg as f32) as i32;
            if roll >= TREE_MAX
                || tx < x0 - reach
                || tx > x0 + W + reach
                || tz < z0 - reach
                || tz > z0 + W + reach
            {
                continue;
            }
            let (clx, clz) = (tx - x0, tz - z0);
            let own;
            let c = if clx >= -1 && clz >= -1 && clx <= W && clz <= W {
                col(clx, clz)
            } else {
                own = generator.column(tx, tz);
                &own
            };
            let dens = tree_density(c) * (0.6 + c.det * 1.2).clamp(0.15, 1.3);
            if roll >= dens || c.h <= SEA || c.h + 24 >= H || generator.site.no_trees(tx, tz) {
                continue;
            }
            if c.biome == Biome::Mountain {
                let hat = |x: i32, z: i32| {
                    let (lx, lz) = (x - x0, z - z0);
                    if lx >= -1 && lz >= -1 && lx <= W && lz <= W {
                        col(lx, lz).h
                    } else {
                        generator.column(x, z).h
                    }
                };
                let low = hat(tx + 1, tz)
                    .min(hat(tx - 1, tz))
                    .min(hat(tx, tz + 1))
                    .min(hat(tx, tz - 1));
                if c.h - low >= 3 {
                    continue;
                }
            }
            let h = c.h;
            let size = r.next_f32();
            let kind = r.next_f32();
            match (c.biome, c.var) {
                (Biome::Desert, _) => {
                    for y in 1..=1 + (size * 3.0) as i32 {
                        reg.put(tx, h + y, tz, CACTUS, 0);
                    }
                }
                (Biome::Taiga, Var::OldGrowth) if kind < 0.45 => {
                    mega_spruce(&mut reg, tx, h, tz, 14 + (size * 7.0) as i32)
                }
                (Biome::Taiga | Biome::Snowy | Biome::Mountain, _) => {
                    spruce(&mut reg, tx, h, tz, 6 + (size * 6.0) as i32)
                }
                (Biome::Birch, Var::OldGrowth) => {
                    birch(&mut reg, tx, h, tz, 8 + (size * 4.0) as i32, &mut r)
                }
                (Biome::Birch, _) => birch(&mut reg, tx, h, tz, 5 + (size * 3.0) as i32, &mut r),
                (Biome::Forest, Var::Swamp) => {
                    swamp_oak(&mut reg, tx, h, tz, 4 + (size * 3.0) as i32, &mut r)
                }
                (Biome::Savanna, _) => {
                    if kind < 0.45 {
                        bush(&mut reg, tx, h, tz, &mut r)
                    } else {
                        savanna_oak(&mut reg, tx, h, tz, 4 + (size * 3.0) as i32, &mut r)
                    }
                }
                _ => {
                    let big = match c.var {
                        Var::OldGrowth => 0.5,
                        Var::Sparse => 0.3,
                        Var::Meadow => 0.4,
                        _ if c.biome == Biome::Plains => 0.22,
                        _ => 0.07,
                    };
                    let shrub = if c.biome == Biome::Plains || c.var == Var::Sparse {
                        0.25
                    } else {
                        0.06
                    };
                    if kind < big {
                        big_oak(&mut reg, tx, h, tz, 7 + (size * 5.0) as i32, &mut r)
                    } else if kind < big + shrub {
                        bush(&mut reg, tx, h, tz, &mut r)
                    } else if c.biome == Biome::Forest && kind > 0.75 {
                        birch(&mut reg, tx, h, tz, 5 + (size * 3.0) as i32, &mut r)
                    } else {
                        oak(&mut reg, tx, h, tz, 4 + (size * size * 3.0) as i32, &mut r)
                    }
                }
            }
        }
    }

    for lz in 0..W {
        for lx in 0..W {
            let (x, z) = (x0 + lx, z0 + lz);
            let c = col(lx, lz);
            let h = c.h;
            if h <= SEA || h + 1 >= H {
                continue;
            }
            let below = reg.get(lx, h, lz);
            if reg.get(lx, h + 1, lz) != AIR {
                continue;
            }
            let roll = hash(seed, 2, x, z) as f32 / 4294967296.0;
            let patch = sm(0.45 + c.det * 1.1);
            let j = (c.det * 6.0) as i32;
            let yellow = hash(seed, 3, (x + j).div_euclid(7), (z - j).div_euclid(7)) & 1 == 0;
            let flower = if yellow { DANDELION } else { POPPY };
            let plant = match c.biome {
                Biome::Desert if below == SAND && roll < 0.008 => DEAD_BUSH,
                _ if !is_grass(below) => AIR,
                Biome::Plains => {
                    let m = sm((c.vary - 0.25) / 0.2);
                    let fl = 0.008 + m * 0.34 * sm((c.det + 0.3) / 0.5);
                    if roll < fl {
                        flower
                    } else if roll < fl + 0.02 + 0.2 * patch * (1.0 - 0.5 * m) {
                        TALL_GRASS
                    } else {
                        AIR
                    }
                }
                Biome::Forest if c.var == Var::Swamp => {
                    if roll < 0.06 + 0.3 * patch {
                        if c.det > 0.25 { FERN } else { TALL_GRASS }
                    } else {
                        AIR
                    }
                }
                Biome::Forest | Biome::Birch => {
                    let glade = 1.0 - sm((c.vary + 0.55) / 0.4);
                    if roll < 0.02 + 0.12 * patch + 0.1 * glade {
                        TALL_GRASS
                    } else if roll > 0.99 - 0.06 * glade {
                        flower
                    } else {
                        AIR
                    }
                }
                Biome::Taiga => {
                    let k = if c.var == Var::OldGrowth { 0.25 } else { 0.14 };
                    if roll < 0.02 + k * patch {
                        FERN
                    } else if roll > 0.985 {
                        TALL_GRASS
                    } else {
                        AIR
                    }
                }
                Biome::Savanna if roll < 0.03 + 0.24 * patch => TALL_GRASS,
                Biome::Mountain if roll < 0.05 * patch => TALL_GRASS,
                _ => AIR,
            };
            if plant != AIR {
                reg.blocks[Region::idx(lx, h + 1, lz)] = plant as u8;
            }
        }
    }
    if generator.site.touches(&reg) {
        generator.site.stamp(seed, &mut reg);
    }
    reg
}

pub(crate) fn trunk(reg: &mut Region, x: i32, y0: i32, y1: i32, z: i32, b: BlockId) {
    for y in y0..=y1 {
        reg.put(x, y, z, b, 1);
    }
}

pub(crate) fn oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, LOG);
    for dy in t - 2..=t + 1 {
        let rad: i32 = if dy < t { 2 } else { 1 };
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                let corner = dx.abs() == rad && dz.abs() == rad;
                if corner && (dy == t + 1 || r.next_f32() < 0.5) {
                    continue;
                }
                reg.put(x + dx, h + dy, z + dz, LEAVES, 0);
            }
        }
    }
}

pub(crate) fn blob(reg: &mut Region, x: i32, y: i32, z: i32, rad: f32, leaf: BlockId, r: &mut Rng) {
    let salt = (r.next_f32() * 16777216.0) as u32;
    let ri = rad.ceil() as i32;
    let r2 = rad * rad;
    let (ax, bx, az, bz) = reg.span(x, z, -ri, ri);
    for dy in -ri + 1..=ri - 1 {
        for dx in ax..=bx {
            for dz in az..=bz {
                let d = (dx * dx + dz * dz) as f32 + (dy * dy) as f32 * 2.2;
                if d <= r2 - 0.6 || (d <= r2 + 0.6 && hash(salt, dy as u32, dx, dz) & 1 == 0) {
                    reg.put(x + dx, y + dy, z + dz, leaf, 0);
                }
            }
        }
    }
}

pub(crate) fn big_oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 7) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, LOG);
    blob(reg, x, h + t + 1, z, 3.2, LEAVES, r);
    let n = 2 + (r.next_f32() * 2.0) as i32;
    for i in 0..n {
        let a = (i as f32 + r.next_f32() * 0.6) * std::f32::consts::TAU / n as f32;
        let (dx, dz) = (libm::cosf(a), libm::sinf(a));
        let y0 = h + t - 3 + (r.next_f32() * 2.0) as i32;
        let len = 2 + (r.next_f32() * 2.0) as i32;
        let (mut bx, mut bz) = (x, z);
        for k in 1..=len {
            bx = x + (dx * k as f32).round() as i32;
            bz = z + (dz * k as f32).round() as i32;
            reg.put(bx, y0 + k, bz, LOG, 1);
        }
        blob(
            reg,
            bx,
            y0 + len + 1,
            bz,
            2.4 + r.next_f32() * 0.6,
            LEAVES,
            r,
        );
    }
}

pub(crate) fn bush(reg: &mut Region, x: i32, h: i32, z: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    reg.put(x, h + 1, z, LOG, 1);
    let rad: i32 = if r.next_f32() < 0.5 { 1 } else { 2 };
    for dy in 1..=2 {
        let rr = if dy == 2 { rad - 1 } else { rad };
        for dx in -rr..=rr {
            for dz in -rr..=rr {
                if rr > 0 && dx.abs() == rr && dz.abs() == rr && r.next_f32() < 0.7 {
                    continue;
                }
                reg.put(x + dx, h + dy, z + dz, LEAVES, 0);
            }
        }
    }
}

pub(crate) fn savanna_oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    let (sx, sz) = if r.next_f32() < 0.5 { (1, 0) } else { (0, 1) };
    let s = if r.next_f32() < 0.5 { 1 } else { -1 };
    let bend = t - 2;
    trunk(reg, x, h + 1, h + bend, z, LOG);
    let (tx, tz) = (x + sx * s, z + sz * s);
    trunk(reg, tx, h + bend + 1, h + t, tz, LOG);
    for (dy, rad) in [(1i32, 3i32), (2, 2)] {
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                let corner = dx.abs() == rad && dz.abs() == rad;
                if corner || (dx.abs() + dz.abs() == 2 * rad - 1 && r.next_f32() < 0.4) {
                    continue;
                }
                reg.put(tx + dx, h + t + dy - 1, tz + dz, LEAVES, 0);
            }
        }
    }
}

pub(crate) fn swamp_oak(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, LOG);
    for (dy, rad) in [(-1i32, 3i32), (0, 3), (1, 2)] {
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                let corner = dx.abs() == rad && dz.abs() == rad;
                if corner && (dy != 0 || r.next_f32() < 0.6) {
                    continue;
                }
                reg.put(x + dx, h + t + dy, z + dz, LEAVES, 0);
            }
        }
    }
}

pub(crate) fn birch(reg: &mut Region, x: i32, h: i32, z: i32, t: i32, _r: &mut Rng) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, BIRCH_LOG);
    for dy in t - 3..=t + 1 {
        let rad: i32 = if dy < t && dy > t - 3 { 2 } else { 1 };
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                if !(dx.abs() == rad && dz.abs() == rad) || (rad == 1 && dy < t) {
                    reg.put(x + dx, h + dy, z + dz, BIRCH_LEAVES, 0);
                }
            }
        }
    }
    reg.put(x, h + t + 1, z, BIRCH_LEAVES, 0);
}

pub(crate) fn spruce(reg: &mut Region, x: i32, h: i32, z: i32, t: i32) {
    if !reg.touches(x, z, 4) {
        return;
    }
    trunk(reg, x, h + 1, h + t, z, SPRUCE_LOG);
    reg.put(x, h + t + 1, z, SPRUCE_LEAVES, 0);
    reg.put(x, h + t + 2, z, SPRUCE_LEAVES, 0);
    for dy in (3..=t).rev() {
        let k = t - dy;
        let rad = (1 + k / 2).min(3) - if k % 2 == 0 && k > 0 { 1 } else { 0 };
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                if !(rad >= 2 && dx.abs() == rad && dz.abs() == rad || k == 0 && dx != 0 && dz != 0)
                {
                    reg.put(x + dx, h + dy, z + dz, SPRUCE_LEAVES, 0);
                }
            }
        }
    }
}

pub(crate) fn mega_spruce(reg: &mut Region, x: i32, h: i32, z: i32, t: i32) {
    if !reg.touches(x, z, 6) {
        return;
    }
    for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        trunk(reg, x + dx, h - 2, h + t, z + dz, SPRUCE_LOG);
    }
    for dy in t / 3..=t + 2 {
        let k = t + 2 - dy;
        let rad = ((k as f32 * 0.32 + 1.0).min(4.2) - if k % 3 == 1 { 0.9 } else { 0.0 }).max(0.8);
        let ri = rad.ceil() as i32;
        let (ax, bx, az, bz) = reg.span(x, z, -ri, ri + 1);
        for dx in ax..=bx {
            for dz in az..=bz {
                let (fx, fz) = (dx as f32 - 0.5, dz as f32 - 0.5);
                if fx * fx + fz * fz <= rad * rad + 0.3 {
                    reg.put(x + dx, h + dy, z + dz, SPRUCE_LEAVES, 0);
                }
            }
        }
    }
}
