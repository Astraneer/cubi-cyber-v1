//! Инвентарь игрока (ASP-08 §1.2, §5): 36 ячеек (0–8 хотбар, 9–35 рюкзак), броня 36–39, вторая рука 40; подбор,
//! возврат при закрытии окна, колесо, подскок иконки. Стек — world/stack.rs, окна и действия мышью — screen.rs.

use crate::world::stack::{Container, ItemStack};

pub(crate) const OFFHAND: usize = 40;

pub(crate) struct Inventory {
    pub(crate) main: [ItemStack; 36],
    /// 0 ноги … 3 голова (плоские индексы 36–39).
    pub(crate) armor: [ItemStack; 4],
    pub(crate) offhand: ItemStack,
    pub(crate) selected: usize,
    /// Подскок иконки после подбора, тики (5 → 0).
    pub(crate) bob: [u8; 41],
}

impl Inventory {
    pub(crate) fn new() -> Inventory {
        Inventory { main: [ItemStack::EMPTY; 36], armor: [ItemStack::EMPTY; 4], offhand: ItemStack::EMPTY, selected: 0, bob: [0; 41] }
    }
    pub(crate) fn at(&mut self, i: usize) -> &mut ItemStack {
        match i {
            0..=35 => &mut self.main[i],
            36..=39 => &mut self.armor[i - 36],
            _ => &mut self.offhand,
        }
    }
    pub(crate) fn hand(&self) -> ItemStack {
        self.main[self.selected]
    }
    pub(crate) fn hand_mut(&mut self) -> &mut ItemStack {
        &mut self.main[self.selected]
    }
    pub(crate) fn cap(s: &ItemStack) -> i32 {
        s.max().min(99)
    }
    pub(crate) fn has_room(e: &ItemStack, s: &ItemStack) -> bool {
        !e.is_empty() && e.same_kind(s) && e.stackable() && e.n() < Self::cap(e)
    }
    /// Куда докладывать (getOccupiedSlotWithRoomForStack): выбранный → вторая рука → 0…35 по порядку.
    pub(crate) fn slot_with_room(&self, s: &ItemStack) -> Option<usize> {
        if Self::has_room(&self.main[self.selected], s) {
            return Some(self.selected);
        }
        if Self::has_room(&self.offhand, s) {
            return Some(OFFHAND);
        }
        (0..36).find(|&i| Self::has_room(&self.main[i], s))
    }
    /// Первая пустая ячейка 0…35 (хотбар, потом рюкзак); выбранный слот не важен.
    pub(crate) fn empty_slot(&self) -> Option<usize> {
        (0..36).find(|&i| self.main[i].is_empty())
    }
    pub(crate) fn add_at(&mut self, i: usize, s: &mut ItemStack) {
        let slot = self.at(i);
        if slot.is_empty() {
            *slot = s.with_count(0);
        }
        let k = s.n().min(Self::cap(slot) - slot.count);
        if k > 0 {
            slot.count += k;
            s.count -= k;
            self.bob[i] = 5;
        }
    }
    /// Подбор (PlayerInventory.insertStack): true, если стек уменьшился (частично — тоже true).
    /// Изношенный предмет — только в пустую ячейку и целиком.
    pub(crate) fn insert(&mut self, s: &mut ItemStack) -> bool {
        if s.is_empty() {
            return false;
        }
        if s.damaged() {
            return match self.empty_slot() {
                Some(i) => {
                    self.main[i] = *s;
                    s.count = 0;
                    self.bob[i] = 5;
                    true
                }
                None => false,
            };
        }
        let before = s.n();
        loop {
            let prev = s.n();
            match self.slot_with_room(s).or_else(|| self.empty_slot()) {
                Some(i) => self.add_at(i, s),
                None => break,
            }
            if s.is_empty() || s.n() == prev {
                break;
            }
        }
        s.n() < before
    }
    /// Возврат при закрытии окна (PlayerInventory.offerOrDrop): такие же с местом (выбранный → вторая рука → 0…35),
    /// потом первая пустая; кусками «до полного» в каждую. Ответ — что не влезло (бросок вперёд без владельца).
    pub(crate) fn offer(&mut self, mut s: ItemStack) -> ItemStack {
        while !s.is_empty() {
            let Some(i) = self.slot_with_room(&s).or_else(|| self.empty_slot()) else { return s };
            let room = s.max() - self.at(i).n();
            let k = room.min(s.n()).max(0);
            let mut part = s.with_count(k);
            s.count -= k;
            if part.damaged() {
                self.main[i] = part; // изношенное — только в пустую, целиком (insertStack)
                self.bob[i] = 5;
                continue;
            }
            loop {
                let prev = part.n();
                self.add_at(i, &mut part);
                if part.is_empty() || part.n() == prev {
                    break;
                }
            }
        }
        ItemStack::EMPTY
    }
    /// Ячейка по плоскому индексу: 0–35 main, 36–39 броня, 40 вторая рука.
    pub(crate) fn get(&self, i: usize) -> ItemStack {
        match i {
            0..=35 => self.main[i],
            36..=39 => self.armor[i - 36],
            _ => self.offhand,
        }
    }
    pub(crate) fn set(&mut self, i: usize, s: ItemStack) {
        *self.at(i) = if s.is_empty() { ItemStack::EMPTY } else { s };
    }
    /// Выброс из руки (dropSelectedItem): 1 штука или весь стек.
    pub(crate) fn take_selected(&mut self, all: bool) -> ItemStack {
        let s = self.main[self.selected];
        if s.is_empty() {
            return ItemStack::EMPTY;
        }
        let n = if all { s.n() } else { 1 };
        let out = s.with_count(n);
        self.set(self.selected, s.with_count(s.n() - n));
        out
    }
    /// Первая ячейка 0…35 с тем же предметом и компонентами (getSlotWithStack) — средняя кнопка.
    pub(crate) fn slot_with_stack(&self, s: &ItemStack) -> Option<usize> {
        (0..36).find(|&i| !self.main[i].is_empty() && self.main[i].same_kind(s))
    }
    /// getSwappableHotbarSlot: первая пустая ячейка хотбара начиная с выбранной, иначе выбранная (чар у нас нет).
    pub(crate) fn swappable_hotbar_slot(&self) -> usize {
        (0..9).map(|k| (self.selected + k) % 9).find(|&j| self.main[j].is_empty()).unwrap_or(self.selected)
    }
    /// swapSlotWithHotbar: предмет из рюкзака — в хотбар (pickFromInventory), выбор переходит туда.
    pub(crate) fn swap_with_hotbar(&mut self, slot: usize) {
        self.selected = self.swappable_hotbar_slot();
        self.main.swap(self.selected, slot);
    }
    /// Колесо (scrollInHotbar): шаг всегда 1; вверх (+) — влево, по кругу.
    pub(crate) fn scroll(&mut self, amount: f64) {
        let d = amount.signum() as i32;
        self.selected = (self.selected as i32 - d).rem_euclid(9) as usize;
    }
    /// Тик предметов (PlayerInventory.updateItems): подскок −1.
    pub(crate) fn tick(&mut self) {
        for b in &mut self.bob {
            *b = b.saturating_sub(1);
        }
    }
}

impl Container for Inventory {
    fn size(&self) -> usize {
        41
    }
    fn get(&self, i: usize) -> ItemStack {
        Inventory::get(self, i)
    }
    fn set(&mut self, i: usize, s: ItemStack) {
        Inventory::set(self, i, s)
    }
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    use crate::registry::ids::*;
    use crate::registry::items::*;
    const COB: ItemId = COBBLE as ItemId;

    #[test]
    fn pickup_rules_asp08() {
        // 43: выбранный слот с тем же предметом — первым, затем 0…35.
        let mut inv = Inventory::new();
        inv.selected = 4;
        inv.main[4] = ItemStack::of(COB, 10);
        inv.main[0] = ItemStack::of(COB, 20);
        let mut s = ItemStack::of(COB, 30);
        assert!(inv.insert(&mut s));
        assert_eq!((inv.main[4].count, inv.main[0].count, inv.bob[4]), (40, 20, 5));
        // 44: переполнение выбранного — остаток в следующий с местом.
        inv.main[4] = ItemStack::of(COB, 10);
        inv.main[0] = ItemStack::of(COB, 20);
        let mut s = ItemStack::of(COB, 60);
        inv.insert(&mut s);
        assert_eq!((inv.main[4].count, inv.main[0].count), (64, 26));
        assert!(s.is_empty());
        // 45: новый стек — в первую пустую по индексу, не в выбранную.
        let mut inv = Inventory::new();
        inv.selected = 4;
        let mut s = ItemStack::of(DIRT as ItemId, 5);
        inv.insert(&mut s);
        assert_eq!(inv.main[0], ItemStack::of(DIRT as ItemId, 5));
        assert!(inv.main[4].is_empty());
        // 46: изношенный — только в пустую, не складывается.
        let mut inv = Inventory::new();
        let pick = ItemStack { item: I_WOODEN_PICKAXE, count: 1, damage: 3 };
        inv.main[0] = pick;
        inv.main[1] = ItemStack::of(DIRT as ItemId, 1);
        let mut s = pick;
        assert!(inv.insert(&mut s));
        assert_eq!(inv.main[2], pick);
        // 47: колесо — шаг 1 по кругу.
        let mut inv = Inventory::new();
        inv.scroll(1.0);
        assert_eq!(inv.selected, 8);
        inv.scroll(-1.0);
        assert_eq!(inv.selected, 0);
        inv.selected = 0;
        inv.scroll(3.0);
        assert_eq!(inv.selected, 8);
        // Вторая рука — после выбранного, до 0…35; полный инвентарь — частичный подбор.
        let mut inv = Inventory::new();
        inv.selected = 5;
        inv.offhand = ItemStack::of(COB, 60);
        inv.main[0] = ItemStack::of(COB, 1);
        let mut s = ItemStack::of(COB, 3);
        inv.insert(&mut s);
        assert_eq!((inv.offhand.count, inv.main[0].count), (63, 1));
        let mut inv = Inventory::new();
        for i in 0..36 {
            inv.main[i] = ItemStack::of(COB, 64);
        }
        inv.main[7] = ItemStack::of(COB, 62);
        let mut s = ItemStack::of(COB, 5);
        assert!(inv.insert(&mut s));
        assert_eq!((inv.main[7].count, s.count), (64, 3));
        let mut s = ItemStack::of(DIRT as ItemId, 1);
        assert!(!inv.insert(&mut s));
    }
}
