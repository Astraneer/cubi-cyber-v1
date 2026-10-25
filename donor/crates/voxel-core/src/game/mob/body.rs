//! Тело моба (ASP-16 §3): тот же решатель коллизии, что у игрока (physics.rs: формы блоков, ось Y первой, ступенька
//! 0.6), ход LivingEntity.travel на суше, в воде и в лаве, падение и его урон, прыжок и всплытие. Ввода клавиш нет —
//! forward и speed ставит MoveControl (ai.rs), прыжок — JumpControl.

use super::Mob;
use crate::game::physics::{collide, hlen2, space_empty, Bufs, Solids};
use crate::game::player::{yaw_sin_cos, STEP};
use crate::registry::blocks::{FLUID_LAVA, FLUID_WATER};
use crate::registry::shapes::Aabb;

/// Наибольшая высота жидкости рода kind над низом коробки b (Entity.updateMovementInFluid): коробка, сжатая на 0.001,
/// задевает клетку жидкости, чей верх не ниже низа коробки.
pub(crate) fn fluid_in(w: &mut impl Solids, b: &Aabb, kind: u8) -> Option<f64> {
    let b = b.contract(0.001);
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

impl Mob {
    /// Entity.baseTick: касание воды (fallDistance = 0) и лавы, глаза в воде.
    pub(crate) fn update_fluids(&mut self, w: &mut impl Solids) {
        let b = self.bbox();
        let wet = fluid_in(w, &b, FLUID_WATER);
        self.in_water = wet.is_some();
        self.fluid_h = wet.unwrap_or(0.0);
        if self.in_water {
            self.fall = 0.0;
        }
        self.in_lava = fluid_in(w, &b, FLUID_LAVA).is_some();
        if self.in_lava {
            self.fall *= 0.5;
        }
        let ey = self.pos[1] + self.eye_h();
        let (x, y, z) = (self.pos[0].floor() as i32, ey.floor() as i32, self.pos[2].floor() as i32);
        let (k, h) = w.fluid(x, y, z);
        self.eye_water = k == FLUID_WATER && y as f64 + h > ey;
    }

    /// Высота «плавания» (Entity.getSwimHeight): 0.4, у кого глаза ниже 0.4 — 0.
    pub(crate) fn swim_height(&self) -> f64 {
        if self.eye_h() < 0.4 {
            0.0
        } else {
            0.4
        }
    }

    /// LivingEntity.tickMovement, хвост: прыжок или всплытие, ввод ×0.98, travel. Ответ — приземление этого тика
    /// (fallDistance) для урона падения.
    pub(crate) fn tick_body(&mut self, w: &mut impl Solids, bufs: &mut Bufs) -> Option<f32> {
        if self.jump_cd > 0 {
            self.jump_cd -= 1;
        }
        for c in &mut self.vel {
            if c.abs() < 0.003 {
                *c = 0.0;
            }
        }
        if !self.alive() {
            self.jumping = false;
            self.forward = 0.0;
        }
        if self.jumping {
            let d = if self.in_lava { 0.0 } else { self.fluid_h };
            let wet = self.in_water && d > 0.0;
            let sh = self.swim_height();
            if wet && (!self.on_ground || d > sh) {
                self.vel[1] += 0.04f32 as f64; // swimUpward(WATER)
            } else if self.in_lava && (!self.on_ground || d > sh) {
                self.vel[1] += 0.04f32 as f64; // swimUpward(LAVA)
            } else if (self.on_ground || (wet && d <= sh)) && self.jump_cd == 0 {
                self.vel[1] = 0.42f32 as f64;
                self.jump_cd = 10;
            }
        } else {
            self.jump_cd = 0;
        }
        let f = self.forward * 0.98;
        self.travel(w, f, bufs)
    }

    /// LivingEntity.travel: вода и лава (ускорение 0.02, сопротивление 0.8 / 0.5, гравитация /16, выход на берег) или суша
    /// (ускорение speed·0.216/p³ на земле, 0.02 в воздухе; трение p·0.91, гравитация 0.08, сопротивление 0.98). Курица
    /// падает медленно (vy ×0.6).
    pub(crate) fn travel(&mut self, w: &mut impl Solids, f: f32, bufs: &mut Bufs) -> Option<f32> {
        if self.in_water || self.in_lava {
            let y0 = self.pos[1];
            self.push_input(f, 0.02);
            let m = self.vel;
            let landed = self.mv(w, m, bufs);
            if self.in_water {
                let k = 0.8f32 as f64;
                self.vel = [self.vel[0] * k, self.vel[1] * k, self.vel[2] * k];
                self.vel[1] -= 0.08 / 16.0;
            } else {
                self.vel = [self.vel[0] * 0.5, self.vel[1] * 0.5, self.vel[2] * 0.5];
                self.vel[1] -= 0.08 / 4.0;
            }
            if self.h_coll {
                let up = self.vel[1] + 0.6f32 as f64 - (self.pos[1] - y0);
                let probe = self.bbox().moved([self.vel[0], up, self.vel[2]]);
                if space_empty(w, &probe, &mut bufs.a) && !any_fluid(w, &probe) {
                    self.vel[1] = 0.3f32 as f64;
                }
            }
            return landed;
        }
        let p = self.slip(w);
        let fr: f32 = if self.on_ground { p * 0.91 } else { 0.91 };
        let acc: f32 = if self.on_ground { self.speed * (0.21600002f32 / (p * p * p)) } else { 0.02 };
        self.push_input(f, acc);
        let m = self.vel;
        let landed = self.mv(w, m, bufs);
        let q = self.vel[1] - 0.08;
        self.vel = [self.vel[0] * fr as f64, q * (0.98f32 as f64), self.vel[2] * fr as f64];
        if self.kind == super::MobKind::Chicken && !self.on_ground && self.vel[1] < 0.0 {
            self.vel[1] *= 0.6; // ChickenEntity.tickMovement: медленное падение
        }
        landed
    }

    /// updateVelocity: ввод (0, f) по yaw ×speed (ввод короче 1 — без нормировки).
    fn push_input(&mut self, f: f32, speed: f32) {
        let fz = (f * speed) as f64;
        if (f as f64) * (f as f64) < 1e-7 {
            return;
        }
        let (sn, cs) = yaw_sin_cos(self.yaw);
        self.vel[0] += -fz * sn as f64;
        self.vel[2] += fz * cs as f64;
    }

    /// Скользкость блока под ногами (y − 0.500001).
    fn slip(&self, w: &mut impl Solids) -> f32 {
        let y = (self.pos[1] - 0.500001f32 as f64).floor() as i32;
        w.slip(self.pos[0].floor() as i32, y, self.pos[2].floor() as i32)
    }

    /// Entity.move: коллизия со ступенькой, флаги, учёт падения (приземление → Some(fallDistance)).
    pub(crate) fn mv(&mut self, w: &mut impl Solids, m: [f64; 3], bufs: &mut Bufs) -> Option<f32> {
        let b = self.bbox();
        let v = collide(w, &b, m, self.on_ground, STEP, bufs);
        if v[0] * v[0] + v[1] * v[1] + v[2] * v[2] > 1e-7 {
            for i in 0..3 {
                self.pos[i] += v[i];
            }
        }
        let tol = 1e-5f32 as f64;
        let bx = (v[0] - m[0]).abs() >= tol;
        let bz = (v[2] - m[2]).abs() >= tol;
        self.h_coll = bx || bz;
        let vert = m[1] != v[1];
        let ground = vert && m[1] < 0.0;
        self.on_ground = ground;
        if !self.in_water {
            if let Some(h) = fluid_in(w, &self.bbox(), FLUID_WATER) {
                self.in_water = true;
                self.fluid_h = h;
                self.fall = 0.0;
            }
        }
        let mut landed = None;
        if ground {
            if self.fall > 0.0 {
                landed = Some(self.fall);
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
            self.vel[1] = 0.0;
        }
        landed
    }

    /// Горизонтальный сдвиг за тик (для BodyControl и шагов).
    pub(crate) fn moved_xz2(&self) -> f64 {
        hlen2([self.pos[0] - self.prev[0], 0.0, self.pos[2] - self.prev[2]])
    }
}

/// Есть ли в коробке жидкость (WorldView.containsFluid).
fn any_fluid(w: &mut impl Solids, b: &Aabb) -> bool {
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
