//! ИИ моба (ASP-16 §4, ASP-17 §3, ASP-18 §4): две очереди целей (поведение и выбор цели атаки) с флагами MOVE / LOOK /
//! JUMP / TARGET и приоритетами (меньшее число вытесняет большее, равные — нет), полный пересчёт раз в 2 тика
//! ((age + id) % 2 == 0), в остальные — только цели «каждый тик»; навигация по пути (path.rs), MoveControl (поворот до
//! 90°/тик, forward = speed = mult·attr, прыжок к узлу выше 0.6), LookControl (голова 10°/тик, тангаж до ±40° сразу),
//! BodyControl (тело — по ходу, стоя догоняет голову за 10–20 тиков), JumpControl (запрос живёт тик).

use super::path::{self, Spec};
use super::{Mob, MobKind, MobStats, Out, Seen};
use crate::abi::Sfx;
use crate::cell::H;
use crate::game::physics::Solids;
use crate::game::player::STEP;
use crate::game::view::Events;
use crate::game::GameRng;
use crate::registry::blocks::{bdef, collision_of};
use crate::registry::ids::*;
use crate::registry::items::{item_by_mc, ItemId, I_WHEAT_SEEDS};
use crate::registry::shapes::Aabb;
use crate::world::WorldData;

const MOVE: u8 = 1;
const LOOK: u8 = 2;
const JUMP: u8 = 4;
const TARGET: u8 = 8;

/// Цели (перечисление, не трейты — проще проверять и «усыплять»).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum G {
    Swim,
    Panic,
    Tempt,
    EatGrass,
    Wander,
    LookAt,
    LookAround,
    Melee,
    // Очередь целей атаки.
    Revenge,
    Hunt,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Slot {
    pri: u8,
    flags: u8,
    every: bool,
    pub(crate) running: bool,
    pub(crate) g: G,
}

const fn slot(pri: u8, g: G, flags: u8, every: bool) -> Slot {
    Slot { pri, flags, every, running: false, g }
}

/// Цели вида (initGoals): ASP-17 §3.1, ASP-18 §4.1. Размножение и «за родителем» — позже (P1).
fn goals_of(k: MobKind) -> Vec<Slot> {
    use G::*;
    match k {
        MobKind::Pig => vec![
            slot(0, Swim, JUMP, true),
            slot(1, Panic, MOVE, false),
            slot(4, Tempt, MOVE | LOOK, false),
            slot(6, Wander, MOVE, false),
            slot(7, LookAt, LOOK, false),
            slot(8, LookAround, MOVE | LOOK, true),
        ],
        MobKind::Cow | MobKind::Chicken => vec![
            slot(0, Swim, JUMP, true),
            slot(1, Panic, MOVE, false),
            slot(3, Tempt, MOVE | LOOK, false),
            slot(5, Wander, MOVE, false),
            slot(6, LookAt, LOOK, false),
            slot(7, LookAround, MOVE | LOOK, true),
        ],
        MobKind::Sheep => vec![
            slot(0, Swim, JUMP, true),
            slot(1, Panic, MOVE, false),
            slot(3, Tempt, MOVE | LOOK, false),
            slot(5, EatGrass, MOVE | LOOK | JUMP, false),
            slot(6, Wander, MOVE, false),
            slot(7, LookAt, LOOK, false),
            slot(8, LookAround, MOVE | LOOK, true),
        ],
        // Зомби без SwimGoal: тонет и ходит по дну.
        MobKind::Zombie => vec![
            slot(2, Melee, MOVE | LOOK, true),
            slot(7, Wander, MOVE, false),
            slot(8, LookAt, LOOK, false),
            slot(8, LookAround, MOVE | LOOK, true),
        ],
    }
}

fn targets_of(k: MobKind) -> Vec<Slot> {
    match k {
        MobKind::Zombie => vec![slot(1, G::Revenge, TARGET, false), slot(2, G::Hunt, TARGET, false)],
        _ => Vec::new(),
    }
}

/// Навигация (EntityNavigation): путь — клетки ступней, индекс следующего узла, множитель скорости, цель поиска,
/// таймеры застревания.
#[derive(Clone, Debug, Default)]
pub(crate) struct Nav {
    pub(crate) path: Vec<[i32; 3]>,
    pub(crate) idx: usize,
    pub(crate) mult: f32,
    pub(crate) goal: Option<[i32; 3]>,
    ticks: u32,
    check_at: u32,
    check_pos: [f64; 3],
    node_at: u32,
    node_idx: usize,
    node_timeout: f64,
}

impl Nav {
    pub(crate) fn idle(&self) -> bool {
        self.idx >= self.path.len()
    }
    pub(crate) fn stop(&mut self) {
        self.path.clear();
        self.idx = 0;
        self.goal = None;
    }
}

/// MoveControl: ожидание, «иди к точке», прыжок.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum MoveState {
    #[default]
    Wait,
    MoveTo,
    Jumping,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MoveCtl {
    pub(crate) state: MoveState,
    pub(crate) to: [f64; 3],
    pub(crate) mult: f32,
}

/// LookControl: точка взгляда, таймер (2 тика после lookAt), пределы поворота за тик.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LookCtl {
    pub(crate) at: [f64; 3],
    pub(crate) timer: i32,
    pub(crate) max_yaw: f32,
    pub(crate) max_pitch: f32,
}

/// Состояние целей и контроллеров.
#[derive(Clone, Debug)]
pub(crate) struct Ai {
    pub(crate) slots: Vec<Slot>,
    pub(crate) tslots: Vec<Slot>,
    pub(crate) nav: Nav,
    pub(crate) mv: MoveCtl,
    pub(crate) look: LookCtl,
    body_last_head: f32,
    body_still: i32,
    pub(crate) jump_req: bool,
    look_who: Option<u32>,
    look_left: i32,
    around: [f64; 2],
    around_left: i32,
    tempt_who: Option<u32>,
    tempt_cd: i32,
    // Ближний бой (MeleeAttackGoal / ZombieAttackGoal).
    melee_check: u64,
    melee_cd: i32,
    melee_repath: i32,
    melee_seen_at: Option<[f64; 3]>,
    melee_ticks: i32,
    // Цель атаки: тиков без видимости и предел; время последней мести.
    unseen: i32,
    unseen_max: i32,
    revenge_at: Option<u64>,
}

impl Ai {
    pub(crate) fn new(k: MobKind) -> Ai {
        Ai {
            slots: goals_of(k),
            tslots: targets_of(k),
            nav: Nav::default(),
            mv: MoveCtl::default(),
            look: LookCtl::default(),
            body_last_head: 0.0,
            body_still: 0,
            jump_req: false,
            look_who: None,
            look_left: 0,
            around: [0.0; 2],
            around_left: 0,
            tempt_who: None,
            tempt_cd: 0,
            melee_check: 0,
            melee_cd: 0,
            melee_repath: 0,
            melee_seen_at: None,
            melee_ticks: 0,
            unseen: 0,
            unseen_max: 60,
            revenge_at: None,
        }
    }
    /// Работает ли цель.
    pub(crate) fn running(&self, g: G) -> bool {
        self.slots.iter().chain(self.tslots.iter()).any(|s| s.g == g && s.running)
    }
}

/// Мир и окружение для ИИ на тик.
pub(crate) struct AiCtx<'a> {
    pub(crate) w: &'a mut WorldData,
    pub(crate) players: &'a [Seen],
    pub(crate) time: u64,
    /// Тьма неба (ambientDarkness 0…11).
    pub(crate) dark: i32,
    pub(crate) budget: &'a mut i32,
    pub(crate) out: &'a mut Vec<Out>,
    /// Месть всей стаей: (моб, игрок) — сервер раздаст цель зомби рядом.
    pub(crate) revenge: &'a mut Vec<(u32, u32)>,
    pub(crate) stats: &'a mut MobStats,
    pub(crate) ev: &'a mut Events,
    pub(crate) fx: &'a mut GameRng,
}

// ---------------------------------------------------------------- углы (MathHelper)

pub(crate) fn wrap(a: f32) -> f32 {
    let r = a.rem_euclid(360.0);
    if r >= 180.0 {
        r - 360.0
    } else {
        r
    }
}
/// LookControl.changeAngle: от from к to не больше max.
pub(crate) fn change_angle(from: f32, to: f32, max: f32) -> f32 {
    from + wrap(to - from).clamp(-max, max)
}
/// MathHelper.clampAngle: value не дальше delta от mean.
pub(crate) fn clamp_angle(value: f32, mean: f32, delta: f32) -> f32 {
    mean - wrap(mean - value).clamp(-delta, delta)
}
/// MoveControl.wrapDegrees: поворот не больше max, ответ в [0, 360].
fn turn_to(from: f32, to: f32, max: f32) -> f32 {
    let mut g = from + wrap(to - from).clamp(-max, max);
    if g < 0.0 {
        g += 360.0;
    } else if g > 360.0 {
        g -= 360.0;
    }
    g
}
fn yaw_to(dx: f64, dz: f64) -> f32 {
    (libm::atan2(dz, dx).to_degrees() as f32) - 90.0
}

// ---------------------------------------------------------------- свет и видимость

/// Небесный свет клетки без движка света (до ASP-14): столб над клеткой — полный непрозрачный блок → 0; листва и вода
/// гасят по 1; иначе 15.
pub(crate) fn sky_light(w: &mut WorldData, x: i32, y: i32, z: i32) -> i32 {
    if y >= H {
        return 15;
    }
    let mut l = 15;
    for yy in y.max(0)..H {
        let d = bdef(w.get_block(x, yy, z));
        if d.leaves || d.fluid != 0 {
            l -= 1;
        } else if d.suffocates && !collision_of(w.get_cell(x, yy, z)).is_empty() {
            return 0;
        }
        if l <= 0 {
            return 0;
        }
    }
    l
}
/// Свет клетки с ночной тьмой (getLightLevel(pos, ambientDarkness)), блочного нет.
pub(crate) fn light_at(w: &mut WorldData, p: [i32; 3], dark: i32) -> i32 {
    (sky_light(w, p[0], p[1], p[2]) - dark).max(0)
}
/// Яркость (DimensionType.getBrightness, ambient 0): f/(4 − 3f), f = свет/15.
pub(crate) fn brightness(light: i32) -> f32 {
    let f = light as f32 / 15.0;
    f / (4.0 - 3.0 * f)
}

/// Отрезок a → b не задевает формы коллизии блоков (LivingEntity.canSee: RaycastContext COLLIDER) — листва и стекло
/// закрывают, трава и вода — нет.
pub(crate) fn line_of_sight(w: &mut WorldData, a: [f64; 3], b: [f64; 3]) -> bool {
    let d: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len < 1e-6 {
        return true;
    }
    let cell = |p: [f64; 3]| [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32];
    let (mut c, end) = (cell(a), cell(b));
    let step: [i32; 3] = d.map(|v| if v > 0.0 { 1 } else if v < 0.0 { -1 } else { 0 });
    let (mut tm, mut td) = ([f64::MAX; 3], [f64::MAX; 3]);
    for k in 0..3 {
        if step[k] != 0 {
            let next = if step[k] > 0 { c[k] as f64 + 1.0 } else { c[k] as f64 };
            tm[k] = (next - a[k]) / d[k];
            td[k] = step[k] as f64 / d[k];
        }
    }
    for _ in 0..256 {
        if c[1] >= 0 && c[1] < H {
            let cl = w.get_cell(c[0], c[1], c[2]);
            for s in collision_of(cl) {
                let bx = s.moved([c[0] as f64, c[1] as f64, c[2] as f64]);
                if seg_box(a, b, &bx) {
                    return false;
                }
            }
        }
        if c == end {
            break;
        }
        let k = if tm[0] < tm[1] {
            if tm[0] < tm[2] {
                0
            } else {
                2
            }
        } else if tm[1] < tm[2] {
            1
        } else {
            2
        };
        if tm[k] > 1.0 {
            break;
        }
        c[k] += step[k];
        tm[k] += td[k];
    }
    true
}

/// Отрезок a → b пересекает коробку (метод плит).
pub(crate) fn seg_box(a: [f64; 3], b: [f64; 3], bx: &Aabb) -> bool {
    seg_box_t(a, b, bx).is_some()
}
/// Доля отрезка до входа в коробку (0…1) или None.
pub(crate) fn seg_box_t(a: [f64; 3], b: [f64; 3], bx: &Aabb) -> Option<f64> {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for i in 0..3 {
        let d = b[i] - a[i];
        if d.abs() < 1e-12 {
            if a[i] < bx.lo[i] || a[i] > bx.hi[i] {
                return None;
            }
        } else {
            let (mut u0, mut u1) = ((bx.lo[i] - a[i]) / d, (bx.hi[i] - a[i]) / d);
            if u0 > u1 {
                std::mem::swap(&mut u0, &mut u1);
            }
            t0 = t0.max(u0);
            t1 = t1.min(u1);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

/// Корм вида (tags/item/*_food): курица — семена; корова и овца — пшеница; свинья — морковь, картофель, свёкла. Чего
/// нет в реестре (ферма — команда items), то не манит.
fn is_food(k: MobKind, item: ItemId) -> bool {
    if item == 0 {
        return false;
    }
    match k {
        MobKind::Chicken => item == I_WHEAT_SEEDS,
        MobKind::Cow | MobKind::Sheep => item == item_by_mc("wheat"),
        MobKind::Pig => ["carrot", "potato", "beetroot"].iter().any(|n| item == item_by_mc(n)),
        MobKind::Zombie => false,
    }
}

impl Mob {
    fn spec(&self) -> Spec {
        let (w, h) = self.dims();
        let d = self.def();
        Spec {
            width: w,
            cells: (h.ceil() as i32).max(1),
            max_nodes: (d.follow * 16.0).floor() as i32,
            range: d.follow,
            safe_fall: 3,
            water_penalty: d.water_penalty,
            swims: self.kind != MobKind::Zombie,
        }
    }

    /// Ближайший подходящий игрок в радиусе r (присед — ×0.8), с проверкой видимости (see).
    fn nearest_player(&self, c: &mut AiCtx, r: f64, see: bool, pred: impl Fn(&Seen) -> bool) -> Option<Seen> {
        let mut best: Option<(f64, Seen)> = None;
        let eye = self.eye();
        for p in c.players.iter() {
            if !p.alive || !pred(p) {
                continue;
            }
            let rr = if p.crouch { r * 0.8 } else { r };
            let d2 = (0..3).map(|k| (p.pos[k] - self.pos[k]).powi(2)).sum::<f64>();
            if d2 > rr * rr || best.as_ref().is_some_and(|(b, _)| *b <= d2) {
                continue;
            }
            if see && !line_of_sight(c.w, eye, p.eye) {
                continue;
            }
            best = Some((d2, *p));
        }
        best.map(|(_, p)| p)
    }
    fn player<'a>(&self, c: &'a AiCtx, id: u32) -> Option<&'a Seen> {
        c.players.iter().find(|p| p.id == id && p.alive)
    }

    /// Начать путь к клетке goal (startMovingTo): тот же блок и путь не кончился — старый путь. false — пути нет.
    pub(crate) fn nav_to(&mut self, c: &mut AiCtx, goal: [i32; 3], dist: i32, mult: f32) -> bool {
        if self.ai.nav.goal == Some(goal) && !self.ai.nav.idle() {
            self.ai.nav.mult = mult;
            return true;
        }
        if *c.budget <= 0 {
            return false; // бюджет поиска на тик исчерпан — попробуем в следующий
        }
        let start = self.start_cell();
        let spec = self.spec();
        let found = path::find(c.w, start, goal, dist, &spec, c.budget, c.stats);
        let Some((nodes, _reached)) = found else { return false };
        if nodes.len() < 2 && nodes.first() == Some(&start) {
            return false;
        }
        let n = &mut self.ai.nav;
        n.path = nodes;
        n.idx = 0;
        n.mult = mult;
        n.goal = Some(goal);
        n.check_at = n.ticks;
        n.check_pos = self.pos;
        n.node_idx = usize::MAX;
        true
    }

    /// Клетка старта пути: на земле floor(y + 0.5), в воде — клетка ступней.
    fn start_cell(&self) -> [i32; 3] {
        let y = if self.on_ground { (self.pos[1] + 0.5).floor() } else { self.pos[1].floor() };
        [self.pos[0].floor() as i32, y as i32, self.pos[2].floor() as i32]
    }

    /// Тик навигации: узел достигнут или «проскочен» — следующий; застрял — стоп; иначе moveTo к центру узла.
    pub(crate) fn nav_tick(&mut self, w: &mut WorldData) {
        self.ai.nav.ticks += 1;
        if self.ai.nav.idle() {
            return;
        }
        let (wd, _) = self.dims();
        let r = if wd > 0.75 { wd / 2.0 } else { 0.75 - wd / 2.0 };
        loop {
            let n = &self.ai.nav;
            if n.idle() {
                break;
            }
            let p = n.path[n.idx];
            let (cx, cz) = (p[0] as f64 + 0.5, p[2] as f64 + 0.5);
            let reached = (self.pos[0] - cx).abs() < r && (self.pos[2] - cz).abs() < r && (self.pos[1] - p[1] as f64).abs() < 1.0;
            let skip = n.idx + 1 < n.path.len() && {
                let q = n.path[n.idx + 1];
                let a = [cx - self.pos[0], cz - self.pos[2]];
                let b = [q[0] as f64 + 0.5 - self.pos[0], q[2] as f64 + 0.5 - self.pos[2]];
                let (la, lb) = ((a[0] * a[0] + a[1] * a[1]).sqrt(), (b[0] * b[0] + b[1] * b[1]).sqrt());
                (self.pos[1] - p[1] as f64).abs() < 1.0 && la < 2.0 && (lb < la || la < 0.71) && a[0] * b[0] + a[1] * b[1] < 0.0
            };
            if reached || skip {
                self.ai.nav.idx += 1;
            } else {
                break;
            }
        }
        if self.ai.nav.idle() {
            self.ai.nav.stop();
            return;
        }
        // Застревание (checkTimeouts): за 100 тиков сдвиг меньше (ms ≥ 1 ? ms : ms²)·25 — стоп; узел дольше ×3 расчётного.
        let ms = self.speed.max(1e-3) as f64;
        let n = &mut self.ai.nav;
        if n.ticks - n.check_at >= 100 {
            let need = if ms >= 1.0 { ms } else { ms * ms } * 100.0 * 0.25;
            let moved = ((self.pos[0] - n.check_pos[0]).powi(2) + (self.pos[2] - n.check_pos[2]).powi(2) + (self.pos[1] - n.check_pos[1]).powi(2)).sqrt();
            n.check_at = n.ticks;
            n.check_pos = self.pos;
            if moved < need {
                n.stop();
                return;
            }
        }
        let p = n.path[n.idx];
        if n.node_idx != n.idx {
            n.node_idx = n.idx;
            n.node_at = n.ticks;
            let d = ((p[0] as f64 + 0.5 - self.pos[0]).powi(2) + (p[1] as f64 - self.pos[1]).powi(2) + (p[2] as f64 + 0.5 - self.pos[2]).powi(2)).sqrt();
            n.node_timeout = if ms > 0.0 { d / ms * 20.0 } else { 0.0 };
        } else if n.node_timeout > 0.0 && (n.ticks - n.node_at) as f64 > n.node_timeout * 3.0 {
            n.stop();
            return;
        }
        // Высота цели — верх формы в клетке ступней узла (плита, снежный слой), иначе низ клетки.
        let top = collision_of(w.get_cell(p[0], p[1], p[2])).iter().map(|s| s.hi[1]).fold(0.0f64, f64::max);
        let mult = n.mult;
        self.move_to([p[0] as f64 + 0.5, p[1] as f64 + top, p[2] as f64 + 0.5], mult);
    }

    pub(crate) fn move_to(&mut self, to: [f64; 3], mult: f32) {
        let m = &mut self.ai.mv;
        m.to = to;
        m.mult = mult;
        if m.state != MoveState::Jumping {
            m.state = MoveState::MoveTo;
        }
    }

    /// MoveControl.tick.
    pub(crate) fn move_tick(&mut self, w: &mut WorldData) {
        let attr = self.speed_attr();
        match self.ai.mv.state {
            MoveState::MoveTo => {
                self.ai.mv.state = MoveState::Wait;
                let to = self.ai.mv.to;
                let (dx, dy, dz) = (to[0] - self.pos[0], to[1] - self.pos[1], to[2] - self.pos[2]);
                if dx * dx + dy * dy + dz * dz < 2.5e-7 {
                    self.forward = 0.0;
                    return;
                }
                self.yaw = turn_to(self.yaw, yaw_to(dx, dz), 90.0);
                self.speed = self.ai.mv.mult * attr;
                self.forward = self.speed;
                let (wd, _) = self.dims();
                let c = self.feet_cell();
                let top = collision_of(w.get_cell(c[0], c[1], c[2])).iter().map(|s| s.hi[1]).fold(f64::NEG_INFINITY, f64::max);
                let inside = top.is_finite() && self.pos[1] < c[1] as f64 + top;
                if (dy > STEP as f64 && dx * dx + dz * dz < wd.max(1.0)) || inside {
                    self.ai.jump_req = true;
                    self.ai.mv.state = MoveState::Jumping;
                }
            }
            MoveState::Jumping => {
                self.speed = self.ai.mv.mult * attr;
                self.forward = self.speed;
                if self.on_ground {
                    self.ai.mv.state = MoveState::Wait;
                }
            }
            MoveState::Wait => self.forward = 0.0,
        }
    }

    pub(crate) fn look_at(&mut self, at: [f64; 3], max_yaw: f32, max_pitch: f32) {
        self.ai.look = LookCtl { at, timer: 2, max_yaw, max_pitch };
    }

    /// LookControl.tick: тангаж в 0, к точке — голова не быстрее max_yaw, тангаж до max_pitch; без точки — голова к
    /// телу по 10°/тик; на пути — голова в ±75° от тела.
    pub(crate) fn look_tick(&mut self) {
        self.pitch = 0.0;
        let e = self.eye();
        let l = &mut self.ai.look;
        if l.timer > 0 {
            l.timer -= 1;
            let (dx, dy, dz) = (l.at[0] - e[0], l.at[1] - e[1], l.at[2] - e[2]);
            let h = (dx * dx + dz * dz).sqrt();
            if dx.abs() > 1e-5 || dz.abs() > 1e-5 {
                self.head = change_angle(self.head, yaw_to(dx, dz), l.max_yaw);
            }
            if h > 1e-5 || dy.abs() > 1e-5 {
                let p = -(libm::atan2(dy, h).to_degrees() as f32);
                self.pitch = change_angle(self.pitch, p, l.max_pitch);
            }
        } else {
            self.head = change_angle(self.head, self.body, 10.0);
        }
        if !self.ai.nav.idle() {
            self.head = clamp_angle(self.head, self.body, 75.0);
        }
    }

    /// BodyControl.tick (MobEntity.turnHead).
    pub(crate) fn body_tick(&mut self) {
        if self.moved_xz2() > 2.5e-7 {
            self.body = self.yaw;
            self.head = clamp_angle(self.head, self.body, 75.0);
            self.ai.body_last_head = self.head;
            self.ai.body_still = 0;
            return;
        }
        if (self.head - self.ai.body_last_head).abs() > 15.0 {
            self.ai.body_still = 0;
            self.ai.body_last_head = self.head;
            self.body = clamp_angle(self.body, self.head, 75.0);
        } else {
            self.ai.body_still += 1;
            if self.ai.body_still > 10 {
                let f = ((self.ai.body_still - 10) as f32 / 10.0).clamp(0.0, 1.0);
                self.body = clamp_angle(self.body, self.head, 75.0 * (1.0 - f));
            }
        }
    }

    /// MobEntity.tickNewAi: очереди целей (полный тик — раз в 2), навигация, MoveControl, LookControl, JumpControl.
    pub(crate) fn tick_ai(&mut self, c: &mut AiCtx) {
        self.despawn += 1;
        let full = (self.age.wrapping_add(self.id)) % 2 == 0 || self.age <= 1;
        let mut t = std::mem::take(&mut self.ai.tslots);
        self.run_goals(&mut t, full, c);
        self.ai.tslots = t;
        let mut g = std::mem::take(&mut self.ai.slots);
        self.run_goals(&mut g, full, c);
        self.ai.slots = g;
        self.nav_tick(c.w);
        self.move_tick(c.w);
        self.look_tick();
        self.jumping = std::mem::take(&mut self.ai.jump_req);
    }

    /// GoalSelector.tick: остановить, кто не продолжает; по порядку — старт, если каждый флаг свободен или его держит
    /// цель с большим числом и can_start; затем tick работающих (неполный тик — только «каждый тик»).
    fn run_goals(&mut self, slots: &mut [Slot], full: bool, c: &mut AiCtx) {
        if full {
            for k in 0..slots.len() {
                if slots[k].running && !self.goal_continue(slots[k].g, c) {
                    slots[k].running = false;
                    self.goal_stop(slots[k].g);
                }
            }
            for k in 0..slots.len() {
                if slots[k].running {
                    continue;
                }
                let (pri, flags) = (slots[k].pri, slots[k].flags);
                let free = (0..slots.len()).all(|j| !slots[j].running || slots[j].flags & flags == 0 || pri < slots[j].pri);
                if !free || !self.goal_can_start(slots[k].g, c) {
                    continue;
                }
                for j in 0..slots.len() {
                    if slots[j].running && slots[j].flags & flags != 0 {
                        slots[j].running = false;
                        self.goal_stop(slots[j].g);
                    }
                }
                slots[k].running = true;
                self.goal_start(slots[k].g, c);
            }
        }
        for k in 0..slots.len() {
            if slots[k].running && (full || slots[k].every) {
                self.goal_tick(slots[k].g, c);
            }
        }
    }

    fn panic_fresh(&self, c: &AiCtx) -> bool {
        self.hurt_panics && self.hurt_at.is_some_and(|t| c.time.saturating_sub(t) <= 40)
    }

    fn goal_can_start(&mut self, g: G, c: &mut AiCtx) -> bool {
        match g {
            G::Swim => (self.in_water && self.fluid_h > self.swim_height()) || self.in_lava,
            G::Panic => {
                if !self.panic_fresh(c) {
                    return false;
                }
                match self.find_target(c, 5, 4, true) {
                    Some(p) => self.nav_to(c, p, 0, self.def().panic),
                    None => false,
                }
            }
            G::Tempt => {
                if self.ai.tempt_cd > 0 {
                    self.ai.tempt_cd -= 1;
                    return false;
                }
                let k = self.kind;
                let p = self.nearest_player(c, 10.0, false, |p| !p.creative && (is_food(k, p.hands[0]) || is_food(k, p.hands[1])));
                self.ai.tempt_who = p.map(|p| p.id);
                p.is_some()
            }
            G::EatGrass => {
                let n = if self.baby { 50 } else { 1000 };
                if self.rng.below(n) != 0 {
                    return false;
                }
                let f = self.feet_cell();
                c.w.get_block(f[0], f[1], f[2]) == TALL_GRASS || c.w.get_block(f[0], f[1] - 1, f[2]) == GRASS
            }
            G::Wander => {
                if self.despawn >= 100 || self.rng.below(60) != 0 {
                    return false;
                }
                let (h, v) = if self.in_water { (15, 7) } else { (10, 7) };
                let no_penalty = self.rng.next_f32() >= 0.999;
                match self.find_target(c, h, v, no_penalty) {
                    Some(p) => self.nav_to(c, p, 1, 1.0),
                    None => false,
                }
            }
            G::LookAt => {
                if self.rng.next_f32() >= 0.02 {
                    return false;
                }
                let p = self.nearest_player(c, self.def().look_range, true, |_| true);
                self.ai.look_who = p.map(|p| p.id);
                p.is_some()
            }
            G::LookAround => self.rng.next_f32() < 0.02,
            G::Melee => {
                if c.time.saturating_sub(self.ai.melee_check) < 20 {
                    return false;
                }
                self.ai.melee_check = c.time;
                let Some(t) = self.target.and_then(|id| self.player(c, id).copied()) else { return false };
                if t.creative {
                    return false;
                }
                self.in_attack_range(&t) || self.nav_to(c, cell_of(t.pos), 0, 1.0)
            }
            G::Revenge => {
                let Some((pid, at)) = self.attacker else { return false };
                if self.ai.revenge_at == Some(at) {
                    return false;
                }
                self.player(c, pid).is_some_and(|p| !p.creative)
            }
            G::Hunt => {
                if self.rng.below(5) != 0 {
                    return false;
                }
                let r = self.def().follow;
                let p = self.nearest_player(c, r, true, |p| !p.creative);
                if let Some(p) = p {
                    self.target = Some(p.id);
                }
                p.is_some()
            }
        }
    }

    fn goal_continue(&mut self, g: G, c: &mut AiCtx) -> bool {
        match g {
            G::Swim => self.goal_can_start(G::Swim, c),
            G::Panic | G::Wander => !self.ai.nav.idle(),
            G::Tempt => {
                let k = self.kind;
                self.ai.tempt_who.and_then(|id| self.player(c, id).copied()).is_some_and(|p| {
                    let d2 = (0..3).map(|i| (p.pos[i] - self.pos[i]).powi(2)).sum::<f64>();
                    let r = if p.crouch { 8.0 } else { 10.0 };
                    d2 <= r * r && !p.creative && (is_food(k, p.hands[0]) || is_food(k, p.hands[1]))
                })
            }
            G::EatGrass => self.eating > 0,
            G::LookAt => {
                self.ai.look_left > 0
                    && self.ai.look_who.and_then(|id| self.player(c, id)).is_some_and(|p| {
                        let r = self.def().look_range;
                        (0..3).map(|i| (p.pos[i] - self.pos[i]).powi(2)).sum::<f64>() <= r * r
                    })
            }
            G::LookAround => self.ai.around_left >= 0,
            G::Melee => self.target.and_then(|id| self.player(c, id)).is_some_and(|p| !p.creative),
            G::Revenge | G::Hunt => {
                let Some(t) = self.target.and_then(|id| self.player(c, id).copied()) else { return false };
                let r = self.def().follow;
                let d2 = (0..3).map(|i| (t.pos[i] - self.pos[i]).powi(2)).sum::<f64>();
                if t.creative || d2 > r * r {
                    return false;
                }
                if line_of_sight(c.w, self.eye(), t.eye) {
                    self.ai.unseen = 0;
                } else {
                    self.ai.unseen += 2; // проверка раз в 2 тика
                    if self.ai.unseen > self.ai.unseen_max {
                        return false;
                    }
                }
                true
            }
        }
    }

    fn goal_start(&mut self, g: G, c: &mut AiCtx) {
        match g {
            G::Swim | G::Panic | G::Wander => {}
            G::Tempt => {}
            G::EatGrass => {
                self.eating = 40;
                self.ai.nav.stop();
            }
            G::LookAt => self.ai.look_left = 40 + self.rng.below(40),
            G::LookAround => {
                let a = std::f64::consts::TAU * self.rng.next_f64();
                self.ai.around = [libm::cos(a), libm::sin(a)];
                self.ai.around_left = 20 + self.rng.below(20);
            }
            G::Melee => {
                self.ai.melee_repath = 0;
                self.ai.melee_cd = 0;
                self.ai.melee_ticks = 0;
                self.ai.melee_seen_at = None;
            }
            G::Revenge => {
                let (pid, at) = self.attacker.unwrap();
                self.ai.revenge_at = Some(at);
                self.target = Some(pid);
                self.ai.unseen = 0;
                self.ai.unseen_max = 300;
                c.revenge.push((self.id, pid));
            }
            G::Hunt => {
                self.ai.unseen = 0;
                self.ai.unseen_max = 60;
            }
        }
    }

    fn goal_stop(&mut self, g: G) {
        match g {
            G::Panic | G::Wander => self.ai.nav.stop(),
            G::Tempt => {
                self.ai.tempt_who = None;
                self.ai.tempt_cd = 50; // 100 тиков (тики цели — через один)
                self.ai.nav.stop();
            }
            G::EatGrass => self.eating = 0,
            G::LookAt => self.ai.look_who = None,
            G::Melee => {
                self.arms = false;
                self.ai.nav.stop();
            }
            G::Revenge | G::Hunt => self.target = None,
            G::Swim | G::LookAround => {}
        }
    }

    fn goal_tick(&mut self, g: G, c: &mut AiCtx) {
        match g {
            G::Swim => {
                if self.rng.next_f32() < 0.8 {
                    self.ai.jump_req = true;
                }
            }
            G::Panic | G::Wander => {}
            G::Tempt => {
                let Some(p) = self.ai.tempt_who.and_then(|id| self.player(c, id).copied()) else { return };
                self.look_at(p.eye, 95.0, 40.0);
                let d2 = (0..3).map(|i| (p.pos[i] - self.pos[i]).powi(2)).sum::<f64>();
                if d2 < 6.25 {
                    self.ai.nav.stop();
                } else {
                    let mult = match self.kind {
                        MobKind::Pig => 1.2,
                        MobKind::Cow => 1.25,
                        MobKind::Sheep => 1.1,
                        _ => 1.0,
                    };
                    self.nav_to(c, cell_of(p.pos), 0, mult);
                }
            }
            G::EatGrass => {
                self.eating = (self.eating - 2).max(0); // тик цели — раз в 2 игровых
                if self.eating == 4 {
                    let f = self.feet_cell();
                    if c.w.get_block(f[0], f[1], f[2]) == TALL_GRASS {
                        c.out.push(Out::EatGrass { pos: f });
                    } else if c.w.get_block(f[0], f[1] - 1, f[2]) == GRASS {
                        c.out.push(Out::EatGrass { pos: [f[0], f[1] - 1, f[2]] });
                    }
                    self.sheared = false; // onEatingGrass: шерсть отросла
                }
            }
            G::LookAt => {
                self.ai.look_left -= 2;
                if let Some(p) = self.ai.look_who.and_then(|id| self.player(c, id).copied()) {
                    self.look_at(p.eye, 10.0, 40.0);
                }
            }
            G::LookAround => {
                self.ai.around_left -= 1;
                let e = self.eye();
                let a = self.ai.around;
                self.look_at([e[0] + a[0], e[1], e[2] + a[1]], 10.0, 40.0);
            }
            G::Melee => self.melee_tick(c),
            G::Revenge | G::Hunt => {}
        }
    }

    /// Коробка удара (getAttackBox: своя, расширенная на √2.04 − 0.6 по X и Z) пересекает коробку цели.
    pub(crate) fn in_attack_range(&self, t: &Seen) -> bool {
        let e = (2.04f64).sqrt() - 0.6;
        let b = self.bbox();
        let a = Aabb { lo: [b.lo[0] - e, b.lo[1], b.lo[2] - e], hi: [b.hi[0] + e, b.hi[1], b.hi[2] + e] };
        a.intersects(&t.bbox)
    }

    /// MeleeAttackGoal.tick (каждый тик) + ZombieAttackGoal (руки).
    fn melee_tick(&mut self, c: &mut AiCtx) {
        let Some(t) = self.target.and_then(|id| self.player(c, id).copied()) else { return };
        self.look_at(t.eye, 30.0, 30.0);
        self.ai.melee_repath = (self.ai.melee_repath - 1).max(-1);
        let d2 = (0..3).map(|i| (t.pos[i] - self.pos[i]).powi(2)).sum::<f64>();
        let seen = line_of_sight(c.w, self.eye(), t.eye);
        let moved = self.ai.melee_seen_at.map_or(true, |p| (0..3).map(|i| (t.pos[i] - p[i]).powi(2)).sum::<f64>() >= 1.0);
        if seen && self.ai.melee_repath <= 0 && (moved || self.rng.next_f32() < 0.05) {
            self.ai.melee_seen_at = Some(t.pos);
            let mut cd = 4 + self.rng.below(7);
            if d2 > 1024.0 {
                cd += 10;
            } else if d2 > 256.0 {
                cd += 5;
            }
            if !self.nav_to(c, cell_of(t.pos), 0, 1.0) {
                cd += 15;
            }
            self.ai.melee_repath = cd;
        }
        self.ai.melee_cd = (self.ai.melee_cd - 1).max(0);
        if self.in_attack_range(&t) && self.ai.melee_cd <= 0 && seen {
            self.ai.melee_cd = 20;
            let amount = self.def().attack;
            c.out.push(Out::Hit { mob: self.id, player: t.id, amount, from: self.pos });
        }
        self.ai.melee_ticks += 1;
        self.arms = self.ai.melee_ticks >= 5 && self.ai.melee_cd < 10;
    }

    /// Точка блуждания или бегства (FuzzyTargeting / NoPenaltyTargeting): лучшая по «вкусу» из 10 случайных в ±h × ±v.
    /// Fuzzy поднимает из земли; обе — без воды и без штрафа пути; под клеткой — непрозрачный полный блок.
    pub(crate) fn find_target(&mut self, c: &mut AiCtx, h: i32, v: i32, no_penalty: bool) -> Option<[i32; 3]> {
        let base = self.feet_cell();
        let spec = self.spec();
        let mut best: Option<([i32; 3], f32)> = None;
        for _ in 0..10 {
            let dx = self.rng.below(2 * h + 1) - h;
            let dy = self.rng.below(2 * v + 1) - v;
            let dz = self.rng.below(2 * h + 1) - h;
            let mut p = [base[0] + dx, base[1] + dy, base[2] + dz];
            if p[1] < 1 || p[1] >= H - 2 {
                continue;
            }
            if !no_penalty {
                let mut k = 0;
                while k < 16 && p[1] < H - 2 && path::solid_at(c.w, p) {
                    p[1] += 1;
                    k += 1;
                }
            }
            let below = c.w.get_cell(p[0], p[1] - 1, p[2]);
            if !crate::registry::blocks::opaque(crate::cell::block_of(below)) || collision_of(below).is_empty() {
                continue;
            }
            if c.w.fluid(p[0], p[1], p[2]).0 != 0 {
                continue;
            }
            match path::node_penalty(c.w, p, &spec) {
                Some(pen) if pen == 0.0 => {}
                _ => continue,
            }
            let favor = self.favor(c, p);
            if best.is_none_or(|(_, f)| favor > f) {
                best = Some((p, favor));
            }
        }
        best.map(|(p, _)| p)
    }

    /// «Вкус» клетки (getPathfindingFavor): животным — 10 над блоком травы, иначе яркость − 0.5; монстрам — 0.5 − яркость.
    pub(crate) fn favor(&self, c: &mut AiCtx, p: [i32; 3]) -> f32 {
        if self.def().hostile {
            return 0.5 - brightness(light_at(c.w, p, c.dark));
        }
        if super::is_grass_block(c.w, p[0], p[1] - 1, p[2]) {
            10.0
        } else {
            brightness(light_at(c.w, p, c.dark)) - 0.5
        }
    }

    /// Голос (MobEntity.baseTick): rand(1000) < счётчик++ → звук, счётчик = −задержка.
    pub(crate) fn tick_ambient(&mut self, ev: &mut Events, fx: &mut GameRng) {
        if !self.alive() {
            return;
        }
        let n = self.ambient;
        self.ambient += 1;
        if self.rng.below(1000) < n {
            self.ambient = -self.def().ambient_delay;
            self.voice(ev, fx, 0);
        }
    }

    /// Курица несёт яйцо раз в 6000–11999 тиков (только взрослая): звук и предмет у ступней. true — снесла.
    pub(crate) fn tick_egg(&mut self, ev: &mut Events, fx: &mut GameRng) -> bool {
        if self.kind != MobKind::Chicken || self.baby || !self.alive() {
            return false;
        }
        self.egg -= 1;
        if self.egg > 0 {
            return false;
        }
        self.egg = 6000 + self.rng.below(6000);
        let p = (fx.next_f32() - fx.next_f32()) * 0.2 + 1.0;
        self.sound(ev, Sfx::ChickenEgg, p);
        true
    }
}

fn cell_of(p: [f64; 3]) -> [i32; 3] {
    [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32]
}
