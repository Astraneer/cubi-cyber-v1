//! Pure version-4 numerical kernels and procedural structures from the approved donor.
//! Source: donor/crates/voxel-core/src/generator at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! World storage, async ownership, publication and client secrecy are implemented separately.

use super::cell::{BlockId, F_EAST, F_NORTH, F_SOUTH, F_WEST, H, SEA};
use super::noise::{Rng, hash};
use super::region::Region;
use super::{Biome, Column, Gen, birch, oak, spruce};
use rules::ids::*;

pub(crate) const R_ZONE: i32 = 32;
const R_FLAT: f32 = 34.0;
const R_BLEND: f32 = 58.0;
const R_NO_TREES: i32 = 46;
const R_BUILT: f32 = 31.5;
pub(crate) const DECK: i32 = 26;
pub(crate) const ROOF: i32 = 31;
pub(crate) const PARKOUR_N: i32 = DECK;
const PARKOUR_R: f32 = 5.5;
const PARKOUR_STEP: f32 = 2.6;
const FALLS: [(i32, i32); 4] = [(4, -2), (2, 4), (-4, 2), (-2, -4)];
const SPOUT: i32 = 10;
const GATE_DX: i32 = 17;
const GATE_DZ: i32 = -17;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Hall {
    Wind,
    Steel,
    Double,
}

impl Hall {
    pub(crate) const fn u_min(self) -> i32 {
        match self {
            Hall::Wind => -5,
            Hall::Steel => -4,
            Hall::Double => -7,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Pavilion {
    pub(crate) hall: Hall,
    pub(crate) dx: i32,
    pub(crate) dz: i32,
    pub(crate) face: u8,
}

const fn pav(hall: Hall, dx: i32, dz: i32, face: u8) -> Pavilion {
    Pavilion { hall, dx, dz, face }
}

pub(crate) const PAVILIONS: [Pavilion; 3] = [
    pav(Hall::Double, -19, 6, F_EAST),
    pav(Hall::Steel, -19, -6, F_EAST),
    pav(Hall::Wind, -19, -16, F_EAST),
];
const BRIDGE: (i32, i32) = (-2, 2);

pub(crate) const fn face_vec(f: u8) -> (i32, i32) {
    match f {
        F_NORTH => (0, -1),
        F_SOUTH => (0, 1),
        F_WEST => (-1, 0),
        _ => (1, 0),
    }
}

impl Pavilion {
    pub(crate) const fn off(&self, u: i32, v: i32) -> (i32, i32) {
        let (fx, fz) = face_vec(self.face);
        let (rx, rz) = (fz, -fx); // «право» машины (mek/sides.rs dir_of: лицо на юг — право на восток)
        (self.dx + u * rx + v * fx, self.dz + u * rz + v * fz)
    }
    pub(crate) fn bbox(&self, m: i32) -> (i32, i32, i32, i32) {
        let (a, b) = (self.off(self.hall.u_min(), -2), self.off(3, 2));
        (
            a.0.min(b.0) - m,
            a.1.min(b.1) - m,
            a.0.max(b.0) + m,
            a.1.max(b.1) + m,
        )
    }
    fn covers(&self, dx: i32, dz: i32, m: i32) -> bool {
        let (x0, z0, x1, z1) = self.bbox(m);
        (x0..=x1).contains(&dx) && (z0..=z1).contains(&dz)
    }
}

fn near_pavilion(dx: i32, dz: i32, m: i32) -> bool {
    PAVILIONS.iter().any(|p| p.covers(dx, dz, m))
}
fn near_gate(dx: i32, dz: i32, m: i32) -> bool {
    (dx - GATE_DX).abs() <= 3 + m && (dz - GATE_DZ).abs() <= m
}

pub(crate) const SKY_X: i32 = -12;
pub(crate) const SKY_Z: i32 = 3;
pub(crate) const SKY_DAY: f64 = 0.45;
pub(crate) const SKY_NIGHT: f64 = 0.0;

pub(crate) struct SpawnSite {
    active: bool,
    pub(crate) x: i32,
    pub(crate) z: i32,
    pub(crate) y0: i32,
    parkour: Vec<(i32, i32, i32)>,
}

impl SpawnSite {
    pub(crate) fn find(g: &Gen) -> SpawnSite {
        for ring in 0..=32i32 {
            for dz in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs().max(dz.abs()) != ring {
                        continue;
                    }
                    if let Some(y0) = Self::fits(g, dx * 16, dz * 16) {
                        return SpawnSite::new(dx * 16, dz * 16, y0);
                    }
                }
            }
        }
        let h = g.column_raw(0, 0).h;
        SpawnSite::new(0, 0, h.clamp(SEA + 4, 88))
    }

    fn fits(g: &Gen, x: i32, z: i32) -> Option<i32> {
        let mut hs = Vec::with_capacity(64);
        for (r, n) in [(0, 1), (8, 8), (16, 12), (24, 16), (40, 20), (58, 24)] {
            for k in 0..n {
                let a = k as f32 * std::f32::consts::TAU / n as f32;
                let (px, pz) = (
                    x + (r as f32 * libm::cosf(a)).round() as i32,
                    z + (r as f32 * libm::sinf(a)).round() as i32,
                );
                let c = g.column_raw(px, pz);
                if c.h <= SEA + 1
                    || matches!(
                        c.biome,
                        Biome::Ocean | Biome::River | Biome::Beach | Biome::Peaks
                    )
                {
                    return None;
                }
                if r <= 24 {
                    hs.push(c.h);
                }
            }
        }
        let (lo, hi) = (*hs.iter().min()?, *hs.iter().max()?);
        hs.sort_unstable();
        let med = hs[hs.len() / 2];
        (hi - lo <= 12 && (SEA + 4..=88).contains(&med)).then_some(med)
    }

    pub(crate) fn placeholder() -> SpawnSite {
        SpawnSite {
            active: false,
            x: 0,
            z: 0,
            y0: SEA,
            parkour: Vec::new(),
        }
    }

    fn new(x: i32, z: i32, y0: i32) -> SpawnSite {
        let mut parkour = Vec::with_capacity(PARKOUR_N as usize);
        let a0 = 22f32.to_radians();
        let da = PARKOUR_STEP / PARKOUR_R;
        for k in 1..=PARKOUR_N {
            let a = a0 + (k - 1) as f32 * da;
            let (dx, dz) = (
                (PARKOUR_R * libm::cosf(a)).round() as i32,
                (PARKOUR_R * libm::sinf(a)).round() as i32,
            );
            parkour.push((dx, k, dz));
        }
        SpawnSite {
            active: true,
            x,
            z,
            y0,
            parkour,
        }
    }

    #[inline]
    pub(crate) fn protects(&self, x: i32, z: i32) -> bool {
        let (dx, dz) = (
            i64::from(x) - i64::from(self.x),
            i64::from(z) - i64::from(self.z),
        );
        self.active
            && dx.abs() <= i64::from(R_ZONE)
            && dz.abs() <= i64::from(R_ZONE)
            && dx * dx + dz * dz <= i64::from(R_ZONE * R_ZONE)
    }

    #[inline]
    pub(crate) fn no_trees(&self, x: i32, z: i32) -> bool {
        let (dx, dz) = (
            i64::from(x) - i64::from(self.x),
            i64::from(z) - i64::from(self.z),
        );
        self.active
            && dx.abs() < i64::from(R_NO_TREES)
            && dz.abs() < i64::from(R_NO_TREES)
            && dx * dx + dz * dz < i64::from(R_NO_TREES * R_NO_TREES)
    }

    pub(crate) fn sky_button(&self, p: [i32; 3]) -> Option<bool> {
        if !self.active || p[1] != self.y0 + 2 || p[2] != self.z + SKY_Z {
            return None;
        }
        match i64::from(p[0]) - i64::from(self.x) - i64::from(SKY_X) {
            0 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    fn sky_stand(&self, reg: &mut Region) {
        let (x0, z, y0) = (self.x + SKY_X, self.z + SKY_Z, self.y0);
        for i in 0..3 {
            set(reg, x0 + i, y0 + 1, z, SPRUCE_PLANKS);
            set(
                reg,
                x0 + i,
                y0 + 2,
                z,
                [WHITE_WOOL, SPRUCE_LOG, BLACK_WOOL][i as usize],
            );
            set(reg, x0 + i, y0 + 3, z, SPRUCE_PLANKS);
        }
        set(reg, x0 + 1, y0 + 4, z, TORCH);
    }

    pub(crate) fn in_gate(&self, p: [i32; 3]) -> bool {
        let (gx, gz) = (self.x + GATE_DX, self.z + GATE_DZ);
        self.active
            && p[2] == gz
            && (i64::from(p[0]) - i64::from(gx)).abs() <= 2
            && p[1] > self.y0
            && p[1] <= self.y0 + 7
    }

    pub(crate) fn gate_box(&self) -> [i32; 6] {
        let (gx, gz) = (self.x + GATE_DX, self.z + GATE_DZ);
        [gx - 2, self.y0 + 1, gz, gx + 3, self.y0 + 8, gz + 1]
    }

    pub(crate) fn parkour(&self) -> &[(i32, i32, i32)] {
        &self.parkour
    }

    #[inline]
    pub(crate) fn shape(&self, x: i32, z: i32, c: &mut Column) {
        let (dx, dz) = (
            (i64::from(x) - i64::from(self.x)) as f32,
            (i64::from(z) - i64::from(self.z)) as f32,
        );
        let d2 = dx * dx + dz * dz;
        if d2 >= R_BLEND * R_BLEND || !self.active {
            return;
        }
        let d = d2.sqrt();
        let t = super::sm((d - R_FLAT) / (R_BLEND - R_FLAT));
        c.h = (self.y0 as f32 + (c.h - self.y0) as f32 * t).round() as i32;
    }

    pub(crate) fn touches(&self, reg: &Region) -> bool {
        self.active && reg.touches(self.x, self.z, R_FLAT as i32 + 1)
    }

    pub(crate) fn stamp(&self, seed: u32, reg: &mut Region) {
        let (cx, cz, y0) = (self.x, self.z, self.y0);
        let (ax, bx, az, bz) = reg.span(cx, cz, -(R_FLAT as i32), R_FLAT as i32);
        for dz in az..=bz {
            for dx in ax..=bx {
                let r = ((dx * dx + dz * dz) as f32).sqrt();
                if r > R_BUILT {
                    continue;
                }
                let (x, z) = (cx + dx, cz + dz);
                for y in y0 + 1..H {
                    set(reg, x, y, z, AIR);
                }
                let floor = self.floor(dx, dz, r);
                set(reg, x, y0, z, floor);
                let bed = is_bed(dx, dz, r);
                if bed {
                    set(reg, x, y0, z, DIRT);
                    set(reg, x, y0 + 1, z, GRASS);
                }
                if floor == WATER {
                    if r <= 6.5 {
                        set(reg, x, y0 - 1, z, WATER);
                        set(reg, x, y0 - 2, z, SAND);
                    } else {
                        set(reg, x, y0 - 1, z, SAND);
                    }
                }
                if bed {
                    let roll = hash(seed, 0x5A17, x, z) as f32 / 4294967296.0;
                    let meadow = dx > 0 && dz > 0;
                    let fl = if meadow { 0.35 } else { 0.05 };
                    let plant = if roll < fl {
                        if hash(seed, 0x5A18, x.div_euclid(3), z.div_euclid(3)) & 1 == 0 {
                            DANDELION
                        } else {
                            POPPY
                        }
                    } else if roll < fl + 0.25 {
                        if dx < 0 && dz < 0 { FERN } else { TALL_GRASS }
                    } else {
                        AIR
                    };
                    set(reg, x, y0 + 2, z, plant);
                }
            }
        }
        self.hedges(reg);
        self.trees(seed, reg);
        self.pergolas(reg);
        for p in &PAVILIONS {
            self.pavilion(reg, p);
        }
        self.bridge(reg);
        self.gates(reg);
        self.posts(reg);
        self.lamps(reg);
        self.sky_stand(reg);
        self.tower(reg);
        self.parkour_blocks(reg);
    }

    fn pavilion(&self, reg: &mut Region, p: &Pavilion) {
        let y0 = self.y0;
        for v in -2..=2 {
            for u in p.hall.u_min()..=3 {
                let (dx, dz) = p.off(u, v);
                let (x, z) = (self.x + dx, self.z + dz);
                for y in y0 + 1..H {
                    set(reg, x, y, z, AIR);
                }
                set(reg, x, y0 - 1, z, STONE);
                set(reg, x, y0, z, if u <= -4 { COBBLE } else { SPRUCE_PLANKS });
                let pillar = u.abs() == 3 && v.abs() == 2;
                if pillar {
                    for k in 1..=4 {
                        set(reg, x, y0 + k, z, SPRUCE_LOG);
                    }
                    set(reg, x, y0 + 6, z, TORCH);
                } else if v == -2 && u.abs() <= 2 {
                    for k in 1..=4 {
                        set(
                            reg,
                            x,
                            y0 + k,
                            z,
                            if k == 1 { STONE } else { SPRUCE_PLANKS },
                        );
                    }
                }
                if u >= -3 {
                    let edge = u.abs() == 3 || v.abs() == 2;
                    set(
                        reg,
                        x,
                        y0 + 5,
                        z,
                        if edge { SPRUCE_LOG } else { SPRUCE_PLANKS },
                    );
                }
            }
        }
    }

    fn bridge(&self, reg: &mut Region) {
        let p = &PAVILIONS[0];
        for dz in BRIDGE.0..=BRIDGE.1 {
            for v in -2..=2 {
                let (dx, _) = p.off(0, v);
                let (x, z) = (self.x + dx, self.z + dz);
                for y in self.y0 + 1..self.y0 + 5 {
                    set(reg, x, y, z, AIR);
                }
                set(
                    reg,
                    x,
                    self.y0 + 5,
                    z,
                    if v.abs() == 2 {
                        SPRUCE_LOG
                    } else {
                        SPRUCE_PLANKS
                    },
                );
            }
        }
    }

    fn pergolas(&self, reg: &mut Region) {
        let y = self.y0 + 1;
        for (cx, cz) in [(-16, 16), (16, 16), (-16, -16), (16, -16)] {
            if near_pavilion(cx, cz, 4) || near_gate(cx, cz, 4) {
                continue;
            }
            let back = if cz > 0 { 1 } else { -1 }; // скамья — у внешней стороны, лицом к площади
            for dz in -1..=1 {
                for dx in -2..=2 {
                    let (x, z) = (self.x + cx + dx, self.z + cz + dz);
                    for k in 1..=6 {
                        set(reg, x, y + k, z, AIR);
                    }
                    set(reg, x, y, z, STONE);
                    if dx.abs() == 2 && dz.abs() == 1 {
                        for k in 1..=3 {
                            set(reg, x, y + k, z, SPRUCE_LOG);
                        }
                    } else if dz == back && dx.abs() <= 1 {
                        set(reg, x, y + 1, z, SPRUCE_PLANKS);
                    }
                    set(reg, x, y + 4, z, LEAVES);
                }
            }
            set(reg, self.x + cx, y + 5, self.z + cz, TORCH);
        }
    }

    fn lamps(&self, reg: &mut Region) {
        for (sx, sz) in [(1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let (x, z) = (self.x + 13 * sx, self.z + 13 * sz);
            set(reg, x, self.y0 + 1, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 2, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 3, z, TORCH);
        }
    }

    fn floor(&self, dx: i32, dz: i32, r: f32) -> BlockId {
        let (ax, az) = (dx.abs(), dz.abs());
        let axis = ax <= 1 || az <= 1; // луч или дорога по оси шириной 3
        if ax <= 3 && az <= 3 {
            return STONE; // пол Маяка
        }
        if r <= 8.5 && axis {
            return PLANKS; // мостики
        }
        if r <= 7.5 {
            return WATER;
        }
        if r <= 8.5 {
            return COBBLE; // бортик
        }
        if r <= 18.5 {
            if dz < 0 && ax <= 1 && r > 14.5 {
                return IRON_BLOCK;
            }
            if axis {
                return SANDSTONE;
            }
            if ax == az {
                return COBBLE;
            }
            if sundial_mark(dx, dz, r) {
                return COAL_BLOCK;
            }
            return STONE;
        }
        if r <= 19.5 {
            return COBBLE;
        }
        if r <= 28.5 {
            if axis {
                return SANDSTONE; // дороги
            }
            return GRASS;
        }
        if r <= 29.5 || r > 30.5 {
            return COBBLE; // бордюры дорожки
        }
        GRAVEL
    }

    fn hedges(&self, reg: &mut Region) {
        for d in 21..=27 {
            if d == 24 {
                continue;
            }
            for (sx, sz) in [(1, 1), (1, -1), (-1, 1), (-1, -1)] {
                for (hx, hz) in [(2 * sx, d * sz), (d * sx, 2 * sz)] {
                    if near_pavilion(hx, hz, 0) {
                        continue;
                    }
                    set(reg, self.x + hx, self.y0 + 1, self.z + hz, LEAVES);
                    set(reg, self.x + hx, self.y0 + 2, self.z + hz, LEAVES);
                }
            }
        }
    }

    fn trees(&self, seed: u32, reg: &mut Region) {
        let (cx, cz, h) = (self.x, self.z, self.y0 + 1);
        let ok = |dx: i32, dz: i32| !near_pavilion(dx, dz, 3) && !near_gate(dx, dz, 3);
        let rng = |x: i32, z: i32| Rng(hash(seed, 0x7EE5, x, z));
        for (dx, dz, t) in [
            (-9, -22, 9),
            (-15, -16, 11),
            (-21, -9, 8),
            (-22, -16, 7),
            (-16, -23, 7),
        ] {
            if !ok(dx, dz) {
                continue;
            }
            spruce(reg, cx + dx, h, cz + dz, t);
        }
        for (dx, dz, t) in [(-9, 22, 7), (-15, 17, 8), (-22, 10, 6), (-20, 18, 7)] {
            if !ok(dx, dz) {
                continue;
            }
            birch(reg, cx + dx, h, cz + dz, t, &mut rng(cx + dx, cz + dz));
        }
        for (dx, dz, t) in [(13, 20, 5), (21, 12, 6), (21, -10, 6), (10, -22, 5)] {
            if !ok(dx, dz) {
                continue;
            }
            oak(reg, cx + dx, h, cz + dz, t, &mut rng(cx + dx, cz + dz));
        }
    }

    fn gates(&self, reg: &mut Region) {
        for (ux, uz) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
            let (px, pz) = (-uz, ux); // поперёк дороги
            for p in -3..=3 {
                let (x, z) = (self.x + ux * 28 + px * p, self.z + uz * 28 + pz * p);
                if p.abs() == 3 {
                    for k in 1..=8 {
                        set(reg, x, self.y0 + k, z, SPRUCE_LOG);
                    }
                    set(reg, x, self.y0 + 9, z, SANDSTONE);
                    set(reg, x, self.y0 + 10, z, TORCH);
                } else {
                    set(reg, x, self.y0 + 8, z, SPRUCE_PLANKS);
                    if p.abs() <= 1 {
                        set(reg, x, self.y0 + 9, z, SANDSTONE);
                    }
                }
            }
        }
        let (gx, gz) = (self.x + GATE_DX, self.z + GATE_DZ);
        for p in -3..=3 {
            let x = gx + p;
            if p.abs() == 3 {
                for k in 1..=8 {
                    set(reg, x, self.y0 + k, gz, COAL_BLOCK);
                }
                set(reg, x, self.y0 + 9, gz, IRON_BLOCK);
            } else {
                set(reg, x, self.y0 + 8, gz, COAL_BLOCK);
                set(reg, x, self.y0, gz, COAL_BLOCK);
                for k in 1..=7 {
                    set(reg, x, self.y0 + k, gz, AIR);
                }
                if p == 0 {
                    set(reg, x, self.y0 + 9, gz, IRON_BLOCK);
                }
            }
        }
        let step = if GATE_DZ < 0 { 1 } else { -1 }; // от Врат к центру
        let mut z = gz + step;
        while z != self.z {
            for x in gx - 1..=gx + 1 {
                let (dx, dz) = (x - self.x, z - self.z);
                if is_garden(dx, dz, ((dx * dx + dz * dz) as f32).sqrt()) {
                    set(reg, x, self.y0, z, GRAVEL);
                    set(reg, x, self.y0 + 1, z, AIR);
                    set(reg, x, self.y0 + 2, z, AIR);
                }
            }
            z += step;
        }
    }

    fn posts(&self, reg: &mut Region) {
        for k in 0..12 {
            if k % 3 == 0 {
                continue;
            }
            let a = k as f32 * std::f32::consts::TAU / 12.0;
            let (dx, dz) = (
                (31.0 * libm::cosf(a)).round() as i32,
                (31.0 * libm::sinf(a)).round() as i32,
            );
            let (x, z) = (self.x + dx, self.z + dz);
            set(reg, x, self.y0 + 1, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 2, z, SPRUCE_LOG);
            set(reg, x, self.y0 + 3, z, TORCH);
        }
    }

    fn tower(&self, reg: &mut Region) {
        let (cx, cz, y0) = (self.x, self.z, self.y0);
        let &(lx, _, lz) = self.parkour.last().unwrap();
        for dz in -3..=3i32 {
            for dx in -3..=3i32 {
                let (ax, az) = (dx.abs(), dz.abs());
                let wall = ax == 3 || az == 3;
                let corner = ax == 3 && az == 3;
                let perp = if ax == 3 { az } else { ax };
                let (x, z) = (cx + dx, cz + dz);
                for k in 1..=ROOF + 1 {
                    let b = if k < DECK {
                        if !wall {
                            AIR
                        } else if corner {
                            SPRUCE_LOG
                        } else if k <= 4 && perp <= 1 {
                            AIR // проёмы к мостикам
                        } else if k % 6 == 0 {
                            SANDSTONE // пояса
                        } else if (13..=16).contains(&k) && perp <= 1 {
                            AIR // арки колокола
                        } else if k > 6 && perp == 0 && k % 6 >= 2 && !(dz == 3 && k <= 9) {
                            GLASS // окна-бойницы (на юге над проёмом — доска «Как играть», render/boards.js)
                        } else {
                            STONE
                        }
                    } else if k == DECK {
                        if corner { SPRUCE_LOG } else { SPRUCE_PLANKS }
                    } else if k < ROOF {
                        if corner {
                            SPRUCE_LOG
                        } else if k == DECK + 1 && wall {
                            let gap = (dx - lx).abs().max((dz - lz).abs()) <= 2 && perp <= 1;
                            if gap { AIR } else { GLASS }
                        } else {
                            AIR
                        }
                    } else if k == ROOF {
                        if ax <= 1 && az <= 1 { GLASS } else { SANDSTONE }
                    } else if ax <= 1 && az <= 1 {
                        if ax == 0 && az == 0 {
                            IRON_BLOCK
                        } else {
                            SANDSTONE
                        }
                    } else {
                        AIR
                    };
                    set(reg, x, y0 + k, z, b);
                }
            }
        }
        for (dx, dz) in FALLS {
            let (x, z) = (cx + dx, cz + dz);
            set(reg, x, y0 + SPOUT, z, SANDSTONE);
            for k in 1..SPOUT {
                set(reg, x, y0 + k, z, WATER);
            }
        }
    }

    fn parkour_blocks(&self, reg: &mut Region) {
        for (i, &(dx, k, dz)) in self.parkour.iter().enumerate() {
            let b = if (i + 1) % 7 == 0 || i + 1 == self.parkour.len() {
                SANDSTONE
            } else {
                BIRCH_PLANKS
            };
            set(reg, self.x + dx, self.y0 + k, self.z + dz, b);
        }
    }
}

fn is_bed(dx: i32, dz: i32, r: f32) -> bool {
    is_garden(dx, dz, r)
        && !near_pavilion(dx, dz, 2)
        && !((dx - GATE_DX).abs() <= 1
            && (dz.signum() == GATE_DZ.signum() && dz.abs() < GATE_DZ.abs()))
}

fn is_garden(dx: i32, dz: i32, r: f32) -> bool {
    r > 19.5 && r <= 27.5 && dx.abs() > 2 && dz.abs() > 2
}

fn sundial_mark(dx: i32, dz: i32, r: f32) -> bool {
    if !(10.5..=17.0).contains(&r) {
        return false;
    }
    let a = libm::atan2f(dz as f32, dx as f32).to_degrees();
    [
        (-144.0f32, 13.0f32),
        (-132.8, 13.0),
        (-54.0, 10.5),
        (24.8, 13.0),
        (36.0, 13.0),
    ]
    .iter()
    .any(|&(m, r0)| {
        let d = (a - m + 540.0) % 360.0 - 180.0;
        r >= r0 && d.abs().to_radians() * r < 0.75
    })
}

#[inline]
fn set(reg: &mut Region, x: i32, y: i32, z: i32, b: BlockId) {
    if let Some(i) = reg.index_of(x, y, z) {
        reg.blocks[i] = b as u8;
    }
}
