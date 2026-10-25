//! Окна с ячейками (ASP-08 §1.3–§4, ASP-09 §1.6–§3, ASP-10 §2): окно инвентаря (сетка 2×2, броня,
//! вторая рука), окно верстака (3×3), сундука (9×3) и печи (вход, топливо, выход) — модель ScreenHandler: слоты,
//! курсорный стек, все действия onSlotClick (SlotActionType), Shift-клик (quickMove), перетаскивание (QUICK_CRAFT),
//! двойной клик (PICKUP_ALL), закрытие. Хранилища — типаж Container (инвентарь игрока, сундук и печь —
//! blockentity.rs), сетка и результат живут в самом окне (у окна инвентаря сетка при закрытом окне всегда пуста —
//! равносильно сетке «у игрока»).
//! JS держит только автомат мыши и клавиш (web/game/screen.js, ASP-08 §2.8) и рисование.
//! Коды действий над слотом (SlotActionType) — abi.rs.

use crate::abi::{A_CLONE, A_PICKUP, A_PICKUP_ALL, A_QUICK_CRAFT, A_QUICK_MOVE, A_SWAP, A_THROW, SLOT_OUTSIDE};
use crate::game::crafting::{Grid, RecipeBook};
use crate::game::inventory::{Inventory, OFFHAND};
use crate::registry::items::{equip_slot, ItemId, I_BUCKET};
use crate::registry::blocks::MekKind;
use crate::world::blockentity::{fuel_time, smelt_result, FURNACE_FUEL, FURNACE_IN, FURNACE_OUT};
use crate::world::mek::{mek_def, role_accepts, Role};
use crate::world::stack::{Container, ItemStack};

/// Вид слота: обычный, результат крафта, броня (место: 0 ноги … 3 голова), вторая рука; печь: вход, топливо (только
/// топливо и пустое ведро, ведро — по одному), выход (класть нельзя, брать — только целиком в курсор с тем же).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SlotKind {
    Plain,
    Result,
    Armor(u8),
    Offhand,
    FurnaceIn,
    Fuel,
    Output,
    /// Слот машины Mekanism с ролью (MEK-08 §11.1).
    Mek(Role),
}

/// Где лежит вещь слота: инвентарь игрока (плоский индекс 0–40), сетка окна, результат, блок-сущность.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Store {
    Player(u8),
    Grid(u8),
    Result,
    Block(u8),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotDef {
    pub(crate) store: Store,
    pub(crate) kind: SlotKind,
    /// Левый верхний угол иконки 16×16 в окне (пиксели GUI).
    pub(crate) x: i16,
    pub(crate) y: i16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuKind {
    /// Окно инвентаря (E): 176×166, слоты ASP-08 §1.5.
    Player,
    /// Верстак в клетке: 176×166, слоты ASP-09 §3.2.
    Crafting([i32; 3]),
    /// Одинарный сундук: 176×168, слоты ASP-08 §1.6.
    Chest([i32; 3]),
    /// Печь: 176×166, слоты ASP-10 §2.2.
    Furnace([i32; 3]),
    /// Машина Mekanism: 176×166, слоты и виджеты — из таблицы машин (world/mek mek_def).
    Mek([i32; 3], MekKind),
}

/// Что окно отдаёт наружу: бросок вперёд по взгляду (owned — с «бросившим», ASP-07 §2.3) и надевание брони (звук).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum UiEv {
    Drop { stack: ItemStack, owned: bool },
    Equip(ItemId),
}

/// Всё, что окно трогает снаружи (без заимствований внутри Menu).
pub(crate) struct Ui<'a> {
    pub(crate) inv: &'a mut Inventory,
    pub(crate) block: Option<&'a mut dyn Container>,
    pub(crate) book: &'a RecipeBook,
    pub(crate) ev: &'a mut Vec<UiEv>,
    /// Жив ли игрок: мёртвому вещи при закрытии не возвращаются, а выпадают.
    pub(crate) alive: bool,
    pub(crate) creative: bool,
    /// Окно блока в зоне Перекрёстка (витрина Mekanism, G13): только смотреть — клики ничего не двигают. Считают одинаково
    /// клиент (предсказание) и сервер: `interact::protected` клетки блока.
    pub(crate) locked: bool,
}

pub(crate) struct Menu {
    pub(crate) kind: MenuKind,
    pub(crate) slots: Vec<SlotDef>,
    pub(crate) cursor: ItemStack,
    pub(crate) grid: Grid,
    pub(crate) result: ItemStack,
    /// Размер окна в пикселях GUI.
    pub(crate) w: i32,
    pub(crate) h: i32,
    pub(crate) qc_stage: u8,
    pub(crate) qc_mode: u8,
    pub(crate) qc_slots: Vec<usize>, // множество без повторов; порядок на итог не влияет
}

pub(crate) fn slot(store: Store, kind: SlotKind, x: i32, y: i32) -> SlotDef {
    SlotDef { store, kind, x: x as i16, y: y as i16 }
}

/// Рюкзак 9–35 и хотбар 0–8 игрока — нижняя часть окна: y рюкзака y0 (инвентарь, верстак, печь — 84, сундук — 85),
/// хотбара y0 + 58.
pub(crate) fn player_rows(v: &mut Vec<SlotDef>, y0: i32) {
    for k in 0..27 {
        v.push(slot(Store::Player(9 + k as u8), SlotKind::Plain, 8 + 18 * (k % 9), y0 + 18 * (k / 9)));
    }
    for k in 0..9 {
        v.push(slot(Store::Player(k as u8), SlotKind::Plain, 8 + 18 * k, y0 + 58));
    }
}

impl Menu {
    /// Окно инвентаря: 0 результат, 1–4 сетка 2×2, 5–8 броня (голова → ноги), 9–35 рюкзак, 36–44 хотбар, 45 вторая рука.
    pub(crate) fn player() -> Menu {
        let mut v = vec![slot(Store::Result, SlotKind::Result, 154, 28)];
        for i in 0..4 {
            v.push(slot(Store::Grid(i as u8), SlotKind::Plain, 98 + 18 * (i % 2), 18 + 18 * (i / 2)));
        }
        for k in 0..4 {
            let e = 3 - k as u8; // HEAD 3 … FEET 0; плоский индекс 36 + e
            v.push(slot(Store::Player(36 + e), SlotKind::Armor(e), 8, 8 + 18 * k));
        }
        player_rows(&mut v, 84);
        v.push(slot(Store::Player(OFFHAND as u8), SlotKind::Offhand, 77, 62));
        Menu::with(MenuKind::Player, v, Grid::new(2, 2))
    }
    /// Окно верстака: 0 результат, 1–9 сетка 3×3 (1 + x + 3y), 10–36 рюкзак, 37–45 хотбар.
    pub(crate) fn crafting(pos: [i32; 3]) -> Menu {
        let mut v = vec![slot(Store::Result, SlotKind::Result, 124, 35)];
        for i in 0..9 {
            v.push(slot(Store::Grid(i as u8), SlotKind::Plain, 30 + 18 * (i % 3), 17 + 18 * (i / 3)));
        }
        player_rows(&mut v, 84);
        Menu::with(MenuKind::Crafting(pos), v, Grid::new(3, 3))
    }
    /// Окно сундука: 0–26 сундук (8 + 18k, 18 + 18j), 27–53 рюкзак (y 85), 54–62 хотбар (y 143); 176×168.
    pub(crate) fn chest(pos: [i32; 3]) -> Menu {
        let mut v = Vec::with_capacity(63);
        for k in 0..27 {
            v.push(slot(Store::Block(k as u8), SlotKind::Plain, 8 + 18 * (k % 9), 18 + 18 * (k / 9)));
        }
        player_rows(&mut v, 85);
        let mut m = Menu::with(MenuKind::Chest(pos), v, Grid::new(0, 0));
        m.h = 168;
        m
    }
    /// Окно печи: 0 вход (56, 17), 1 топливо (56, 53), 2 выход (116, 35), 3–29 рюкзак, 30–38 хотбар; 176×166.
    pub(crate) fn furnace(pos: [i32; 3]) -> Menu {
        let mut v = vec![
            slot(Store::Block(FURNACE_IN as u8), SlotKind::FurnaceIn, 56, 17),
            slot(Store::Block(FURNACE_FUEL as u8), SlotKind::Fuel, 56, 53),
            slot(Store::Block(FURNACE_OUT as u8), SlotKind::Output, 116, 35),
        ];
        player_rows(&mut v, 84);
        Menu::with(MenuKind::Furnace(pos), v, Grid::new(0, 0))
    }
    /// Окно машины Mekanism: слоты машины по таблице (порядок — он же порядок Shift-клика), затем рюкзак (y 84) и
    /// хотбар; 176×166.
    pub(crate) fn mek(pos: [i32; 3], k: MekKind) -> Menu {
        let d = mek_def(k);
        let mut v: Vec<SlotDef> = d.slots.iter().enumerate().map(|(i, &(r, x, y))| slot(Store::Block(i as u8), SlotKind::Mek(r), x as i32, y as i32)).collect();
        player_rows(&mut v, 84);
        Menu::with(MenuKind::Mek(pos, k), v, Grid::new(0, 0))
    }
    /// Клетка блока окна (верстак, сундук, печь, машина) или None — окно инвентаря.
    pub(crate) fn block_pos(&self) -> Option<[i32; 3]> {
        match self.kind {
            MenuKind::Player => None,
            MenuKind::Crafting(p) | MenuKind::Chest(p) | MenuKind::Furnace(p) | MenuKind::Mek(p, _) => Some(p),
        }
    }
    pub(crate) fn with(kind: MenuKind, slots: Vec<SlotDef>, grid: Grid) -> Menu {
        Menu { kind, slots, cursor: ItemStack::EMPTY, grid, result: ItemStack::EMPTY, w: 176, h: 166, qc_stage: 0, qc_mode: 0, qc_slots: Vec::new() }
    }

    // ------------------------------------------------------------ ячейки слотов

    pub(crate) fn get(&self, u: &Ui, id: usize) -> ItemStack {
        match self.slots[id].store {
            Store::Player(i) => u.inv.get(i as usize),
            Store::Grid(i) => self.grid.cells[i as usize],
            Store::Result => self.result,
            Store::Block(i) => u.block.as_deref().map_or(ItemStack::EMPTY, |b| b.get(i as usize)),
        }
    }
    /// Записать (Slot.setStack). Сетка — пересчёт результата (onContentChanged); броня — звук надевания
    /// (LivingEntity.onEquipStack: новый предмет этого места и не равный прежнему).
    pub(crate) fn put(&mut self, u: &mut Ui, id: usize, s: ItemStack) {
        let s = if s.is_empty() { ItemStack::EMPTY } else { s };
        match self.slots[id].store {
            Store::Player(i) => {
                let old = u.inv.get(i as usize);
                u.inv.set(i as usize, s);
                if let SlotKind::Armor(e) = self.slots[id].kind {
                    if !s.is_empty() && !old.same_kind(&s) && equip_slot(s.item) == Some(e) {
                        u.ev.push(UiEv::Equip(s.item));
                    }
                }
            }
            Store::Grid(i) => {
                self.grid.cells[i as usize] = s;
                self.result = u.book.result(&self.grid);
            }
            Store::Result => self.result = s,
            Store::Block(i) => {
                if let Some(b) = u.block.as_deref_mut() {
                    b.set(i as usize, s);
                }
            }
        }
    }
    pub(crate) fn is_result(&self, id: usize) -> bool {
        self.slots[id].kind == SlotKind::Result
    }
    /// Slot.canInsert: в результат и выход печи — нельзя; в броню — только предмет своего места; в топливо — топливо
    /// или пустое ведро (FurnaceFuelSlot).
    pub(crate) fn can_insert(&self, id: usize, s: &ItemStack) -> bool {
        match self.slots[id].kind {
            SlotKind::Result | SlotKind::Output => false,
            SlotKind::Armor(e) => equip_slot(s.item) == Some(e),
            SlotKind::Fuel => fuel_time(s.id()) > 0 || s.id() == I_BUCKET,
            SlotKind::Mek(r) => match self.kind {
                MenuKind::Mek(_, k) => role_accepts(k, r, s.id()),
                _ => false,
            },
            _ => true,
        }
    }
    /// ScreenHandler.canInsertIntoSlot (перетаскивание, двойной клик): у окон игрока и верстака — не результат.
    pub(crate) fn can_insert_into(&self, id: usize) -> bool {
        !self.is_result(id)
    }
    /// Slot.getMaxItemCount(stack): min(99, предел стека); броня — 1; пустое ведро в топливе — 1.
    pub(crate) fn limit(&self, id: usize, s: &ItemStack) -> i32 {
        let base = match self.slots[id].kind {
            SlotKind::Armor(_) => 1,
            SlotKind::Fuel if s.id() == I_BUCKET => 1,
            _ => 99,
        };
        base.min(s.max())
    }
    /// canTakePartial: можно брать и класть обратно свой стек — у результата нет (только целиком).
    pub(crate) fn takes_partial(&self, u: &Ui, id: usize) -> bool {
        self.can_insert(id, &self.get(u, id))
    }
    /// ScreenHandler.canInsertItemIntoSlot(slot, stack, allowOverflow = true): пусто или тот же предмет не сверх предела
    /// (полный стек тоже проходит).
    pub(crate) fn fits_overflow(&self, u: &Ui, id: usize, c: &ItemStack) -> bool {
        let s = self.get(u, id);
        s.is_empty() || (s.same_kind(c) && s.n() <= c.max())
    }
    /// Одно ли хранилище у двух слотов (Shift + двойной клик: slot.inventory == ...): весь инвентарь игрока — одно.
    pub(crate) fn same_inventory(&self, a: usize, b: usize) -> bool {
        use Store::*;
        matches!((self.slots[a].store, self.slots[b].store), (Player(_), Player(_)) | (Grid(_), Grid(_)) | (Result, Result) | (Block(_), Block(_)))
    }

    /// Slot.takeStackRange: tryTakeStackRange (результат — только целиком) + onTakeItem.
    pub(crate) fn take_range(&mut self, u: &mut Ui, id: usize, min: i32, max: i32) -> ItemStack {
        let s = self.get(u, id);
        if s.is_empty() {
            return ItemStack::EMPTY;
        }
        if !self.takes_partial(u, id) && max < s.n() {
            return ItemStack::EMPTY;
        }
        let n = min.min(max);
        let taken = if self.is_result(id) { s } else { s.with_count(n.min(s.n())) };
        if taken.n() <= 0 {
            return ItemStack::EMPTY;
        }
        self.put(u, id, s.with_count(s.n() - taken.n()));
        self.on_take(u, id);
        taken
    }
    /// Slot.onTakeItem: у результата — расход сетки (−1 из каждой клетки рамки, остатки рецепта) и пересчёт.
    pub(crate) fn on_take(&mut self, u: &mut Ui, id: usize) {
        if !self.is_result(id) {
            return;
        }
        let (inv, ev) = (&mut *u.inv, &mut *u.ev);
        u.book.consume(&mut self.grid, &mut |s| {
            let left = inv.offer(s);
            if !left.is_empty() {
                ev.push(UiEv::Drop { stack: left, owned: false });
            }
        });
        self.result = u.book.result(&self.grid);
    }
    /// Slot.insertStack: положить из src в слот не больше n; ответ — остаток.
    pub(crate) fn insert_into(&mut self, u: &mut Ui, id: usize, mut src: ItemStack, n: i32) -> ItemStack {
        if src.is_empty() || !self.can_insert(id, &src) {
            return src;
        }
        let have = self.get(u, id);
        let k = n.min(src.n()).min(self.limit(id, &src) - have.n());
        if k <= 0 {
            return src;
        }
        if have.is_empty() {
            let part = src.with_count(k);
            src.count -= k;
            self.put(u, id, part);
        } else if have.same_kind(&src) {
            src.count -= k;
            self.put(u, id, have.with_count(have.n() + k));
        }
        if src.is_empty() {
            ItemStack::EMPTY
        } else {
            src
        }
    }
    pub(crate) fn drop(&mut self, u: &mut Ui, stack: ItemStack, owned: bool) {
        if !stack.is_empty() {
            u.ev.push(UiEv::Drop { stack, owned });
        }
    }

    // ------------------------------------------------------------ действия

    /// onSlotClick: одно действие ASP-08 §2. slot — номер слота окна, −999 вне окна, −1 мимо слотов.
    pub(crate) fn click(&mut self, u: &mut Ui, slot: i32, button: i32, action: i32) {
        if u.locked {
            return;
        }
        if action == A_QUICK_CRAFT {
            return self.quick_craft(u, slot, button);
        }
        if self.qc_stage != 0 {
            return self.qc_reset(); // недоделанная тяга глотает действие
        }
        let n = self.slots.len() as i32;
        if slot >= n {
            return;
        }
        match action {
            A_PICKUP | A_QUICK_MOVE if button == 0 || button == 1 => {
                if slot == SLOT_OUTSIDE {
                    if !self.cursor.is_empty() {
                        let s = if button == 0 {
                            std::mem::take(&mut self.cursor)
                        } else {
                            let one = self.cursor.with_count(1);
                            self.cursor = self.cursor.with_count(self.cursor.n() - 1);
                            one
                        };
                        if self.cursor.is_empty() {
                            self.cursor = ItemStack::EMPTY;
                        }
                        self.drop(u, s, true);
                    }
                } else if slot < 0 {
                } else if action == A_QUICK_MOVE {
                    let id = slot as usize;
                    let mut r = self.quick_move(u, id);
                    // Повтор, пока в слоте тот же предмет (areItemsEqual — без компонентов): крафт «до упора».
                    while !r.is_empty() && self.get(u, id).id() == r.id() {
                        r = self.quick_move(u, id);
                    }
                } else {
                    self.pickup(u, slot as usize, button == 0);
                }
            }
            A_SWAP if slot >= 0 && ((0..9).contains(&button) || button == OFFHAND as i32) => self.swap(u, slot as usize, button as usize),
            A_CLONE if u.creative && self.cursor.is_empty() && slot >= 0 => {
                let s = self.get(u, slot as usize);
                if !s.is_empty() {
                    self.cursor = s.with_count(s.max());
                }
            }
            A_THROW if self.cursor.is_empty() && slot >= 0 => {
                let id = slot as usize;
                let k = if button == 0 { 1 } else { self.get(u, id).n() };
                let s = self.take_range(u, id, k, i32::MAX);
                self.drop(u, s, true);
            }
            A_PICKUP_ALL if slot >= 0 => self.pickup_all(u, slot as usize, button),
            _ => {}
        }
    }

    /// PICKUP: ЛКМ — всё/обмен, ПКМ — половина (вверх) или одна штука (ASP-08 §2.1).
    pub(crate) fn pickup(&mut self, u: &mut Ui, id: usize, left: bool) {
        let s = self.get(u, id);
        let c = self.cursor;
        if s.is_empty() {
            if !c.is_empty() {
                self.cursor = self.insert_into(u, id, c, if left { c.n() } else { 1 });
            }
            return;
        }
        if c.is_empty() {
            let n = if left { s.n() } else { (s.n() + 1) / 2 };
            let t = self.take_range(u, id, n, i32::MAX);
            if !t.is_empty() {
                self.cursor = t;
            }
        } else if self.can_insert(id, &c) {
            if s.same_kind(&c) {
                self.cursor = self.insert_into(u, id, c, if left { c.n() } else { 1 });
            } else if c.n() <= self.limit(id, &c) {
                self.cursor = s;
                self.put(u, id, c);
            }
        } else if s.same_kind(&c) {
            // Результат крафта в курсор с тем же предметом — только целиком.
            let t = self.take_range(u, id, s.n(), c.max() - c.n());
            if !t.is_empty() {
                self.cursor.count += t.n();
            }
        }
    }

    /// ScreenHandler.insertItem: проход сложения по [from, to) (или с конца), затем ОДИН первый пустой слот.
    pub(crate) fn insert_item(&mut self, u: &mut Ui, s: &mut ItemStack, from: usize, to: usize, rev: bool) -> bool {
        let mut moved = false;
        let order: Vec<usize> = if rev { (from..to).rev().collect() } else { (from..to).collect() };
        if s.stackable() {
            for &i in &order {
                if s.is_empty() {
                    break;
                }
                let have = self.get(u, i);
                if have.is_empty() || !have.same_kind(s) {
                    continue;
                }
                let (j, k) = (have.n() + s.n(), self.limit(i, &have));
                if j <= k {
                    s.count = 0;
                    self.put(u, i, have.with_count(j));
                    moved = true;
                } else if have.n() < k {
                    s.count -= k - have.n();
                    self.put(u, i, have.with_count(k));
                    moved = true;
                }
            }
        }
        if !s.is_empty() {
            for &i in &order {
                if self.get(u, i).is_empty() && self.can_insert(i, s) {
                    let k = s.n().min(self.limit(i, s));
                    let part = s.with_count(k);
                    s.count -= k;
                    self.put(u, i, part);
                    moved = true;
                    break;
                }
            }
        }
        if s.is_empty() {
            *s = ItemStack::EMPTY;
        }
        moved
    }

    /// quickMove окна (ASP-08 §1.5, ASP-09 §3.2). Ответ — копия исходного стека или пусто (ничего не сдвинулось).
    pub(crate) fn quick_move(&mut self, u: &mut Ui, id: usize) -> ItemStack {
        let orig = self.get(u, id);
        if orig.is_empty() {
            return ItemStack::EMPTY;
        }
        let mut s = orig;
        let ok = match self.kind {
            MenuKind::Player => {
                let equip = equip_slot(s.item).map(|e| 8 - e as usize).filter(|&a| self.get(u, a).is_empty());
                if id == 0 {
                    self.insert_item(u, &mut s, 9, 45, true)
                } else if (1..9).contains(&id) {
                    self.insert_item(u, &mut s, 9, 45, false)
                } else if let Some(a) = equip {
                    self.insert_item(u, &mut s, a, a + 1, false)
                } else if (9..36).contains(&id) {
                    self.insert_item(u, &mut s, 36, 45, false)
                } else if (36..45).contains(&id) {
                    self.insert_item(u, &mut s, 9, 36, false)
                } else {
                    self.insert_item(u, &mut s, 9, 45, false)
                }
            }
            MenuKind::Crafting(_) => {
                if id == 0 {
                    self.insert_item(u, &mut s, 10, 46, true)
                } else if (10..46).contains(&id) {
                    // Причуда верстака: из инвентаря — сначала в сетку 3×3, только потом рюкзак ↔ хотбар.
                    self.insert_item(u, &mut s, 1, 10, false)
                        || if id < 37 { self.insert_item(u, &mut s, 37, 46, false) } else { self.insert_item(u, &mut s, 10, 37, false) }
                } else {
                    self.insert_item(u, &mut s, 10, 46, false)
                }
            }
            // GenericContainerScreenHandler.quickMove: из сундука — в инвентарь с конца (хотбар справа налево, потом
            // рюкзак); из инвентаря — в сундук с начала; рюкзак ↔ хотбар здесь не перекладывается.
            MenuKind::Chest(_) => {
                if id < 27 {
                    self.insert_item(u, &mut s, 27, 63, true)
                } else {
                    self.insert_item(u, &mut s, 0, 27, false)
                }
            }
            // AbstractFurnaceScreenHandler.quickMove (ASP-10 §2.4): выход — в инвентарь с конца; вход и топливо — в
            // инвентарь с начала; из инвентаря сначала «плавится?» → вход, потом «топливо?» → топливо (занято — ничего,
            // без перекладки), прочее — рюкзак ↔ хотбар.
            // MekanismContainer.quickMoveStack (MEK-08 §4.2): из машины — в инвентарь; из инвентаря — в слоты машины
            // по порядку: сначала доложить в непустые подходящие, потом в пустые; иначе рюкзак ↔ хотбар.
            MenuKind::Mek(_, k) => {
                let n = mek_def(k).slots.len();
                if id < n {
                    self.insert_item(u, &mut s, n, n + 36, false)
                } else {
                    let mut ok = false;
                    for pass in 0..2 {
                        for j in 0..n {
                            if s.is_empty() || !self.can_insert(j, &s) {
                                continue;
                            }
                            let have = self.get(u, j);
                            if (pass == 0) == have.is_empty() {
                                continue;
                            }
                            ok |= self.insert_item(u, &mut s, j, j + 1, false);
                        }
                    }
                    ok || if id < n + 27 { self.insert_item(u, &mut s, n + 27, n + 36, false) } else { self.insert_item(u, &mut s, n, n + 27, false) }
                }
            }
            MenuKind::Furnace(_) => {
                if id == FURNACE_OUT {
                    self.insert_item(u, &mut s, 3, 39, true)
                } else if id == FURNACE_IN || id == FURNACE_FUEL {
                    self.insert_item(u, &mut s, 3, 39, false)
                } else if smelt_result(s.id()).is_some() {
                    self.insert_item(u, &mut s, 0, 1, false)
                } else if fuel_time(s.id()) > 0 {
                    self.insert_item(u, &mut s, 1, 2, false)
                } else if (3..30).contains(&id) {
                    self.insert_item(u, &mut s, 30, 39, false)
                } else {
                    self.insert_item(u, &mut s, 3, 30, false)
                }
            }
        };
        if !ok {
            return ItemStack::EMPTY;
        }
        self.put(u, id, s); // остаток (или пусто) остаётся в слоте
        if s.n() == orig.n() {
            return ItemStack::EMPTY;
        }
        self.on_take(u, id);
        if self.is_result(id) {
            self.drop(u, s, false); // излишек последнего крафта — на землю
        }
        orig
    }

    /// SWAP: цифра 0–8 или вторая рука 40 — ячейка инвентаря, а не слот окна (ASP-08 §2.3).
    pub(crate) fn swap(&mut self, u: &mut Ui, id: usize, hb: usize) {
        let mut h = u.inv.get(hb);
        let s = self.get(u, id);
        if h.is_empty() && s.is_empty() {
            return;
        }
        if h.is_empty() {
            u.inv.set(hb, s);
            self.put(u, id, ItemStack::EMPTY);
            self.on_take(u, id);
        } else if s.is_empty() {
            if self.can_insert(id, &h) {
                let p = self.limit(id, &h);
                if h.n() > p {
                    let part = h.with_count(p);
                    h.count -= p;
                    u.inv.set(hb, h);
                    self.put(u, id, part);
                } else {
                    u.inv.set(hb, ItemStack::EMPTY);
                    self.put(u, id, h);
                }
            }
        } else if self.can_insert(id, &h) {
            let p = self.limit(id, &h);
            if h.n() > p {
                let part = h.with_count(p);
                h.count -= p;
                u.inv.set(hb, h);
                self.put(u, id, part);
                self.on_take(u, id);
                let mut rest = s;
                if !u.inv.insert(&mut rest) || !rest.is_empty() {
                    self.drop(u, rest, true);
                }
            } else {
                u.inv.set(hb, s);
                self.put(u, id, h);
                self.on_take(u, id);
            }
        }
    }

    /// PICKUP_ALL (двойной клик): курсор не пуст, слот пуст — два прохода (неполные, потом любые) с начала.
    pub(crate) fn pickup_all(&mut self, u: &mut Ui, id: usize, button: i32) {
        let c = self.cursor;
        if c.is_empty() || !self.get(u, id).is_empty() {
            return;
        }
        let n = self.slots.len();
        let order: Vec<usize> = if button == 0 { (0..n).collect() } else { (0..n).rev().collect() };
        for pass in 0..2 {
            for &i in &order {
                if self.cursor.n() >= self.cursor.max() {
                    return;
                }
                let s = self.get(u, i);
                if s.is_empty() || !self.fits_overflow(u, i, &self.cursor) || !self.can_insert_into(i) {
                    continue;
                }
                if pass == 0 && s.n() == s.max() {
                    continue;
                }
                let t = self.take_range(u, i, s.n(), self.cursor.max() - self.cursor.n());
                self.cursor.count += t.n();
            }
        }
    }

    /// QUICK_CRAFT: кнопка = стадия | режим << 2; стадии 0 начало, 1 слот, 2 конец; режимы 0 поровну, 1 по одному,
    /// 2 полными стеками (креатив).
    pub(crate) fn quick_craft(&mut self, u: &mut Ui, slot: i32, button: i32) {
        let prev = self.qc_stage;
        let stage = (button & 3) as u8;
        let mode = ((button >> 2) & 3) as u8;
        self.qc_stage = stage;
        if (prev != 1 || stage != 2) && prev != stage {
            return self.qc_reset();
        }
        if self.cursor.is_empty() {
            return self.qc_reset();
        }
        match stage {
            0 => {
                self.qc_mode = mode;
                if mode == 2 && !u.creative || mode > 2 {
                    return self.qc_reset();
                }
                self.qc_stage = 1;
                self.qc_slots.clear();
            }
            1 => {
                if slot < 0 || slot as usize >= self.slots.len() {
                    return;
                }
                let id = slot as usize;
                if self.drag_accepts(u, id, self.qc_mode, self.qc_slots.len()) && !self.qc_slots.contains(&id) {
                    self.qc_slots.push(id);
                }
            }
            2 => {
                if self.qc_slots.len() == 1 {
                    let id = self.qc_slots[0] as i32;
                    let m = self.qc_mode as i32;
                    self.qc_reset();
                    return self.click(u, id, m, A_PICKUP);
                }
                if !self.qc_slots.is_empty() {
                    let orig = self.cursor;
                    let nslots = self.qc_slots.len() as i32;
                    let share = Self::share(self.qc_mode, nslots, &orig);
                    let mut left = orig.n();
                    for sid in self.qc_slots.clone() {
                        let c = self.cursor;
                        let ok = self.fits_overflow(u, sid, &c)
                            && self.can_insert(sid, &c)
                            && (self.qc_mode == 2 || c.n() >= nslots)
                            && self.can_insert_into(sid);
                        if !ok {
                            continue;
                        }
                        let l = self.get(u, sid).n();
                        let m = orig.max().min(self.limit(sid, &orig));
                        let n = (share + l).min(m);
                        left -= n - l;
                        self.put(u, sid, orig.with_count(n));
                    }
                    self.cursor = if left > 0 { orig.with_count(left) } else { ItemStack::EMPTY };
                }
                self.qc_reset();
            }
            _ => self.qc_reset(),
        }
    }
    /// calculateStackSize: поровну ⌊n/N⌋, по одному, полными стеками.
    pub(crate) fn share(mode: u8, nslots: i32, c: &ItemStack) -> i32 {
        match mode {
            0 => c.n() / nslots.max(1),
            1 => 1,
            _ => c.max(),
        }
    }
    pub(crate) fn qc_reset(&mut self) {
        self.qc_stage = 0;
        self.qc_slots.clear();
    }

    /// Можно ли зацепить слот тягой, когда уже выбрано n (HandledScreen.mouseDragged = стадия 1 QUICK_CRAFT).
    pub(crate) fn drag_accepts(&self, u: &Ui, id: usize, mode: u8, n: usize) -> bool {
        let c = self.cursor;
        id < self.slots.len()
            && !c.is_empty()
            && self.fits_overflow(u, id, &c)
            && self.can_insert(id, &c)
            && (mode == 2 || c.n() > n as i32)
            && self.can_insert_into(id)
    }
    /// Предпросмотр тяги (HandledScreen.calculateOffset и drawSlot) — та же формула, что у стадии 2:
    /// [остаток курсора, затем на каждый слот: будущее число, 1 — упёрлось в предел (жёлтое число)].
    pub(crate) fn drag_preview(&self, u: &Ui, mode: u8, ids: &[usize]) -> Vec<i32> {
        let c = self.cursor;
        let mut out = vec![0];
        if c.is_empty() {
            return out;
        }
        let n = ids.len() as i32;
        let share = Self::share(mode, n, &c);
        let mut left = if mode == 2 { c.max() } else { c.n() };
        for &id in ids {
            if id >= self.slots.len() || !self.fits_overflow(u, id, &c) || !self.can_insert_into(id) {
                out.extend_from_slice(&[-1, 0]);
                continue;
            }
            let l = self.get(u, id).n();
            let k = c.max().min(self.limit(id, &c));
            let m = share + l;
            if mode != 2 {
                left -= m.min(k) - l;
            }
            out.extend_from_slice(&[m.min(k), (m > k) as i32]);
        }
        out[0] = left;
        out
    }

    /// Shift + двойной клик (HandledScreen.mouseReleased): QUICK_MOVE по каждому слоту того же хранилища, где лежит
    /// такой же предмет, как запомненный при нажатии (quickMovingStack).
    pub(crate) fn quick_move_all(&mut self, u: &mut Ui, slot: usize, like: ItemStack) {
        if u.locked || like.is_empty() || slot >= self.slots.len() || self.is_result(slot) {
            return;
        }
        for i in 0..self.slots.len() {
            if self.same_inventory(i, slot) && !self.get(u, i).is_empty() && self.fits_overflow(u, i, &like) {
                self.click(u, i as i32, 0, A_QUICK_MOVE);
            }
        }
    }

    /// Закрытие (ASP-08 §4, ASP-09 §1.7): курсор, затем клетки сетки по порядку — в инвентарь (offerOrDrop), место
    /// ищется заново; не влезло или игрок мёртв — бросок вперёд без владельца. Результат — не вещь, пропадает.
    pub(crate) fn close(&mut self, u: &mut Ui) {
        let c = std::mem::take(&mut self.cursor);
        self.give_back(u, c);
        self.result = ItemStack::EMPTY;
        for i in 0..self.grid.size() {
            let s = std::mem::take(&mut self.grid.cells[i]);
            self.give_back(u, s);
        }
        self.qc_reset();
    }
    pub(crate) fn give_back(&mut self, u: &mut Ui, s: ItemStack) {
        if s.is_empty() {
            return;
        }
        let left = if u.alive { u.inv.offer(s) } else { s };
        self.drop(u, left, false);
    }
}

#[cfg(test)]
mod screen_tests {
    use super::*;
    use crate::registry::ids::*;
    use crate::registry::items::*;

    // Предметы эталонов ASP-08 §13: dirt, cobble, plank, log (1 бревно → 4 доски), pick (деревянная, прочность 59),
    // helmet (железный, голова).
    const DIRT_I: ItemId = DIRT as ItemId;
    const COB: ItemId = COBBLE as ItemId;
    const PLANK: ItemId = PLANKS as ItemId;
    const LOG_I: ItemId = LOG as ItemId;
    const PICK: ItemId = I_WOODEN_PICKAXE;
    const HELMET: ItemId = I_LEATHER_HELMET + 4;

    struct T {
        inv: Inventory,
        book: RecipeBook,
        ev: Vec<UiEv>,
        m: Menu,
    }
    fn st(item: ItemId, n: i32) -> ItemStack {
        ItemStack::of(item, n)
    }
    impl T {
        fn player() -> T {
            T { inv: Inventory::new(), book: RecipeBook::standard(), ev: Vec::new(), m: Menu::player() }
        }
        fn table() -> T {
            T { inv: Inventory::new(), book: RecipeBook::standard(), ev: Vec::new(), m: Menu::crafting([0, 64, 0]) }
        }
        fn with<R>(&mut self, f: impl FnOnce(&mut Menu, &mut Ui) -> R) -> R {
            let mut u = Ui { inv: &mut self.inv, block: None, book: &self.book, ev: &mut self.ev, alive: true, creative: false, locked: false };
            f(&mut self.m, &mut u)
        }
        fn click(&mut self, slot: i32, button: i32, action: i32) {
            self.with(|m, u| m.click(u, slot, button, action));
        }
        fn get(&mut self, slot: usize) -> ItemStack {
            self.with(|m, u| m.get(u, slot))
        }
        /// Положить в слот окна (как setStack: сетка пересчитывает результат).
        fn set(&mut self, slot: usize, s: ItemStack) {
            self.with(|m, u| m.put(u, slot, s));
        }
        fn n(&mut self, slot: usize) -> i32 {
            self.get(slot).n()
        }
        /// Тяга: стадия 0, слоты, стадия 2 (кнопка = стадия | режим << 2).
        fn drag(&mut self, mode: i32, slots: &[i32]) {
            self.click(SLOT_OUTSIDE, mode << 2, A_QUICK_CRAFT);
            for &s in slots {
                self.click(s, 1 | mode << 2, A_QUICK_CRAFT);
            }
            self.click(SLOT_OUTSIDE, 2 | mode << 2, A_QUICK_CRAFT);
        }
        fn drops(&self) -> Vec<(ItemStack, bool)> {
            self.ev.iter().filter_map(|e| if let UiEv::Drop { stack, owned } = e { Some((*stack, *owned)) } else { None }).collect()
        }
    }

    #[test]
    fn e01_e12_pickup() {
        // 1–4: ЛКМ всё; ПКМ половина вверх.
        for (n, b, cur, left) in [(64, 0, 64, 0), (64, 1, 32, 32), (63, 1, 32, 31), (1, 1, 1, 0)] {
            let mut t = T::player();
            t.set(36, st(DIRT_I, n));
            t.click(36, b, A_PICKUP);
            assert_eq!((t.m.cursor.n(), t.n(36)), (cur, left), "{n} {b}");
        }
        // 5: ПКМ курсором по пустому — одна.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 10);
        t.click(9, 1, A_PICKUP);
        assert_eq!((t.n(9), t.m.cursor.n()), (1, 9));
        // 6: ЛКМ до предела.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 10);
        t.set(9, st(DIRT_I, 60));
        t.click(9, 0, A_PICKUP);
        assert_eq!((t.n(9), t.m.cursor.n()), (64, 6));
        // 7: обмен и для ПКМ.
        let mut t = T::player();
        t.m.cursor = st(COB, 5);
        t.set(9, st(DIRT_I, 3));
        t.click(9, 1, A_PICKUP);
        assert_eq!((t.get(9), t.m.cursor), (st(COB, 5), st(DIRT_I, 3)));
        // 8: одинаковые целые кирки — ничего; 9: разный износ — обмен.
        let mut t = T::player();
        t.m.cursor = st(PICK, 1);
        t.set(9, st(PICK, 1));
        t.click(9, 0, A_PICKUP);
        assert_eq!((t.get(9), t.m.cursor), (st(PICK, 1), st(PICK, 1)));
        let worn = ItemStack { item: PICK, count: 1, damage: 3 };
        t.set(9, worn);
        t.click(9, 0, A_PICKUP);
        assert_eq!((t.get(9), t.m.cursor), (st(PICK, 1), worn));
        // 10: ПКМ по результату — весь результат.
        let mut t = T::player();
        t.set(1, st(LOG_I, 1));
        assert_eq!(t.get(0), st(PLANK, 4));
        t.click(0, 1, A_PICKUP);
        assert_eq!(t.m.cursor, st(PLANK, 4));
        assert!(t.get(1).is_empty() && t.get(0).is_empty());
        // 11: курсор 62 + 4 > 64 — ничего; 12: 60 + 4 — крафт.
        let mut t = T::player();
        t.set(1, st(LOG_I, 2));
        t.m.cursor = st(PLANK, 62);
        t.click(0, 0, A_PICKUP);
        assert_eq!((t.m.cursor.n(), t.n(1), t.n(0)), (62, 2, 4));
        t.m.cursor = st(PLANK, 60);
        t.click(0, 0, A_PICKUP);
        assert_eq!((t.m.cursor.n(), t.n(1), t.get(0)), (64, 1, st(PLANK, 4)));
    }

    #[test]
    fn e13_e20_drag() {
        // 13: 64 по 4 — по 16.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 64);
        t.drag(0, &[9, 10, 11, 12]);
        assert_eq!([t.n(9), t.n(10), t.n(11), t.n(12)], [16; 4]);
        assert!(t.m.cursor.is_empty());
        // 14: 10 по 3 — по 3, остаток 1.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 10);
        t.drag(0, &[9, 10, 11]);
        assert_eq!(([t.n(9), t.n(10), t.n(11)], t.m.cursor.n()), ([3; 3], 1));
        // 15: чужой предмет не берётся; доля от N = 2.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 64);
        t.set(10, st(DIRT_I, 60));
        t.set(11, st(COB, 1));
        t.drag(0, &[9, 10, 11]);
        assert_eq!((t.n(9), t.n(10), t.get(11), t.m.cursor.n()), (32, 64, st(COB, 1), 28));
        // 16: полный стек того же — тоже ячейка, но ничего не получает.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 30);
        t.set(9, st(DIRT_I, 64));
        t.drag(0, &[9, 10, 11]);
        assert_eq!((t.n(9), t.n(10), t.n(11), t.m.cursor.n()), (64, 10, 10, 10));
        // 17: ПКМ-тяга по 5 — по одной.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 64);
        t.drag(1, &[9, 10, 11, 12, 13]);
        assert_eq!(((9..14).map(|i| t.n(i)).collect::<Vec<_>>(), t.m.cursor.n()), (vec![1; 5], 59));
        // 18: ячеек не больше, чем предметов.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 2);
        t.drag(0, &[9, 10, 11, 12, 13]);
        assert_eq!(((9..14).map(|i| t.n(i)).collect::<Vec<_>>(), t.m.cursor.n()), (vec![1, 1, 0, 0, 0], 0));
        // 19: одна ячейка — обычный клик той же кнопкой.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 20);
        t.drag(1, &[9]);
        assert_eq!((t.n(9), t.m.cursor.n()), (1, 19));
        // 20: PICKUP посреди тяги — отброшен, тяга сброшена.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 5);
        t.click(SLOT_OUTSIDE, 0, A_QUICK_CRAFT);
        t.click(9, 0, A_PICKUP);
        assert_eq!((t.n(9), t.m.cursor.n(), t.m.qc_stage), (0, 5, 0));
    }

    #[test]
    fn e21_e22_pickup_all() {
        // 21: неполные первыми (сетка, рюкзак, вторая рука), потом полный.
        let mut t = T::player();
        t.m.cursor = st(PLANK, 10);
        t.set(1, st(PLANK, 2));
        t.set(9, st(PLANK, 64));
        t.set(20, st(PLANK, 5));
        t.set(45, st(PLANK, 30));
        t.click(36, 0, A_PICKUP_ALL);
        assert_eq!((t.m.cursor.n(), t.n(1), t.n(20), t.n(45), t.n(9)), (64, 0, 0, 0, 47));
        // 22: слот не пуст — ничего.
        let mut t = T::player();
        t.m.cursor = st(PLANK, 10);
        t.set(36, st(PLANK, 3));
        t.set(9, st(PLANK, 5));
        t.click(36, 0, A_PICKUP_ALL);
        assert_eq!((t.m.cursor.n(), t.n(36), t.n(9)), (10, 3, 5));
    }

    #[test]
    fn e23_e31_quick_move() {
        // 23: хотбар → рюкзак: сложение, потом первый пустой.
        let mut t = T::player();
        t.set(36, st(DIRT_I, 32));
        t.set(9, st(DIRT_I, 60));
        t.click(36, 0, A_QUICK_MOVE);
        assert_eq!((t.n(9), t.n(10), t.n(36)), (64, 28, 0));
        // 24: хотбар полон — ничего.
        let mut t = T::player();
        t.set(9, st(DIRT_I, 10));
        for i in 36..45 {
            t.set(i, st(COB, 64));
        }
        t.click(9, 0, A_QUICK_MOVE);
        assert_eq!(t.n(9), 10);
        // 25: Shift по результату — 12 досок в правой ячейке хотбара.
        let mut t = T::player();
        t.set(1, st(LOG_I, 3));
        t.click(0, 0, A_QUICK_MOVE);
        assert_eq!((t.get(44), t.n(1), t.n(0)), (st(PLANK, 12), 0, 0));
        // 26: сначала к такому же стеку.
        let mut t = T::player();
        t.set(1, st(LOG_I, 3));
        t.set(9, st(PLANK, 10));
        t.click(0, 0, A_QUICK_MOVE);
        assert_eq!((t.n(9), t.n(44)), (22, 0));
        // 27: почти полный инвентарь — излишек на землю без владельца.
        let mut t = T::player();
        t.set(1, st(LOG_I, 1));
        for i in 0..36 {
            t.inv.main[i] = st(COB, 64);
        }
        t.inv.main[9] = st(PLANK, 62);
        t.click(0, 0, A_QUICK_MOVE);
        assert_eq!((t.inv.main[9].n(), t.n(1), t.n(0)), (64, 0, 0));
        assert_eq!(t.drops(), vec![(st(PLANK, 2), false)]);
        // 28–30: броня.
        let mut t = T::player();
        t.set(9, st(HELMET, 1));
        t.click(9, 0, A_QUICK_MOVE);
        assert_eq!((t.get(5), t.n(9)), (st(HELMET, 1), 0));
        assert!(t.ev.contains(&UiEv::Equip(HELMET)));
        t.set(9, st(HELMET, 1));
        t.click(9, 0, A_QUICK_MOVE);
        assert_eq!((t.get(36), t.get(5)), (st(HELMET, 1), st(HELMET, 1)));
        let mut t = T::player();
        t.set(5, st(HELMET, 1));
        t.click(5, 0, A_QUICK_MOVE);
        assert_eq!((t.get(9), t.n(5)), (st(HELMET, 1), 0));
        // 31: землю в шлем не положить.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 1);
        t.click(5, 0, A_PICKUP);
        assert_eq!((t.n(5), t.m.cursor.n()), (0, 1));
    }

    #[test]
    fn e32_e40_swap_throw_outside() {
        // 32–33: цифра над слотом.
        let mut t = T::player();
        t.set(9, st(COB, 10));
        t.click(9, 0, A_SWAP);
        assert_eq!((t.inv.main[0], t.n(9)), (st(COB, 10), 0));
        let mut t = T::player();
        t.set(9, st(COB, 10));
        t.inv.main[0] = st(DIRT_I, 5);
        t.click(9, 0, A_SWAP);
        assert_eq!((t.inv.main[0], t.get(9)), (st(COB, 10), st(DIRT_I, 5)));
        // 34–35: результат в пустую ячейку хотбара; в занятую — ничего.
        let mut t = T::player();
        t.set(1, st(LOG_I, 1));
        t.click(0, 2, A_SWAP);
        assert_eq!((t.inv.main[2], t.n(1)), (st(PLANK, 4), 0));
        let mut t = T::player();
        t.set(1, st(LOG_I, 1));
        t.inv.main[2] = st(DIRT_I, 1);
        t.click(0, 2, A_SWAP);
        assert_eq!((t.inv.main[2], t.n(1), t.get(0)), (st(DIRT_I, 1), 1, st(PLANK, 4)));
        // 36: Q и Ctrl+Q над слотом — с владельцем.
        let mut t = T::player();
        t.set(36, st(DIRT_I, 64));
        t.click(36, 0, A_THROW);
        assert_eq!((t.n(36), t.drops()), (63, vec![(st(DIRT_I, 1), true)]));
        let mut t = T::player();
        t.set(36, st(DIRT_I, 64));
        t.click(36, 1, A_THROW);
        assert_eq!((t.n(36), t.drops()), (0, vec![(st(DIRT_I, 64), true)]));
        // 37: с вещью в курсоре Q — ничего.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 5);
        t.set(36, st(DIRT_I, 64));
        t.click(36, 0, A_THROW);
        assert_eq!((t.n(36), t.drops().len()), (64, 0));
        // 38: Q над результатом — весь результат одного крафта.
        let mut t = T::player();
        t.set(1, st(LOG_I, 2));
        t.click(0, 0, A_THROW);
        assert_eq!((t.drops(), t.n(1)), (vec![(st(PLANK, 4), true)], 1));
        // 39: ПКМ вне окна ×3 — по одной, ЛКМ — остальное.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 64);
        for _ in 0..3 {
            t.click(SLOT_OUTSIDE, 1, A_PICKUP);
        }
        t.click(SLOT_OUTSIDE, 0, A_PICKUP);
        let d: Vec<i32> = t.drops().iter().map(|d| d.0.n()).collect();
        assert_eq!((d, t.m.cursor.is_empty()), (vec![1, 1, 1, 61], true));
        // 40: Shift-клик вне окна — тоже бросок.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 64);
        t.click(SLOT_OUTSIDE, 0, A_QUICK_MOVE);
        assert_eq!(t.drops(), vec![(st(DIRT_I, 64), true)]);
        // Клик по фону окна (−1) — ничего.
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 64);
        t.click(-1, 0, A_PICKUP);
        assert_eq!((t.m.cursor.n(), t.drops().len()), (64, 0));
    }

    #[test]
    fn e41_e47_close_insert_scroll() {
        // 41: закрытие возвращает курсор и сетку в инвентарь.
        let mut t = T::player();
        t.inv.selected = 0;
        t.inv.main[0] = st(DIRT_I, 50);
        t.m.cursor = st(DIRT_I, 20);
        t.set(1, st(LOG_I, 1));
        t.with(|m, u| m.close(u));
        assert_eq!((t.inv.main[0], t.inv.main[1], t.inv.main[2]), (st(DIRT_I, 64), st(DIRT_I, 6), st(LOG_I, 1)));
        assert!(t.drops().is_empty() && t.m.result.is_empty() && t.m.cursor.is_empty());
        // 42: без места — бросок вперёд без владельца.
        let mut t = T::player();
        for i in 0..36 {
            t.inv.main[i] = st(COB, 64);
        }
        t.m.cursor = st(DIRT_I, 5);
        t.with(|m, u| m.close(u));
        assert_eq!(t.drops(), vec![(st(DIRT_I, 5), false)]);
        // 43–46 — подбор (inventory.rs), 47 — колесо: проверены в inventory_tests.
    }

    #[test]
    fn e48_e51_container_model() {
        // Сундука в срезе 2 нет; модель Container проверяем окном верстака (те же правила insertItem).
        // 48-аналог: результат Shift-кликом — хотбар справа налево (W1).
        let mut t = T::table();
        t.set(5, st(LOG_I, 10));
        t.click(0, 0, A_QUICK_MOVE);
        assert_eq!((t.inv.main[8], t.n(5), t.n(0)), (st(PLANK, 40), 0, 0));
    }

    #[test]
    fn w01_w15_crafting_table() {
        // W2: ПКМ по результату — все 4.
        let mut t = T::table();
        t.set(5, st(LOG_I, 1));
        t.click(0, 1, A_PICKUP);
        assert_eq!((t.m.cursor, t.n(5)), (st(PLANK, 4), 0));
        // W3–W4: Shift из рюкзака — в сетку (причуда верстака).
        let mut t = T::table();
        t.inv.main[9] = st(COB, 64);
        t.click(10, 0, A_QUICK_MOVE);
        assert_eq!((t.get(1), t.inv.main[9].n()), (st(COB, 64), 0));
        let mut t = T::table();
        t.set(1, st(COB, 60));
        t.inv.main[9] = st(COB, 10);
        t.click(10, 0, A_QUICK_MOVE);
        assert_eq!((t.n(1), t.n(2), t.inv.main[9].n(), t.n(0)), (64, 6, 0, 0));
        // W5: сетка занята — рюкзак → хотбар.
        let mut t = T::table();
        for i in 1..10 {
            t.set(i, st(DIRT_I, 1));
        }
        t.inv.main[9] = st(COB, 10);
        t.click(10, 0, A_QUICK_MOVE);
        assert_eq!((t.inv.main[0], t.n(5)), (st(COB, 10), 1));
        // W6: из хотбара — тоже в сетку; W7: из сетки — в рюкзак.
        let mut t = T::table();
        t.inv.main[0] = st(COB, 10);
        t.click(37, 0, A_QUICK_MOVE);
        assert_eq!(t.get(1), st(COB, 10));
        let mut t = T::table();
        t.set(3, st(PLANK, 5));
        t.click(3, 0, A_QUICK_MOVE);
        assert_eq!(t.inv.main[9], st(PLANK, 5));
        // W8: закрытие — курсор, затем клетки 0…8 по порядку.
        let mut t = T::table();
        t.m.cursor = st(DIRT_I, 5);
        t.set(1, st(LOG_I, 1));
        t.set(9, st(COB, 3));
        t.with(|m, u| m.close(u));
        assert_eq!((t.inv.main[0], t.inv.main[1], t.inv.main[2]), (st(DIRT_I, 5), st(LOG_I, 1), st(COB, 3)));
        assert!(t.drops().is_empty());
        // W11: двойной клик берёт и из сетки.
        let mut t = T::table();
        t.m.cursor = st(PLANK, 10);
        t.set(1, st(PLANK, 2));
        t.set(2, st(PLANK, 3));
        t.inv.main[9] = st(PLANK, 5);
        t.click(20, 0, A_PICKUP_ALL);
        assert_eq!((t.m.cursor.n(), t.n(1), t.n(2), t.n(10), t.n(0)), (20, 0, 0, 0, 0));
        // W12: в окне инвентаря Shift из рюкзака — в хотбар (не в 2×2).
        let mut t = T::player();
        t.inv.main[9] = st(COB, 64);
        t.click(9, 0, A_QUICK_MOVE);
        assert_eq!((t.inv.main[0], t.n(1)), (st(COB, 64), 0));
        // W13: разные доски в кирке; W14: кольцо — сундук в правую ячейку.
        let mut t = T::table();
        t.set(1, st(PLANK, 1));
        t.set(2, st(BIRCH_PLANKS as ItemId, 1));
        t.set(3, st(SPRUCE_PLANKS as ItemId, 1));
        t.set(5, st(I_STICK, 1));
        t.set(8, st(I_STICK, 1));
        t.click(0, 0, A_PICKUP);
        assert_eq!(t.m.cursor, st(PICK, 1));
        assert!((1..10).all(|i| t.get(i).is_empty()));
        let mut t = T::table();
        for i in [1, 2, 3, 4, 6, 7, 8, 9] {
            t.set(i, st(PLANK, 1));
        }
        t.click(0, 0, A_QUICK_MOVE);
        assert_eq!(t.inv.main[8], st(CHEST as ItemId, 1));
        // W15: курсор 61 + 4 > 64 — ничего.
        let mut t = T::table();
        t.set(5, st(LOG_I, 1));
        t.m.cursor = st(PLANK, 61);
        t.click(0, 0, A_PICKUP);
        assert_eq!((t.m.cursor.n(), t.n(5)), (61, 1));
        // Кирка Shift-кликом: доски 5/3/4, палки 2/7 — 2 кирки по одной в правые ячейки хотбара (ASP-09 §7 п. 11).
        let mut t = T::table();
        t.set(1, st(PLANK, 5));
        t.set(2, st(PLANK, 3));
        t.set(3, st(PLANK, 4));
        t.set(5, st(I_STICK, 2));
        t.set(8, st(I_STICK, 7));
        t.click(0, 0, A_QUICK_MOVE);
        assert_eq!((t.inv.main[8], t.inv.main[7], t.n(1), t.n(2), t.n(3), t.n(5), t.n(8), t.n(0)), (st(PICK, 1), st(PICK, 1), 3, 1, 2, 0, 5, 0));
        // Остановка на другом рецепте: кольцо, две верхние клетки левого столбца по 2 — 1 сундук, потом палки.
        let mut t = T::table();
        for i in [1, 2, 3, 4, 6, 7, 8, 9] {
            t.set(i, st(PLANK, 1));
        }
        t.set(1, st(PLANK, 2));
        t.set(4, st(PLANK, 2));
        t.click(0, 0, A_QUICK_MOVE);
        assert_eq!((t.inv.main[8], t.get(0)), (st(CHEST as ItemId, 1), st(I_STICK, 4)));
    }

    #[test]
    fn drag_preview_and_quick_move_all() {
        // Предпросмотр: 64 по 4 пустым — по 16, курсор 0; с полным стеком — жёлтое (упёрлось).
        let mut t = T::player();
        t.m.cursor = st(DIRT_I, 64);
        t.set(10, st(DIRT_I, 60));
        let p = t.with(|m, u| m.drag_preview(u, 0, &[9, 10, 11, 12]));
        assert_eq!(p, vec![12, 16, 0, 64, 1, 16, 0, 16, 0]);
        assert!(t.with(|m, u| m.drag_accepts(u, 9, 0, 3)));
        assert!(!t.with(|m, u| m.drag_accepts(u, 0, 0, 0)), "результат в тягу не берётся");
        assert!(!t.with(|m, u| m.drag_accepts(u, 5, 0, 0)), "земля не в шлем");
        // Shift + двойной клик: QUICK_MOVE по всем слотам того же хранилища по порядку. Рюкзак и хотбар — одно
        // хранилище (PlayerInventory), поэтому, как в коде Java, земля уходит в хотбар (36), а затем, когда обход
        // доходит до 36, — обратно в первую пустую ячейку рюкзака (причуда ASP-08 §8.2, повторяем).
        let mut t = T::player();
        t.set(9, st(DIRT_I, 5));
        t.set(10, st(DIRT_I, 7));
        t.set(11, st(COB, 7));
        t.with(|m, u| m.quick_move_all(u, 9, st(DIRT_I, 5)));
        assert_eq!((t.n(9), t.n(10), t.n(36), t.n(11)), (12, 0, 0, 7));
        // В окне верстака сетка — своё хранилище: Shift + двойной клик по сетке уводит все доски из сетки.
        let mut t = T::table();
        t.set(1, st(PLANK, 3));
        t.set(5, st(PLANK, 2));
        t.inv.main[9] = st(PLANK, 1);
        t.with(|m, u| m.quick_move_all(u, 1, st(PLANK, 3)));
        assert_eq!((t.n(1), t.n(5), t.inv.main[9].n()), (0, 0, 6));
    }

    #[test]
    fn durability_bar_reference() {
        // Полоска прочности (ASP-08 §13): шаг и цвет в f32, как Item.getItemBarStep/Color.
        fn bar(damage: u16, max: u16) -> (i32, u32) {
            let step = (13.0f32 - damage as f32 * 13.0 / max as f32).round().clamp(0.0, 13.0) as i32;
            let f = ((max - damage) as f32 / max as f32).max(0.0);
            let (r, g) = if f >= 0.5 { ((255.0 * (2.0 - 2.0 * f)) as u32, 255) } else { (255, (255.0 * 2.0 * f) as u32) };
            (step, r << 16 | g << 8)
        }
        let cases = [(1, 59, 13, 0x08FF00), (14, 59, 10, 0x79FF00), (28, 59, 7, 0xF2FF00), (29, 59, 7, 0xFAFF00), (30, 59, 6, 0xFFFA00),
            (54, 59, 1, 0xFF2B00), (58, 59, 0, 0xFF0800), (66, 131, 6, 0xFFFD00), (126, 131, 0, 0xFF1300), (125, 250, 7, 0xFFFF00), (126, 250, 6, 0xFFFC00)];
        for (d, m, s, c) in cases {
            assert_eq!(bar(d, m), (s, c), "износ {d}/{m}");
        }
    }
}
