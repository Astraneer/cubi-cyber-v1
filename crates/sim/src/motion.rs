//! Java1.21.1 numerical recurrences from approved donor player.rs.
//! The owning Player validates permissions/Unknown and exposes immutable-world ticks.
//! No donor runtime, world mutation or synchronous generation is included.
use crate::collision::{
    Bufs, EPS, Solids, collide, edge_guard, hlen2, space_below_empty, space_empty,
};
use rules::blocks::{FLUID_LAVA, FLUID_WATER};
use rules::shapes::Aabb;

pub(crate) const STEP: f32 = 0.6; // generic.step_height
pub(crate) const EYE_STAND: f32 = 1.62;
pub(crate) const EYE_CROUCH: f32 = 1.27;
pub(crate) const SWING_TICKS: i32 = 6;
pub(crate) const FLY_SPEED: f64 = 0.9; // блоков за тик: 18 б/с, как прежний полёт камеры; с Ctrl ×5 (90 б/с)
pub(crate) const FLY_ACC: f32 = 0.05;

#[derive(Clone, Copy, Default, Debug)]
pub struct Keys {
    pub fwd: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
}

#[inline]
pub(crate) fn axis(a: bool, b: bool) -> f32 {
    (a as i32 - b as i32) as f32
}

fn seg_hits(a: [f64; 3], b: [f64; 3], bx: &Aabb) -> bool {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for i in 0..3 {
        let d = b[i] - a[i];
        if d.abs() < 1e-12 {
            if a[i] < bx.lo[i] || a[i] > bx.hi[i] {
                return false;
            }
        } else {
            let (mut u0, mut u1) = ((bx.lo[i] - a[i]) / d, (bx.hi[i] - a[i]) / d);
            if u0 > u1 {
                std::mem::swap(&mut u0, &mut u1);
            }
            t0 = t0.max(u0);
            t1 = t1.min(u1);
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

pub(crate) fn yaw_sin_cos(yaw: f32) -> (f32, f32) {
    let r = (yaw * (std::f32::consts::PI / 180.0)) as f64;
    (libm::sin(r) as f32, libm::cos(r) as f32)
}

pub struct Motion {
    pub pos: [f64; 3],
    pub prev: [f64; 3],
    pub vel: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub h_coll: bool,
    pub soft: bool,
    pub fall: f32,
    pub landed: Option<(u64, f32)>,
    pub sprinting: bool,
    pub jump_cd: i32,
    pub dtap: i32,
    pub sneak_in: bool, // Shift, опрошенный в этом тике (input.sneaking)
    pub fwd_in: f32, // вперёд после множителей приседа и предмета (input.movementForward) — для isWalking
    pub side: f32,   // sidewaysSpeed / forwardSpeed после ×0.98
    pub fwd: f32,
    pub crouch: bool,
    pub hspeed: f32,
    pub prev_hspeed: f32,
    pub stride: f32,
    pub prev_stride: f32,
    pub cam_y: f32,
    pub last_cam_y: f32,
    pub fov: f32,
    pub last_fov: f32,
    pub flying: bool,
    pub noclip: bool,
    pub mode: crate::GameMode,
    pub in_water: bool,
    pub fluid_h: f64,
    pub in_lava: bool,
    pub lava_h: f64,
    pub eye_water: bool,
    pub support: Option<[i32; 3]>,
    pub swinging: bool,
    pub swing_ticks: i32,
    pub swing: f32,
    pub last_swing: f32,
    pub ticks: u64,
    pub(crate) bufs: Bufs,
}

impl Motion {
    pub(crate) fn new(pos: [f64; 3]) -> Motion {
        Self::with_buffers(pos, Bufs::default())
    }
    pub(crate) fn with_buffers(pos: [f64; 3], bufs: Bufs) -> Motion {
        Motion {
            pos,
            prev: pos,
            vel: [0.0, -0.0784, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
            h_coll: false,
            soft: false,
            fall: 0.0,
            landed: None,
            sprinting: false,
            jump_cd: 0,
            dtap: 0,
            sneak_in: false,
            fwd_in: 0.0,
            side: 0.0,
            fwd: 0.0,
            crouch: false,
            hspeed: 0.0,
            prev_hspeed: 0.0,
            stride: 0.0,
            prev_stride: 0.0,
            cam_y: EYE_STAND,
            last_cam_y: EYE_STAND,
            fov: 1.0,
            last_fov: 1.0,
            flying: false,
            noclip: false,
            mode: crate::GameMode::Survival,
            in_water: false,
            fluid_h: 0.0,
            in_lava: false,
            lava_h: 0.0,
            eye_water: false,
            support: None,
            swinging: false,
            swing_ticks: 0,
            swing: 0.0,
            last_swing: 0.0,
            ticks: 0,
            bufs,
        }
    }

    pub(crate) fn height(&self) -> f64 {
        if self.crouch { 1.5 } else { 1.8 }
    }
    pub(crate) fn sneaking(&self) -> bool {
        self.sneak_in
    }
    pub(crate) fn eye_height(&self) -> f32 {
        if self.crouch { EYE_CROUCH } else { EYE_STAND }
    }
    pub(crate) fn bbox(&self) -> Aabb {
        let [x, y, z] = self.pos;
        Aabb {
            lo: [x - 0.3, y, z - 0.3],
            hi: [x + 0.3, y + self.height(), z + 0.3],
        }
    }
    pub(crate) fn fits(&mut self, w: &mut impl Solids, h: f64) -> bool {
        let [x, y, z] = self.pos;
        let b = Aabb {
            lo: [x - 0.3, y, z - 0.3],
            hi: [x + 0.3, y + h, z + 0.3],
        }
        .contract(1e-7);
        space_empty(w, &b, &mut self.bufs.a)
    }
    pub(crate) fn speed_attr(&self) -> f32 {
        let base = 0.1f32 as f64;
        if self.sprinting {
            (base * (1.0 + 0.3f32 as f64)) as f32
        } else {
            base as f32
        }
    }

    pub(crate) fn turn(&mut self, dx: f64, dy: f64, sens: f64) {
        let d = sens * (0.6f32 as f64) + 0.2f32 as f64;
        let f = d * d * d * 8.0;
        self.pitch = (self.pitch + (dy * f) as f32 * 0.15).clamp(-90.0, 90.0);
        self.yaw += (dx * f) as f32 * 0.15;
    }

    pub(crate) fn swing_hand(&mut self) {
        if !self.swinging || self.swing_ticks >= SWING_TICKS / 2 || self.swing_ticks < 0 {
            self.swing_ticks = -1;
            self.swinging = true;
        }
    }
    pub(crate) fn tick_swing(&mut self) {
        if self.swinging {
            self.swing_ticks += 1;
            if self.swing_ticks >= SWING_TICKS {
                self.swing_ticks = 0;
                self.swinging = false;
            }
        } else {
            self.swing_ticks = 0;
        }
        self.swing = self.swing_ticks as f32 / SWING_TICKS as f32;
    }

    pub(crate) fn camera_tick(&mut self) {
        self.last_fov = self.fov;
        let t = (self.speed_attr() / 0.1 + 1.0) / 2.0; // эффекты FOV 1.0; в отладочном полёте бега нет
        let t = if self.flying && !self.noclip {
            1.1f32 * t
        } else {
            t
        };
        self.fov = (self.fov + (t - self.fov) * 0.5).clamp(0.1, 1.5);
        self.last_cam_y = self.cam_y;
        self.cam_y += (self.eye_height() - self.cam_y) * 0.5;
    }

    pub(crate) fn tick(&mut self, w: &mut impl Solids, k: Keys, using_item: bool) {
        self.ticks += 1;
        self.prev = self.pos; // resetPosition
        self.prev_hspeed = self.hspeed; // baseTick
        self.last_swing = self.swing;
        if !self.noclip {
            self.update_fluids(w); // Entity.baseTick: касание воды (fallDistance = 0), лавы, глаза в воде
        }
        if self.noclip {
            self.prev_stride = self.stride;
            self.tick_swing();
            self.fly(k);
            self.stride += (0.0 - self.stride) * 0.4;
            self.crouch = false;
            return;
        }
        if self.dtap > 0 {
            self.dtap -= 1;
        }
        let prev_sneak = self.sneak_in;
        let prev_walk = self.fwd_in >= 0.8;
        let slow = !self.flying && self.fits(w, 1.5) && (prev_sneak || !self.fits(w, 1.8));
        let (mut f, mut s) = (axis(k.fwd, k.back), axis(k.left, k.right));
        if slow {
            f *= 0.3;
            s *= 0.3;
        }
        self.sneak_in = k.sneak;
        if using_item {
            f *= 0.2;
            s *= 0.2;
            self.dtap = 0;
        }
        self.fwd_in = f;
        self.push_out(w);
        if prev_sneak {
            self.dtap = 0;
        }
        let can_start = !self.sprinting && f >= 0.8 && !using_item;
        if self.on_ground && !prev_sneak && !prev_walk && can_start {
            if self.dtap <= 0 && !k.sprint {
                self.dtap = 7;
            } else {
                self.sprinting = true;
            }
        }
        if can_start && k.sprint {
            self.sprinting = true;
        }
        if self.sprinting
            && (f <= 1e-5 || (self.h_coll && !self.soft) || self.in_water || self.in_lava)
        {
            self.sprinting = false;
        }
        if self.in_water && k.sneak && !self.flying {
            self.vel[1] -= 0.04f32 as f64;
        }
        if self.flying {
            let j = k.jump as i32 - k.sneak as i32;
            if j != 0 {
                self.vel[1] += (j as f32 * FLY_ACC * 3.0) as f64;
            }
        }
        self.prev_stride = self.stride;
        if self.jump_cd > 0 {
            self.jump_cd -= 1;
        }
        for c in &mut self.vel {
            if c.abs() < 0.003 {
                *c = 0.0;
            }
        }
        self.tick_swing(); // tickNewAi
        if k.jump && !self.flying {
            let d = if self.in_lava {
                self.lava_h
            } else {
                self.fluid_h
            };
            let wet = self.in_water && d > 0.0;
            let shallow = d <= 0.4;
            if !wet || (self.on_ground && shallow) {
                if !self.in_lava || (self.on_ground && shallow) {
                    if (self.on_ground || (wet && shallow)) && self.jump_cd == 0 {
                        self.jump();
                        self.jump_cd = 10;
                    }
                } else {
                    self.vel[1] += 0.04f32 as f64; // swimUpward(LAVA)
                }
            } else {
                self.vel[1] += 0.04f32 as f64; // swimUpward(WATER)
            }
        } else {
            self.jump_cd = 0;
        }
        self.side = s * 0.98;
        self.fwd = f * 0.98;
        self.travel(w, k.sneak, k.jump);
        let target = if self.on_ground {
            (hlen2(self.vel).sqrt() as f32).min(0.1)
        } else {
            0.0
        };
        self.stride += (target - self.stride) * 0.4;
        if self.flying && self.on_ground {
            self.flying = false;
        }
        self.crouch = if k.sneak && !self.flying {
            true
        } else {
            !self.fits(w, 1.8)
        };
    }

    pub(crate) fn jump(&mut self) {
        self.vel[1] = 0.42f32 as f64;
        if self.sprinting {
            let (sn, cs) = yaw_sin_cos(self.yaw);
            self.vel[0] += -(sn as f64) * 0.2;
            self.vel[2] += cs as f64 * 0.2;
        }
    }

    pub(crate) fn climbing(&self, w: &mut impl Solids) -> bool {
        w.climbable(
            self.pos[0].floor() as i32,
            self.pos[1].floor() as i32,
            self.pos[2].floor() as i32,
        )
    }

    pub(crate) fn fluid_push(&mut self, w: &mut impl Solids, kind: u8, speed: f64) {
        let b = self.bbox().contract(0.001);
        let (mut acc, mut n, mut deep) = ([0.0f64; 3], 0, 0.0f64);
        for z in b.lo[2].floor() as i32..b.hi[2].ceil() as i32 {
            for y in b.lo[1].floor() as i32..b.hi[1].ceil() as i32 {
                for x in b.lo[0].floor() as i32..b.hi[0].ceil() as i32 {
                    let (k, h) = w.fluid(x, y, z);
                    if k != kind {
                        continue;
                    }
                    let top = y as f64 + h;
                    if top < b.lo[1] {
                        continue;
                    }
                    deep = deep.max(top - b.lo[1]);
                    let mut v = w.flow(x, y, z);
                    if deep < 0.4 {
                        v = [v[0] * deep, v[1] * deep, v[2] * deep];
                    }
                    for i in 0..3 {
                        acc[i] += v[i];
                    }
                    n += 1;
                }
            }
        }
        let len = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if n == 0 || len(acc) <= 0.0 {
            return;
        }
        let mut v = [
            acc[0] / n as f64 * speed,
            acc[1] / n as f64 * speed,
            acc[2] / n as f64 * speed,
        ];
        if self.vel[0].abs() < 0.003 && self.vel[2].abs() < 0.003 && len(v) < 0.0045000000000000005
        {
            let l = len(v);
            v = [
                v[0] / l * 0.0045000000000000005,
                v[1] / l * 0.0045000000000000005,
                v[2] / l * 0.0045000000000000005,
            ];
        }
        for (velocity, amount) in self.vel.iter_mut().zip(v) {
            *velocity += amount;
        }
    }

    pub(crate) fn touch_fluid(&mut self, w: &mut impl Solids, kind: u8) -> Option<f64> {
        let b = self.bbox().contract(0.001);
        let mut best: Option<f64> = None;
        for z in b.lo[2].floor() as i32..b.hi[2].ceil() as i32 {
            for y in b.lo[1].floor() as i32..b.hi[1].ceil() as i32 {
                for x in b.lo[0].floor() as i32..b.hi[0].ceil() as i32 {
                    let (k, h) = w.fluid(x, y, z);
                    if k != kind {
                        continue;
                    }
                    let top = y as f64 + h;
                    if top < b.lo[1] {
                        continue;
                    }
                    let d = top - b.lo[1];
                    best = Some(best.map_or(d, |m| m.max(d)));
                }
            }
        }
        best
    }

    pub(crate) fn update_fluids(&mut self, w: &mut impl Solids) {
        if !self.flying {
            self.fluid_push(w, FLUID_WATER, 0.014);
            self.fluid_push(w, FLUID_LAVA, 0.007);
        }
        let (wet, lava, eye) = self.probe_fluids(w);
        self.in_water = wet.is_some();
        self.fluid_h = wet.unwrap_or(0.0);
        if self.in_water {
            self.fall = 0.0;
        }
        self.in_lava = lava.is_some();
        self.lava_h = lava.unwrap_or(0.0);
        if self.in_lava {
            self.fall *= 0.5; // Entity.baseTick: в лаве падение гаснет вдвое каждый тик
        }
        self.eye_water = eye;
    }

    pub(crate) fn probe_fluids(&mut self, w: &mut impl Solids) -> (Option<f64>, Option<f64>, bool) {
        let wet = self.touch_fluid(w, FLUID_WATER);
        let lava = self.touch_fluid(w, FLUID_LAVA);
        let ey = self.pos[1] + self.eye_height() as f64;
        let (x, y, z) = (
            self.pos[0].floor() as i32,
            ey.floor() as i32,
            self.pos[2].floor() as i32,
        );
        let (k, h) = w.fluid(x, y, z);
        (wet, lava, k == FLUID_WATER && y as f64 + h > ey)
    }

    pub(crate) fn fluid_in_box(w: &mut impl Solids, b: &Aabb) -> bool {
        for z in b.lo[2].floor() as i32..b.hi[2].ceil() as i32 {
            for y in b.lo[1].floor() as i32..b.hi[1].ceil() as i32 {
                for x in b.lo[0].floor() as i32..b.hi[0].ceil() as i32 {
                    if w.fluid(x, y, z).0 != 0 {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub(crate) fn fall_ray(&mut self, w: &mut impl Solids, v: [f64; 3]) -> bool {
        let a = self.pos;
        let b = [a[0] + v[0], a[1] + v[1], a[2] + v[2]];
        let lo = |i: usize| a[i].min(b[i]).floor() as i32;
        let hi = |i: usize| a[i].max(b[i]).floor() as i32;
        for z in lo(2)..=hi(2) {
            for y in lo(1)..=hi(1) {
                for x in lo(0)..=hi(0) {
                    let (k, h) = w.fluid(x, y, z);
                    if k == FLUID_WATER {
                        let o = [x as f64, y as f64, z as f64];
                        if seg_hits(
                            a,
                            b,
                            &Aabb {
                                lo: o,
                                hi: [o[0] + 1.0, o[1] + h, o[2] + 1.0],
                            },
                        ) {
                            return true;
                        }
                    }
                    if w.climbable(x, y, z) {
                        self.bufs.b.clear();
                        w.shapes_at(x, y, z, &mut self.bufs.b);
                        if self.bufs.b.iter().any(|s| seg_hits(a, b, s)) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    pub(crate) fn travel_fluid(&mut self, w: &mut impl Solids, sneak: bool) {
        let y0 = self.pos[1];
        let water = self.in_water;
        let (mut ix, mut iz) = (self.side as f64, self.fwd as f64);
        let l2 = ix * ix + iz * iz;
        if l2 >= 1e-7 {
            if l2 > 1.0 {
                let n = l2.sqrt();
                ix /= n;
                iz /= n;
            }
            let a = 0.02f32 as f64;
            let (sn, cs) = yaw_sin_cos(self.yaw);
            self.vel[0] += ix * a * cs as f64 - iz * a * sn as f64;
            self.vel[2] += iz * a * cs as f64 + ix * a * sn as f64;
        }
        let m = self.vel;
        self.mv(w, m, sneak);
        if self.h_coll && self.climbing(w) {
            self.vel[1] = 0.2;
        }
        if water {
            let f = 0.8f32 as f64;
            self.vel = [self.vel[0] * f, self.vel[1] * f, self.vel[2] * f];
            self.vel[1] -= 0.08 / 16.0;
        } else {
            if self.lava_h <= 0.4 {
                self.vel = [
                    self.vel[0] * 0.5,
                    self.vel[1] * 0.8f32 as f64,
                    self.vel[2] * 0.5,
                ];
                self.vel[1] -= 0.08 / 16.0;
            } else {
                self.vel = [self.vel[0] * 0.5, self.vel[1] * 0.5, self.vel[2] * 0.5];
            }
            self.vel[1] -= 0.08 / 4.0;
        }
        if self.h_coll {
            let up = self.vel[1] + 0.6f32 as f64 - (self.pos[1] - y0);
            let probe = self.bbox().moved([self.vel[0], up, self.vel[2]]);
            if space_empty(w, &probe, &mut self.bufs.a) && !Self::fluid_in_box(w, &probe) {
                self.vel[1] = 0.3f32 as f64;
            }
        }
    }

    pub(crate) fn travel(&mut self, w: &mut impl Solids, sneak: bool, jump: bool) {
        let fly = self.flying;
        if !fly && (self.in_water || self.in_lava) {
            return self.travel_fluid(w, sneak);
        }
        let vy0 = self.vel[1];
        let sneak = sneak && !fly;
        let p = self.slip_here(w);
        let fr: f32 = if self.on_ground { p * 0.91 } else { 0.91 };
        let acc: f32 = if self.on_ground {
            self.speed_attr() * (0.21600002f32 / (p * p * p))
        } else if fly {
            if self.sprinting {
                FLY_ACC * 2.0
            } else {
                FLY_ACC
            }
        } else if self.sprinting {
            0.025999999
        } else {
            0.02
        };
        let (mut ix, mut iz) = (self.side as f64, self.fwd as f64);
        let l2 = ix * ix + iz * iz;
        if l2 >= 1e-7 {
            if l2 > 1.0 {
                let n = l2.sqrt();
                ix /= n;
                iz /= n;
            }
            let (vx, vz) = (ix * acc as f64, iz * acc as f64);
            let (sn, cs) = yaw_sin_cos(self.yaw);
            self.vel[0] += vx * cs as f64 - vz * sn as f64;
            self.vel[2] += vz * cs as f64 + vx * sn as f64;
        }
        if !fly && self.climbing(w) {
            self.fall = 0.0; // onLanding
            let c = 0.15f32 as f64;
            self.vel[0] = self.vel[0].clamp(-c, c);
            self.vel[2] = self.vel[2].clamp(-c, c);
            self.vel[1] = self.vel[1].max(-c);
            if self.vel[1] < 0.0 && sneak {
                self.vel[1] = 0.0; // isHoldingOntoLadder: игрок с Shift держится
            }
        }
        let m = self.vel;
        self.mv(w, m, sneak);
        if !fly && (self.h_coll || jump) && self.climbing(w) {
            self.vel[1] = 0.2;
        }
        let q = self.vel[1] - 0.08;
        self.vel = [
            self.vel[0] * fr as f64,
            q * (0.98f32 as f64),
            self.vel[2] * fr as f64,
        ];
        if fly {
            self.vel[1] = vy0 * 0.6;
            self.fall = 0.0; // onLanding
        }
    }

    pub(crate) fn mv(&mut self, w: &mut impl Solids, m: [f64; 3], sneak: bool) {
        let b = self.bbox();
        let m = if sneak && m[1] <= 0.0 && self.above_ground(w, &b) {
            edge_guard(w, &b, m, STEP, &mut self.bufs.a)
        } else {
            m
        };
        let v = collide(w, &b, m, self.on_ground, STEP, &mut self.bufs);
        let d2 = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
        if d2 > 1e-7 {
            if self.fall != 0.0 && d2 >= 1.0 && self.fall_ray(w, v) {
                self.fall = 0.0;
            }
            for (position, amount) in self.pos.iter_mut().zip(v) {
                *position += amount;
            }
        }
        let tol = 1e-5f32 as f64;
        let bx = (v[0] - m[0]).abs() >= tol;
        let bz = (v[2] - m[2]).abs() >= tol;
        self.h_coll = bx || bz;
        let vert = m[1] != v[1];
        let ground = vert && m[1] < 0.0;
        self.soft = self.h_coll && self.collided_softly(v);
        self.update_support(w, ground, v);
        self.on_ground = ground;
        if !self.in_water
            && let Some(h) = self.touch_fluid(w, FLUID_WATER)
        {
            self.in_water = true;
            self.fluid_h = h;
            self.fall = 0.0;
        }
        if ground {
            if self.fall > 0.0 {
                self.landed = Some((self.ticks, self.fall));
            }
            self.fall = 0.0;
        } else if v[1] < 0.0 {
            self.fall -= v[1] as f32;
        } else if v[1] > 0.0 {
            self.fall = 0.0;
        }
        if bx {
            self.vel[0] = 0.0;
        }
        if bz {
            self.vel[2] = 0.0;
        }
        if vert {
            self.vel[1] = 0.0; // Block.onEntityLand
        }
        if !(self.on_ground && sneak) {
            self.hspeed += (hlen2(v).sqrt() as f32) * 0.6;
        }
    }

    pub(crate) fn above_ground(&mut self, w: &mut impl Solids, b: &Aabb) -> bool {
        self.on_ground
            || (self.fall < STEP
                && !space_below_empty(w, b, 0.0, 0.0, STEP - self.fall, &mut self.bufs.a))
    }

    pub(crate) fn collided_softly(&self, v: [f64; 3]) -> bool {
        let (sn, cs) = yaw_sin_cos(self.yaw);
        let (d, e) = (sn as f64, cs as f64);
        let g = self.side as f64 * e - self.fwd as f64 * d;
        let h = self.fwd as f64 * e + self.side as f64 * d;
        let i = g * g + h * h;
        let j = hlen2(v);
        let tol = 1e-5f32 as f64;
        if i < tol || j < tol {
            return false;
        }
        libm::acos(((g * v[0] + h * v[2]) / (i * j).sqrt()).clamp(-1.0, 1.0)) < 0.13962634f32 as f64
    }

    pub(crate) fn update_support(&mut self, w: &mut impl Solids, ground: bool, v: [f64; 3]) {
        if !ground {
            self.support = None;
            return;
        }
        let b = self.bbox();
        let slab = Aabb {
            lo: [b.lo[0], b.lo[1] - 1e-6, b.lo[2]],
            hi: [b.hi[0], b.lo[1], b.hi[2]],
        };
        let found = self.find_support(w, &slab);
        if found.is_some() || self.on_ground {
            self.support = found;
        } else {
            self.support = self.find_support(w, &slab.moved([-v[0], 0.0, -v[2]]));
        }
    }
    pub(crate) fn find_support(&mut self, w: &mut impl Solids, area: &Aabb) -> Option<[i32; 3]> {
        let mut best: Option<([i32; 3], f64)> = None;
        let lo = |a: usize| (area.lo[a] - EPS).floor() as i32;
        let hi = |a: usize| (area.hi[a] + EPS).floor() as i32;
        for z in lo(2)..=hi(2) {
            for y in lo(1)..=hi(1) {
                for x in lo(0)..=hi(0) {
                    self.bufs.b.clear();
                    w.shapes_at(x, y, z, &mut self.bufs.b);
                    if !self.bufs.b.iter().any(|s| s.intersects(area)) {
                        continue;
                    }
                    let c = [x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5];
                    let d = (0..3).map(|i| (c[i] - self.pos[i]).powi(2)).sum::<f64>();
                    let better = match best {
                        None => true,
                        Some((p, bd)) => d < bd || (d == bd && (p[1], p[2], p[0]) < (y, z, x)),
                    };
                    if better {
                        best = Some(([x, y, z], d));
                    }
                }
            }
        }
        best.map(|(p, _)| p)
    }
    pub(crate) fn slip_here(&mut self, w: &mut impl Solids) -> f32 {
        let y = (self.pos[1] - 0.500001f32 as f64).floor() as i32;
        let (x, z) = match self.support {
            Some(p) => (p[0], p[2]),
            None => (self.pos[0].floor() as i32, self.pos[2].floor() as i32),
        };
        w.slip(x, y, z)
    }

    pub(crate) fn push_out(&mut self, w: &mut impl Solids) {
        let d = 0.6f32 as f64 * 0.35;
        let [x, _, z] = self.pos;
        for (px, pz) in [
            (x - d, z + d),
            (x - d, z - d),
            (x + d, z - d),
            (x + d, z + d),
        ] {
            let c = [
                px.floor() as i32,
                self.pos[1].floor() as i32,
                pz.floor() as i32,
            ];
            if !self.would_collide(w, c) {
                continue;
            }
            let (fx, fz) = (px - c[0] as f64, pz - c[2] as f64);
            let mut best: Option<(usize, i32)> = None;
            let mut dist = f64::MAX;
            for (a, sg) in [(0usize, -1i32), (0, 1), (2, -1), (2, 1)] {
                let g = if a == 0 { fx } else { fz };
                let h = if sg > 0 { 1.0 - g } else { g };
                let mut n = c;
                n[a] += sg;
                if h < dist && !self.would_collide(w, n) {
                    dist = h;
                    best = Some((a, sg));
                }
            }
            if let Some((a, sg)) = best {
                self.vel[a] = 0.1 * sg as f64;
            }
        }
    }
    pub(crate) fn would_collide(&mut self, w: &mut impl Solids, c: [i32; 3]) -> bool {
        let b = self.bbox();
        let col = Aabb {
            lo: [c[0] as f64, b.lo[1], c[2] as f64],
            hi: [c[0] as f64 + 1.0, b.hi[1], c[2] as f64 + 1.0],
        }
        .contract(1e-7);
        let (y0, y1) = (col.lo[1].floor() as i32, col.hi[1].floor() as i32);
        for y in y0..=y1 {
            if !w.chokes(c[0], y, c[2]) {
                continue;
            }
            self.bufs.b.clear();
            w.shapes_at(c[0], y, c[2], &mut self.bufs.b);
            if self.bufs.b.iter().any(|s| s.intersects(&col)) {
                return true;
            }
        }
        false
    }

    pub(crate) fn fly(&mut self, k: Keys) {
        let speed = if k.sprint { FLY_SPEED * 5.0 } else { FLY_SPEED };
        let (f, s, u) = (
            axis(k.fwd, k.back) as f64,
            axis(k.left, k.right) as f64,
            axis(k.jump, k.sneak) as f64,
        );
        let (sn, cs) = yaw_sin_cos(self.yaw);
        let (sn, cs) = (sn as f64, cs as f64);
        self.pos[0] += (-sn * f + cs * s) * speed;
        self.pos[2] += (cs * f + sn * s) * speed;
        self.pos[1] += u * speed;
        self.vel = [0.0; 3];
        self.fall = 0.0;
        self.on_ground = false;
        self.sprinting = false;
    }

    pub(crate) fn set_flight(&mut self, on: bool) {
        if self.noclip {
            return;
        }
        self.flying = on;
        if on {
            self.vel[1] = self.vel[1].max(0.0);
            self.fall = 0.0;
        }
    }

    pub(crate) fn set_flying(&mut self, on: bool) {
        self.noclip = on;
        self.flying = on;
        self.in_water = false;
        self.in_lava = false;
        self.eye_water = false;
        self.vel = [0.0; 3];
        self.fall = 0.0;
        self.sprinting = false;
        self.support = None;
    }

    pub(crate) fn teleport(&mut self, p: [f64; 3]) {
        self.pos = p;
        self.prev = p;
        self.vel = [0.0; 3];
        self.fall = 0.0;
        self.last_cam_y = self.cam_y;
    }

    pub(crate) fn view(&self, a: f64) -> [f64; 10] {
        let af = a as f32;
        let l = |p: f64, c: f64| p + (c - p) * a;
        let eye = (self.last_cam_y + (self.cam_y - self.last_cam_y) * af) as f64;
        let fov = self.last_fov + (self.fov - self.last_fov) * af;
        let g = self.hspeed - self.prev_hspeed;
        let h = -(self.hspeed + g * af);
        let i = self.prev_stride + (self.stride - self.prev_stride) * af;
        let pi = std::f32::consts::PI;
        let (s, c) = (libm::sinf(h * pi), libm::cosf(h * pi));
        [
            l(self.prev[0], self.pos[0]),
            l(self.prev[1], self.pos[1]) + eye,
            l(self.prev[2], self.pos[2]),
            self.yaw as f64,
            self.pitch as f64,
            fov as f64,
            (s * i * 0.5) as f64,
            (-(c * i).abs()) as f64,
            (s * i * 3.0) as f64,
            ((libm::cosf(h * pi - 0.2) * i).abs() * 5.0) as f64,
        ]
    }

    pub(crate) fn eye(&self, a: f64) -> [f64; 3] {
        let l = |p: f64, c: f64| p + (c - p) * a;
        [
            l(self.prev[0], self.pos[0]),
            l(self.prev[1], self.pos[1]) + self.eye_height() as f64,
            l(self.prev[2], self.pos[2]),
        ]
    }

    pub(crate) fn look(&self) -> [f64; 3] {
        crate::look_direction(self.yaw, self.pitch)
    }
}
