//! Мобы у клиента: зеркало по Spawn / MobMoves / Despawn / Anim, отрисовка с отставанием ~2 тика сервера по общему
//! курсору (номер тика — Bundle.tick, ход приходит только при изменении: неподвижный между ходами), ноги
//! (LimbAnimator: speed += (min(4·ход, 1) − speed)·0.4), красная вспышка 10 тиков и «дёрг» ног (speed = 1.5) от урона,
//! крен смерти, крылья курицы, шея овцы, шаги (звук «у ступней» вида, каждые 1.67 блока по земле); луч прицела по
//! коробкам мобов (дальность удара 3); данные кадра для JS (web/mobs.js).

use std::collections::{BTreeMap, VecDeque};

use super::ai::seg_box_t;
use super::MobKind;
use crate::abi::Ev;
use crate::game::view::Events;
use crate::proto::{MobMove, MM_BABY, MM_DEAD, MM_EAT, MM_GROUND, MM_SHEARED, MM_WATER};
use crate::registry::shapes::Aabb;

/// Отставание отрисовки от последнего тика сервера и подстройка (как у других игроков).
const DELAY: f64 = 2.0;
const LOW: f64 = 1.5;
const HIGH: f64 = 3.0;

pub(crate) struct MobView {
    pub(crate) kind: MobKind,
    samples: VecDeque<(u64, MobMove)>,
    limb: f32,
    limb_prev: f32,
    amt: f32,
    amt_prev: f32,
    hurt: i32,
    dead_at: Option<u64>,
    eat_at: Option<u64>,
    step_dist: f32,
    step_next: f32,
    age: u32,
    // Курица: раскрытие крыльев, скорость и фаза взмаха (ChickenEntity.tickMovement).
    wing_dev: f32,
    wing_speed: f32,
    flap: f32,
    flap_prev: f32,
    dev_prev: f32,
}

impl MobView {
    fn new(kind: MobKind, tick: u64, mv: MobMove) -> MobView {
        let mut samples = VecDeque::new();
        samples.push_back((tick, mv));
        MobView {
            kind,
            samples,
            limb: 0.0,
            limb_prev: 0.0,
            amt: 0.0,
            amt_prev: 0.0,
            hurt: 0,
            dead_at: (mv.flags & MM_DEAD != 0).then_some(tick),
            eat_at: (mv.flags & MM_EAT != 0).then_some(tick),
            step_dist: 0.0,
            step_next: 1.0,
            age: 0,
            wing_dev: 0.0,
            wing_speed: 1.0,
            flap: 0.0,
            flap_prev: 0.0,
            dev_prev: 0.0,
        }
    }
    fn push(&mut self, tick: u64, mv: MobMove) {
        let Some(&(t, last)) = self.samples.back() else { return };
        if tick <= t {
            if tick == t {
                self.samples.back_mut().unwrap().1 = mv;
            }
            return;
        }
        // Между ходами моб стоял: повторить прошлое на тике перед этим (иначе сдвиг размажется на все пропущенные тики).
        if tick > t + 1 {
            self.samples.push_back((tick - 1, last));
        }
        let d2: f32 = (0..3).map(|k| (mv.pos[k] - last.pos[k]).powi(2)).sum();
        if d2 > 64.0 {
            self.samples.clear(); // телепорт: без пролёта
        }
        if mv.flags & MM_DEAD != 0 && self.dead_at.is_none() {
            self.dead_at = Some(tick);
        }
        match (mv.flags & MM_EAT != 0, self.eat_at) {
            (true, None) => self.eat_at = Some(tick),
            (false, Some(_)) => self.eat_at = None,
            _ => {}
        }
        self.samples.push_back((tick, mv));
        while self.samples.len() > 40 {
            self.samples.pop_front();
        }
    }
    /// Состояние в момент t (тики сервера): позиция линейно, углы по короткой дуге; вне — крайний ход.
    pub(crate) fn at(&self, t: f64) -> MobMove {
        let s = &self.samples;
        let (first, last) = (s.front().unwrap(), s.back().unwrap());
        if t <= first.0 as f64 {
            return first.1;
        }
        if t >= last.0 as f64 {
            return last.1;
        }
        let k = s.iter().position(|e| e.0 as f64 > t).unwrap_or(s.len() - 1);
        let (a, b) = (&s[k - 1], &s[k]);
        let f = ((t - a.0 as f64) / (b.0 - a.0) as f64) as f32;
        let ang = |x: f32, y: f32| x + super::ai::wrap(y - x) * f;
        let (p, q) = (a.1, b.1);
        MobMove {
            id: p.id,
            pos: std::array::from_fn(|i| p.pos[i] + (q.pos[i] - p.pos[i]) * f),
            body: ang(p.body, q.body),
            head: ang(p.head, q.head),
            pitch: p.pitch + (q.pitch - p.pitch) * f,
            flags: if f < 0.5 { p.flags } else { q.flags },
            var: q.var,
        }
    }
}

/// Зеркало мобов клиента и общий курсор отрисовки.
#[derive(Default)]
pub(crate) struct MobMirror {
    pub(crate) list: BTreeMap<u32, MobView>,
    pub(crate) t: f64,
    t_prev: f64,
    latest: u64,
}

impl MobMirror {
    pub(crate) fn spawn(&mut self, id: u32, kind: u8, mv: MobMove, tick: u64) {
        let Some(kind) = MobKind::from_u8(kind) else { return };
        self.latest = self.latest.max(tick);
        if self.list.is_empty() || self.t == 0.0 {
            self.t = tick as f64 - DELAY;
            self.t_prev = self.t;
        }
        self.list.insert(id, MobView::new(kind, tick, mv));
    }
    pub(crate) fn moves(&mut self, list: Vec<MobMove>, tick: u64) {
        self.latest = self.latest.max(tick);
        for mv in list {
            if let Some(m) = self.list.get_mut(&mv.id) {
                m.push(tick, mv);
            }
        }
    }
    pub(crate) fn remove(&mut self, ids: &[u32]) {
        for id in ids {
            self.list.remove(id);
        }
    }
    /// Урон (Msg::Anim kind 1): красная вспышка 10 тиков, ноги — полный размах.
    pub(crate) fn hurt(&mut self, id: u32) {
        if let Some(m) = self.list.get_mut(&id) {
            m.hurt = 10;
            m.amt = 1.5;
        }
    }
    /// Тик клиента: курсор +1 (подстройка к отставанию ~2 тика), ноги, крылья, шаги.
    pub(crate) fn advance(&mut self, last_tick: u64, ev: &mut Events) {
        self.latest = self.latest.max(last_tick);
        let latest = self.latest as f64;
        let lag = latest - self.t;
        let step = if lag > HIGH {
            1.1
        } else if lag < LOW {
            0.9
        } else {
            1.0
        };
        self.t_prev = self.t;
        self.t = (self.t + step).min(latest);
        if latest - self.t > 4.0 * HIGH {
            self.t = latest - DELAY;
            self.t_prev = self.t;
        }
        let (t0, t1) = (self.t_prev, self.t);
        for m in self.list.values_mut() {
            let (a, b) = (m.at(t0), m.at(t1));
            let d = ((b.pos[0] - a.pos[0]).powi(2) + (b.pos[2] - a.pos[2]).powi(2)).sqrt();
            m.age = m.age.wrapping_add(1);
            m.amt_prev = m.amt;
            m.limb_prev = m.limb;
            let dead = b.flags & MM_DEAD != 0;
            let target = if dead { 0.0 } else { (d * 4.0).min(1.0) };
            m.amt += (target - m.amt) * 0.4;
            m.limb += m.amt;
            if m.hurt > 0 {
                m.hurt -= 1;
            }
            // Курица: крылья раскрыты в воздухе, складываются на земле; взмах ~5.7 Гц.
            let ground = b.flags & MM_GROUND != 0;
            m.dev_prev = m.wing_dev;
            m.flap_prev = m.flap;
            m.wing_dev = (m.wing_dev + if ground { -1.0 } else { 4.0 } * 0.3).clamp(0.0, 1.0);
            if !ground && m.wing_speed < 1.0 {
                m.wing_speed = 1.0;
            }
            m.wing_speed *= 0.9;
            m.flap += m.wing_speed * 2.0;
            // Шаги (Entity.move): путь ×0.6, звук на переходе через целое — по земле, не в воде.
            let d3 = ((b.pos[0] - a.pos[0]).powi(2) + (b.pos[1] - a.pos[1]).powi(2) + (b.pos[2] - a.pos[2]).powi(2)).sqrt();
            m.step_dist += d3 * 0.6;
            if m.step_dist > m.step_next && ground && b.flags & MM_WATER == 0 && !dead {
                m.step_next = m.step_dist.floor() + 1.0;
                let feet = [b.pos[0].floor() as i32, b.pos[1].floor() as i32, b.pos[2].floor() as i32];
                ev.push(Ev::Sound as i32, feet, m.kind.def().snd[3] as i32);
            }
        }
    }

    /// Ближайший живой моб на луче из глаз eye по направлению dir не дальше reach: (номер, расстояние).
    pub(crate) fn pick(&self, eye: [f64; 3], dir: [f64; 3], reach: f64) -> Option<(u32, f64)> {
        let b = [eye[0] + dir[0] * reach, eye[1] + dir[1] * reach, eye[2] + dir[2] * reach];
        let mut best: Option<(u32, f64)> = None;
        for (&id, m) in &self.list {
            let s = m.at(self.t);
            if s.flags & MM_DEAD != 0 {
                continue;
            }
            let bx = mob_box(m.kind, &s);
            let t = if bx.lo.iter().zip(bx.hi.iter()).zip(eye.iter()).all(|((lo, hi), e)| *lo <= *e && *e <= *hi) { Some(0.0) } else { seg_box_t(eye, b, &bx) };
            if let Some(t) = t {
                let d = t * reach;
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((id, d));
                }
            }
        }
        best
    }

    /// Флаги моба сейчас (для предсказания ПКМ: острижена ли овца).
    pub(crate) fn flags(&self, id: u32) -> Option<(MobKind, u8)> {
        self.list.get(&id).map(|m| (m.kind, m.at(self.t).flags))
    }

    /// Кадр для JS: по DRAW_STRIDE чисел на моба (см. web/mobs.js) — номер, вид, ступни (x, y, z), тело°, голова от тела°,
    /// тангаж°, фаза ног, размах ног, красный (0/1), крен смерти°, флаги MM_*, вариант (цвет овцы), возраст с долей тика,
    /// доп. угол (курица — крылья, рад; овца — тики поедания травы, −1 — нет).
    pub(crate) fn draws(&self, alpha: f64) -> Vec<f32> {
        let a = alpha as f32;
        let t = self.t_prev + (self.t - self.t_prev) * alpha;
        let mut v = Vec::with_capacity(self.list.len() * DRAW_STRIDE);
        for (&id, m) in &self.list {
            let s = m.at(t);
            let dead = s.flags & MM_DEAD != 0;
            let roll = match m.dead_at {
                Some(at) if dead => {
                    let f = (((t - at as f64) as f32 - 1.0) / 20.0 * 1.6).max(0.0).sqrt().min(1.0);
                    f * 90.0
                }
                _ => 0.0,
            };
            let limb = m.limb_prev + (m.limb - m.limb_prev) * a;
            let amt = if dead { 0.0 } else { (m.amt_prev + (m.amt - m.amt_prev) * a).min(1.0) };
            let red = if m.hurt > 0 || dead { 1.0 } else { 0.0 };
            let extra = match m.kind {
                MobKind::Chicken => {
                    let flap = m.flap_prev + (m.flap - m.flap_prev) * a;
                    let dev = m.dev_prev + (m.wing_dev - m.dev_prev) * a;
                    (libm::sinf(flap) + 1.0) * dev
                }
                MobKind::Sheep => m.eat_at.map_or(-1.0, |at| (t - at as f64) as f32),
                _ => 0.0,
            };
            let baby = s.flags & MM_BABY != 0;
            let limb = if baby { limb * 3.0 } else { limb };
            v.extend_from_slice(&[
                id as f32,
                m.kind as u8 as f32,
                s.pos[0],
                s.pos[1],
                s.pos[2],
                s.body,
                super::ai::wrap(s.head - s.body),
                s.pitch,
                limb,
                amt,
                red,
                roll,
                s.flags as f32,
                s.var as f32,
                m.age as f32 + a,
                extra,
            ]);
        }
        v
    }

    /// Острижена ли (для звука и предсказания).
    pub(crate) fn sheared(&self, id: u32) -> bool {
        self.flags(id).is_some_and(|(_, f)| f & MM_SHEARED != 0)
    }
}

/// Чисел на моба в `MobMirror::draws`.
pub(crate) const DRAW_STRIDE: usize = 16;

/// Коробка моба по состоянию хода.
pub(crate) fn mob_box(kind: MobKind, s: &MobMove) -> Aabb {
    let d = kind.def();
    let k = if s.flags & MM_BABY != 0 { 0.5 } else { 1.0 };
    let (w, h) = (d.w * k, d.h * k);
    let p = s.pos.map(|v| v as f64);
    Aabb { lo: [p[0] - w / 2.0, p[1], p[2] - w / 2.0], hi: [p[0] + w / 2.0, p[1] + h, p[2] + w / 2.0] }
}

// ---------------------------------------------------------------- у клиента: прицел, удар, ПКМ, кадр

use crate::game::client::ClientSim;
use crate::game::interact::ENTITY_REACH;
use crate::proto::Act;
use crate::registry::items::{idef, I_SHEARS};
use crate::world::stack::ItemStack;
use crate::world::WorldData;

impl ClientSim {
    /// Прицел по мобам (GameRenderer.updateTargetedEntity): моб на луче ближе 3 блоков и ближе блока — цель удара;
    /// тогда блок под прицелом не выбран (ни рамки, ни ломания).
    pub(crate) fn aim_mobs(&mut self, _w: &mut WorldData) {
        self.aim_mob = None;
        if self.creative() && self.mobs.list.is_empty() {
            return;
        }
        let eye = self.ctl.player.eye(1.0);
        let dir = self.ctl.player.look();
        let Some((id, d)) = self.mobs.pick(eye, dir, ENTITY_REACH) else { return };
        let block = self.ctl.target.map(|h| ((0..3).map(|k| (h.point[k] - eye[k]).powi(2)).sum::<f64>()).sqrt());
        if block.is_none_or(|b| d < b) {
            self.aim_mob = Some(id);
            self.ctl.target = None;
        }
    }

    /// ЛКМ по мобу под прицелом (doAttack → attackEntity): удар серверу и взмах. true — был моб.
    pub(crate) fn attack_aimed_mob(&mut self) -> bool {
        let Some(id) = self.aim_mob else { return false };
        self.sync_sel();
        self.acts.push(Act::Attack { target: id });
        self.swing();
        true
    }

    /// ПКМ по мобу (interactEntity): стрижка овцы ножницами — предсказание (ножницы −1 прочности) и действие серверу.
    /// false — моб не отвечает на ПКМ (дальше — обычное использование предмета).
    pub(crate) fn use_aimed_mob(&mut self) -> bool {
        let Some(id) = self.aim_mob else { return false };
        let Some((kind, flags)) = self.mobs.flags(id) else { return false };
        let held = self.ctl.inv.hand();
        let shear = held.id() == I_SHEARS && kind == MobKind::Sheep && flags & (MM_SHEARED | MM_BABY | crate::proto::MM_DEAD) == 0;
        if !shear {
            return false;
        }
        self.sync_sel();
        let worn = ItemStack { damage: held.damage + 1, ..held };
        let slot = self.ctl.inv.selected;
        self.ctl.inv.set(slot, if worn.damage >= idef(I_SHEARS).max_damage { ItemStack::EMPTY } else { worn });
        self.acts.push(Act::UseEntity { target: id, hand: 0 });
        self.swing();
        true
    }

    /// Мобы в кадре (DRAW_STRIDE чисел на моба).
    pub(crate) fn mob_draws(&self, alpha: f64) -> Vec<f32> {
        self.mobs.draws(alpha)
    }
}
