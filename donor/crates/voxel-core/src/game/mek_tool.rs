//! Предметы Mekanism в руке по машине (MEK-09 §7, MEK-10 §5.3) — одна функция для клиента (Predict) и сервера (Auth),
//! зовётся из interact_block до окна блока (порядок «блок → предмет» ванильного interactBlock, как в моде):
//!   конфигуратор (режим — в damage стека, клавиша N / Shift+N): ПКМ по машине открывает окно, действие — Shift+ПКМ;
//!     «предметы» — режим грани под прицелом вперёд по кругу; «опустошить» — все слоты высыпаются из нажатой грани;
//!     «поворот» — ПКМ поворачивает на 90° по часовой, Shift+ПКМ ставит лицо от нажатой грани; «ключ» — снять машину
//!     целиком (с содержимым) сразу в инвентарь;
//!   улучшение скорости или энергии: Shift+ПКМ — установить сразу, сколько влезает (до 8), из руки уходит установленное.
//! Режимы энергии и веществ — вне M3 (энергия по сторонам — постоянная таблица вида).
//! M5: режим «газы» — Shift+ПКМ по грани сепаратора: сторона газа ничего → водород → кислород. Передатчики (кабель,
//! транспортёр, труба, трубка) в любом режиме, кроме ключа: Shift+ПКМ по рукаву — режим стороны NORMAL → PUSH → PULL →
//! NONE; по центру — у транспортёра следующий цвет, у прочих — сторона грани, по которой щёлкнули (MEK-02 §5.3,
//! MEK-14 §8). Правка — у сервера (режимы и цвета живут в его реестре), клиент видит новые рукава в ячейках.

use crate::cell::{block_of, cell_of, offset, state_of, Cell, F_EAST, F_NORTH, F_SOUTH, F_WEST};
use crate::game::interact::{ActCtx, Hit, Mode, Use};
use crate::registry::blocks::{bdef, main_of, multiblock_parts, BeKind};
use crate::registry::ids::{AIR, BOUNDING};
use crate::registry::items::{block_item, ItemId, I_CONFIGURATOR};
use crate::world::grid::is_transmitter;
use crate::world::mek::sides::{cycle_gas, cycle_mode, item_modes, rel_of};
use crate::world::mek::upgrade_kind;
use crate::world::stack::ItemStack;

/// Режимы конфигуратора (в M3): настройка предметов, опустошить, поворот, ключ; M5 — настройка газов (в конец: номера
/// режимов в стеках прежние).
pub(crate) const CONF_ITEMS: u16 = 0;
pub(crate) const CONF_EMPTY: u16 = 1;
pub(crate) const CONF_ROTATE: u16 = 2;
pub(crate) const CONF_WRENCH: u16 = 3;
pub(crate) const CONF_GAS: u16 = 4;
pub(crate) const CONF_MODES: u16 = 5;

/// Следующий (back — предыдущий) режим конфигуратора.
pub(crate) fn next_mode(m: u16, back: bool) -> u16 {
    let m = m % CONF_MODES;
    if back {
        (m + CONF_MODES - 1) % CONF_MODES
    } else {
        (m + 1) % CONF_MODES
    }
}

/// Сменить режим конфигуратора в главной руке (клавиша N): true — сменён.
pub(crate) fn cycle_hand(s: &mut ItemStack, back: bool) -> bool {
    if s.id() != I_CONFIGURATOR {
        return false;
    }
    s.damage = next_mode(s.damage, back);
    true
}

/// Поворот лица по часовой (вид сверху): N → E → S → W → N.
fn clockwise(f: u8) -> u8 {
    match f {
        F_NORTH => F_EAST,
        F_EAST => F_SOUTH,
        F_SOUTH => F_WEST,
        _ => F_NORTH,
    }
}

/// ПКМ предметом Mekanism по машине: Some — обработано (окно не открывается), None — дальше обычным путём.
pub(crate) fn use_on_machine(c: &mut ActCtx, hand: usize, h: Hit) -> Option<Use> {
    let stack = if hand == 0 { c.inv.hand() } else { c.inv.offhand };
    let item = stack.id();
    if item != I_CONFIGURATOR && upgrade_kind(item).is_none() {
        return None;
    }
    let pos = main_of(c.w.get_cell(h.pos[0], h.pos[1], h.pos[2]), h.pos);
    // Витрина на Перекрёстке (G13): конфигуратор и улучшения машин в зоне не работают — ПКМ откроет окно посмотреть.
    if crate::game::interact::protected(c.w, pos) {
        return None;
    }
    let cell = c.w.get_cell(pos[0], pos[1], pos[2]);
    let b = block_of(cell);
    if item == I_CONFIGURATOR && is_transmitter(cell) {
        return configure_transmitter(c, stack.damage % CONF_MODES, h, cell);
    }
    let Some(BeKind::Mek(kind)) = bdef(b).be else { return None };
    let sneak = c.player.sneaking();
    let facing = crate::registry::blocks::facing_of(cell);
    if let Some(u) = upgrade_kind(item) {
        if !sneak {
            return None;
        }
        // MEK-10 §5.3 (ItemUpgrade.useOn): сразу, сколько влезает.
        let Some(m) = c.w.be_mut().mek_mut(pos) else { return Some(Use::Pass) };
        let n = m.install(u, stack.count);
        if n == 0 {
            return Some(Use::Pass);
        }
        if !c.creative() {
            let s = if hand == 0 { c.inv.hand_mut() } else { &mut c.inv.offhand };
            *s = if s.count > n { s.with_count(s.count - n) } else { ItemStack::EMPTY };
        }
        return Some(Use::Swing);
    }
    let mode = stack.damage % CONF_MODES;
    if mode == CONF_ROTATE {
        // Поворот: ПКМ — по часовой; Shift+ПКМ — лицо от нажатой грани (только по горизонтали).
        let to = if sneak { h.face ^ 1 } else { clockwise(facing) };
        if !(F_NORTH..=F_EAST).contains(&to) || to == facing {
            return Some(Use::Swing);
        }
        let s = (state_of(cell) & !7) | to as u16;
        c.write(pos, cell_of(b, s));
        return Some(Use::Swing);
    }
    if !sneak {
        return None; // в остальных режимах ПКМ открывает окно
    }
    match mode {
        CONF_ITEMS => {
            if item_modes(kind).is_empty() {
                return Some(Use::Pass);
            }
            let rel = rel_of(facing, h.face);
            if let Some(m) = c.w.be_mut().mek_mut(pos) {
                m.sides[rel] = cycle_mode(kind, m.sides[rel], false);
            }
            Some(Use::Swing)
        }
        CONF_EMPTY => {
            if let Mode::Auth { sim, .. } = &mut c.mode {
                let out = offset(pos, h.face);
                if let Some(m) = c.w.be_mut().mek_mut(pos) {
                    let n = m.nslots();
                    for i in 0..n {
                        let s = std::mem::take(&mut m.slots[i]);
                        sim.items.drop_from_block(out, s, &mut sim.rng, &mut sim.ids);
                    }
                }
            }
            Some(Use::Swing)
        }
        CONF_WRENCH => {
            wrench(c, pos, cell);
            Some(Use::Swing)
        }
        CONF_GAS => {
            if kind != crate::registry::blocks::MekKind::Separator {
                return Some(Use::Pass);
            }
            let rel = rel_of(facing, h.face);
            if let Some(m) = c.w.be_mut().mek_mut(pos) {
                m.gas_sides[rel] = cycle_gas(m.gas_sides[rel]);
            }
            Some(Use::Swing)
        }
        _ => None,
    }
}

/// Сторона передатчика под прицелом: рукав (точка попадания вне центра) или центр (None).
fn transmitter_side(cell: Cell, h: Hit) -> Option<u8> {
    let (a, b) = if bdef(block_of(cell)).shape == crate::registry::blocks::Shape::Large { (4.0, 12.0) } else { (5.0, 11.0) };
    let l = [(h.point[0] - h.pos[0] as f64) * 16.0, (h.point[1] - h.pos[1] as f64) * 16.0, (h.point[2] - h.pos[2] as f64) * 16.0];
    const E: f64 = 1e-4;
    // Оси: Y — низ/верх (0/1), Z — север/юг (2/3), X — запад/восток (4/5).
    for (k, lo, hi) in [(1usize, 0u8, 1u8), (2, 2, 3), (0, 4, 5)] {
        if l[k] < a - E {
            return Some(lo);
        }
        if l[k] > b + E {
            return Some(hi);
        }
    }
    None
}

/// Конфигуратор по передатчику (не ключ; ПКМ без Shift — дальше обычным путём): режим стороны или цвет транспортёра.
fn configure_transmitter(c: &mut ActCtx, mode: u16, h: Hit, cell: Cell) -> Option<Use> {
    use crate::world::grid::{tkind, Kind, M_NONE};
    if mode == CONF_WRENCH || !c.player.sneaking() {
        return None;
    }
    let p = h.pos;
    let side = transmitter_side(cell, h);
    let Mode::Auth { .. } = &c.mode else { return Some(Use::Swing) }; // режимы и цвета — у сервера
    if side.is_none() && tkind(cell) == Some(Kind::Item) {
        let col = (c.w.grid.color(p) + 1) % (crate::world::logistics::COLORS + 1);
        c.w.grid.set_color(p, col);
    } else {
        let f = side.unwrap_or(h.face);
        let m = (c.w.grid.mode(p, f) + 1) % (M_NONE + 1);
        c.w.grid.set_mode(p, f, m);
    }
    // Рукава заново у себя и соседей (связь с соседом зависит от обеих сторон и цветов); сеть пересвяжет их сама.
    crate::game::sim::fix_arms(c.w, p);
    for f in 0..6u8 {
        let q = offset(p, f);
        if is_transmitter(c.w.get_cell(q[0], q[1], q[2])) {
            crate::game::sim::fix_arms(c.w, q);
        }
    }
    c.w.grid.relink(p);
    Some(Use::Swing)
}

/// Ключ: снять машину (и части многоблока) — предмет с данными сразу в инвентарь, нет места — под ноги.
fn wrench(c: &mut ActCtx, pos: [i32; 3], cell: Cell) {
    let b = block_of(cell);
    if !c.write(pos, AIR as Cell) {
        return;
    }
    for dy in multiblock_parts(b) {
        let q = [pos[0], pos[1] + dy, pos[2]];
        if block_of(c.w.get_cell(q[0], q[1], q[2])) == BOUNDING {
            c.write(q, AIR as Cell);
        }
    }
    let Mode::Auth { sim, .. } = &mut c.mode else { return };
    let mut s = ItemStack::of(block_item(b) as ItemId, 1);
    if let Some(i) = sim.mek_broken.iter().position(|(q, _)| *q == pos) {
        let (_, m) = sim.mek_broken.remove(i);
        s.damage = sim.mek_items.stash(&m);
    }
    if !c.inv.insert(&mut s) || !s.is_empty() {
        let p = c.player;
        sim.items.throw(p.pos, p.eye_height(), p.yaw, p.pitch, s, false, true, &mut sim.rng, &mut sim.ids);
    }
}

/// Режим конфигуратора для сообщения над хотбаром (abi Msg::Conf*).
pub(crate) fn mode_msg(m: u16) -> i32 {
    crate::abi::MSG_CONF + (m % CONF_MODES) as i32
}
