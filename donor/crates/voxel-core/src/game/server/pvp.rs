//! PvP на сервере (G10, MP-7): удар игрока по игроку (Act::Attack по номеру игрока), проверки сервера (дальность и
//! видимость цели — античит), урон через survival (окно неуязвимости, броня и её износ, смерть с выпадением вещей),
//! отбрасывание по сети (Msg::Velocity → VelocityAck), защита Перекрёстка и метка боя.
//!
//! Защита (spawn.md, карточка G10): в зоне Перекрёстка (`interact::protected`) и ещё `GRACE_TICKS` (10 с) после выхода
//! из неё игрока бить нельзя. Тот, кто сам ударил игрока (не под защитой), получает метку боя на `TAG_TICKS` (10 с) и
//! защиту теряет: «ударил и убежал в зону» не спасает. Бить игрок может и под защитой (иначе метка была бы ни к чему) —
//! первый же удар по незащищённому её снимает. Урон не-игроков (падение, мобы) зона режет, как прежде (`hurt_hit`).
//!
//! Числа удара — PlayerEntity.attack 1.21.1 (как по мобам, `strike_of`): урон оружия × (0.2 + 0.8·h²), крит ×1.5 в
//! падении, отбрасывание 0.4 от бьющего и добавочное 0.5 бегом (после полной перезарядки).
//! Подмодуль server.rs: пользуется его закрытыми полями.

use super::mobs::{knock, ATTACK_SLACK};
use super::*;
use crate::abi::{Ev, Sfx};
use crate::game::client::ClientSim;
use crate::game::health::DamageKind;
use crate::game::interact::{feet, protected, ENTITY_REACH};
use crate::registry::items::tool_attack;

/// Защита после выхода из зоны Перекрёстка, тиков (10 с).
pub(crate) const GRACE_TICKS: i32 = 200;
/// Метка боя после удара по игроку, тиков (10 с): защиты зоны на это время нет.
pub(crate) const TAG_TICKS: i32 = 200;

/// Удар игрока: что получилось из руки, перезарядки и позы.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Strike {
    pub(crate) dmg: f32,
    /// Перезарядка больше 0.9 (полный удар).
    pub(crate) strong: bool,
    pub(crate) crit: bool,
    /// Добавочное отбрасывание бегом (1 — есть).
    pub(crate) kb: f32,
}

impl Strike {
    /// Звук удара (Sfx 19…22): отбрасывание бегом, крит, полный, слабый.
    pub(crate) fn sfx(&self) -> Sfx {
        if self.kb > 0.0 {
            Sfx::AttackKnockback
        } else if self.crit {
            Sfx::AttackCrit
        } else if self.strong {
            Sfx::AttackStrong
        } else {
            Sfx::AttackWeak
        }
    }
}

impl ServerSim {
    /// Раз в тик сервера (до игроков): метка боя убывает; в зоне защита полная, вне неё — 10 с на убыль.
    pub(crate) fn tick_pvp(&mut self) {
        let d = self.sim.world.d.clone();
        let w = d.borrow();
        for p in self.players.iter_mut() {
            p.pvp_tag = (p.pvp_tag - 1).max(0);
            if protected(&w, feet(&p.player)) {
                p.zone_grace = GRACE_TICKS;
            } else {
                p.zone_grace = (p.zone_grace - 1).max(0);
            }
        }
    }

    /// Защищён ли игрок i от ударов игроков: в зоне или не позже 10 с после выхода, и сам никого недавно не бил.
    pub(crate) fn pvp_shielded(&self, i: usize) -> bool {
        let p = &self.players[i];
        p.zone_grace > 0 && p.pvp_tag == 0
    }

    /// Удар игрока i по цели с номером target: моб или игрок.
    pub(crate) fn attack_entity(&mut self, i: usize, target: u32) {
        if self.sim.mobs.index(target).is_some() {
            self.attack_mob(i, target);
        } else {
            self.attack_player(i, target);
        }
    }

    /// Удар по перезарядке и позе (PlayerEntity.attack): урон оружия × (0.2 + 0.8·h²), h — доля перезарядки;
    /// полный (h > 0.9) удар бегом добавляет отбрасывание, в падении без бега — крит ×1.5.
    pub(crate) fn strike_of(&self, i: usize) -> Strike {
        let p = &self.players[i];
        let pl = &p.player;
        let (base, speed) = tool_attack(p.inv.hand().id()).unwrap_or((1.0, 4.0));
        let per = 20.0 / speed;
        let h = ((p.atk_ticks as f32 + 0.5) / per).clamp(0.0, 1.0);
        let mut dmg = base * (0.2 + h * h * 0.8);
        let strong = h > 0.9;
        let kb = if pl.sprinting && strong { 1.0 } else { 0.0 };
        let crit = strong && pl.fall > 0.0 && !pl.on_ground && !pl.in_water && !pl.sprinting && !pl.flying;
        if crit {
            dmg *= 1.5;
        }
        Strike { dmg, strong, crit, kb }
    }

    /// Act::Attack по игроку: цель жива и не он сам; дальность (3 + запас 3, как canInteractWithEntity) и видимость
    /// (хоть одна из трёх точек тела — ступни, середина, глаза — не за сплошным блоком) — иначе отказ; перезарядка
    /// сбрасывается; урон — через survival (`PlayerAttack`: окно, броня, смерть); полный удар отбрасывает (0.4 от
    /// бьющего), бег добавляет 0.5 вдоль взгляда.
    pub(crate) fn attack_player(&mut self, i: usize, target: u32) {
        let Some(j) = self.idx(target) else { return };
        if j == i || !self.players[j].online || !self.players[j].hp.alive() {
            return;
        }
        let eye = self.players[i].player.eye(1.0);
        let bx = self.players[j].player.bbox();
        let d2: f64 = (0..3).map(|a| (eye[a].clamp(bx.lo[a], bx.hi[a]) - eye[a]).powi(2)).sum();
        let reach = ENTITY_REACH + ATTACK_SLACK;
        if !(d2 <= reach * reach) {
            self.stats.rejected += 1;
            return;
        }
        let (mx, mz) = ((bx.lo[0] + bx.hi[0]) / 2.0, (bx.lo[2] + bx.hi[2]) / 2.0);
        let body = [bx.lo[1] + 0.1, (bx.lo[1] + bx.hi[1]) / 2.0, bx.hi[1] - 0.1];
        let seen = {
            let d = self.sim.world.d.clone();
            let mut w = d.borrow_mut();
            body.iter().any(|&y| ClientSim::line_clear_in(&mut w, eye, [mx, y, mz]))
        };
        if !seen {
            self.stats.rejected += 1;
            return;
        }
        let s = self.strike_of(i);
        let (id, from, yaw) = (self.players[i].id, self.players[i].player.pos, self.players[i].player.yaw);
        self.players[i].atk_ticks = 0;
        if !self.pvp_shielded(j) {
            self.players[i].pvp_tag = TAG_TICKS; // ударил незащищённого: своей защиты на 10 с нет
        }
        let Some(hit) = self.hurt_hit(j, DamageKind::PlayerAttack, s.dmg, Some(id)) else { return };
        let feet = self.players[i].feet_cell();
        self.events.push(Ev::Sound as i32, feet, s.sfx() as i32);
        if !hit.died {
            self.knock_player(j, hit.full.then_some(from), (s.kb > 0.0).then_some(yaw));
        }
    }

    /// Отбрасывание игрока j (takeKnockback): `from` — откуда бьют (0.4 от этой точки, только полный удар); `sprint_yaw` —
    /// добавочное 0.5 вдоль взгляда бьющего. Скорость уходит клиенту (Msg::Velocity), теневое тело берёт её по VelocityAck.
    pub(crate) fn knock_player(&mut self, j: usize, from: Option<[f64; 3]>, sprint_yaw: Option<f32>) {
        let p = &mut self.players[j];
        let (pos, ground) = (p.player.pos, p.player.on_ground);
        let mut v = p.kb_pending.map_or(p.player.vel, |(_, v)| v);
        if let Some(f) = from {
            let (dx, dz) = (f[0] - pos[0], f[2] - pos[2]);
            p.hp.tilt_yaw = (libm::atan2(dz, dx).to_degrees() as f32) - p.player.yaw;
            v = knock(v, ground, 0.4, dx, dz);
        }
        if let Some(yaw) = sprint_yaw {
            let r = (yaw as f64).to_radians();
            v = knock(v, ground, 0.5, libm::sin(r), -libm::cos(r));
        }
        if from.is_none() && sprint_yaw.is_none() {
            return;
        }
        p.kb_next = p.kb_next.wrapping_add(1).max(1);
        p.kb_pending = Some((p.kb_next, v));
        p.own.push(Msg::Velocity { id: p.kb_next, v });
    }
}
