//! Надеть броню из руки (ArmorItem.use → equipAndSwap, 1.21.1): ПКМ с броней в воздух кладёт её на своё место
//! (шлем, нагрудник, поножи, ботинки), а то, что там было, оказывается в руке; звук надевания. Одно действие над ActCtx:
//! клиент предсказывает, сервер решает (Act::UseAir с бронёй в руке — отдельного сообщения нет). Через окно инвентаря броня
//! надевается кликом и Shift (screen.rs) — тот же звук.

use crate::abi::{Ev, Sfx};
use crate::game::interact::{feet, ActCtx, Use};
use crate::registry::items::equip_slot;

/// ПКМ бронёй в руке hand: Swing — надета (прежняя на месте — в руке), Pass — в руке не броня.
pub(crate) fn equip_from_hand(c: &mut ActCtx, hand: usize) -> Use {
    let stack = if hand == 0 { c.inv.hand() } else { c.inv.offhand };
    let Some(slot) = equip_slot(stack.id()) else { return Use::Pass };
    let idx = 36 + slot as usize;
    let worn = c.inv.get(idx);
    c.inv.set(idx, stack);
    if hand == 0 {
        *c.inv.hand_mut() = worn;
    } else {
        c.inv.offhand = worn;
    }
    c.emit(Ev::Sound as i32, feet(c.player), Sfx::ArmorEquip as i32);
    Use::Swing
}
