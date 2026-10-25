//! Отпечаток состояния игры для точного повтора (web/game/record.js: запись ввода → ?replay → сверка): FNV-1a 64 по
//! разделам — сервер (время и ГСЧ, правки, блок-сущности, предметы, падающие, тики, свой игрок, его инвентарь) и клиент
//! (игрок, инвентарь, окно, время). Разделы — те же, что у проверки сохранения (save/tests.rs snapshot), прямо из
//! структур игры; позиции и скорости — биты f64. Не входит производное и вид: prev, крышки и смотрящие сундуков, рука,
//! HUD, ломание, события. Чтение без правок: порядок печей (ticking_furnaces чистит список) — не входит.

use super::Game;
use crate::world::blockentity::BlockEntity;

/// FNV-1a 64.
fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.as_bytes() {
        h = (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl Game {
    /// Разделы отпечатка: (имя, текст). Текст длинный — наружу идёт только хеш (state_hash).
    pub(crate) fn digest_parts(&self) -> Vec<(&'static str, String)> {
        let b = |v: [f64; 3]| format!("{:x},{:x},{:x}", v[0].to_bits(), v[1].to_bits(), v[2].to_bits());
        let sv = &self.server.sim;
        let s = &sv.sim;
        let me = self.server.pid.and_then(|id| sv.player(id));
        let mut out = Vec::new();
        out.push((
            "time",
            format!(
                "{} {} {} {:x} {:x} {:x} {} {} {}",
                s.time,
                s.day,
                s.day_cycle,
                s.rng.0,
                s.tick_rng.0,
                sv.fx_rng.0,
                s.ids.0,
                s.ticks.seq(),
                me.map_or(0, |m| m.id)
            ),
        ));
        {
            let w = s.world.d.borrow();
            let mut chunks: Vec<(i32, i32)> = w.edited_chunks().collect();
            chunks.sort();
            let edits: Vec<String> = chunks.iter().map(|&(x, z)| format!("{x},{z}:{:?}", w.chunk_edits(x, z))).collect();
            out.push(("edits", format!("{} {}", w.edit_count(), edits.join(" "))));
            let mut bes: Vec<String> = w
                .be()
                .iter()
                .map(|(p, seq, be)| match be {
                    BlockEntity::Furnace(f) => format!("{p:?}#{seq} f {:?} {} {} {} {}", f.slots, f.burn, f.fuel_total, f.cook, f.cook_total),
                    BlockEntity::Chest(c) => format!("{p:?}#{seq} c {:?}", c.slots),
                    BlockEntity::Mek(m) => format!("{p:?}#{seq} m {m:?}"),
                })
                .collect();
            bes.sort();
            out.push(("blockents", format!("{} {}", w.be().next_seq(), bes.join(" "))));
            // M5: предметы в пути, режимы и цвета передатчиков — раздел только когда есть (прежние отпечатки целы).
            let lg = &w.logistics;
            if !lg.stacks.is_empty() || !w.grid.modes.is_empty() || !w.grid.colors.is_empty() {
                out.push(("logistics", format!("{} {:?} {:?} {:?}", lg.next_id, lg.stacks, w.grid.modes, w.grid.colors)));
            }
        }
        // Машины в предметах (world/mek/items.rs) — раздел только когда есть: пустой не меняет прежние отпечатки.
        if !s.mek_items.is_empty() || !s.mek_broken.is_empty() {
            out.push(("mekitems", format!("{:?} {:?}", s.mek_items, s.mek_broken)));
        }
        let items: Vec<String> = s
            .items
            .list
            .iter()
            .map(|e| {
                format!(
                    "#{} {:?} {} {} {} {} {} {} {} {} {:x} {}",
                    e.id, e.stack, b(e.pos), b(e.vel), e.on_ground, e.age, e.item_age, e.spin_age, e.pickup_delay, e.thrown, e.offset.to_bits(), e.removed
                )
            })
            .collect();
        out.push(("items", items.join(" | ")));
        let falling: Vec<String> =
            s.falling.iter().map(|f| format!("#{} {:x} {} {} {} {}", f.id, f.cell, b(f.pos), b(f.vel), f.on_ground, f.age)).collect();
        out.push(("falling", falling.join(" | ")));
        // Мобы (G6): тело, здоровье, ИИ-состояние, что влияет на ход; ГСЧ спауна и каждого моба; заселённые куски.
        let mobs: Vec<String> = s
            .mobs
            .list
            .iter()
            .map(|m| {
                format!(
                    "#{} {:?} {} {} {} {} {:x} {:x} {:x} {:x} {} {:x} {:x} {} {} {} {} {} {:?} {:?} {:x}",
                    m.id,
                    m.kind,
                    m.baby,
                    m.age,
                    b(m.pos),
                    b(m.vel),
                    m.yaw.to_bits(),
                    m.body.to_bits(),
                    m.head.to_bits(),
                    m.life.hp.to_bits(),
                    m.on_ground,
                    m.fall.to_bits(),
                    m.rng.0,
                    m.sheared,
                    m.color,
                    m.egg,
                    m.despawn,
                    m.ambient,
                    m.target,
                    m.ai.nav.path.get(m.ai.nav.idx),
                    m.life.fire
                )
            })
            .collect();
        let mut pop: Vec<(i32, i32)> = s.mobs.populated.iter().copied().collect();
        pop.sort();
        out.push(("mobs", format!("{:x} {} {:?} | {}", s.mobs.spawn_rng.0, pop.len(), pop.first(), mobs.join(" | "))));
        out.push(("kb", me.map_or(String::new(), |m| format!("{} {} {:?}", m.atk_ticks, m.kb_next, m.kb_pending))));
        let mut tc: Vec<(i32, i32)> = s.ticks.chunks().collect();
        tc.sort();
        let ticks: Vec<String> = tc.iter().map(|&(x, z)| format!("{:?}", s.ticks.chunk_entries(x, z))).collect();
        out.push(("ticks", format!("{} {}", s.ticks.len(), ticks.join(" "))));
        let body = |p: &super::player::Player| {
            format!(
                "{} {} {:x} {:x} {} {:x} {} {}",
                b(p.pos),
                b(p.vel),
                p.yaw.to_bits(),
                p.pitch.to_bits(),
                p.on_ground,
                p.fall.to_bits(),
                p.flying,
                p.crouch
            ) + if p.flying && !p.noclip { " F" } else { "" }
                + &if p.mode != crate::game::rules::GameMode::Survival { format!(" M{}", p.mode.code()) } else { String::new() }
        };
        let inv = |v: &super::inventory::Inventory| {
            let slots: Vec<String> = (0..41).map(|i| format!("{:?}", v.get(i))).collect();
            format!("{} {}", v.selected, slots.join(" "))
        };
        out.push(("player", me.map_or(String::new(), |m| body(&m.player))));
        // Здоровье (срез 4): сервер — всё состояние Health и жидкости тела, клиент — зеркало Vitals и счётчики кадра.
        out.push((
            "health",
            me.map_or(String::new(), |m| {
                let h = &m.hp;
                format!(
                    "{:x} {} {:x} {} {} {} {} {} {} {} {} {:?} {} {} {}",
                    h.hp.to_bits(),
                    h.time_until_regen,
                    h.last_damage_taken.to_bits(),
                    h.hurt_time,
                    h.air,
                    h.spawn_protect,
                    h.now,
                    h.last_hurt,
                    h.regen_timer,
                    h.death_time,
                    h.fire,
                    m.death_kind,
                    m.player.in_water,
                    m.player.in_lava,
                    m.player.eye_water
                )
            }),
        ));
        out.push(("gate", me.map_or(String::new(), |m| format!("{} {}", m.gate, m.gate_ready))));
        out.push(("inventory", me.map_or(String::new(), |m| inv(&m.inv))));
        let c = &self.client.sim;
        out.push(("client", format!("{} {} {} {} {}", body(&c.ctl.player), c.time, c.day, c.world.edit_count(), self.screen_kind())));
        out.push(("clientinv", inv(&c.ctl.inv)));
        out.push(("clienthealth", format!("{:?} {} {} {}", c.vit, c.cam_hurt, c.hud_regen, c.death_ticks)));
        out
    }

    /// Отпечаток: «итог раздел=хеш …» (хеши — 16 hex), итог — FNV-1a по всем разделам подряд.
    pub fn state_hash(&self) -> String {
        let parts = self.digest_parts();
        let mut all = String::new();
        let mut each = Vec::new();
        for (name, text) in &parts {
            all.push_str(name);
            all.push('=');
            all.push_str(text);
            all.push('\n');
            each.push(format!("{name}={:016x}", fnv(text)));
        }
        format!("{:016x} {}", fnv(&all), each.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use crate::abi::{K_FWD, K_LEFT};
    use crate::{Game, World};

    /// Те же вызовы — тот же отпечаток (повтор); лишний тик — другой; разделы названы.
    #[test]
    fn same_inputs_same_hash() {
        let run = |extra: bool| {
            let w = World::new(1337);
            let mut g = Game::new(&w);
            g.spawn(8.0, 8.0);
            g.set_look(-30.0, 10.0);
            for i in 0..60 {
                g.set_keys(if i < 40 { K_FWD } else { K_LEFT });
                g.turn(3.0, -1.5, 0.5);
                g.tick();
            }
            if extra {
                g.tick();
            }
            g.state_hash()
        };
        let (a, b, c) = (run(false), run(false), run(true));
        assert_eq!(a, b, "повтор тех же вызовов");
        assert_ne!(a, c, "лишний тик меняет отпечаток");
        for name in ["time=", "edits=", "player=", "inventory=", "client=", "clientinv="] {
            assert!(a.contains(name), "раздел {name} в {a}");
        }
    }
}
