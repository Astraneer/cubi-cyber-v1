//! Игрок: физика Java 1.21.1 на суше (ASP-02), лазанье по лестнице (LivingEntity.applyClimbingSpeed), камера (ASP-01:
//! FOV, высота глаз, покачивание), взмах руки (ASP-04 §8), полёт выживания (abilities.flying, G16) и отладочный полёт
//! сквозь стены.
//! f64 — позиция и скорость (double в Java), f32 — ввод, ускорения, трение, fallDistance, FOV, покачивание (float).

use crate::game::physics::{collide, edge_guard, hlen2, space_below_empty, space_empty, Bufs, Solids, EPS};
use crate::registry::blocks::{FLUID_LAVA, FLUID_WATER};
use crate::registry::shapes::Aabb;

pub(crate) const STEP: f32 = 0.6; // generic.step_height
pub(crate) const EYE_STAND: f32 = 1.62;
pub(crate) const EYE_CROUCH: f32 = 1.27;
pub(crate) const SWING_TICKS: i32 = 6;
pub(crate) const FLY_SPEED: f64 = 0.9; // блоков за тик: 18 б/с, как прежний полёт камеры; с Ctrl ×5 (90 б/с)
/// PlayerAbilities.flySpeed: ускорение полёта выживания (бег — ×2), подъём и спуск — ×3 за тик.
pub(crate) const FLY_ACC: f32 = 0.05;

/// Удержания клавиш в момент тика (KeyBinding.isPressed).
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Keys {
    pub(crate) fwd: bool,
    pub(crate) back: bool,
    pub(crate) left: bool,
    pub(crate) right: bool,
    pub(crate) jump: bool,
    pub(crate) sneak: bool,
    pub(crate) sprint: bool,
}

#[inline]
pub(crate) fn axis(a: bool, b: bool) -> f32 {
    (a as i32 - b as i32) as f32
}

/// Пересекает ли отрезок a→b коробку (метод плит): для луча быстрого падения.
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

/// Синус и косинус yaw в градусах (Java берёт таблицу MathHelper; у нас точные — mc/decisions.md).
pub(crate) fn yaw_sin_cos(yaw: f32) -> (f32, f32) {
    let r = (yaw * (std::f32::consts::PI / 180.0)) as f64;
    (libm::sin(r) as f32, libm::cos(r) as f32)
}

pub(crate) struct Player {
    pub(crate) pos: [f64; 3],
    pub(crate) prev: [f64; 3],
    pub(crate) vel: [f64; 3],
    /// Градусы, как в Minecraft: yaw 0 — взгляд на +Z (юг), 90 — на −X; pitch > 0 — вниз. Yaw не заворачивается.
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) on_ground: bool,
    pub(crate) h_coll: bool,
    pub(crate) soft: bool,
    pub(crate) fall: f32,
    /// fallDistance в момент последнего приземления и тик (для урона в ASP-12 и проверок).
    pub(crate) landed: Option<(u64, f32)>,
    pub(crate) sprinting: bool,
    pub(crate) jump_cd: i32,
    pub(crate) dtap: i32,
    pub(crate) sneak_in: bool, // Shift, опрошенный в этом тике (input.sneaking)
    pub(crate) fwd_in: f32,    // вперёд после множителей приседа и предмета (input.movementForward) — для isWalking
    pub(crate) side: f32,      // sidewaysSpeed / forwardSpeed после ×0.98
    pub(crate) fwd: f32,
    /// Поза CROUCHING: коробка 1.5, глаза 1.27.
    pub(crate) crouch: bool,
    pub(crate) hspeed: f32,
    pub(crate) prev_hspeed: f32,
    pub(crate) stride: f32,
    pub(crate) prev_stride: f32,
    pub(crate) cam_y: f32,
    pub(crate) last_cam_y: f32,
    pub(crate) fov: f32,
    pub(crate) last_fov: f32,
    /// Полёт (PlayerAbilities.flying): полёт выживания (F, G16) — физика полёта Minecraft с коллизией; разрешение —
    /// `game::rules::may_fly`. Сел на землю — выключается сам (ClientPlayerEntity.tickMovement).
    pub(crate) flying: bool,
    /// Отладочный полёт сквозь стены (клавиша отладки, ?fly, сцены, стенд): без инерции и коллизии; при нём flying тоже
    /// true. Действует как креатив (ломание, дальность, неуязвимость) только с отладкой сервера.
    pub(crate) noclip: bool,
    /// Режим игры (G17, game/rules.rs): меняет только сервер (`/gamemode`), клиенту — Msg::Mode.
    pub(crate) mode: crate::game::rules::GameMode,
    /// Касание воды (touchingWater) и высота воды над низом коробки; касание лавы и её высота; глаза в воде (текущий тик).
    /// Считаются в начале каждого тика по позиции (Entity.baseTick), в Move не ходят.
    pub(crate) in_water: bool,
    pub(crate) fluid_h: f64,
    pub(crate) in_lava: bool,
    pub(crate) lava_h: f64,
    pub(crate) eye_water: bool,
    pub(crate) support: Option<[i32; 3]>,
    pub(crate) swinging: bool,
    pub(crate) swing_ticks: i32,
    pub(crate) swing: f32,
    pub(crate) last_swing: f32,
    pub(crate) ticks: u64,
    pub(crate) bufs: Bufs,
}

impl Player {
    pub(crate) fn new(pos: [f64; 3]) -> Player {
        Player {
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
            mode: crate::game::rules::GameMode::Survival,
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
            bufs: Bufs::default(),
        }
    }

    pub(crate) fn height(&self) -> f64 {
        if self.crouch {
            1.5
        } else {
            1.8
        }
    }
    /// Shift в этом тике (isSneaking): отмена использования блока при предмете в руке.
    pub(crate) fn sneaking(&self) -> bool {
        self.sneak_in
    }
    /// Глаза по позе, без сглаживания (getStandingEyeHeight): от них луч прицела.
    pub(crate) fn eye_height(&self) -> f32 {
        if self.crouch {
            EYE_CROUCH
        } else {
            EYE_STAND
        }
    }
    pub(crate) fn bbox(&self) -> Aabb {
        let [x, y, z] = self.pos;
        Aabb { lo: [x - 0.3, y, z - 0.3], hi: [x + 0.3, y + self.height(), z + 0.3] }
    }
    /// Помещается ли поза высоты h на текущем месте (canChangeIntoPose: коробка, сжатая на 1e-7, пуста).
    pub(crate) fn fits(&mut self, w: &mut impl Solids, h: f64) -> bool {
        let [x, y, z] = self.pos;
        let b = Aabb { lo: [x - 0.3, y, z - 0.3], hi: [x + 0.3, y + h, z + 0.3] }.contract(1e-7);
        space_empty(w, &b, &mut self.bufs.a)
    }
    /// Атрибут скорости (float): 0.1, бег — +30 % (ADD_MULTIPLIED_TOTAL).
    pub(crate) fn speed_attr(&self) -> f32 {
        let base = 0.1f32 as f64;
        if self.sprinting {
            (base * (1.0 + 0.3f32 as f64)) as f32
        } else {
            base as f32
        }
    }

    /// Поворот мышью (Mouse.updateMouse, Entity.changeLookDirection): отсчёты · (s·0.6+0.2)³·8 · 0.15°.
    pub(crate) fn turn(&mut self, dx: f64, dy: f64, sens: f64) {
        let d = sens * (0.6f32 as f64) + 0.2f32 as f64;
        let f = d * d * d * 8.0;
        self.pitch = (self.pitch + (dy * f) as f32 * 0.15).clamp(-90.0, 90.0);
        self.yaw += (dx * f) as f32 * 0.15;
    }

    /// Взмах руки (LivingEntity.swingHand): перезапуск, если взмаха нет или прошла половина.
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

    /// GameRenderer.tick до тика игрока: FOV и высота камеры по состоянию на конец прошлого тика.
    pub(crate) fn camera_tick(&mut self) {
        self.last_fov = self.fov;
        let t = (self.speed_attr() / 0.1 + 1.0) / 2.0; // эффекты FOV 1.0; в отладочном полёте бега нет
        // AbstractClientPlayerEntity.getFovMultiplier: в полёте ×1.1 (отладочный — без, как прежде: стенд и сцены).
        let t = if self.flying && !self.noclip { 1.1f32 * t } else { t };
        self.fov = (self.fov + (t - self.fov) * 0.5).clamp(0.1, 1.5);
        self.last_cam_y = self.cam_y;
        self.cam_y += (self.eye_height() - self.cam_y) * 0.5;
    }

    /// Один тик игрока (ClientPlayerEntity.tickMovement → PlayerEntity → LivingEntity, ASP-02 §3.2).
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
        // Замедление приседа — по Shift прошлого тика или если встать некуда (inSneakingPose); в полёте позы приседа нет.
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
        // Бег (ASP-01 §2.9): двойное W на земле — окно 7 тиков; Ctrl — и в воздухе; срыв — только упор, нет «вперёд».
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
        // Плавание кролем (ASP-03 §6) пока не сделано: в воде и лаве бег не держится (иначе вода без гравитации).
        if self.sprinting && (!(f > 1e-5) || (self.h_coll && !self.soft) || self.in_water || self.in_lava) {
            self.sprinting = false;
        }
        // Shift в воде (ClientPlayerEntity.tickMovement): knockDownwards −0.04F до обнуления малых скоростей.
        if self.in_water && k.sneak && !self.flying {
            self.vel[1] -= 0.04f32 as f64;
        }
        // Полёт (ClientPlayerEntity.tickMovement): Shift — вниз, Space — вверх, по flySpeed × 3 за тик.
        if self.flying {
            let j = k.jump as i32 - k.sneak as i32;
            if j != 0 {
                self.vel[1] += (j as f32 * FLY_ACC * 3.0) as f64;
            }
        }
        // PlayerEntity.tickMovement → LivingEntity.tickMovement.
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
            // LivingEntity.tickMovement: в жидкости Space — всплытие +0.04F (swimUpward), обычный прыжок — на земле при
            // высоте жидкости ≤ 0.4 (getSwimHeight) или вне жидкости (ASP-03 §4.2).
            let d = if self.in_lava { self.lava_h } else { self.fluid_h };
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
        let target = if self.on_ground { (hlen2(self.vel).sqrt() as f32).min(0.1) } else { 0.0 };
        self.stride += (target - self.stride) * 0.4;
        // Сел на землю в полёте — полёт выключен (ClientPlayerEntity.tickMovement после движения).
        if self.flying && self.on_ground {
            self.flying = false;
        }
        // Поза в конце тика (PlayerEntity.updatePose): присед — если зажат Shift (не в полёте) или встать некуда.
        self.crouch = if k.sneak && !self.flying { true } else { !self.fits(w, 1.8) };
    }

    pub(crate) fn jump(&mut self) {
        self.vel[1] = 0.42f32 as f64;
        if self.sprinting {
            let (sn, cs) = yaw_sin_cos(self.yaw);
            self.vel[0] += -(sn as f64) * 0.2;
            self.vel[2] += cs as f64 * 0.2;
        }
    }

    /// Лезет ли (LivingEntity.isClimbing): блок в клетке ступней — лестница.
    pub(crate) fn climbing(&self, w: &mut impl Solids) -> bool {
        w.climbable(self.pos[0].floor() as i32, self.pos[1].floor() as i32, self.pos[2].floor() as i32)
    }

    /// Толчок течения (Entity.updateMovementInFluid, часть про скорость): среднее векторов течения задетых клеток (у мелкой
    /// воды — домноженное на глубину, пока она меньше 0.4), × speed (вода 0.014, лава 0.007), прибавляется к скорости; у
    /// покоящегося тела слабее 0.0045 — подтягивается до него. У источников среди источников течения нет — толчка нет.
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
        let mut v = [acc[0] / n as f64 * speed, acc[1] / n as f64 * speed, acc[2] / n as f64 * speed];
        if self.vel[0].abs() < 0.003 && self.vel[2].abs() < 0.003 && len(v) < 0.0045000000000000005 {
            let l = len(v);
            v = [v[0] / l * 0.0045000000000000005, v[1] / l * 0.0045000000000000005, v[2] / l * 0.0045000000000000005];
        }
        for i in 0..3 {
            self.vel[i] += v[i];
        }
    }

    /// Касание жидкости рода kind (Entity.updateMovementInFluid): коробка, сжатая на 0.001, задевает клетку жидкости, чей
    /// верх не ниже низа коробки. Ответ — наибольшая высота жидкости над низом коробки.
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

    /// Entity.baseTick → updateWaterState / updateSubmergedInWaterState: касание воды (fallDistance = 0), касание лавы,
    /// глаза в воде (точка глаз позы, несглаженная; верх воды в клетке строго выше).
    pub(crate) fn update_fluids(&mut self, w: &mut impl Solids) {
        if !self.flying {
            // PlayerEntity.isPushedByFluids: летящего течение не толкает.
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

    /// Жидкости вокруг тела по позиции сейчас, без изменений тела: (высота воды над низом коробки при касании, то же для
    /// лавы, глаза в воде). Сервер читает это для воздуха и горения до теневого тика.
    pub(crate) fn probe_fluids(&mut self, w: &mut impl Solids) -> (Option<f64>, Option<f64>, bool) {
        let wet = self.touch_fluid(w, FLUID_WATER);
        let lava = self.touch_fluid(w, FLUID_LAVA);
        let ey = self.pos[1] + self.eye_height() as f64;
        let (x, y, z) = (self.pos[0].floor() as i32, ey.floor() as i32, self.pos[2].floor() as i32);
        let (k, h) = w.fluid(x, y, z);
        (wet, lava, k == FLUID_WATER && y as f64 + h > ey)
    }

    /// Есть ли в коробке жидкость (WorldView.containsFluid): клетки floor(min)…ceil(max) − 1, высота не важна.
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

    /// Быстрое падение (Entity.move, RaycastContext FALLDAMAGE_RESETTING + FluidHandling.WATER): отрезок от ступней по
    /// ходу v задевает воду (до её поверхности) или лестницу (форма коллизии).
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
                        if seg_hits(a, b, &Aabb { lo: o, hi: [o[0] + 1.0, o[1] + h, o[2] + 1.0] }) {
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

    /// LivingEntity.travel в воде и лаве (ASP-03 §4.1): ускорение 0.02F, ход, трение, гравитация g/16, выход на берег.
    /// Вода: ×(0.8F, 0.8F, 0.8F); лава: у поверхности (≤ 0.4) ×(0.5, 0.8F, 0.5), глубже ×0.5, и ещё −g/4.
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
                self.vel = [self.vel[0] * 0.5, self.vel[1] * 0.8f32 as f64, self.vel[2] * 0.5];
                self.vel[1] -= 0.08 / 16.0;
            } else {
                self.vel = [self.vel[0] * 0.5, self.vel[1] * 0.5, self.vel[2] * 0.5];
            }
            self.vel[1] -= 0.08 / 4.0;
        }
        // Выход на берег: упор сбоку, и коробка, поднятая на ~0.6, свободна от блоков и жидкости — vy = 0.3F.
        if self.h_coll {
            let up = self.vel[1] + 0.6f32 as f64 - (self.pos[1] - y0);
            let probe = self.bbox().moved([self.vel[0], up, self.vel[2]]);
            if space_empty(w, &probe, &mut self.bufs.a) && !Self::fluid_in_box(w, &probe) {
                self.vel[1] = 0.3f32 as f64;
            }
        }
    }

    /// LivingEntity.travel на суше и в воздухе: трение и ускорение по onGround прошлого move. На лестнице
    /// (applyClimbingSpeed, до хода): падение обнуляется, по горизонтали не больше 0.15, вниз не быстрее 0.15 (б/т), с
    /// Shift вниз — 0 (держится); после хода — упёрся в стену или держит прыжок — вверх 0.2 (подъём 0.1176 б/т после
    /// гравитации и сопротивления).
    /// Полёт (PlayerEntity.travel при abilities.flying): ускорение flySpeed (бег ×2), без лестниц и плавания (shouldSwimInFluids
    /// false), защиты края нет; вертикальная скорость после хода — прежняя × 0.6, гравитации нет, fallDistance = 0.
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
        // updateVelocity: нормализация только при длине > 1, поворот по yaw.
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
        self.vel = [self.vel[0] * fr as f64, q * (0.98f32 as f64), self.vel[2] * fr as f64];
        if fly {
            self.vel[1] = vy0 * 0.6;
            self.fall = 0.0; // onLanding
        }
    }

    /// Entity.move: защита края → коллизия со ступенькой → флаги, опора, учёт падения, покачивание.
    pub(crate) fn mv(&mut self, w: &mut impl Solids, m: [f64; 3], sneak: bool) {
        let b = self.bbox();
        // Защита края: Shift в этом тике, не на взлёте, на земле или над опорой в пределах 0.6 − fallDistance.
        let m = if sneak && !(m[1] > 0.0) && self.above_ground(w, &b) {
            edge_guard(w, &b, m, STEP, &mut self.bufs.a)
        } else {
            m
        };
        let v = collide(w, &b, m, self.on_ground, STEP, &mut self.bufs);
        let d2 = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
        if d2 > 1e-7 {
            // Быстрое падение (≥ 1 блока за тик): луч по отрезку хода — вода и лестница обнуляют fallDistance.
            if self.fall != 0.0 && d2 >= 1.0 && self.fall_ray(w, v) {
                self.fall = 0.0;
            }
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
        self.soft = self.h_coll && self.collided_softly(v);
        self.update_support(w, ground, v);
        self.on_ground = ground;
        // LivingEntity.fall: касания воды ещё не было — проверить в новой позиции (касание, fallDistance = 0).
        if !self.in_water {
            if let Some(h) = self.touch_fluid(w, FLUID_WATER) {
                self.in_water = true;
                self.fluid_h = h;
                self.fall = 0.0;
            }
        }
        // Учёт падения (Entity.fall): спуск тика приземления не входит; подъём обнуляет — правило сервера.
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
        // getMoveEffect: на земле с Shift фаза покачивания (и шаги) стоит.
        if !(self.on_ground && sneak) {
            self.hspeed += (hlen2(v).sqrt() as f32) * 0.6;
        }
    }

    /// onGround или опора в пределах 0.6 − fallDistance (PlayerEntity.method_30263).
    pub(crate) fn above_ground(&mut self, w: &mut impl Solids, b: &Aabb) -> bool {
        self.on_ground || (self.fall < STEP && !space_below_empty(w, b, 0.0, 0.0, STEP - self.fall, &mut self.bufs.a))
    }

    /// ClientPlayerEntity.hasCollidedSoftly: угол между вводом по yaw и фактическим смещением < 8°.
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

    /// Опорный блок (Entity.updateSupportingBlockPos): из слоя 1e-6 под коробкой — ближайший центром к ступням.
    pub(crate) fn update_support(&mut self, w: &mut impl Solids, ground: bool, v: [f64; 3]) {
        if !ground {
            self.support = None;
            return;
        }
        let b = self.bbox();
        let slab = Aabb { lo: [b.lo[0], b.lo[1] - 1e-6, b.lo[2]], hi: [b.hi[0], b.lo[1], b.hi[2]] };
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
                    // При равенстве — больший по (y, z, x), как BlockPos.compareTo.
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
    /// Скользкость блока «влияющего на скорость»: XZ опорного блока, Y = floor(y − 0.500001).
    pub(crate) fn slip_here(&mut self, w: &mut impl Solids) -> f32 {
        let y = (self.pos[1] - 0.500001f32 as f64).floor() as i32;
        let (x, z) = match self.support {
            Some(p) => (p[0], p[2]),
            None => (self.pos[0].floor() as i32, self.pos[2].floor() as i32),
        };
        w.slip(x, y, z)
    }

    /// Выталкивание из блоков (ClientPlayerEntity.pushOutOfBlocks): 4 точки ±0.35·ширины; занятая клетка —
    /// скорость ±0.1 к ближайшему свободному краю.
    pub(crate) fn push_out(&mut self, w: &mut impl Solids) {
        let d = 0.6f32 as f64 * 0.35;
        let [x, _, z] = self.pos;
        for (px, pz) in [(x - d, z + d), (x - d, z - d), (x + d, z - d), (x + d, z + d)] {
            let c = [px.floor() as i32, self.pos[1].floor() as i32, pz.floor() as i32];
            if !self.would_collide(w, c) {
                continue;
            }
            let (fx, fz) = (px - c[0] as f64, pz - c[2] as f64);
            let mut best: Option<(usize, i32)> = None;
            let mut dist = f64::MAX;
            // W, E, N, S: (ось, знак).
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
        let col = Aabb { lo: [c[0] as f64, b.lo[1], c[2] as f64], hi: [c[0] as f64 + 1.0, b.hi[1], c[2] as f64 + 1.0] }.contract(1e-7);
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

    /// Отладочный полёт: без инерции и коллизии, как прежняя камера (18 б/с, Ctrl — 90).
    pub(crate) fn fly(&mut self, k: Keys) {
        let speed = if k.sprint { FLY_SPEED * 5.0 } else { FLY_SPEED };
        let (f, s, u) = (axis(k.fwd, k.back) as f64, axis(k.left, k.right) as f64, axis(k.jump, k.sneak) as f64);
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

    /// Включить или выключить полёт выживания (F): падение гасится — висит на месте; выключил в воздухе — падает,
    /// fallDistance считается с этой высоты. Отладочный полёт сквозь стены не трогает.
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

    /// Включить или выключить отладочный полёт сквозь стены (с ним и полёт; выключил — падает).
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

    /// Мгновенно переставить (без интерполяции и без отставания камеры).
    pub(crate) fn teleport(&mut self, p: [f64; 3]) {
        self.pos = p;
        self.prev = p;
        self.vel = [0.0; 3];
        self.fall = 0.0;
        self.last_cam_y = self.cam_y;
    }

    /// Камера кадра с долей a ∈ [0, 1) между прошлым и текущим тиком: позиция глаз (сглаженная высота), yaw и
    /// pitch — текущие, множитель FOV, покачивание [сдвиг x, y, крен Z°, кивок X°] (GameRenderer.bobView).
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

    /// Глаза для луча: интерполированная позиция + высота глаз позы без сглаживания (getCameraPosVec).
    pub(crate) fn eye(&self, a: f64) -> [f64; 3] {
        let l = |p: f64, c: f64| p + (c - p) * a;
        [l(self.prev[0], self.pos[0]), l(self.prev[1], self.pos[1]) + self.eye_height() as f64, l(self.prev[2], self.pos[2])]
    }

    /// Единичный вектор взгляда (Entity.getRotationVector).
    pub(crate) fn look(&self) -> [f64; 3] {
        let f = (self.pitch * (std::f32::consts::PI / 180.0)) as f64;
        let g = (-self.yaw * (std::f32::consts::PI / 180.0)) as f64;
        let (h, i) = (libm::cos(g), libm::sin(g));
        let (j, k) = (libm::cos(f), libm::sin(f));
        [i * j, -k, h * j]
    }
}

#[cfg(test)]
mod player_tests {
    use super::*;

    /// Мир из коробок (как модель-генератор tools/mc_sim.py): пол — плита под y = 0.
    struct Boxes {
        boxes: Vec<Aabb>,
        slip: f32,
    }
    fn bx(x0: f64, y0: f64, z0: f64, x1: f64, y1: f64, z1: f64) -> Aabb {
        Aabb { lo: [x0, y0, z0], hi: [x1, y1, z1] }
    }
    impl Boxes {
        fn flat(extra: &[Aabb]) -> Boxes {
            let mut boxes = vec![bx(-1e4, -64.0, -1e4, 1e4, 0.0, 1e4)];
            boxes.extend_from_slice(extra);
            Boxes { boxes, slip: 0.6 }
        }
        fn of(boxes: &[Aabb]) -> Boxes {
            Boxes { boxes: boxes.to_vec(), slip: 0.6 }
        }
    }
    impl Solids for Boxes {
        fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            // Часть каждой коробки, попавшая в клетку: формы клетки, как у мира из блоков.
            let cell = bx(x as f64, y as f64, z as f64, x as f64 + 1.0, y as f64 + 1.0, z as f64 + 1.0);
            for b in &self.boxes {
                let lo = [b.lo[0].max(cell.lo[0]), b.lo[1].max(cell.lo[1]), b.lo[2].max(cell.lo[2])];
                let hi = [b.hi[0].min(cell.hi[0]), b.hi[1].min(cell.hi[1]), b.hi[2].min(cell.hi[2])];
                if (0..3).all(|a| lo[a] < hi[a]) {
                    out.push(Aabb { lo, hi });
                }
            }
        }
        fn slip(&mut self, _: i32, _: i32, _: i32) -> f32 {
            self.slip
        }
        fn chokes(&mut self, _: i32, _: i32, _: i32) -> bool {
            false
        }
    }

    fn keys(fwd: bool) -> Keys {
        Keys { fwd, ..Default::default() }
    }
    fn at(x: f64, y: f64, z: f64) -> Player {
        Player::new([x, y, z])
    }
    /// Установившаяся скорость (б/с) за последние 100 тиков из 400.
    fn steady(k: Keys, sprint_flag: bool) -> f64 {
        let mut w = Boxes::flat(&[]);
        let mut p = at(0.5, 0.0, 0.5);
        p.sprinting = sprint_flag;
        p.sneak_in = k.sneak;
        let mut track = Vec::new();
        for _ in 0..400 {
            p.tick(&mut w, k, false);
            track.push((p.pos[0], p.pos[2]));
        }
        let ((x0, z0), (x1, z1)) = (track[299], track[399]);
        (x1 - x0).hypot(z1 - z0) / 100.0 * 20.0
    }

    #[test]
    fn speeds() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-5;
        let v = steady(keys(true), false);
        assert!(near(v, 4.31718), "ходьба {v}");
        let v = steady(Keys { sprint: true, ..keys(true) }, true);
        assert!(near(v, 5.61234), "бег {v}");
        let v = steady(Keys { left: true, ..keys(true) }, false);
        assert!(near(v, 4.40529), "диагональ {v}");
        let v = steady(Keys { sneak: true, ..keys(true) }, false);
        assert!(near(v, 1.29515), "присед {v}");
        let v = steady(Keys { sneak: true, left: true, ..keys(true) }, false);
        assert!(near(v, 1.83163), "присед по диагонали {v}");
        let v = steady(Keys { sneak: true, sprint: true, ..keys(true) }, true);
        assert!(near(v, 1.68370), "присед с бегом {v}");
    }

    #[test]
    fn accel_brake_sprint_jumps() {
        // Разгон с места (б/т), 10 блоков: шагом 48 тиков, бегом с Ctrl с первого тика 37; тормозной путь 0.2558 / 0.3326;
        // бег-прыжки подряд 7.100 б/с (ASP-02 §5, tools/mc_sim.py).
        let mut w = Boxes::flat(&[]);
        let mut p = at(0.5, 0.0, 0.5);
        let want = [0.0980, 0.1515, 0.1807, 0.1967, 0.2054, 0.2101, 0.2127, 0.2142, 0.2149, 0.2154];
        for e in want {
            let z0 = p.pos[2];
            p.tick(&mut w, keys(true), false);
            assert!((p.pos[2] - z0 - e).abs() < 1e-4, "разгон");
        }
        for (sprint, ticks, brake) in [(false, 48, 0.2558), (true, 37, 0.3326)] {
            let mut p = at(0.5, 0.0, 0.5);
            let k = Keys { sprint, ..keys(true) };
            let mut t = 0;
            while p.pos[2] - 0.5 < 10.0 {
                p.tick(&mut w, k, false);
                t += 1;
            }
            assert_eq!(t, ticks, "10 блоков, бег {sprint}");
            for _ in 0..200 {
                p.tick(&mut w, k, false);
            }
            let z0 = p.pos[2];
            for _ in 0..20 {
                p.tick(&mut w, Keys::default(), false);
            }
            assert!((p.pos[2] - z0 - brake).abs() < 1e-4, "тормозной путь {}", p.pos[2] - z0);
        }
        let mut p = at(0.5, 0.0, 0.5);
        let k = Keys { sprint: true, ..keys(true) };
        for _ in 0..100 {
            p.tick(&mut w, k, false);
        }
        let z0 = p.pos[2];
        for _ in 0..600 {
            p.tick(&mut w, Keys { jump: true, ..k }, false);
        }
        let v = (p.pos[2] - z0) / 600.0 * 20.0;
        assert!((v - 7.1).abs() < 1e-3, "бег-прыжки {v}");
    }

    #[test]
    fn jump_heights_and_period() {
        let mut w = Boxes::flat(&[]);
        let mut p = at(0.5, 0.0, 0.5);
        p.tick(&mut w, Keys::default(), false);
        let want = [0.4200, 0.7532, 1.0013, 1.1661, 1.2492, 1.2522, 1.1768, 1.0244, 0.7967, 0.4952, 0.1213, 0.0];
        let mut ys = Vec::new();
        p.tick(&mut w, Keys { jump: true, ..Default::default() }, false);
        ys.push(p.pos[1]);
        while !p.on_ground {
            p.tick(&mut w, Keys::default(), false);
            ys.push(p.pos[1]);
        }
        assert_eq!(ys.len(), 12, "приземление на 12-м тике");
        for (y, e) in ys.iter().zip(want) {
            assert!((y - e).abs() < 1e-4, "{ys:?}");
        }
        let apex = ys.iter().cloned().fold(0.0, f64::max);
        assert!((apex - 1.252203).abs() < 1e-6, "апекс {apex}");
        // Space зажат — прыжок каждые 12 тиков; под потолком на 2.0 — каждые 10 (cooldown).
        for (ceil, period) in [(None, 12), (Some(2.0), 10)] {
            let extra: Vec<Aabb> = ceil.map(|c| vec![bx(-100.0, c, -100.0, 100.0, c + 1.0, 100.0)]).unwrap_or_default();
            let mut w = Boxes::flat(&extra);
            let mut p = at(0.5, 0.0, 0.5);
            p.tick(&mut w, Keys::default(), false);
            let mut jumps = Vec::new();
            for t in 1..80 {
                let y0 = p.pos[1];
                p.tick(&mut w, Keys { jump: true, ..Default::default() }, false);
                if y0 == 0.0 && p.pos[1] > 0.15 {
                    jumps.push(t);
                }
            }
            assert!(jumps.windows(2).all(|j| j[1] - j[0] == period), "потолок {ceil:?}: {jumps:?}");
        }
    }

    #[test]
    fn step_slab_and_wall() {
        // Плита 0.5 — подъём ровно на 0.5 за один тик без прыжка; полный блок — стена (упор в 0.3 от центра).
        let mut w = Boxes::flat(&[bx(-100.0, 0.0, 2.0, 100.0, 0.5, 100.0)]);
        let mut p = at(0.5, 0.0, 1.0);
        let mut climbed = None;
        for t in 0..10 {
            p.tick(&mut w, keys(true), false);
            if p.pos[1] > 0.0 {
                climbed = Some((t, p.pos[1]));
                break;
            }
        }
        let (_, y) = climbed.expect("залез на плиту");
        assert_eq!(y, 0.5);
        let mut w = Boxes::flat(&[bx(-100.0, 0.0, 2.0, 100.0, 1.0, 100.0)]);
        let mut p = at(0.5, 0.0, 1.0);
        for _ in 0..10 {
            p.tick(&mut w, keys(true), false);
        }
        assert_eq!(p.pos[1], 0.0);
        assert!((p.pos[2] - 1.7).abs() < 1e-6, "упор z = {}", p.pos[2]);
        // Прыжок вплотную: на 1.0 и 1.25 — залезает, на 1.5 — нет.
        for (h, ok) in [(1.0, true), (1.25, true), (1.5, false)] {
            let mut w = Boxes::flat(&[bx(-5.0, 0.0, 2.0, 6.0, h, 3.0)]);
            let mut p = at(0.5, 0.0, 1.7);
            p.tick(&mut w, keys(true), false);
            let mut up = false;
            for t in 0..30 {
                p.tick(&mut w, Keys { jump: t == 0, ..keys(true) }, false);
                if p.on_ground && p.pos[1] >= h - 1e-9 {
                    up = true;
                    break;
                }
            }
            assert_eq!(up, ok, "высота {h}");
        }
    }

    #[test]
    fn sneak_edge() {
        // Присед у края: центр выходит за край на 0.2977, не падает; вдоль края — тоже не падает.
        let mut w = Boxes::of(&[bx(-100.0, -1.0, -100.0, 100.0, 0.0, 0.0)]);
        let mut p = at(0.5, 0.0, -2.0);
        p.sneak_in = true;
        let k = Keys { sneak: true, ..keys(true) };
        for _ in 0..200 {
            p.tick(&mut w, k, false);
        }
        assert_eq!(p.pos[1], 0.0);
        assert!((p.pos[2] - 0.2977).abs() < 1e-4, "вынос центра {}", p.pos[2]);
        for _ in 0..40 {
            p.tick(&mut w, Keys { left: true, ..k }, false);
        }
        assert_eq!(p.pos[1], 0.0, "вдоль края не падает");
        // Внешний угол по диагонали: 0.2803 по обеим осям.
        let mut w = Boxes::of(&[bx(-100.0, -1.0, -100.0, 0.0, 0.0, 0.0)]);
        let mut p = at(-2.0, 0.0, -2.0);
        p.yaw = -45.0;
        p.sneak_in = true;
        for _ in 0..300 {
            p.tick(&mut w, k, false);
        }
        assert_eq!(p.pos[1], 0.0);
        assert!((p.pos[0] - 0.2803).abs() < 1e-4 && (p.pos[2] - 0.2803).abs() < 1e-4, "угол {:?}", p.pos);
    }

    #[test]
    fn fall_distance() {
        // Сошёл с края на ходу: тики в воздухе и fallDistance у земли (ASP-02 §5).
        for (h, air, fd) in [(1.0, 5, 0.7685), (2.0, 7, 1.5926), (3.0, 9, 2.6946), (4.0, 10, 3.3463), (10.0, 17, 9.6671), (23.0, 26, 21.8335)] {
            let mut w = Boxes::of(&[bx(-100.0, h - 1.0, -100.0, 100.0, h, 0.0), bx(-100.0, -64.0, -100.0, 100.0, 0.0, 100.0)]);
            let mut p = at(0.5, h, -1.0);
            for _ in 0..3 {
                p.tick(&mut w, keys(true), false);
            }
            let (mut left, mut t) = (None, 0);
            loop {
                p.tick(&mut w, keys(true), false);
                t += 1;
                if left.is_none() && !p.on_ground {
                    left = Some(t);
                }
                if left.is_some() && p.on_ground {
                    break;
                }
            }
            assert_eq!(t - left.unwrap() + 1, air, "H = {h}");
            let got = p.landed.unwrap().1 as f64;
            assert!((got - fd).abs() < 1e-4, "H = {h}: {got}");
        }
    }

    #[test]
    fn double_tap_window() {
        // Нажатие W на тике 0, отпускание, повтор на тике k: бег при k ≤ 6.
        for k in 2..10 {
            let mut w = Boxes::flat(&[]);
            let mut p = at(0.5, 0.0, 0.5);
            p.tick(&mut w, Keys::default(), false);
            p.tick(&mut w, Keys::default(), false);
            for t in 0..20 {
                p.tick(&mut w, keys(t == 0 || t >= k), false);
            }
            assert_eq!(p.sprinting, k <= 6, "k = {k}");
        }
    }

    #[test]
    fn sprint_stops_at_wall_angle() {
        // Бег почти вдоль стены: ≤ 7.5° — держится (мягкое касание), ≥ 8.5° — срывается.
        for (ang, keeps) in [(0.0f32, true), (5.0, true), (7.5, true), (8.5, false), (20.0, false)] {
            let mut w = Boxes::flat(&[bx(-1e4, 0.0, 3.0, 1e4, 3.0, 1e4)]);
            let mut p = at(0.5, 0.0, 2.0);
            p.yaw = -(90.0 - ang);
            p.sprinting = true;
            let mut hit = false;
            for _ in 0..40 {
                p.tick(&mut w, keys(true), false);
                hit |= p.h_coll;
                if !p.sprinting {
                    break;
                }
            }
            if ang > 0.0 {
                assert!(hit, "{ang}");
            }
            assert_eq!(p.sprinting, keeps, "угол {ang}");
        }
    }

    #[test]
    fn camera_fov_and_eye() {
        // FOV бега: 1 → 1.075 → 1.1125 … → 1.15 (×0.5 за тик); глаза при приседе: 1.62 → 1.445 → 1.3575 …
        let mut p = at(0.5, 0.0, 0.5);
        p.sprinting = true;
        let mut fovs = Vec::new();
        for _ in 0..4 {
            p.camera_tick();
            fovs.push(p.fov * 70.0);
        }
        assert!((fovs[0] - 75.25).abs() < 1e-3 && (fovs[1] - 77.875).abs() < 1e-3, "{fovs:?}");
        p.crouch = true;
        let mut ys = Vec::new();
        for _ in 0..3 {
            p.camera_tick();
            ys.push(p.cam_y);
        }
        assert!((ys[0] - 1.445).abs() < 1e-4 && (ys[1] - 1.3575).abs() < 1e-4, "{ys:?}");
        // Мышь: 100 % (0.5) — 0.15° на отсчёт, 2400 отсчётов на оборот; pitch упирается в ±90.
        let mut p = at(0.0, 0.0, 0.0);
        p.turn(2400.0, 0.0, 0.5);
        assert!((p.yaw - 360.0).abs() < 1e-3, "{}", p.yaw);
        p.turn(0.0, 1e6, 0.5);
        assert_eq!(p.pitch, 90.0);
    }

    #[test]
    fn swing_rhythm() {
        // Взмах при удержании (вызов каждый тик): 0, 1/6, 2/6, 3/6, 0, …; одиночный — 6 тиков.
        let mut w = Boxes::flat(&[]);
        let mut p = at(0.5, 0.0, 0.5);
        let mut got = Vec::new();
        for _ in 0..9 {
            p.swing_hand();
            p.tick(&mut w, Keys::default(), false);
            got.push((p.swing * 6.0).round() as i32);
        }
        assert_eq!(got, vec![0, 1, 2, 3, 0, 1, 2, 3, 0]);
        let mut p = at(0.5, 0.0, 0.5);
        p.swing_hand();
        let mut got = Vec::new();
        for _ in 0..7 {
            p.tick(&mut w, Keys::default(), false);
            got.push((p.swing * 6.0).round() as i32);
        }
        assert_eq!(got, vec![0, 1, 2, 3, 4, 5, 0]);
    }
}
