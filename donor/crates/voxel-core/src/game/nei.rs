//! Панель предметов и рецептов (G18, как NEI/JEI): список предметов игры и книга рецептов для R («как сделать») и U
//! («где используется») — верстак, печь, топливо, все машины Mekanism (камера, дробилка, плавильня, инфузор, очиститель,
//! сепаратор, вещества инфузора и очистителя, топливо генератора, энергия из предмета). Всё собирается из тех же таблиц,
//! что считают игру (crafting.rs, blockentity.rs, world/mek), и уходит в JS одним JSON при старте (`nei_data`): копий
//! таблиц в JS нет (web/game/nei.js только рисует и ищет).
//!
//! «+» у рецепта верстака (JEI transfer) — `Game::nei_fill`: рецепт раскладывается в сетку открытого окна (инвентарь
//! 2×2, верстак 3×3) обычными действиями над слотами — те же клики, что мышью (PICKUP, ПКМ по одному, тяга QUICK_CRAFT),
//! поэтому сервер проверяет их как обычно и протокол не меняется. Тег (доски) — по клетке тот вид, которого больше.

use crate::abi::{A_PICKUP, A_QUICK_CRAFT, A_QUICK_MOVE, SLOT_OUTSIDE};
use crate::game::crafting::{Ing, RecipeBook, RecipeKind};
use crate::game::screen::{MenuKind, SlotKind, Store};
use crate::game::Game;
use crate::registry::blocks::MekKind;
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::blockentity::{fuel_time, smelt_result, COOK_TIME};
use crate::world::mek::recipes::{chem_of, energy_of, oxygen_of, sub_color, sub_name, CRUSH, ENRICH, INFUSE, PURIFY};
use crate::world::mek::recipes::{SUB_HYDROGEN, SUB_LAVA, SUB_OXYGEN, SUB_WATER};
use crate::world::mek::{mek_def, SEP_J_PER_OP};
use crate::world::stack::ItemStack;

/// Мод предмета: 0 — Minecraft, 1 — Mekanism (поиск «@mekanism»).
pub(crate) const MOD_MC: u8 = 0;
pub(crate) const MOD_MEK: u8 = 1;

/// Страница рецептов (вкладка JEI): верстак, печь, топливо печи и машины.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cat {
    Craft,
    Smelt,
    Fuel,
    Enrich,
    Crush,
    Smelter,
    Infuse,
    Purify,
    Separator,
    InfuseSub,
    Oxygen,
    Heat,
    Energy,
}

pub(crate) const CATS: [Cat; 13] = [
    Cat::Craft,
    Cat::Smelt,
    Cat::Fuel,
    Cat::Enrich,
    Cat::Crush,
    Cat::Smelter,
    Cat::Infuse,
    Cat::Purify,
    Cat::Separator,
    Cat::InfuseSub,
    Cat::Oxygen,
    Cat::Heat,
    Cat::Energy,
];

/// Название вкладки и её «катализатор» (JEI): блок, на котором идёт рецепт; U по нему показывает всю вкладку.
pub(crate) fn cat_meta(c: Cat) -> (&'static str, ItemId) {
    let b = |x: crate::cell::BlockId| x as ItemId;
    match c {
        Cat::Craft => ("Верстак", b(CRAFTING_TABLE)),
        Cat::Smelt => ("Печь", b(FURNACE)),
        Cat::Fuel => ("Топливо печи", b(FURNACE)),
        Cat::Enrich => (mek_def(MekKind::Enrich).title, b(ENRICHMENT_CHAMBER)),
        Cat::Crush => (mek_def(MekKind::Crusher).title, b(CRUSHER)),
        Cat::Smelter => (mek_def(MekKind::Smelter).title, b(ENERGIZED_SMELTER)),
        Cat::Infuse => (mek_def(MekKind::Infuser).title, b(METALLURGIC_INFUSER)),
        Cat::Purify => (mek_def(MekKind::Purifier).title, b(PURIFICATION_CHAMBER)),
        Cat::Separator => (mek_def(MekKind::Separator).title, b(ELECTROLYTIC_SEPARATOR)),
        Cat::InfuseSub => ("Вещество инфузии", b(METALLURGIC_INFUSER)),
        Cat::Oxygen => ("Кислород очистителя", b(PURIFICATION_CHAMBER)),
        Cat::Heat => ("Топливо генератора", b(HEAT_GENERATOR)),
        Cat::Energy => ("Энергия из предмета", b(ENERGY_CUBE)),
    }
}

/// Все предметы игры для панели: сначала Minecraft, потом Mekanism, внутри — по номеру.
pub(crate) fn item_ids() -> Vec<ItemId> {
    let mut v: Vec<ItemId> = (1..ITEMS_END)
        .chain(G7_FIRST..G7_END)
        .filter(|&i| {
            let d = idef(i);
            !d.name.is_empty() && d.kind != ItemKind::None
        })
        .collect();
    v.sort_by_key(|&i| (mod_of(i), i));
    v
}

/// Мод предмета по имени: руды и машины Mekanism, его сырьё, пыль, схемы и сталь; медь, золото и редстоун — ваниль.
pub(crate) fn mod_of(id: ItemId) -> u8 {
    let mc = idef(id).mc;
    if matches!(mc, "raw_copper" | "raw_gold" | "copper_ingot" | "gold_ingot" | "redstone" | "copper_ore" | "gold_ore" | "redstone_ore" | "redstone_block") {
        return MOD_MC;
    }
    let b = |x: crate::cell::BlockId| x as ItemId;
    let mek = (b(OSMIUM_ORE)..=b(ELECTRIC_PUMP)).contains(&id) || (I_RAW_OSMIUM..=I_UPGRADE_GAS).contains(&id);
    if mek {
        MOD_MEK
    } else {
        MOD_MC
    }
}

/// Рецепт страницы: клетки входа (варианты предмета и число; пустой список — пустая клетка) шириной w, выходы,
/// вещества (номер SUB_*, мБ) на входе и выходе, энергия на операцию (Дж; gain — получаемая), время в тиках.
#[derive(Clone, Debug)]
pub(crate) struct NRec {
    pub(crate) cat: Cat,
    pub(crate) w: u8,
    pub(crate) h: u8,
    pub(crate) shapeless: bool,
    pub(crate) cells: Vec<(Vec<ItemId>, i32)>,
    pub(crate) outs: Vec<(ItemId, i32)>,
    pub(crate) sub_in: Vec<(u8, u32)>,
    pub(crate) sub_out: Vec<(u8, u32)>,
    pub(crate) energy: u64,
    pub(crate) gain: u64,
    pub(crate) ticks: u32,
}

impl NRec {
    fn new(cat: Cat) -> NRec {
        NRec { cat, w: 1, h: 1, shapeless: false, cells: Vec::new(), outs: Vec::new(), sub_in: Vec::new(), sub_out: Vec::new(), energy: 0, gain: 0, ticks: 0 }
    }
    fn io(cat: Cat, input: ItemId, in_n: i32, out: ItemId, out_n: i32) -> NRec {
        let mut r = NRec::new(cat);
        r.cells.push((vec![input], in_n));
        r.outs.push((out, out_n));
        r
    }
    fn machine(mut self, k: MekKind) -> NRec {
        let d = mek_def(k);
        self.energy = d.per_tick * d.ticks as u64;
        self.ticks = d.ticks;
        self
    }
    /// Делается ли предмет этим рецептом (R).
    pub(crate) fn makes(&self, id: ItemId) -> bool {
        self.outs.iter().any(|o| o.0 == id)
    }
    /// Идёт ли предмет в этот рецепт (U).
    pub(crate) fn uses(&self, id: ItemId) -> bool {
        self.cells.iter().any(|c| c.0.contains(&id))
    }
}

fn alts(i: &Ing) -> Vec<ItemId> {
    match *i {
        Ing::Item(x) => vec![x],
        Ing::Set(v) => v.to_vec(),
    }
}

/// Книга рецептов панели (порядок постоянный: номер рецепта — аргумент `nei_fill`).
pub(crate) fn recipes() -> Vec<NRec> {
    let mut v = Vec::new();
    // Верстак: рецепты книги крафта (без особой «починки»); узор — после обрезки рамки.
    for r in &RecipeBook::standard().list {
        let mut n = NRec::new(Cat::Craft);
        match &r.kind {
            RecipeKind::Shaped(p) => {
                n.w = p.w as u8;
                n.h = p.h as u8;
                n.cells = p.cells.iter().map(|c| (c.as_ref().map_or(Vec::new(), alts), 1)).collect();
            }
            RecipeKind::Shapeless(ings) => {
                let side = if ings.len() == 1 { 1 } else if ings.len() <= 4 { 2 } else { 3 };
                n.w = side;
                n.h = side;
                n.shapeless = true;
                n.cells = ings.iter().map(|i| (alts(i), 1)).collect();
            }
            RecipeKind::Repair => continue,
        }
        n.outs.push((r.out, r.count));
        v.push(n);
    }
    let items = item_ids();
    // Печь и плавильня: одна таблица плавки (smelt_result), печь — 200 тиков на топливе, плавильня — по описанию машины.
    for &i in &items {
        if let Some(o) = smelt_result(i) {
            let mut r = NRec::io(Cat::Smelt, i, 1, o, 1);
            r.ticks = COOK_TIME as u32;
            v.push(r);
        }
    }
    for &i in &items {
        let t = fuel_time(i);
        if t > 0 {
            let mut r = NRec::new(Cat::Fuel);
            r.cells.push((vec![i], 1));
            r.ticks = t as u32;
            let rem = idef(i).remainder;
            if rem != 0 {
                r.outs.push((rem, 1));
            }
            v.push(r);
        }
    }
    for e in &ENRICH {
        v.push(NRec::io(Cat::Enrich, e.input, e.in_n, e.out, e.out_n).machine(MekKind::Enrich));
    }
    for e in &CRUSH {
        v.push(NRec::io(Cat::Crush, e.input, e.in_n, e.out, e.out_n).machine(MekKind::Crusher));
    }
    for &i in &items {
        if let Some(o) = smelt_result(i) {
            v.push(NRec::io(Cat::Smelter, i, 1, o, 1).machine(MekKind::Smelter));
        }
    }
    for f in &INFUSE {
        let mut r = NRec::io(Cat::Infuse, f.input, 1, f.out, 1).machine(MekKind::Infuser);
        r.sub_in.push((f.sub, f.amount));
        v.push(r);
    }
    for p in &PURIFY {
        let mut r = NRec::io(Cat::Purify, p.input, 1, p.out, p.out_n).machine(MekKind::Purifier);
        r.sub_in.push((SUB_OXYGEN, p.rate * r.ticks));
        v.push(r);
    }
    // Сепаратор: ведро воды → бак +1 000 мБ (ведро остаётся); операция — 2 мБ воды → 2 водорода + 1 кислород, 400 Дж.
    {
        let ops = 1000 / 2;
        let mut r = NRec::io(Cat::Separator, I_WATER_BUCKET, 1, I_BUCKET, 1);
        r.sub_in.push((SUB_WATER, 1000));
        r.sub_out.push((SUB_HYDROGEN, 2 * ops));
        r.sub_out.push((SUB_OXYGEN, ops));
        r.energy = SEP_J_PER_OP * ops as u64;
        r.ticks = 0;
        v.push(r);
    }
    for &i in &items {
        if let Some((s, mb)) = chem_of(i) {
            let mut r = NRec::new(Cat::InfuseSub);
            r.cells.push((vec![i], 1));
            r.sub_out.push((s, mb));
            v.push(r);
        }
    }
    for &i in &items {
        let mb = oxygen_of(i);
        if mb > 0 {
            let mut r = NRec::new(Cat::Oxygen);
            r.cells.push((vec![i], 1));
            r.sub_out.push((SUB_OXYGEN, mb));
            v.push(r);
        }
    }
    // Тепловой генератор: ведро лавы — 1 000 мБ (ведро остаётся), прочее топливо — время горения / 20 мБ.
    for &i in &items {
        let mb = if i == I_LAVA_BUCKET { 1000 } else { (fuel_time(i) / 20) as u32 };
        if mb > 0 {
            let mut r = NRec::new(Cat::Heat);
            r.cells.push((vec![i], 1));
            r.sub_out.push((SUB_LAVA, mb));
            let rem = idef(i).remainder;
            if rem != 0 {
                r.outs.push((rem, 1));
            }
            v.push(r);
        }
    }
    for &i in &items {
        let j = energy_of(i);
        if j > 0 {
            let mut r = NRec::new(Cat::Energy);
            r.cells.push((vec![i], 1));
            r.gain = j;
            v.push(r);
        }
    }
    v
}

/// Книга панели, собранная один раз (проверка «+» зовётся в кадре, пока открыта страница рецепта).
pub(crate) fn book() -> &'static [NRec] {
    static BOOK: std::sync::OnceLock<Vec<NRec>> = std::sync::OnceLock::new();
    BOOK.get_or_init(recipes)
}

/// Строка в JSON (кавычки, косая черта и управляющие — экранировать).
fn js(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Всё для панели одним JSON: предметы [номер, мод, имя Minecraft], вкладки [название, катализатор], вещества
/// [название, цвет], рецепты {c вкладка, w, h, s бесформенный, i [[варианты], число], o [[предмет, число]],
/// si / so [[вещество, мБ]], e Дж, g Дж получаемой, t тиков}.
pub(crate) fn data_json() -> String {
    let list = |v: &[ItemId]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
    let pairs = |v: &[(u8, u32)]| v.iter().map(|(a, b)| format!("[{a},{b}]")).collect::<Vec<_>>().join(",");
    let items: Vec<String> = item_ids().iter().map(|&i| format!("[{i},{},{}]", mod_of(i), js(idef(i).mc))).collect();
    let cats: Vec<String> = CATS.iter().map(|&c| {
        let (t, k) = cat_meta(c);
        format!("[{},{k}]", js(t))
    }).collect();
    let subs: Vec<String> = (0..=SUB_WATER).map(|s| format!("[{},\"#{:06x}\"]", js(sub_name(s)), sub_color(s))).collect();
    let recs: Vec<String> = book()
        .iter()
        .map(|r| {
            let cells: Vec<String> = r.cells.iter().map(|(a, n)| format!("[[{}],{n}]", list(a))).collect();
            let outs: Vec<String> = r.outs.iter().map(|(a, n)| format!("[{a},{n}]")).collect();
            format!(
                "{{\"c\":{},\"w\":{},\"h\":{},\"s\":{},\"i\":[{}],\"o\":[{}],\"si\":[{}],\"so\":[{}],\"e\":{},\"g\":{},\"t\":{}}}",
                CATS.iter().position(|&c| c == r.cat).unwrap(),
                r.w,
                r.h,
                r.shapeless as u8,
                cells.join(","),
                outs.join(","),
                pairs(&r.sub_in),
                pairs(&r.sub_out),
                r.energy,
                r.gain,
                r.ticks
            )
        })
        .collect();
    format!("{{\"items\":[{}],\"cats\":[{}],\"subs\":[{}],\"recipes\":[{}]}}", items.join(","), cats.join(","), subs.join(","), recs.join(","))
}

// ---------------------------------------------------------------- «+»: разложить рецепт верстака в сетку окна

/// Ответы `nei_fill` / `nei_check` меньше нуля.
pub(crate) const FILL_NOT_CRAFT: i32 = -1;
pub(crate) const FILL_NO_GRID: i32 = -2;
pub(crate) const FILL_TOO_BIG: i32 = -3;
pub(crate) const FILL_BLOCKED: i32 = -4;
/// Окно блока в зоне Перекрёстка — только смотреть (клики ничего не двигают).
pub(crate) const FILL_LOCKED: i32 = -5;
/// Потолок кликов одного «+» (предсказанных кликов без ответа сервера клиент держит 256).
const MAX_CLICKS: usize = 240;

/// Сетка окна и где лежат вещи: слоты сетки по клетке, слоты рюкзака и хотбара (0–35).
struct GridMap {
    gw: usize,
    gh: usize,
    cells: Vec<usize>,
    inv: Vec<usize>,
    locked: bool,
}

/// Клетки сетки окна (в нумерации сетки gw × gh) для клеток рецепта; None — не влезает.
fn place(r: &NRec, gw: usize, gh: usize) -> Option<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    if r.shapeless {
        if r.cells.len() > gw * gh {
            return None;
        }
        for k in 0..r.cells.len() {
            out.push((k, k));
        }
    } else {
        let (w, h) = (r.w as usize, r.h as usize);
        if w > gw || h > gh {
            return None;
        }
        for y in 0..h {
            for x in 0..w {
                if !r.cells[x + y * w].0.is_empty() {
                    out.push((x + y * w, x + y * gw));
                }
            }
        }
    }
    Some(out)
}

/// Выбор вида для каждой клетки на n крафтов (по клетке — вариант, которого осталось больше); None — не хватает.
fn assign(r: &NRec, used: &[(usize, usize)], have: &std::collections::BTreeMap<ItemId, i32>, n: i32) -> Option<Vec<ItemId>> {
    let mut left = have.clone();
    let mut out = Vec::new();
    for &(rc, _) in used {
        let best = r.cells[rc]
            .0
            .iter()
            .copied()
            .filter(|&i| idef(i).max_stack as i32 >= n)
            .max_by_key(|i| (left.get(i).copied().unwrap_or(0), std::cmp::Reverse(*i)))?;
        let l = left.entry(best).or_insert(0);
        if *l < n {
            return None;
        }
        *l -= n;
        out.push(best);
    }
    Some(out)
}

impl Game {
    fn grid_map(&self) -> Option<GridMap> {
        let m = self.client.sim.ctl.screen.as_ref()?;
        if !matches!(m.kind, MenuKind::Player | MenuKind::Crafting(_)) {
            return None;
        }
        let locked = m.block_pos().is_some_and(|p| crate::game::interact::protected(&self.client.sim.world.d.borrow(), p));
        let mut cells = vec![usize::MAX; m.grid.w * m.grid.h];
        let mut inv = Vec::new();
        for (i, s) in m.slots.iter().enumerate() {
            match (s.store, s.kind) {
                (Store::Grid(k), _) => cells[k as usize] = i,
                (Store::Player(k), SlotKind::Plain) if k < 36 => inv.push(i),
                _ => {}
            }
        }
        Some(GridMap { gw: m.grid.w, gh: m.grid.h, cells, inv, locked })
    }
    fn nei_slot(&self, i: usize) -> ItemStack {
        let Some(m) = self.client.sim.ctl.screen.as_ref() else { return ItemStack::EMPTY };
        match m.slots[i].store {
            Store::Player(k) => self.client.sim.ctl.inv.get(k as usize),
            Store::Grid(k) => m.grid.cells[k as usize],
            _ => ItemStack::EMPTY,
        }
    }
    fn nei_cursor(&self) -> ItemStack {
        self.client.sim.ctl.screen.as_ref().map_or(ItemStack::EMPTY, |m| m.cursor)
    }
    /// Сколько каждого предмета у игрока для крафта: рюкзак, хотбар, сетка окна и курсор (их вещи вернутся в рюкзак).
    fn nei_have(&self, g: &GridMap) -> std::collections::BTreeMap<ItemId, i32> {
        let mut have = std::collections::BTreeMap::new();
        let c = self.nei_cursor();
        for s in g.inv.iter().chain(g.cells.iter()).map(|&i| self.nei_slot(i)).chain(std::iter::once(c)) {
            if !s.is_empty() {
                *have.entry(s.item).or_insert(0) += s.n();
            }
        }
        have
    }
    /// Проверка «+» без действий: [крафтов можно разложить (или код < 0), затем номера клеток рецепта, которых не хватает
    /// даже на один крафт]. Номер рецепта — порядок `nei::recipes`.
    pub fn nei_check(&self, idx: u32) -> Vec<i32> {
        let recs = book();
        let Some(r) = recs.get(idx as usize).filter(|r| r.cat == Cat::Craft) else { return vec![FILL_NOT_CRAFT] };
        let Some(g) = self.grid_map() else { return vec![FILL_NO_GRID] };
        if g.locked {
            return vec![FILL_LOCKED];
        }
        let Some(used) = place(r, g.gw, g.gh) else { return vec![FILL_TOO_BIG] };
        let have = self.nei_have(&g);
        let n = (1..=64).rev().find(|&n| assign(r, &used, &have, n).is_some()).unwrap_or(0);
        let mut out = vec![n];
        if n == 0 {
            // Недостающие клетки: по порядку, каждой — свой предмет из того, что ещё осталось.
            let mut left = have.clone();
            for &(rc, _) in &used {
                let pick = r.cells[rc].0.iter().copied().max_by_key(|i| left.get(i).copied().unwrap_or(0));
                match pick.and_then(|i| left.get_mut(&i).filter(|l| **l > 0)) {
                    Some(l) => *l -= 1,
                    None => out.push(rc as i32),
                }
            }
        }
        out
    }
    /// «+» (JEI transfer): разложить рецепт idx в сетку открытого окна кликами по слотам; max — сколько выйдет
    /// (Shift+«+»), иначе на один крафт. Сетку сначала убирает в рюкзак (Shift-клик). Ответ — крафтов разложено,
    /// 0 — не хватает предметов (ничего не тронуто), < 0 — код FILL_*.
    pub fn nei_fill(&mut self, idx: u32, max: bool) -> i32 {
        self.nei_fill_n(idx, max).0
    }
    /// То же и число кликов (проверки).
    pub(crate) fn nei_fill_n(&mut self, idx: u32, max: bool) -> (i32, usize) {
        let recs = book();
        let Some(r) = recs.get(idx as usize).filter(|r| r.cat == Cat::Craft) else { return (FILL_NOT_CRAFT, 0) };
        let Some(g) = self.grid_map() else { return (FILL_NO_GRID, 0) };
        if g.locked {
            return (FILL_LOCKED, 0);
        }
        let Some(used) = place(r, g.gw, g.gh) else { return (FILL_TOO_BIG, 0) };
        let have = self.nei_have(&g);
        let n = if max { (1..=64).rev().find(|&n| assign(r, &used, &have, n).is_some()) } else { assign(r, &used, &have, 1).map(|_| 1) };
        let Some(n) = n else { return (0, 0) };
        let pick = assign(r, &used, &have, n).unwrap();
        let mut clicks = 0usize;
        let click = |gm: &mut Game, clicks: &mut usize, slot: i32, button: i32, action: i32| {
            *clicks += 1;
            gm.screen_click(slot, button, action);
        };
        // Курсор — в рюкзак (в первую пустую ячейку), сетка — Shift-кликом в рюкзак.
        if !self.nei_cursor().is_empty() {
            let Some(&e) = g.inv.iter().find(|&&i| self.nei_slot(i).is_empty()) else { return (FILL_BLOCKED, clicks) };
            click(self, &mut clicks, e as i32, 0, A_PICKUP);
        }
        for &c in &g.cells {
            if !self.nei_slot(c).is_empty() {
                click(self, &mut clicks, c as i32, 0, A_QUICK_MOVE);
                if !self.nei_slot(c).is_empty() {
                    return (FILL_BLOCKED, clicks);
                }
            }
        }
        // По предмету: его клетки и сколько каждой ещё нужно.
        let mut kinds: Vec<ItemId> = pick.clone();
        kinds.sort_unstable();
        kinds.dedup();
        for x in kinds {
            let cells: Vec<usize> = used.iter().zip(&pick).filter(|(_, &p)| p == x).map(|((_, gc), _)| g.cells[*gc]).collect();
            let mut need = vec![n; cells.len()];
            let mut src = usize::MAX;
            loop {
                let open: Vec<usize> = (0..cells.len()).filter(|&i| need[i] > 0).collect();
                if open.is_empty() || clicks >= MAX_CLICKS {
                    break;
                }
                let cur = self.nei_cursor();
                if cur.is_empty() {
                    // Источник — самая большая стопка этого предмета в рюкзаке.
                    let Some(&s) = g.inv.iter().filter(|&&i| self.nei_slot(i).item == x).max_by_key(|&&i| self.nei_slot(i).n()) else { break };
                    src = s;
                    click(self, &mut clicks, s as i32, 0, A_PICKUP);
                    continue;
                }
                let (m, c) = (cur.n(), open.len() as i32);
                let k = open.iter().map(|&i| need[i]).min().unwrap();
                if m >= c * k {
                    // Всем по k: ПКМ по одному (c·k кликов) или вернуть лишнее в источник и тянуть ЛКМ (floor(m/c) = k).
                    let trim = (m - (c * k + c - 1)).max(0);
                    if c * k <= trim + c + 2 {
                        for &i in &open {
                            for _ in 0..k {
                                click(self, &mut clicks, cells[i] as i32, 1, A_PICKUP);
                            }
                            need[i] -= k;
                        }
                    } else {
                        for _ in 0..trim {
                            click(self, &mut clicks, src as i32, 1, A_PICKUP);
                        }
                        self.nei_drag(&open.iter().map(|&i| cells[i]).collect::<Vec<_>>(), &mut clicks);
                        for &i in &open {
                            need[i] -= k;
                        }
                    }
                } else if m >= c {
                    // Мало на всех по k: поровну ЛКМ-тягой (каждой floor(m/c) < k).
                    self.nei_drag(&open.iter().map(|&i| cells[i]).collect::<Vec<_>>(), &mut clicks);
                    for &i in &open {
                        need[i] -= m / c;
                    }
                } else {
                    // Меньше, чем клеток: по одному тем, кому нужно больше.
                    let mut o = open.clone();
                    o.sort_by_key(|&i| std::cmp::Reverse(need[i]));
                    for &i in o.iter().take(m as usize) {
                        click(self, &mut clicks, cells[i] as i32, 1, A_PICKUP);
                        need[i] -= 1;
                    }
                }
            }
            if !self.nei_cursor().is_empty() && src != usize::MAX {
                click(self, &mut clicks, src as i32, 0, A_PICKUP); // остаток — обратно в источник
            }
            if !self.nei_cursor().is_empty() {
                if let Some(&e) = g.inv.iter().find(|&&i| self.nei_slot(i).is_empty()) {
                    click(self, &mut clicks, e as i32, 0, A_PICKUP);
                }
            }
        }
        (n, clicks)
    }
    /// ЛКМ-тяга курсора по слотам (QUICK_CRAFT: начало, слоты, конец) — каждому floor(курсор / слотов).
    fn nei_drag(&mut self, slots: &[usize], clicks: &mut usize) {
        self.screen_click(SLOT_OUTSIDE, 0, A_QUICK_CRAFT);
        for &s in slots {
            self.screen_click(s as i32, 1, A_QUICK_CRAFT);
        }
        self.screen_click(SLOT_OUTSIDE, 2, A_QUICK_CRAFT);
        *clicks += slots.len() + 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::screen::Menu;
    use crate::world::World;

    fn table_game() -> Game {
        let w = World::new(1337);
        for x in -3..=3 {
            for z in -3..=3 {
                w.set_block(x, 100, z, STONE);
            }
        }
        w.set_block(1, 101, 1, CRAFTING_TABLE);
        let mut g = Game::new(&w);
        g.teleport(0.5, 101.0, 0.5);
        g.tick();
        g
    }

    #[test]
    fn items_and_recipes_cover_the_game() {
        let items = item_ids();
        assert!(items.len() > 150, "предметов {}", items.len());
        assert!(items.iter().any(|&i| mod_of(i) == MOD_MEK) && items.iter().any(|&i| mod_of(i) == MOD_MC));
        assert_eq!(mod_of(I_COPPER_INGOT), MOD_MC);
        assert_eq!(mod_of(I_OSMIUM_INGOT), MOD_MEK);
        assert_eq!(mod_of(ENRICHMENT_CHAMBER as ItemId), MOD_MEK);
        let recs = recipes();
        // Каждая вкладка не пуста; все предметы рецептов — в списке панели.
        for c in CATS {
            assert!(recs.iter().any(|r| r.cat == c), "пустая вкладка {c:?}");
        }
        for r in &recs {
            for (a, n) in &r.cells {
                assert!(*n >= 1 || a.is_empty());
                for i in a {
                    assert!(items.contains(i), "{} не в панели ({:?})", idef(*i).mc, r.cat);
                }
            }
            for (o, _) in &r.outs {
                assert!(items.contains(o), "{} не в панели", idef(*o).mc);
            }
        }
        // Число рецептов верстака = книга крафта без «починки».
        assert_eq!(recs.iter().filter(|r| r.cat == Cat::Craft).count(), RecipeBook::standard().list.len() - 1);
        // У предметов из машин Mekanism R находит их страницу.
        for (id, cat) in [(I_DUST_IRON, Cat::Enrich), (I_DUST_IRON, Cat::Crush), (I_ENRICHED_IRON, Cat::Infuse), (I_CLUMP_IRON, Cat::Purify), (I_OSMIUM_INGOT, Cat::Smelter)] {
            assert!(recs.iter().any(|r| r.cat == cat && r.makes(id)), "{} {cat:?}", idef(id).mc);
        }
        // U: уголь — топливо печи, вещество инфузии и топливо генератора.
        for cat in [Cat::Fuel, Cat::InfuseSub, Cat::Heat, Cat::Enrich] {
            assert!(recs.iter().any(|r| r.cat == cat && r.uses(I_COAL)), "уголь {cat:?}");
        }
    }

    #[test]
    fn data_json_is_complete() {
        let s = data_json();
        assert!(s.starts_with("{\"items\":[") && s.ends_with("]}"));
        assert_eq!(s.matches("\"c\":").count(), recipes().len());
        assert!(s.contains("\"Обогатительная камера\"") && s.contains("\"Кислород\""));
        // Скобки сбалансированы (грубая проверка JSON без зависимостей).
        let (a, b) = (s.matches('[').count(), s.matches(']').count());
        assert_eq!(a, b);
        assert_eq!(s.matches('{').count(), s.matches('}').count());
    }

    /// Окно сетки — результат и число в каждой клетке сетки.
    fn grid_state(g: &Game) -> (ItemStack, Vec<ItemStack>) {
        let m = g.client.sim.ctl.screen.as_ref().unwrap();
        (m.result, m.grid.cells[..m.grid.w * m.grid.h].to_vec())
    }

    #[test]
    fn fill_puts_every_craft_recipe_in_the_table() {
        let recs = recipes();
        let mut worst = 0;
        for (idx, r) in recs.iter().enumerate().filter(|(_, r)| r.cat == Cat::Craft) {
            let mut g = table_game();
            g.clear_inventory();
            // По 2 стопки первого варианта каждого ингредиента (стопка ведра — 1, тогда по 9 штук).
            let mut give = std::collections::BTreeMap::new();
            for (a, _) in &r.cells {
                if let Some(&i) = a.first() {
                    *give.entry(i).or_insert(0) += 1;
                }
            }
            for (&i, &cells) in &give {
                let per = idef(i).max_stack as i32;
                for _ in 0..(cells * 2).min(9) {
                    g.give(i, per);
                }
            }
            g.open_screen(Menu::crafting([1, 101, 1]));
            assert_eq!(g.nei_check(idx as u32)[0] >= 1, true, "{} проверка", idef(r.outs[0].0).mc);
            assert_eq!(g.nei_fill(idx as u32, false), 1, "{}", idef(r.outs[0].0).mc);
            let (res, cells) = grid_state(&g);
            assert_eq!((res.item, res.count), (r.outs[0].0, r.outs[0].1), "{} — результат на один крафт", idef(r.outs[0].0).mc);
            assert!(cells.iter().all(|c| c.count <= 1), "по одному в клетке");
            // Shift+«+»: каждая занятая клетка — поровну, и это столько, сколько крафтов вернул fill.
            let (n, used) = g.nei_fill_n(idx as u32, true);
            assert!(used <= MAX_CLICKS + 12, "{}: кликов {used}", idef(r.outs[0].0).mc);
            worst = worst.max(used);
            let (res, cells) = grid_state(&g);
            assert_eq!(res.item, r.outs[0].0);
            let filled: Vec<i32> = cells.iter().filter(|c| !c.is_empty()).map(|c| c.count).collect();
            assert!(n >= 1 && filled.iter().all(|&c| c == n), "{}: n {n}, клетки {filled:?}", idef(r.outs[0].0).mc);
            // Сервер принял те же клики: расхождений окна нет, сетка та же.
            g.tick();
            g.tick();
            assert_eq!(g.server.sim.stats.win_mismatch, 0, "{}", idef(r.outs[0].0).mc);
            assert_eq!(grid_state(&g).1, cells);
        }
        println!("больше всего кликов на Shift+«+»: {worst}");
    }

    #[test]
    fn fill_planks_mixed_and_missing() {
        let mut g = table_game();
        g.clear_inventory();
        let recs = recipes();
        let table = recs.iter().position(|r| r.cat == Cat::Craft && r.makes(CRAFTING_TABLE as ItemId)).unwrap();
        let pick = recs.iter().position(|r| r.cat == Cat::Craft && r.makes(I_WOODEN_PICKAXE)).unwrap();
        // 2 дубовые + 2 берёзовые доски — верстак из смеси (тег досок по клеткам).
        g.give(PLANKS as ItemId, 2);
        g.give(BIRCH_PLANKS as ItemId, 2);
        g.open_inventory();
        assert_eq!(g.nei_fill(table as u32, false), 1);
        assert_eq!(grid_state(&g).0.item, CRAFTING_TABLE as ItemId);
        // Кирке нужен верстак: в сетку 2×2 инвентаря не помещается.
        assert_eq!(g.nei_check(pick as u32), vec![FILL_TOO_BIG]);
        g.close_screen();
        g.open_screen(Menu::crafting([1, 101, 1]));
        // Нет палок: проверка называет обе клетки палок, «+» ничего не трогает.
        let miss = g.nei_check(pick as u32);
        assert_eq!(miss[0], 0);
        assert_eq!(miss.len(), 1 + 2, "две клетки палок: {miss:?}");
        assert_eq!(g.nei_fill(pick as u32, false), 0);
        // Вещи в сетке и в курсоре идут в счёт и убираются перед раскладкой.
        g.give(I_STICK, 2);
        let st = (10..46).find(|&i| g.nei_slot(i).item == I_STICK).unwrap();
        g.screen_click(st as i32, 0, A_PICKUP); // палки — в курсор
        assert!(!g.nei_cursor().is_empty());
        assert_eq!(g.nei_fill(pick as u32, false), 1);
        assert_eq!(grid_state(&g).0.item, I_WOODEN_PICKAXE);
        assert!(g.nei_cursor().is_empty());
    }

    #[test]
    fn fill_max_respects_stack_and_shared_items() {
        // Кирка: 64 доски → 21 крафт на 3 клетки досок; палки (64) — ровно по 21, лишние остаются в рюкзаке.
        let mut g = table_game();
        g.clear_inventory();
        g.give(PLANKS as ItemId, 64);
        g.give(I_STICK, 64);
        g.open_screen(Menu::crafting([1, 101, 1]));
        let recs = recipes();
        let pick = recs.iter().position(|r| r.cat == Cat::Craft && r.makes(I_WOODEN_PICKAXE)).unwrap();
        assert_eq!(g.nei_fill(pick as u32, true), 21);
        let (_, cells) = grid_state(&g);
        assert_eq!(cells.iter().filter(|c| !c.is_empty()).map(|c| c.count).collect::<Vec<_>>(), vec![21; 5]);
        let left: i32 = (0..36).map(|k| g.client.sim.ctl.inv.get(k)).filter(|s| s.item == I_STICK).map(|s| s.count).sum();
        assert_eq!(left, 64 - 42);
    }
}
