//! Вёдра (ASP-15 §5, BucketItem.use): одно действие над ActCtx — клиент в режиме Predict, сервер в Auth (Act::Bucket).
//!
//! Пустое ведро берёт только источник: его луч (`raycast_ex` с `sources`) видит коробку источника 0…8/9 (или куб, если
//! сверху такая же жидкость) и не видит текучую жидкость. Клетка → воздух, в руке −1 пустое, полное — в руку (если пусто)
//! или в инвентарь, нет места — выпадает. Полное ведро жидкость не видит: цель — обычный луч по обводкам, жидкость ставится
//! в клетку «попадание + грань» (растение и факел там ломаются со звуком и дропом); источник уже стоит — успех без записи,
//! ведро пустеет; течение — через 5 тиков (30 у лавы). В полёте (отладка) вёдра не расходуются. Звук: набрать — у
//! игрока, вылить — в клетке жидкости.

use crate::abi::{Ev, Sfx, EV_BROKEN};
use crate::cell::{block_of, offset, Cell, H};
use crate::game::fluids::{fluid_of, is_solid, Fl, Liquid};
use crate::game::interact::{feet, protected, ActCtx, Hit, Mode, Use};
use crate::registry::blocks::{bdef, FLUID_WATER};
use crate::registry::ids::AIR;
use crate::registry::items::{idef, ItemId, ItemKind, I_BUCKET, I_LAVA_BUCKET, I_WATER_BUCKET};
use crate::world::stack::ItemStack;

/// ПКМ ведром в руке hand по цели h (для пустого — клетка источника, для полного — кликнутый блок и грань).
pub(crate) fn use_bucket(c: &mut ActCtx, hand: usize, h: Hit) -> Use {
    let stack = if hand == 0 { c.inv.hand() } else { c.inv.offhand };
    let ItemKind::Bucket { fluid } = idef(stack.id()).kind else { return Use::Pass };
    if fluid == 0 {
        fill(c, hand, h)
    } else {
        empty(c, hand, if fluid == FLUID_WATER { Liquid::Water } else { Liquid::Lava }, h)
    }
}

/// Набрать источник в клетке h.pos.
fn fill(c: &mut ActCtx, hand: usize, h: Hit) -> Use {
    let cell = c.w.get_cell(h.pos[0], h.pos[1], h.pos[2]);
    let Some(f) = fluid_of(cell).filter(|f| f.still) else { return Use::Fail };
    if !c.write(h.pos, AIR as Cell) {
        return Use::Fail;
    }
    c.emit(Ev::Sound as i32, feet(c.player), Sfx::BucketFill as i32);
    exchange(c, hand, if f.liquid == Liquid::Water { I_WATER_BUCKET } else { I_LAVA_BUCKET });
    Use::Swing
}

/// Вылить жидкость l в клетку «попадание + грань».
fn empty(c: &mut ActCtx, hand: usize, l: Liquid, h: Hit) -> Use {
    let t = offset(h.pos, h.face);
    if t[1] < 0 || t[1] >= H {
        return Use::Fail;
    }
    if protected(c.w, t) {
        c.zone_msg = true;
        return Use::Fail;
    }
    let cur = c.w.get_cell(t[0], t[1], t[2]);
    let cur_fluid = fluid_of(cur);
    let b = block_of(cur);
    // BlockState.canBucketPlace: воздух, заменяемое или не твёрдое.
    if !(b == AIR || bdef(b).replaceable || !is_solid(cur)) {
        return Use::Fail;
    }
    if cur_fluid.is_none() && b != AIR {
        // Растение, факел, снежный слой: World.breakBlock — звук и частицы ломания, дроп (без инструмента).
        c.emit(EV_BROKEN, t, b as i32);
        c.drop_loot(cur, t);
    }
    // Запись не изменила клетку (там уже такой же источник) — всё равно успех.
    c.write(t, Fl::source(l).cell());
    c.emit(Ev::Sound as i32, t, Sfx::BucketEmpty as i32);
    if !c.creative() {
        let s = if hand == 0 { c.inv.hand_mut() } else { &mut c.inv.offhand };
        *s = ItemStack::of(I_BUCKET, 1);
    }
    Use::Swing
}

/// ItemUsage.exchangeStack: −1 пустое ведро; стало пусто — полное в руку, иначе в инвентарь, нет места — выбросить (сервер).
fn exchange(c: &mut ActCtx, hand: usize, full: ItemId) {
    if c.creative() {
        return;
    }
    let s = if hand == 0 { c.inv.hand_mut() } else { &mut c.inv.offhand };
    s.count -= 1;
    if s.count <= 0 {
        *s = ItemStack::of(full, 1);
        return;
    }
    let mut give = ItemStack::of(full, 1);
    c.inv.insert(&mut give);
    if !give.is_empty() {
        if let Mode::Auth { sim, .. } = &mut c.mode {
            let p = c.player;
            sim.items.throw(p.pos, p.eye_height(), p.yaw, p.pitch, give, false, true, &mut sim.rng, &mut sim.ids);
        }
    }
}
