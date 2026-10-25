//! Мобы на сервере (G6): снимок игроков для ИИ, тик мобов после мира и удары по игрокам через survival (броня, окно,
//! смерть) с отбрасыванием (Msg::Velocity → клиент ставит скорость и подтверждает VelocityAck в тике — тогда теневой
//! тик сервера идёт с той же скоростью); удар игрока по мобу (Act::Attack: дальность, перезарядка удара, крит,
//! отбрасывание, звук); ПКМ по мобу (стрижка овцы); слежение — Ent::Mob / Msg::MobMoves в радиусе TRACK_MOBS.
//! Подмодуль server.rs: пользуется его закрытыми полями.

use super::*;
use crate::abi::{Ev, Sfx};
use crate::game::health::DamageKind;
use crate::game::mob::{Blow, MobKind, Out, Seen};
use crate::registry::ids::WHITE_WOOL;
use crate::registry::items::I_SHEARS;

/// Радиус слежения за мобами от ступней (ваниль: животные 10 кусков, зомби 8; у нас — до заселённого кольца).
pub(crate) const TRACK_MOBS: f64 = 128.0;
/// Дальность удара по сущности (entity_interaction_range 3) и запас сервера (+3, как canInteractWithEntity).
pub(crate) const ATTACK_SLACK: f64 = 3.0;

impl ServerSim {
    /// Игроки, какими их видит ИИ мобов.
    pub(crate) fn seen_players(&self) -> Vec<Seen> {
        self.players
            .iter()
            .filter(|p| p.online)
            .map(|p| {
                let pl = &p.player;
                Seen {
                    id: p.id,
                    pos: pl.pos,
                    eye: pl.eye(1.0),
                    bbox: pl.bbox(),
                    alive: p.hp.alive(),
                    creative: (self.debug && pl.noclip) || crate::game::rules::invulnerable(pl), // творческий и наблюдатель — мобы не замечают
                    crouch: pl.crouch,
                    hands: [p.inv.hand().id(), p.inv.get(OFFHAND).id()],
                }
            })
            .collect()
    }

    /// Мобы после мира (фаза «сущности»): тик, затем удары по игрокам.
    pub(crate) fn tick_mobs(&mut self) {
        let seen = self.seen_players();
        let outs = self.sim.tick_mobs(&seen, &mut self.events);
        for o in outs {
            if let Out::Hit { mob, player, amount, from } = o {
                let Some(i) = self.idx(player) else { continue };
                self.mob_hits_player(i, mob, amount, from);
            }
        }
    }

    /// Удар моба по игроку (MobEntity.tryAttack → LivingEntity.damage): урон через survival; полный удар — отбрасывание
    /// 0.4 от моба (замена скорости игрока, ASP-12 §5) по сети.
    fn mob_hits_player(&mut self, i: usize, mob: u32, amount: f32, from: [f64; 3]) {
        if !self.hurt(i, DamageKind::MobAttack, amount) {
            return;
        }
        if let Some(m) = self.sim.mobs.list.iter_mut().find(|m| m.id == mob) {
            m.ai.nav.stop(); // замах: путь заново по перезарядке погони
        }
        let p = &mut self.players[i];
        if p.hp.hurt_time != 10 || !p.hp.alive() {
            return; // добор в окне — без отбрасывания
        }
        let pl = &p.player;
        let (dx, dz) = (from[0] - pl.pos[0], from[2] - pl.pos[2]);
        p.hp.tilt_yaw = (libm::atan2(dz, dx).to_degrees() as f32) - pl.yaw;
        let v = knock(pl.vel, pl.on_ground, 0.4, dx, dz);
        p.kb_next = p.kb_next.wrapping_add(1).max(1);
        p.kb_pending = Some((p.kb_next, v));
        p.own.push(Msg::Velocity { id: p.kb_next, v });
    }

    /// Act::VelocityAck: клиент поставил скорость отбрасывания в этом тике — та же скорость теневому телу (до хода).
    pub(crate) fn velocity_ack(&mut self, i: usize, id: u32) {
        let p = &mut self.players[i];
        if let Some((k, v)) = p.kb_pending {
            if k == id {
                p.player.vel = v;
                p.kb_pending = None;
            }
        }
    }

    /// Act::Attack по мобу (PlayerEntity.attack): дальность, перезарядка удара (урон ×(0.2 + 0.8·h²)), крит в падении
    /// (×1.5), добавочное отбрасывание бегом; звук удара всем рядом.
    pub(crate) fn attack_mob(&mut self, i: usize, target: u32) {
        let Some(k) = self.sim.mobs.index(target) else { return };
        let p = &self.players[i];
        let pl = &p.player;
        let eye = pl.eye(1.0);
        let bx = self.sim.mobs.list[k].bbox();
        let d2: f64 = (0..3).map(|a| (eye[a].clamp(bx.lo[a], bx.hi[a]) - eye[a]).powi(2)).sum();
        let reach = crate::game::interact::ENTITY_REACH + ATTACK_SLACK;
        if d2 > reach * reach || !self.sim.mobs.list[k].alive() {
            return;
        }
        let s = self.strike_of(i); // урон, крит и бег — общая формула с ударом по игроку (server/pvp.rs)
        let p = &self.players[i];
        let pl = &p.player;
        let from = pl.pos;
        let yaw = pl.yaw;
        let feet = p.feet_cell();
        let id = p.id;
        self.players[i].atk_ticks = 0;
        let ok = self.sim.hurt_mob(k, Blow { kind: DamageKind::PlayerAttack, amount: s.dmg, by: Some((id, from, yaw, s.kb)) }, &mut self.events);
        if !ok {
            return;
        }
        self.events.push(Ev::Sound as i32, feet, s.sfx() as i32);
    }

    /// Act::UseEntity: ПКМ по мобу. Стрижка (SheepEntity.interactMob): ножницы в руке, овца жива, взрослая, не острижена →
    /// 1–3 шерсти её цвета, ножницы −1 прочности, звук.
    pub(crate) fn use_entity(&mut self, i: usize, target: u32, hand: u8) {
        let Some(k) = self.sim.mobs.index(target) else { return };
        let p = &self.players[i];
        let eye = p.player.eye(1.0);
        let bx = self.sim.mobs.list[k].bbox();
        let d2: f64 = (0..3).map(|a| (eye[a].clamp(bx.lo[a], bx.hi[a]) - eye[a]).powi(2)).sum();
        let reach = crate::game::interact::ENTITY_REACH + ATTACK_SLACK;
        if d2 > reach * reach || hand > 1 {
            return;
        }
        let slot = if hand == 0 { p.inv.selected } else { OFFHAND };
        let held = p.inv.get(slot);
        let m = &self.sim.mobs.list[k];
        if !(held.id() == I_SHEARS && m.kind == MobKind::Sheep && m.alive() && !m.baby && !m.sheared) {
            return;
        }
        let max = crate::registry::items::idef(I_SHEARS).max_damage;
        let worn = ItemStack { damage: held.damage + 1, ..held };
        let broke = worn.damage >= max;
        self.players[i].inv.set(slot, if broke { ItemStack::EMPTY } else { worn });
        if broke {
            let feet = self.players[i].feet_cell();
            self.events.push(crate::abi::EV_TOOL_BROKE, feet, I_SHEARS as i32);
        }
        let s = &mut self.sim;
        let m = &mut s.mobs.list[k];
        m.sheared = true;
        let n = 1 + m.rng.below(3);
        let wool = WHITE_WOOL as u16 + m.color as u16;
        let at = [m.pos[0], m.pos[1] + 1.0, m.pos[2]];
        let fx = &mut s.mobs.fx;
        let pitch = (fx.next_f32() - fx.next_f32()) * 0.2 + 1.0;
        m.sound(&mut self.events, Sfx::SheepShear, pitch);
        for _ in 0..n {
            // dropItem(шерсть, +1): скорость конструктора и добавка ((r − r)·0.1, r·0.05, (r − r)·0.1).
            let mut e = s.items.make(s.ids.next(), at, ItemStack::of(wool, 1), &mut s.rng);
            let r = &mut s.rng;
            e.vel[0] += ((r.next_f32() - r.next_f32()) * 0.1) as f64;
            e.vel[1] += (r.next_f32() * 0.05) as f64;
            e.vel[2] += ((r.next_f32() - r.next_f32()) * 0.1) as f64;
            e.pickup_delay = 10;
            s.items.list.push(e);
        }
    }

    /// Отладка: n мобов вида kind у точки pos (каждый следующий — на 0.7 восточнее).
    pub(crate) fn debug_spawn_mob(&mut self, kind: u8, pos: [f64; 3], n: u8) {
        let Some(k) = MobKind::from_u8(kind) else { return };
        if !pos.iter().all(|v| v.is_finite()) {
            return;
        }
        for j in 0..n.max(1) {
            self.sim.spawn_mob(k, [pos[0] + j as f64 * 0.7, pos[1], pos[2]], false);
        }
    }
}

/// takeKnockback для игрока: v.xz/2 − норм(x, z)·s, на земле vy = min(0.4, vy/2 + s).
pub(crate) fn knock(v: [f64; 3], on_ground: bool, s: f64, x: f64, z: f64) -> [f64; 3] {
    let d = (x * x + z * z).sqrt();
    let (nx, nz) = if d < 1e-4 { (0.0, 0.0) } else { (x / d * s, z / d * s) };
    [v[0] / 2.0 - nx, if on_ground { (v[1] / 2.0 + s).min(0.4) } else { v[1] }, v[2] / 2.0 - nz]
}

/// Мобы для игрока: новые в радиусе — Spawn, изменившиеся — MobMoves, анимации урона — Anim 1; номера — в alive (пропавшие
/// и ушедшие уберёт общий Despawn в track_entities).
pub(crate) fn track_mobs(p: &mut ServerPlayer, s: &Sim, anims: &[(u32, u8)], alive: &mut HashSet<u32>, msgs: &mut Vec<Msg>) {
    let me = p.player.pos;
    let mut moves = Vec::new();
    for m in s.mobs.list.iter().filter(|m| !m.removed) {
        let d2 = (0..3).map(|k| (m.pos[k] - me[k]).powi(2)).sum::<f64>();
        if d2 > TRACK_MOBS * TRACK_MOBS {
            continue;
        }
        alive.insert(m.id);
        let mv = m.moved();
        let h = mob_hash(&mv);
        match p.tracked.get_mut(&m.id) {
            None => {
                msgs.push(Msg::Spawn { id: m.id, ent: Ent::Mob { kind: m.kind as u8, mv } });
                p.tracked.insert(m.id, Tracked { pos: m.pos, spin: h, count: 1 });
            }
            Some(t) => {
                if t.spin != h {
                    moves.push(mv);
                    t.spin = h;
                    t.pos = m.pos;
                }
            }
        }
    }
    if !moves.is_empty() {
        msgs.push(Msg::MobMoves(moves));
    }
    for &(id, kind) in anims {
        if alive.contains(&id) {
            msgs.push(Msg::Anim { id, kind });
        }
    }
}

/// Отпечаток хода (что изменилось — уходит).
fn mob_hash(m: &crate::proto::MobMove) -> u32 {
    let mut h = 0x811c_9dc5u32;
    let mut eat = |x: u32| {
        for b in x.to_le_bytes() {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
    };
    for v in m.pos {
        eat(v.to_bits());
    }
    eat(m.body.to_bits());
    eat(m.head.to_bits());
    eat(m.pitch.to_bits());
    eat(m.flags as u32 | (m.var as u32) << 8);
    h
}
