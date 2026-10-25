//! Pure version-4 numerical kernels and procedural structures from the approved donor.
//! Source: donor/crates/voxel-core/src/generator at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! World storage, async ownership, publication and client secrecy are implemented separately.

pub(crate) struct Noise {
    pub(crate) perm: [u8; 512],
}

pub(crate) const GRAD: [(f32, f32); 8] = [
    (1.0, 1.0),
    (-1.0, 1.0),
    (1.0, -1.0),
    (-1.0, -1.0),
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
];

impl Noise {
    pub(crate) fn new(seed: u32) -> Self {
        let mut p: [u8; 256] = [0; 256];
        for (i, v) in p.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut s = seed.wrapping_mul(2654435761).wrapping_add(0x9E3779B9);
        for i in (1..256).rev() {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let j = (s % (i as u32 + 1)) as usize;
            p.swap(i, j);
        }
        let mut perm = [0u8; 512];
        for i in 0..512 {
            perm[i] = p[i & 255];
        }
        Noise { perm }
    }

    pub(crate) fn perlin(&self, x: f32, y: f32) -> f32 {
        let xf = x.floor();
        let yf = y.floor();
        let xi = (xf as i32 & 255) as usize;
        let yi = (yf as i32 & 255) as usize;
        let x = x - xf;
        let y = y - yf;
        let u = x * x * x * (x * (x * 6.0 - 15.0) + 10.0);
        let v = y * y * y * (y * (y * 6.0 - 15.0) + 10.0);
        let p = &self.perm;
        let a = p[xi] as usize + yi;
        let b = p[xi + 1] as usize + yi;
        let g = |h: u8, x: f32, y: f32| {
            let (gx, gy) = GRAD[(h & 7) as usize];
            gx * x + gy * y
        };
        let x1 = lerp(g(p[a], x, y), g(p[b], x - 1.0, y), u);
        let x2 = lerp(g(p[a + 1], x, y - 1.0), g(p[b + 1], x - 1.0, y - 1.0), u);
        lerp(x1, x2, v) * 0.7
    }

    pub(crate) fn fbm(&self, x: f32, y: f32, octaves: u32) -> f32 {
        let (mut sum, mut amp, mut f, mut norm) = (0.0, 1.0, 1.0, 0.0);
        for o in 0..octaves {
            sum += amp * self.perlin(x * f + o as f32 * 17.13, y * f - o as f32 * 9.71);
            norm += amp;
            amp *= 0.5;
            f *= 2.03;
        }
        sum / norm
    }
}

pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
pub(crate) fn sm(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
pub fn hash(seed: u32, salt: u32, a: i32, b: i32) -> u32 {
    let mut h = seed ^ salt.wrapping_mul(0x27D4EB2F);
    h ^= (a as u32).wrapping_mul(0x9E3779B1);
    h = h.rotate_left(13).wrapping_mul(0x85EBCA77);
    h ^= (b as u32).wrapping_mul(0xC2B2AE3D);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846CA68B);
    h ^ (h >> 16)
}
pub struct Rng(pub u32);
impl Rng {
    pub fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E3779B9);
        let mut h = self.0;
        h ^= h >> 16;
        h = h.wrapping_mul(0x7FEB352D);
        h ^= h >> 15;
        h = h.wrapping_mul(0x846CA68B);
        h ^= h >> 16;
        (h >> 8) as f32 / 16777216.0
    }
}
