//! Машина в предмете (решение после MEK-01, catalog GEN-02): ломаешь машину — выпадает она сама, а энергия, слоты и бак
//! остаются в предмете (без прогресса и тепла: бак лавы хранится, тепло — нет). Поставил — машина та же.
//!
//! Данные стека — не в самом ItemStack (он Copy и ходит по сети и в сохранение 3–5 байтами), а в хранилище сервера
//! Sim::mek_items: стек машины несёт только номер записи в поле damage (у блоков-предметов износа нет, max_damage 0), как
//! «метаданные» старого Minecraft. Разные номера не складываются (same_kind сравнивает damage), пустая машина — номер 0 и
//! складывается с такими же. Номер освобождается, когда машину ставят; номер у предмета, сгоревшего в лаве или исчезнувшего
//! через 5 минут, остаётся в хранилище (запись — десятки байт; уборка — когда понадобится).
//! Клиенту данные не нужны: окно поставленной машины приходит с сервера (Msg::WinData и слоты окна).

use std::collections::BTreeMap;

use super::{MekBe, Tank};
use crate::world::stack::ItemStack;

/// Что машина уносит в предмет.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MekItem {
    pub(crate) energy: u64,
    pub(crate) slots: [ItemStack; 5],
    pub(crate) tank: Tank,
    /// M3: стороны предметов, выталкивание, улучшения (скорость, энергия) — как в моде (MEK-09 §7.3).
    pub(crate) sides: [u8; 6],
    pub(crate) eject: bool,
    pub(crate) upgrades: [u8; 3],
    /// M4: баки газов сепаратора и их режимы.
    pub(crate) gas: [Tank; 2],
    pub(crate) gas_mode: [u8; 2],
    /// M5: стороны газов сепаратора (0 — по умолчанию вида).
    pub(crate) gas_sides: [u8; 6],
}

impl MekItem {
    /// Из машины; None — нечего хранить (пустая машина — обычный складываемый предмет).
    pub(crate) fn of(m: &MekBe) -> Option<MekItem> {
        let d = MekItem {
            energy: m.energy,
            slots: m.slots,
            tank: if m.tank.amount == 0 { Tank::default() } else { m.tank },
            sides: m.sides,
            eject: m.eject,
            upgrades: m.upgrades,
            gas: m.gas,
            gas_mode: m.gas_mode,
            gas_sides: if m.gas_sides == super::sides::default_gas_sides(m.kind) { [0; 6] } else { m.gas_sides },
        };
        let custom = d.sides != super::sides::default_sides(m.kind)
            || d.eject
            || d.upgrades != [0; 3]
            || d.gas_mode != [0; 2]
            || d.gas_sides != [0; 6];
        let gas = d.gas.iter().any(|t| t.amount > 0);
        (d.energy > 0 || d.tank.amount > 0 || gas || d.slots.iter().any(|s| !s.is_empty()) || custom).then_some(d)
    }
    /// В поставленную машину (энергия — не больше ёмкости).
    pub(crate) fn apply(&self, m: &mut MekBe) {
        m.upgrades = self.upgrades;
        m.energy = self.energy.min(m.cap());
        let n = m.nslots();
        m.slots[..n].copy_from_slice(&self.slots[..n]);
        m.tank = self.tank;
        if self.sides != [0; 6] {
            m.sides = self.sides;
        }
        m.eject = self.eject;
        m.gas = self.gas;
        m.gas_mode = self.gas_mode;
        if self.gas_sides != [0; 6] {
            m.gas_sides = self.gas_sides;
        }
    }
}

/// Хранилище данных машин в предметах: номер (damage стека, 1…65 535) → данные.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MekItems {
    map: BTreeMap<u16, MekItem>,
    next: u16,
}

impl MekItems {
    /// Положить данные машины: номер для стека (0 — нечего хранить или хранилище полно).
    pub(crate) fn stash(&mut self, m: &MekBe) -> u16 {
        let Some(d) = MekItem::of(m) else { return 0 };
        for _ in 0..u16::MAX {
            self.next = self.next.wrapping_add(1).max(1);
            if !self.map.contains_key(&self.next) {
                self.map.insert(self.next, d);
                return self.next;
            }
        }
        0
    }
    /// Забрать данные по номеру (машину поставили).
    pub(crate) fn take(&mut self, id: u16) -> Option<MekItem> {
        self.map.remove(&id)
    }
    /// Посмотреть (творческий режим: стек не тратится — данные остаются).
    pub(crate) fn get(&self, id: u16) -> Option<&MekItem> {
        self.map.get(&id)
    }
    pub(crate) fn get_mut(&mut self, id: u16) -> Option<&mut MekItem> {
        self.map.get_mut(&id)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    /// Для сохранения: следующий номер и записи по порядку номеров.
    pub(crate) fn saved(&self) -> (u16, Vec<(u16, &MekItem)>) {
        (self.next, self.map.iter().map(|(k, v)| (*k, v)).collect())
    }
    pub(crate) fn restore(&mut self, next: u16, list: Vec<(u16, MekItem)>) {
        self.next = next;
        self.map = list.into_iter().collect();
    }
}
