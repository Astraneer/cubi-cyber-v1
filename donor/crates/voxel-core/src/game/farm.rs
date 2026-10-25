//! Ферма (ASP-19): мотыга (вспашка), пашня (влажность от воды в 4 клетках, высыхание, вытаптывание, твёрдый блок сверху),
//! посевы (посадка на пашню при свете, рост по случайным тикам, урожай с бонусом), хлеб и стог. Блоки — registry/blocks.rs
//! (FARMLAND, WHEAT, CARROTS, POTATOES, HAY_BLOCK; влажность и возраст — биты 0–2 состояния).
//!
//! Случайные тики — Sim::random_ticks (blockticks.rs): 3 клетки на секцию 16³ в кусках зоны с правками (посевы и пашня —
//! всегда правки), скорость `random_tick_speed` (отладка: ускорение роста для приёмки). Запись влажности и возраста — сырая
//! `set_cell` (MC: флаг 2, соседей не будит; меш перестроится сам). Свет до среза света ASP-14: `crop_light` — 15 под
//! открытым небом (листва не мешает), иначе от факелов в радиусе 6 (14 − расстояние); ночью свет не убывает, посевы
//! растут и ночью. Не сделано: ядовитый картофель (нет эффектов), огороды в генераторе (отдельное решение).

use crate::abi::{EV_PLACED, EV_TOOL_BROKE};
use crate::cell::{block_of, cell_of, state_of, Cell, F_DOWN, H};
use crate::game::interact::{ActCtx, Hit, Use};
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::registry::blocks::{bdef, is_leaves, light_of, opaque, RandomTick};
use crate::registry::ids::{AIR, CARROTS, DIRT, FARMLAND, GRASS, POTATOES, WHEAT};
use crate::registry::items::{idef, ItemKind, I_WHEAT, I_WHEAT_SEEDS};
use crate::world::stack::ItemStack;
use crate::world::WorldData;

/// Посев ли блок (#crops).
pub(crate) fn is_crop(b: u16) -> bool {
    matches!(b, WHEAT | CARROTS | POTATOES)
}

/// Свет в клетке p для посевов (getBaseLightLevel(p, 0): без поправки на ночь): открытое небо — 15, иначе самый яркий
/// источник рядом (факел 14 − расстояние по осям), как свет блока без учёта преград.
pub(crate) fn crop_light(w: &mut WorldData, p: [i32; 3]) -> u8 {
    if (p[1] + 1..H).all(|y| {
        let b = w.get_block(p[0], y, p[2]);
        is_leaves(b) || !opaque(b)
    }) {
        return 15;
    }
    let mut best = 0i32;
    for dx in -6..=6i32 {
        for dy in -6..=6i32 {
            for dz in -6..=6i32 {
                let d = dx.abs() + dy.abs() + dz.abs();
                if d > 6 {
                    continue;
                }
                let l = light_of(w.get_cell(p[0] + dx, p[1] + dy, p[2] + dz)) as i32 - d;
                best = best.max(l);
            }
        }
    }
    best.max(0) as u8
}

/// Посев держится (CropBlock.canPlaceAt): снизу пашня и свет не ниже 8.
pub(crate) fn crop_survives(w: &mut WorldData, p: [i32; 3]) -> bool {
    w.get_block(p[0], p[1] - 1, p[2]) == FARMLAND && crop_light(w, p) >= 8
}

/// Вода в 9×9 клеток на уровне пашни и на один выше (FarmlandBlock.isWaterNearby): любая — источник, течёт, падает.
fn water_near(w: &mut WorldData, p: [i32; 3]) -> bool {
    (0..=1).any(|dy| (-4..=4).any(|dz| (-4..=4).any(|dx| bdef(w.get_block(p[0] + dx, p[1] + dy, p[2] + dz)).fluid == crate::registry::blocks::FLUID_WATER)))
}

/// Доступная влажность посева (CropBlock.getAvailableMoisture), f32: пашня под 3×3, та же культура по осям или
/// диагоналям делит пополам.
pub(crate) fn moisture_factor(w: &mut WorldData, p: [i32; 3], crop: u16) -> f32 {
    let mut f = 1.0f32;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let c = w.get_cell(p[0] + dx, p[1] - 1, p[2] + dz);
            let mut g = if block_of(c) != FARMLAND {
                0.0f32
            } else if state_of(c) & 7 > 0 {
                3.0
            } else {
                1.0
            };
            if dx != 0 || dz != 0 {
                g /= 4.0;
            }
            f += g;
        }
    }
    let mut same = |dx: i32, dz: i32| w.get_block(p[0] + dx, p[1], p[2] + dz) == crop;
    let along_x = same(-1, 0) || same(1, 0);
    let along_z = same(0, -1) || same(0, 1);
    let diag = same(-1, -1) || same(1, -1) || same(1, 1) || same(-1, 1);
    if (along_x && along_z) || diag {
        f /= 2.0;
    }
    f
}

/// Сколько случайных тиков нужно в среднем на переход стадии: n = ⌊25 / f⌋ + 1 (вероятность 1/n).
pub(crate) fn growth_n(f: f32) -> i32 {
    (25.0f32 / f) as i32 + 1
}

impl Sim {
    /// Случайный тик пашни и посева (FarmlandBlock.randomTick, CropBlock.randomTick).
    pub(crate) fn farm_random_tick(&mut self, w: &mut WorldData, p: [i32; 3], c: Cell, ev: &mut Events) {
        let (b, s) = (block_of(c), state_of(c));
        match bdef(b).random {
            RandomTick::Farmland => {
                let m = s & 7;
                if water_near(w, p) {
                    if m < 7 {
                        w.set_cell(p[0], p[1], p[2], cell_of(FARMLAND, 7));
                    }
                } else if m > 0 {
                    w.set_cell(p[0], p[1], p[2], cell_of(FARMLAND, m - 1));
                } else if !is_crop(w.get_block(p[0], p[1] + 1, p[2])) {
                    self.set_to_dirt(w, p, ev);
                }
            }
            RandomTick::Crop => {
                let age = s & 7;
                if age >= 7 || crop_light(w, p) < 9 {
                    return;
                }
                let n = growth_n(moisture_factor(w, p, b));
                if self.tick_rng.below(n) == 0 {
                    w.set_cell(p[0], p[1], p[2], cell_of(b, age + 1));
                }
            }
            _ => {}
        }
    }

    /// FarmlandBlock.setToDirt: земля вместо пашни с обновлением соседей (посев сверху отвалится сам).
    pub(crate) fn set_to_dirt(&mut self, w: &mut WorldData, p: [i32; 3], ev: &mut Events) {
        self.lifted.push(p);
        self.set_world(w, p, DIRT as Cell, ev);
    }

    /// Запланированный тик пашни (FarmlandBlock.scheduledTick): твёрдый блок сверху — земля.
    pub(crate) fn farmland_scheduled(&mut self, w: &mut WorldData, p: [i32; 3], ev: &mut Events) {
        if crate::game::fluids::is_solid(w.get_cell(p[0], p[1] + 1, p[2])) {
            self.set_to_dirt(w, p, ev);
        }
    }

    /// Приземление на пашню (FarmlandBlock.onLandedUpon): шанс fd − 0.5; топчут живые побольше курицы (игрок, свинья, корова,
    /// овца, зомби: ширина² · высота > 0.512). ГСЧ тратится при любом приземлении. true — пашня стала землёй.
    pub(crate) fn land_on_farmland(&mut self, w: &mut WorldData, land: [i32; 3], fd: f32, tramples: bool, ev: &mut Events) -> bool {
        let roll = self.rng.next_f32();
        if roll < fd - 0.5 && tramples && w.get_block(land[0], land[1], land[2]) == FARMLAND {
            self.set_to_dirt(w, land, ev);
            return true;
        }
        false
    }

    /// Дроп посева по возрасту (loot_table/blocks/wheat, carrots, potatoes): зрелая пшеница — 1 пшеница и семена × бонус,
    /// незрелая — 1 семя; морковь и картофель — 1, зрелые — ещё × бонус. Бонус = 1 + успехи 3 бросков по 0.5714286.
    pub(crate) fn crop_loot(&mut self, cell: Cell) -> Vec<ItemStack> {
        let (b, ripe) = (block_of(cell), state_of(cell) & 7 >= 7);
        let mut out = Vec::new();
        let bonus = |rng: &mut crate::game::GameRng| 1 + (0..3).filter(|_| rng.next_f32() < 0.5714286).count() as i32;
        match b {
            WHEAT => {
                out.push(ItemStack::of(if ripe { I_WHEAT } else { I_WHEAT_SEEDS }, 1));
                if ripe {
                    out.push(ItemStack::of(I_WHEAT_SEEDS, bonus(&mut self.rng)));
                }
            }
            CARROTS | POTATOES => {
                let item = if b == CARROTS { crate::registry::items::I_CARROT } else { crate::registry::items::I_POTATO };
                out.push(ItemStack::of(item, 1));
                if ripe {
                    out.push(ItemStack::of(item, bonus(&mut self.rng)));
                }
            }
            _ => {}
        }
        out
    }
}

/// Мотыга (HoeItem.useOnBlock): трава (любой вариант) и земля под воздухом, не снизу → пашня влажностью 0; звук ставления,
/// −1 прочности. Pass — не годится (ни взмаха, ни звука).
pub(crate) fn till(c: &mut ActCtx, hand: usize, h: Hit) -> Use {
    let cell = c.w.get_cell(h.pos[0], h.pos[1], h.pos[2]);
    let b = block_of(cell);
    if !(bdef(b).base == GRASS || b == DIRT) || h.face == F_DOWN {
        return Use::Pass;
    }
    if c.w.get_block(h.pos[0], h.pos[1] + 1, h.pos[2]) != AIR {
        return Use::Pass;
    }
    if !c.write(h.pos, cell_of(FARMLAND, 0)) {
        return Use::Pass;
    }
    c.emit(EV_PLACED, h.pos, FARMLAND as i32);
    wear_hand(c, hand, 1, h.pos);
    Use::Swing
}

/// Износ предмета в руке на n (не в полёте); сломался — исчезает со звуком (Item.damage).
pub(crate) fn wear_hand(c: &mut ActCtx, hand: usize, n: u16, pos: [i32; 3]) {
    if c.creative() {
        return;
    }
    let s = if hand == 0 { c.inv.hand() } else { c.inv.offhand };
    let max = idef(s.id()).max_damage;
    if max == 0 {
        return;
    }
    let slot = if hand == 0 { c.inv.hand_mut() } else { &mut c.inv.offhand };
    slot.damage += n;
    if slot.damage >= max {
        slot.count -= 1;
        if slot.count <= 0 {
            *slot = ItemStack::EMPTY;
        }
        c.emit(EV_TOOL_BROKE, pos, s.id() as i32);
    }
}

/// Посадка: Seed в руке — обычное ставление посева (interact::place); не вышло, а предмет — еда (морковь, картофель), —
/// Pass: клиент и сервер дальше пробуют съесть.
pub(crate) fn plant(c: &mut ActCtx, hand: usize, h: Hit) -> Use {
    let stack = if hand == 0 { c.inv.hand() } else { c.inv.offhand };
    let ItemKind::Seed { crop, nutrition } = idef(stack.id()).kind else { return Use::Pass };
    let r = crate::game::interact::place(c, hand, crop, h);
    if r == Use::Fail && nutrition > 0 {
        Use::Pass
    } else {
        r
    }
}
