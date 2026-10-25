//! Блок-сущности (E6, ASP-10): печь и одинарный сундук. Хранилище — в WorldData рядом с правками: сущность
//! заводится при записи блока с сущностью (вид — в таблице блоков) и убирается, когда блок стал другим (world set_cell);
//! смена состояния того же блока (LIT, сторона) её не трогает. Убранные копятся — их содержимое рассыпает игра
//! (Sim::set_world, ItemScatterer) до дропа самого блока. Тик печей — фаза блок-сущностей после сущностей, только в
//! активной зоне 9×9 кусков (E13); вне зоны печь замирает и не догоняет. Крышки сундуков — «клиентская» фаза.
//! Окна — game/screen.rs (типаж Container: запись во вход печи идёт через правило сброса прогресса).
//! У каждой сущности — номер появления (seq): по нему идёт порядок тика печей, и он же восстанавливает этот порядок
//! после загрузки (сущности лежат в хранилище по кускам). Изменяемый доступ помечает кусок для сохранения (touched).

use std::collections::{HashMap, HashSet};

use crate::registry::blocks::BeKind;
use crate::registry::ids::*;
use crate::registry::items::{
    idef, item_by_mc, ItemId, I_CHARCOAL, I_COAL, I_IRON_INGOT, I_IRON_NUGGET, I_LAVA_BUCKET, I_LEATHER_HELMET,
    I_RAW_IRON, I_STICK, I_WOODEN_PICKAXE,
};
use crate::world::mek::MekBe;
use crate::world::stack::{Container, ItemStack};

/// Слоты печи (AbstractFurnaceBlockEntity.inventory).
pub(crate) const FURNACE_IN: usize = 0;
pub(crate) const FURNACE_FUEL: usize = 1;
pub(crate) const FURNACE_OUT: usize = 2;
/// Время плавки любого рецепта печи (cookingtime всех smelting-рецептов объёма; «нет рецепта» — тоже 200).
pub(crate) const COOK_TIME: i32 = 200;

// ---------------------------------------------------------------- данные плавки и топлива (ASP-10 §3.3–3.4)

/// Плавка будущих предметов (мобы, ASP-17/19): имена Minecraft «вход → результат». Пока таких предметов нет в реестре,
/// строки просто не находятся; появятся — заработают без правки кода.
pub(crate) const SMELT_BY_NAME: &[(&str, &str)] = &[
    ("porkchop", "cooked_porkchop"),
    ("beef", "cooked_beef"),
    ("chicken", "cooked_chicken"),
    ("mutton", "cooked_mutton"),
    ("rabbit", "cooked_rabbit"),
    ("cod", "cooked_cod"),
    ("salmon", "cooked_salmon"),
    ("potato", "baked_potato"),
];

/// Результат плавки входа (по предмету, износ не важен — AbstractCookingRecipe.matches).
pub(crate) fn smelt_result(item: ItemId) -> Option<ItemId> {
    let b = |x: crate::cell::BlockId| x as ItemId;
    let r = match item {
        I_RAW_IRON => I_IRON_INGOT,
        // Руда как предмет — только шёлком (недостижимо), рецепты в данных Minecraft есть.
        x if x == b(IRON_ORE) => I_IRON_INGOT,
        x if x == b(COAL_ORE) => I_COAL,
        x if x == b(COBBLE) => b(STONE),
        x if x == b(SAND) => b(GLASS),
        x if x == b(LOG) || x == b(BIRCH_LOG) || x == b(SPRUCE_LOG) => I_CHARCOAL,
        // Железные инструменты, меч и броня — в самородок (iron_nugget_from_smelting).
        x if (I_WOODEN_PICKAXE + 10..I_WOODEN_PICKAXE + 15).contains(&x) => I_IRON_NUGGET,
        x if (I_LEATHER_HELMET + 4..I_LEATHER_HELMET + 8).contains(&x) => I_IRON_NUGGET,
        _ => {
            if item == 0 {
                return None;
            }
            if let Some(r) = crate::world::mek::recipes::smelt(item) {
                return Some(r);
            }
            let mc = idef(item).mc;
            let (_, out) = SMELT_BY_NAME.iter().find(|(i, _)| *i == mc)?;
            let r = item_by_mc(out);
            return (r != 0).then_some(r);
        }
    };
    Some(r)
}

/// Время горения топлива в тиках (AbstractFurnaceBlockEntity.createFuelTimeMap); 0 — не топливо.
pub(crate) fn fuel_time(item: ItemId) -> i32 {
    let b = |x: crate::cell::BlockId| x as ItemId;
    match item {
        I_LAVA_BUCKET => 20000,
        x if x == b(COAL_BLOCK) => 16000,
        I_COAL | I_CHARCOAL => 1600,
        x if [LOG, BIRCH_LOG, SPRUCE_LOG, PLANKS, BIRCH_PLANKS, SPRUCE_PLANKS, CRAFTING_TABLE, CHEST, LADDER].iter().any(|&k| x == b(k)) => 300,
        // Деревянные кирка, топор, лопата, мотыга, меч — 200 при любом износе.
        x if (I_WOODEN_PICKAXE..I_WOODEN_PICKAXE + 5).contains(&x) => 200,
        I_STICK => 100,
        x if x == b(DEAD_BUSH) => 100,
        x if (b(WHITE_WOOL)..=b(PINK_WOOL)).contains(&x) => 100,
        _ => 0,
    }
}

// ---------------------------------------------------------------- печь

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Furnace {
    pub(crate) slots: [ItemStack; 3],
    /// Сколько тиков ещё гореть; «горит» = burn > 0.
    pub(crate) burn: i32,
    /// На сколько было последнее топливо — только для огонька.
    pub(crate) fuel_total: i32,
    pub(crate) cook: i32,
    /// У новой печи 0; 200 после первой записи во вход.
    pub(crate) cook_total: i32,
}

impl Furnace {
    pub(crate) fn burning(&self) -> bool {
        self.burn > 0
    }

    /// Результат, если вход есть, рецепт есть и выход примет ещё один (пуст или тот же предмет и не полон).
    pub(crate) fn fits(&self) -> Option<ItemId> {
        let inp = self.slots[FURNACE_IN];
        if inp.is_empty() {
            return None;
        }
        let res = smelt_result(inp.item)?;
        let out = self.slots[FURNACE_OUT];
        if out.is_empty() {
            return Some(res);
        }
        let same = out.item == res && out.damage == 0;
        (same && out.count < (idef(res).max_stack as i32).min(99)).then_some(res)
    }

    /// Тик печи (AbstractFurnaceBlockEntity.tick, ASP-10 §3.1). Ответ — новое LIT, если горение началось или кончилось.
    pub(crate) fn tick(&mut self) -> Option<bool> {
        let was = self.burning();
        if was {
            self.burn -= 1;
        }
        let has_in = !self.slots[FURNACE_IN].is_empty();
        let has_fuel = !self.slots[FURNACE_FUEL].is_empty(); // пустое ведро — тоже «есть»
        if self.burning() || (has_in && has_fuel) {
            let res = self.fits();
            if !self.burning() && res.is_some() {
                let fuel = self.slots[FURNACE_FUEL];
                self.burn = if fuel.is_empty() { 0 } else { fuel_time(fuel.item) };
                self.fuel_total = self.burn;
                if self.burning() && has_fuel {
                    let f = &mut self.slots[FURNACE_FUEL];
                    f.count -= 1;
                    if f.count <= 0 {
                        let rem = idef(fuel.item).remainder;
                        *f = if rem == 0 { ItemStack::EMPTY } else { ItemStack::of(rem, 1) };
                    }
                }
            }
            match res {
                Some(out) if self.burning() => {
                    self.cook += 1;
                    if self.cook == self.cook_total {
                        self.cook = 0;
                        self.cook_total = COOK_TIME; // время рецепта входа — до списания входа
                        let o = &mut self.slots[FURNACE_OUT];
                        if o.is_empty() {
                            *o = ItemStack::of(out, 1);
                        } else {
                            o.count += 1;
                        }
                        let i = &mut self.slots[FURNACE_IN];
                        i.count -= 1; // напрямую, без записи через слот: сброса нет
                        if i.count <= 0 {
                            *i = ItemStack::EMPTY;
                        }
                    }
                }
                _ => self.cook = 0, // горит, но нечего или некуда — сразу 0
            }
        } else if self.cook > 0 {
            self.cook = (self.cook - 2).clamp(0, self.cook_total);
        }
        (was != self.burning()).then_some(self.burning())
    }

    /// Огонёк окна: 0 или 2…14 пикселей снизу (float, как Java).
    pub(crate) fn flame_px(&self) -> i32 {
        if !self.burning() {
            return 0;
        }
        let d = if self.fuel_total == 0 { 200 } else { self.fuel_total };
        let p = (self.burn as f32 / d as f32).clamp(0.0, 1.0);
        (p * 13.0).ceil() as i32 + 1
    }
    /// Стрелка окна: 0…24 пикселя слева.
    pub(crate) fn arrow_px(&self) -> i32 {
        if self.cook_total == 0 || self.cook == 0 {
            return 0;
        }
        let p = (self.cook as f32 / self.cook_total as f32).clamp(0.0, 1.0);
        (p * 24.0).ceil() as i32
    }
}

/// Ячейки печи для окна. Запись во вход (Slot.setStack → AbstractFurnaceBlockEntity.setStack): положили другой
/// предмет или опустошили слот — прогресс с нуля и время рецепта; тот же предмет (доложили, забрали часть) — прогресс
/// цел. Окно пишет остаток после «взять часть» тем же предметом — поэтому правило одно.
impl Container for Furnace {
    fn size(&self) -> usize {
        3
    }
    fn get(&self, i: usize) -> ItemStack {
        self.slots[i]
    }
    fn set(&mut self, i: usize, s: ItemStack) {
        let s = if s.is_empty() { ItemStack::EMPTY } else { s };
        let same = !s.is_empty() && self.slots[i].same_kind(&s);
        self.slots[i] = s;
        if i == FURNACE_IN && !same {
            self.cook_total = COOK_TIME;
            self.cook = 0;
        }
    }
}

// ---------------------------------------------------------------- сундук

/// Крышка (ChestLidAnimator): ±0.1 за тик, float.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Lid {
    pub(crate) open: bool,
    pub(crate) progress: f32,
    pub(crate) last: f32,
}

impl Lid {
    pub(crate) fn step(&mut self) {
        self.last = self.progress;
        if !self.open && self.progress > 0.0 {
            self.progress = (self.progress - 0.1).max(0.0);
        } else if self.open && self.progress < 1.0 {
            self.progress = (self.progress + 0.1).min(1.0);
        }
    }
    /// Стоит на месте (закрыта или открыта до упора).
    #[allow(dead_code)]
    pub(crate) fn settled(&self) -> bool {
        self.last == self.progress && (if self.open { self.progress >= 1.0 } else { self.progress <= 0.0 })
    }
    /// Угол крышки в радианах на доле тика alpha: (1 − (1 − p)³)·90°.
    pub(crate) fn angle(&self, alpha: f32) -> f32 {
        let g = 1.0 - (self.last + (self.progress - self.last) * alpha);
        (1.0 - g * g * g) * std::f32::consts::FRAC_PI_2
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Chest {
    pub(crate) slots: [ItemStack; 27],
    pub(crate) viewers: u8,
    pub(crate) lid: Lid,
}

impl Chest {
    /// Окно открыто (ViewerCountManager.openContainer): первый смотрящий — звук открытия. Ответ — играть ли звук.
    pub(crate) fn on_open(&mut self) -> bool {
        let first = self.viewers == 0;
        self.viewers = self.viewers.saturating_add(1);
        self.lid.open = true;
        first
    }
    /// Окно закрыто: смотрящих не осталось — звук закрытия (в начале движения крышки) и крышка вниз.
    pub(crate) fn on_close(&mut self) -> bool {
        self.viewers = self.viewers.saturating_sub(1);
        if self.viewers == 0 {
            self.lid.open = false;
            return true;
        }
        false
    }
}

impl Container for Chest {
    fn size(&self) -> usize {
        27
    }
    fn get(&self, i: usize) -> ItemStack {
        self.slots[i]
    }
    fn set(&mut self, i: usize, s: ItemStack) {
        self.slots[i] = if s.is_empty() { ItemStack::EMPTY } else { s };
    }
}

// ---------------------------------------------------------------- хранилище

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BlockEntity {
    Furnace(Furnace),
    Chest(Chest),
    /// Машина Mekanism (world/mek): компоненты и вид — в самой сущности.
    Mek(Box<MekBe>),
}

impl BlockEntity {
    pub(crate) fn new(k: BeKind) -> Self {
        match k {
            BeKind::Furnace => Self::Furnace(Furnace::default()),
            BeKind::Chest => Self::Chest(Chest::default()),
            BeKind::Mek(k) => Self::Mek(Box::new(MekBe::new(k))),
        }
    }
    pub(crate) fn kind(&self) -> BeKind {
        match self {
            Self::Furnace(_) => BeKind::Furnace,
            Self::Chest(_) => BeKind::Chest,
            Self::Mek(m) => BeKind::Mek(m.kind),
        }
    }
    /// Содержимое по порядку слотов (рассыпание при ломании).
    pub(crate) fn stacks(&self) -> &[ItemStack] {
        match self {
            Self::Furnace(f) => &f.slots,
            Self::Chest(c) => &c.slots,
            Self::Mek(m) => &m.slots[..m.nslots()],
        }
    }
    /// Тикает в фазе блок-сущностей (печь, машина Mekanism).
    pub(crate) fn ticks(&self) -> bool {
        matches!(self, Self::Furnace(_) | Self::Mek(_))
    }
}

/// Сущность на месте и её номер появления.
struct Placed {
    seq: u64,
    be: BlockEntity,
}

#[derive(Default)]
pub(crate) struct BlockEntities {
    map: HashMap<[i32; 3], Placed>,
    /// Печи в порядке появления (список тикеров мира в Minecraft): (позиция, номер появления); убранные и заменённые
    /// на том же месте чистятся лениво.
    ticking: Vec<([i32; 3], u64)>,
    /// Убранные заменой блока — содержимое рассыпать (Game::set_world); забирает игра.
    removed: Vec<([i32; 3], BlockEntity)>,
    /// Номер следующей сущности (счётчик лежит в meta сохранения).
    next_seq: u64,
    /// Куски, где сущности заводились, убирались или их брали на запись (take_touched).
    touched: HashSet<(i32, i32)>,
}

/// Кусок клетки сущности.
fn chunk_at(p: [i32; 3]) -> (i32, i32) {
    super::chunk_of(p[0], p[2])
}

impl BlockEntities {
    pub(crate) fn get(&self, p: [i32; 3]) -> Option<&BlockEntity> {
        self.map.get(&p).map(|e| &e.be)
    }
    /// Сущность на запись (кусок помечается для сохранения).
    pub(crate) fn get_mut(&mut self, p: [i32; 3]) -> Option<&mut BlockEntity> {
        let e = self.map.get_mut(&p)?;
        self.touched.insert(chunk_at(p));
        Some(&mut e.be)
    }
    pub(crate) fn furnace(&self, p: [i32; 3]) -> Option<&Furnace> {
        match self.get(p) {
            Some(BlockEntity::Furnace(f)) => Some(f),
            _ => None,
        }
    }
    pub(crate) fn furnace_mut(&mut self, p: [i32; 3]) -> Option<&mut Furnace> {
        match self.get_mut(p) {
            Some(BlockEntity::Furnace(f)) => Some(f),
            _ => None,
        }
    }
    pub(crate) fn chest(&self, p: [i32; 3]) -> Option<&Chest> {
        match self.get(p) {
            Some(BlockEntity::Chest(c)) => Some(c),
            _ => None,
        }
    }
    pub(crate) fn chest_mut(&mut self, p: [i32; 3]) -> Option<&mut Chest> {
        match self.get_mut(p) {
            Some(BlockEntity::Chest(c)) => Some(c),
            _ => None,
        }
    }
    pub(crate) fn mek(&self, p: [i32; 3]) -> Option<&MekBe> {
        match self.get(p) {
            Some(BlockEntity::Mek(m)) => Some(m),
            _ => None,
        }
    }
    pub(crate) fn mek_mut(&mut self, p: [i32; 3]) -> Option<&mut MekBe> {
        match self.get_mut(p) {
            Some(BlockEntity::Mek(m)) => Some(m),
            _ => None,
        }
    }
    /// Машину — на время её тика (соседи меняются сразу, MEK-08 §11.2 путь 1): на месте остаётся пустая того же вида.
    pub(crate) fn mek_take(&mut self, p: [i32; 3]) -> Option<Box<MekBe>> {
        let m = self.mek_mut(p)?;
        let kind = m.kind;
        Some(std::mem::replace(m, MekBe::new(kind)).into())
    }
    /// Вернуть машину после тика.
    pub(crate) fn mek_put(&mut self, p: [i32; 3], m: Box<MekBe>) {
        if let Some(e) = self.mek_mut(p) {
            *e = *m;
        }
    }
    /// Убрать сущность (блок стал другим) — в removed для рассыпания.
    pub(super) fn remove(&mut self, p: [i32; 3]) {
        if let Some(e) = self.map.remove(&p) {
            self.touched.insert(chunk_at(p));
            self.removed.push((p, e.be));
        }
    }
    /// Завести сущность вида k, если на месте нет подходящей.
    pub(super) fn ensure(&mut self, p: [i32; 3], k: BeKind) {
        if self.get(p).map(|b| b.kind()) == Some(k) {
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.map.insert(p, Placed { seq, be: BlockEntity::new(k) });
        self.touched.insert(chunk_at(p));
        if matches!(k, BeKind::Furnace | BeKind::Mek(_)) {
            self.ticking.push((p, seq));
        }
    }
    /// Забыть сущности куска без рассыпания (клиент: кусок пришёл заново или ушёл из радиуса).
    pub(super) fn forget_chunk(&mut self, cx: i32, cz: i32) {
        self.map.retain(|p, _| chunk_at(*p) != (cx, cz));
    }
    /// Забрать убранные (содержимое рассыпает игра).
    pub(super) fn take_removed(&mut self) -> Vec<([i32; 3], BlockEntity)> {
        std::mem::take(&mut self.removed)
    }
    /// Хранилище окна (сундук или печь в клетке).
    pub(crate) fn container(&mut self, p: [i32; 3]) -> Option<&mut dyn Container> {
        match self.get_mut(p) {
            Some(BlockEntity::Furnace(f)) => Some(f),
            Some(BlockEntity::Chest(c)) => Some(c),
            Some(BlockEntity::Mek(m)) => Some(&mut **m),
            None => None,
        }
    }
    /// Тикающие (печи и машины) для фазы тика — в порядке появления (убранные и заменённые выпадают из списка).
    pub(crate) fn ticking_list(&mut self) -> Vec<[i32; 3]> {
        let map = &self.map;
        self.ticking.retain(|(p, seq)| matches!(map.get(p), Some(e) if e.seq == *seq && e.be.ticks()));
        self.ticking.iter().map(|e| e.0).collect()
    }
    /// Все сундуки: позиция и крышка (проход моделей).
    #[allow(dead_code)]
    pub(crate) fn chests(&self) -> impl Iterator<Item = ([i32; 3], &Chest)> {
        self.map.iter().filter_map(|(p, e)| if let BlockEntity::Chest(c) = &e.be { Some((*p, c)) } else { None })
    }
    /// «Клиентский» шаг крышек всех сундуков.
    pub(crate) fn step_lids(&mut self) {
        for e in self.map.values_mut() {
            if let BlockEntity::Chest(c) = &mut e.be {
                c.lid.step(); // у стоящей крышки шаг ничего не меняет (last = progress)
            }
        }
    }
    /// Клетки печей (клиент: горит ли хоть одна — по LIT в ячейке).
    pub(crate) fn furnaces(&self) -> impl Iterator<Item = [i32; 3]> + '_ {
        self.map.iter().filter(|(_, e)| matches!(e.be, BlockEntity::Furnace(_))).map(|(p, _)| *p)
    }
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    // ---------------------------------------------------------------- сохранение (save/)

    /// Все сущности: позиция, номер появления, сущность (порядок любой).
    pub(crate) fn iter(&self) -> impl Iterator<Item = ([i32; 3], u64, &BlockEntity)> {
        self.map.iter().map(|(p, e)| (*p, e.seq, &e.be))
    }
    /// Номер следующей сущности.
    pub(crate) fn next_seq(&self) -> u64 {
        self.next_seq
    }
    pub(crate) fn set_next_seq(&mut self, n: u64) {
        self.next_seq = self.next_seq.max(n);
    }
    /// Сущность из хранилища — на место (прежняя там заменяется без рассыпания); печь встаёт в список тика по номеру
    /// появления: порядок тика тот же, что до сохранения.
    pub(super) fn insert_loaded(&mut self, p: [i32; 3], seq: u64, be: BlockEntity) {
        let furnace = be.ticks();
        self.map.insert(p, Placed { seq, be });
        self.next_seq = self.next_seq.max(seq + 1);
        if furnace {
            let at = self.ticking.partition_point(|e| e.1 < seq);
            self.ticking.insert(at, (p, seq));
        }
    }
    /// Забрать куски с изменёнными сущностями.
    pub(super) fn take_touched(&mut self) -> HashSet<(i32, i32)> {
        std::mem::take(&mut self.touched)
    }
}

#[cfg(test)]
mod blockentity_tests {
    use super::*;
    use crate::registry::items::I_BUCKET;

    fn st(item: ItemId, n: i32) -> ItemStack {
        ItemStack::of(item, n)
    }
    /// Печь с входом, топливом, выходом; первый tick() — тик поджига t0.
    fn furnace(inp: ItemStack, fuel: ItemStack, out: ItemStack) -> Furnace {
        let mut f = Furnace::default();
        f.set(FURNACE_IN, inp);
        f.set(FURNACE_FUEL, fuel);
        f.set(FURNACE_OUT, out);
        f
    }
    const LOG_I: ItemId = LOG as ItemId;
    const COB: ItemId = COBBLE as ItemId;

    #[test]
    fn e01_e02_iron_and_coal() {
        // 1: 8 сырого железа + 1 уголь: слиток k на t0 + 200k − 1, огонь гаснет на t0 + 1600.
        let mut f = furnace(st(I_RAW_IRON, 8), st(I_COAL, 1), ItemStack::EMPTY);
        let mut lit = Vec::new();
        let mut ingots = Vec::new();
        for t in 0..1700 {
            if let Some(on) = f.tick() {
                lit.push((t, on));
            }
            if t == 0 {
                assert_eq!((f.burn, f.fuel_total, f.cook, f.slots[FURNACE_FUEL]), (1600, 1600, 1, ItemStack::EMPTY));
                assert_eq!((f.flame_px(), f.arrow_px()), (14, 1)); // эталон 2
            }
            if t == 198 {
                assert_eq!(f.arrow_px(), 24);
            }
            if t == 199 {
                assert_eq!((f.arrow_px(), f.flame_px()), (0, 13));
            }
            if t == 1599 {
                assert_eq!((f.burn, f.flame_px()), (1, 2));
            }
            if ingots.last().map_or(0, |&(_, n)| n) != f.slots[FURNACE_OUT].n() {
                ingots.push((t, f.slots[FURNACE_OUT].n()));
            }
        }
        assert_eq!(lit, vec![(0, true), (1600, false)]);
        let want: Vec<(i32, i32)> = (1..=8).map(|k| (200 * k - 1, k)).collect();
        assert_eq!(ingots, want);
        assert!(f.slots[FURNACE_IN].is_empty() && f.slots[FURNACE_OUT] == st(I_IRON_INGOT, 8));
    }

    #[test]
    fn e03_e05_rollback_and_refuel() {
        // 3: 2 сырого железа + палка: на t0+99 стрелка 12 и огонёк 2; t0+100 огонь гаснет, cook 98, затем −2 до 0.
        let mut f = furnace(st(I_RAW_IRON, 2), st(I_STICK, 1), ItemStack::EMPTY);
        let mut off = None;
        for t in 0..=149 {
            if f.tick() == Some(false) {
                off = Some(t);
            }
            match t {
                99 => assert_eq!((f.cook, f.arrow_px(), f.flame_px()), (100, 12, 2)),
                100 => assert_eq!(f.cook, 98),
                101 => assert_eq!(f.cook, 96),
                149 => assert_eq!(f.cook, 0),
                _ => {}
            }
        }
        assert_eq!(off, Some(100));
        // 4: 3 булыжника + бревно: камень на t0+199, на t0+299 cook 100, t0+300 огонь гаснет, t0+349 cook 0, вход 2.
        let mut f = furnace(st(COB, 3), st(LOG_I, 1), ItemStack::EMPTY);
        for t in 0..=349 {
            let r = f.tick();
            match t {
                199 => assert_eq!(f.slots[FURNACE_OUT], st(STONE as ItemId, 1)),
                299 => assert_eq!(f.cook, 100),
                300 => assert_eq!((r, f.cook), (Some(false), 98)),
                349 => assert_eq!(f.cook, 0),
                _ => {}
            }
        }
        assert_eq!(f.slots[FURNACE_IN].n(), 2);
        // 5: 4 бревна во входе, 4 в топливе: выход на 199/399/599/799, поджиги на 0, 300, 600 без смены LIT.
        let mut f = furnace(st(LOG_I, 4), st(LOG_I, 4), ItemStack::EMPTY);
        let mut lit = Vec::new();
        let mut outs = Vec::new();
        for t in 0..=950 {
            let before = f.slots[FURNACE_FUEL].n();
            if let Some(on) = f.tick() {
                lit.push((t, on));
            }
            if f.slots[FURNACE_FUEL].n() != before {
                assert!([0, 300, 600].contains(&t), "поджиг на {t}");
            }
            if t == 300 {
                assert_eq!(f.cook, 101);
            }
            if outs.len() < f.slots[FURNACE_OUT].n() as usize {
                outs.push(t);
            }
        }
        assert_eq!(outs, vec![199, 399, 599, 799]);
        assert_eq!(lit, vec![(0, true), (900, false)]);
        assert_eq!(f.slots[FURNACE_FUEL], st(LOG_I, 1));
        assert_eq!(f.slots[FURNACE_OUT].item, I_CHARCOAL);
    }

    #[test]
    fn e06_e11_output_full_lava_branches() {
        // 6: выход 63 → на t0+199 стал 64, дальше cook 0, LIT выкл на t0+1600, во входе 9.
        let mut f = furnace(st(I_RAW_IRON, 10), st(I_COAL, 1), st(I_IRON_INGOT, 63));
        for t in 0..=1600 {
            let r = f.tick();
            if t == 199 {
                assert_eq!(f.slots[FURNACE_OUT].n(), 64);
            }
            if t >= 200 && t < 1600 {
                assert_eq!(f.cook, 0);
            }
            if t == 1600 {
                assert_eq!(r, Some(false));
            }
        }
        assert_eq!(f.slots[FURNACE_IN].n(), 9);
        // 7: 64 сырого железа + ведро лавы: 20000, сразу ведро; 64-й слиток на t0+12799; огонь до t0+20000.
        let mut f = furnace(st(I_RAW_IRON, 64), ItemStack::of(I_LAVA_BUCKET, 1), ItemStack::EMPTY);
        f.tick();
        assert_eq!((f.burn, f.slots[FURNACE_FUEL]), (20000, st(I_BUCKET, 1)));
        let mut last = 0;
        for t in 1..=20000 {
            let r = f.tick();
            if f.slots[FURNACE_OUT].n() == 64 && last == 0 {
                last = t;
            }
            if t == 20000 {
                assert_eq!(r, Some(false));
            }
        }
        assert_eq!(last, 12799);
        assert!(f.slots[FURNACE_IN].is_empty());
        // 8: не горит, cook 50, выход занят другим — топливо цело, cook 0.
        let mut f = furnace(st(I_RAW_IRON, 1), st(I_COAL, 1), st(I_CHARCOAL, 1));
        f.cook = 50;
        f.cook_total = 200;
        assert_eq!(f.tick(), None);
        assert_eq!((f.cook, f.slots[FURNACE_FUEL].n()), (0, 1));
        // 9: без топлива — cook 48; 10: пустое ведро — сразу 0.
        let mut f = furnace(st(I_RAW_IRON, 1), ItemStack::EMPTY, ItemStack::EMPTY);
        f.cook = 50;
        f.tick();
        assert_eq!(f.cook, 48);
        let mut f = furnace(st(I_RAW_IRON, 1), st(I_BUCKET, 1), ItemStack::EMPTY);
        f.cook = 50;
        f.tick();
        assert_eq!(f.cook, 0);
        // 11: горит, вход пуст — горит впустую.
        let mut f = Furnace { burn: 500, fuel_total: 1600, ..Default::default() };
        assert_eq!(f.tick(), None);
        assert_eq!((f.burn, f.cook, f.burning()), (499, 0, true));
        // Без рецепта (земля) и уголь — ничего, уголь цел.
        let mut f = furnace(st(DIRT as ItemId, 3), st(I_COAL, 1), ItemStack::EMPTY);
        assert_eq!(f.tick(), None);
        assert_eq!(f.slots[FURNACE_FUEL].n(), 1);
    }

    #[test]
    fn e12_e17_slot_rule_and_bars() {
        // 15: новая печь — cook_total 0; после записи во вход — 200.
        let mut f = Furnace::default();
        assert_eq!(f.cook_total, 0);
        f.set(FURNACE_IN, st(I_RAW_IRON, 3));
        assert_eq!(f.cook_total, 200);
        // 12: тот же предмет +5 — прогресс цел; 13: другой — 0; 14: часть — цел, всё — 0.
        f.cook = 120;
        f.set(FURNACE_IN, st(I_RAW_IRON, 8));
        assert_eq!(f.cook, 120);
        f.set(FURNACE_IN, st(I_RAW_IRON, 4));
        assert_eq!(f.cook, 120);
        f.set(FURNACE_IN, ItemStack::EMPTY);
        assert_eq!(f.cook, 0);
        f.set(FURNACE_IN, st(I_RAW_IRON, 4));
        f.cook = 120;
        f.set(FURNACE_IN, st(COB, 4));
        assert_eq!((f.cook, f.cook_total), (0, 200));
        // 16: огонёк.
        let fl = |burn: i32, total: i32| Furnace { burn, fuel_total: total, ..Default::default() }.flame_px();
        assert_eq!((fl(0, 1600), fl(1600, 1600), fl(1, 1600), fl(100, 0)), (0, 14, 2, 8));
        // 17: стрелка.
        let ar = |cook: i32, total: i32| Furnace { cook, cook_total: total, ..Default::default() }.arrow_px();
        assert_eq!([ar(0, 200), ar(1, 200), ar(9, 200), ar(17, 200), ar(191, 200), ar(192, 200), ar(50, 0)], [0, 1, 2, 3, 23, 24, 0]);
        // Таблицы: деревянная кирка горит 200 при любом износе, железная изношенная — в самородок.
        assert_eq!((fuel_time(I_WOODEN_PICKAXE), fuel_time(I_STICK), fuel_time(LOG_I), fuel_time(I_COAL)), (200, 100, 300, 1600));
        assert_eq!(smelt_result(I_WOODEN_PICKAXE + 10), Some(I_IRON_NUGGET));
        assert_eq!(smelt_result(I_LEATHER_HELMET + 5), Some(I_IRON_NUGGET));
        assert_eq!((smelt_result(SAND as ItemId), smelt_result(LOG_I), smelt_result(DIRT as ItemId)), (Some(GLASS as ItemId), Some(I_CHARCOAL), None));
        assert_eq!(fuel_time(I_BUCKET), 0);
    }

    #[test]
    fn e25_e27_lid() {
        // 25: открытие в f32 — упор в 1.0 на 10-м шаге; 26: закрытие — 0.0 на 10-м.
        let mut l = Lid { open: true, ..Default::default() };
        let mut v = Vec::new();
        for _ in 0..10 {
            l.step();
            v.push(l.progress);
        }
        assert_eq!(v, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.70000005, 0.8000001, 0.9000001, 1.0]);
        l.open = false;
        let mut v = Vec::new();
        for _ in 0..10 {
            l.step();
            v.push(l.progress);
        }
        assert_eq!(v, vec![0.9, 0.79999995, 0.6999999, 0.5999999, 0.4999999, 0.39999992, 0.29999992, 0.19999993, 0.09999993, 0.0]);
        // 27: угол при p = 0.1 / 0.5 / 0.9.
        let deg = |p: f32| Lid { open: true, progress: p, last: p }.angle(0.0).to_degrees();
        assert!((deg(0.1) - 24.39).abs() < 0.01 && (deg(0.5) - 78.75).abs() < 0.01 && (deg(0.9) - 89.91).abs() < 0.01);
    }
}
