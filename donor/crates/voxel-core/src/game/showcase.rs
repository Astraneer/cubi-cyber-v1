//! Витрина Mekanism на Перекрёстке (G13): машины трёх павильонов (gen/spawn.rs PAVILIONS — пол, стены и крышу кладёт
//! генератор) ставит сервер при старте — это блок-сущности, генератор их не ставит. Зона защиты перестраивается поверх
//! сохранённого мира (raise): правки в ней снимаются до генератора без рассыпания содержимого, затем машины ставятся
//! дорогой игры (Sim::set_world: рукава кабелей, сети, блок-сущности). Мир не сбрасывается: снаружи зоны ничего не
//! меняется, а игроки в зоне ничего не строят.
//!
//! Машины работают сами, без игрока рядом: их куски всегда в активной зоне (chunks), раз в секунду сервер пополняет
//! вход (бесконечное сырьё: у удвоения — сундук, из которого руду тянет транспортёр M5; у ветра и стали — сам слот входа,
//! как в mek_demo) и очищает сундуки выхода. Брать и класть нельзя: окно в зоне только смотрят (game/screen.rs Ui::locked), ломать — защита зоны.

use crate::cell::{cell_of, BlockId, Cell};
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::gen::spawn::{Hall, Pavilion, PAVILIONS};
use crate::gen::{generate, Region, PAD};
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::mek::recipes::SUB_WATER;
use crate::world::mek::{Tank, CH_IN, GM_EXCESS, INF_EXTRA, INF_IN};
use crate::world::stack::ItemStack;
use crate::world::{local_pos, WorldData};

/// Пополнение входов и очистка сундуков — раз в столько тиков.
pub(crate) const REFILL_EVERY: u64 = 20;
/// Сундук выхода очищается, когда в нём больше стольких предметов (видно, что копится).
pub(crate) const CHEST_KEEP: i32 = 48;

/// Машина витрины: клетка (u, v) павильона, блок, лицо — к зрителю (false) или вправо по ряду (true, источники).
struct Part {
    u: i32,
    v: i32,
    block: BlockId,
    right: bool,
}

const fn part(u: i32, v: i32, block: BlockId, right: bool) -> Part {
    Part { u, v, block, right }
}

/// Состав линии: источник в (−4, −1) лицом вправо (ветряк — в (−5, −1) лопастями к зрителю, кабель из-под мачты), кабель v −1 от u −3, машины в ряд v 0, сундук в конце.
const WIND: [Part; 4] = [
    part(-5, -1, WIND_GENERATOR, false),
    part(-1, 0, ENRICHMENT_CHAMBER, false),
    part(0, 0, ENERGIZED_SMELTER, false),
    part(1, 0, CHEST, false),
];
const STEEL: [Part; 5] = [
    part(-4, -1, HEAT_GENERATOR, true),
    part(-2, 0, METALLURGIC_INFUSER, false),
    part(-1, 0, METALLURGIC_INFUSER, false),
    part(0, 0, ENERGIZED_SMELTER, false),
    part(1, 0, CHEST, false),
];
/// Удвоение: сундук сырья (−7, +1) и транспортёры (−6…−2, +1) к лицу очистителя — их кладёт build.
const SOURCE: (i32, i32) = (-7, 1);
const DOUBLE: [Part; 7] = [
    part(-4, -1, ENERGY_CUBE, true),
    part(-3, 0, ELECTROLYTIC_SEPARATOR, false),
    part(-2, 0, PURIFICATION_CHAMBER, false),
    part(-1, 0, CRUSHER, false),
    part(0, 0, ENRICHMENT_CHAMBER, false),
    part(1, 0, ENERGIZED_SMELTER, false),
    part(2, 0, CHEST, false),
];
fn parts(h: Hall) -> &'static [Part] {
    match h {
        Hall::Wind => &WIND,
        Hall::Steel => &STEEL,
        Hall::Double => &DOUBLE,
    }
}
/// Кабель: от u −3 до последней машины с энергией (сундук энергии не берёт).
fn cable_end(h: Hall) -> i32 {
    match h {
        Hall::Double => 1,
        _ => 0,
    }
}

/// Мировая клетка (u, v, высота над полом k) павильона p площади с центром (sx, y0, sz).
fn cell(site: (i32, i32, i32), p: &Pavilion, u: i32, v: i32, k: i32) -> [i32; 3] {
    let (dx, dz) = p.off(u, v);
    [site.0 + dx, site.1 + k, site.2 + dz]
}

fn site_of(w: &WorldData) -> Option<(i32, i32, i32)> {
    let s = &w.gen().site;
    s.active().then_some((s.x, s.y0, s.z))
}

/// Куски, где стоят машины витрины (всегда в активной зоне сервера).
pub(crate) fn chunks(w: &WorldData) -> Vec<(i32, i32)> {
    let Some(site) = site_of(w) else { return Vec::new() };
    let mut out = Vec::new();
    for p in &PAVILIONS {
        for &(u, v) in &[(p.hall.u_min(), 1), (p.hall.u_min(), -1), (3, 0)] {
            let c = cell(site, p, u, v, 0);
            let k = (c[0].div_euclid(16), c[2].div_euclid(16));
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

/// Перестроить зону Перекрёстка поверх сохранённого мира (старт сервера): правки в зоне — до генератора (без рассыпания
/// содержимого: вещи витрины игрокам не достаются), затем машины витрины.
pub(crate) fn raise(sim: &mut Sim, w: &mut WorldData, ev: &mut Events) {
    let Some(site) = site_of(w) else { return };
    let r = crate::gen::spawn::R_ZONE;
    let (c0x, c0z, c1x, c1z) = ((site.0 - r).div_euclid(16), (site.2 - r).div_euclid(16), (site.0 + r).div_euclid(16), (site.2 + r).div_euclid(16));
    for cz in c0z..=c1z {
        for cx in c0x..=c1x {
            let edits = w.chunk_edits(cx, cz);
            if edits.is_empty() {
                continue;
            }
            let reg = generate(w.gen(), cx, cz);
            for (k, _) in edits {
                let (lx, y, lz) = local_pos(k);
                let (x, z) = (cx * 16 + lx, cz * 16 + lz);
                if w.gen().site.protects(x, z) {
                    let base = reg.cell(Region::idx(lx + PAD, y, lz + PAD));
                    w.set_cell(x, y, z, base);
                }
            }
        }
    }
    drop(w.take_removed()); // сущности снятых правок: содержимое витрины не рассыпается
    for p in &PAVILIONS {
        build(sim, w, ev, site, p);
    }
    refill(w);
}

fn build(sim: &mut Sim, w: &mut WorldData, ev: &mut Events, site: (i32, i32, i32), p: &Pavilion) {
    for u in -3..=cable_end(p.hall) {
        sim.set_world(w, cell(site, p, u, -1, 1), UNIVERSAL_CABLE as Cell, ev);
    }
    if p.hall == Hall::Double {
        // Вход логистикой (M5, как mek_logistics): сундук сырья, ряд транспортёров перед машинами; первый тянет из сундука,
        // последний стоит перед лицом очистителя и отдаёт в него.
        let src = cell(site, p, SOURCE.0, SOURCE.1, 1);
        sim.set_world(w, src, CHEST as Cell, ev);
        for u in SOURCE.0 + 1..=-2 {
            sim.set_world(w, cell(site, p, u, SOURCE.1, 1), LOGISTICAL_TRANSPORTER as Cell, ev);
        }
        let first = cell(site, p, SOURCE.0 + 1, SOURCE.1, 1);
        crate::game::mek_demo::set_link(w, first, p.right() ^ 1, crate::world::grid::M_PULL);
    }
    if p.hall == Hall::Wind {
        // Ветряк лопастями к зрителю отдаёт энергию лицом и низом: кабель — из-под мачты в полу и вверх в ряд.
        for u in -5..=-3 {
            sim.set_world(w, cell(site, p, u, -1, 0), UNIVERSAL_CABLE as Cell, ev);
        }
    }
    for q in parts(p.hall) {
        let at = cell(site, p, q.u, q.v, 1);
        let c = if q.block == CHEST { CHEST as Cell } else { cell_of(q.block, (if q.right { p.right() } else { p.face }) as u16) };
        sim.set_world(w, at, c, ev);
        if q.block == WIND_GENERATOR {
            for dy in 1..=4 {
                sim.set_world(w, [at[0], at[1] + dy, at[2]], cell_of(BOUNDING, dy as u16), ev);
            }
        }
        if let Some(m) = w.be_mut().mek_mut(at) {
            m.eject = crate::world::mek::sides::can_eject(m.kind);
            if q.block == ELECTROLYTIC_SEPARATOR {
                m.gas_mode[0] = GM_EXCESS; // водород — в «сброс излишка»: копится и не останавливает линию
            }
        }
    }
}

/// Стопка в слоте не меньше n предметов item (лишнего не трогает).
fn top_up(s: &mut ItemStack, item: ItemId, n: i32) {
    if s.is_empty() || s.id() == item && s.n() < n {
        *s = ItemStack::of(item, n);
    }
}

/// Раз в REFILL_EVERY тиков (и при постройке): вход машин — бесконечный, куб — полный, сундуки выхода — не копятся.
pub(crate) fn refill(w: &mut WorldData) {
    let Some(site) = site_of(w) else { return };
    for p in &PAVILIONS {
        if p.hall == Hall::Double {
            let src = cell(site, p, SOURCE.0, SOURCE.1, 1);
            if let Some(c) = w.be_mut().chest_mut(src) {
                if c.slots.iter().map(|s| s.n()).sum::<i32>() < 16 {
                    c.slots[0] = ItemStack::of(I_RAW_IRON, 32);
                }
            }
        }
        for q in parts(p.hall) {
            let at = cell(site, p, q.u, q.v, 1);
            if q.block == CHEST {
                if let Some(c) = w.be_mut().chest_mut(at) {
                    if c.slots.iter().map(|s| s.n()).sum::<i32>() > CHEST_KEEP {
                        c.slots = [ItemStack::EMPTY; 27];
                    }
                }
                continue;
            }
            let first_infuser = p.hall == Hall::Steel && q.u == -2;
            let Some(m) = w.be_mut().mek_mut(at) else { continue };
            match q.block {
                WIND_GENERATOR => {}
                HEAT_GENERATOR => top_up(&mut m.slots[0], I_COAL, 16),
                ENERGY_CUBE => m.energy = m.cap(),
                ENRICHMENT_CHAMBER if p.hall == Hall::Wind => top_up(&mut m.slots[CH_IN], I_RAW_IRON, 12),
                METALLURGIC_INFUSER => {
                    top_up(&mut m.slots[INF_EXTRA], I_COAL, 16);
                    if first_infuser {
                        top_up(&mut m.slots[INF_IN], I_IRON_INGOT, 12);
                    }
                }
                ELECTROLYTIC_SEPARATOR => {
                    if m.tank.amount < 12_000 {
                        m.tank = Tank { sub: SUB_WATER, amount: 12_000 };
                    }
                }
                _ => {}
            }
        }
    }
}

/// Клетки машин витрины (для проверок): (линия, блок, клетка).
#[cfg(test)]
pub(crate) fn machines(w: &WorldData) -> Vec<(Hall, BlockId, [i32; 3])> {
    let Some(site) = site_of(w) else { return Vec::new() };
    let mut out = Vec::new();
    for p in &PAVILIONS {
        for q in parts(p.hall) {
            out.push((p.hall, q.block, cell(site, p, q.u, q.v, 1)));
        }
    }
    out
}
