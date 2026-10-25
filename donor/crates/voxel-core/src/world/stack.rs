//! Стек предметов (ASP-08 §1.1) и типаж хранилища ячеек (Container): общие для мира (сундук, печь, предмет на земле)
//! и игрока (инвентарь, окна — game).

use serde::{Deserialize, Serialize};

use crate::registry::items::{idef, ItemId};

/// Стек — и в протоколе (proto.rs) как есть: postcard кладёт его в 3–5 байт.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ItemStack {
    pub(crate) item: ItemId,
    pub(crate) count: i32,
    /// Износ — пока единственный «компонент» стека.
    pub(crate) damage: u16,
}

impl ItemStack {
    pub const EMPTY: ItemStack = ItemStack { item: 0, count: 0, damage: 0 };
    pub(crate) fn of(item: ItemId, count: i32) -> ItemStack {
        ItemStack { item, count, damage: 0 }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.item == 0 || self.count <= 0
    }
    pub(crate) fn id(&self) -> ItemId {
        if self.is_empty() {
            0
        } else {
            self.item
        }
    }
    pub(crate) fn n(&self) -> i32 {
        if self.is_empty() {
            0
        } else {
            self.count
        }
    }
    /// Тот же предмет и те же компоненты, без количества (ItemStack.areItemsAndComponentsEqual).
    pub(crate) fn same_kind(&self, o: &ItemStack) -> bool {
        self.id() == o.id() && (self.is_empty() || self.damage == o.damage)
    }
    pub(crate) fn with_count(&self, c: i32) -> ItemStack {
        if self.is_empty() {
            ItemStack::EMPTY
        } else {
            ItemStack { count: c, ..*self }
        }
    }
    pub(crate) fn max(&self) -> i32 {
        idef(self.item).max_stack as i32
    }
    pub(crate) fn damaged(&self) -> bool {
        idef(self.item).max_damage > 0 && self.damage > 0
    }
    pub(crate) fn stackable(&self) -> bool {
        self.max() > 1 && !self.damaged()
    }
}

/// Хранилище ячеек окна (вместо `&mut [ItemStack]` ASP-08 — решение после ASP-10: печи нужна своя запись входа).
pub(crate) trait Container {
    #[allow(dead_code)]
    fn size(&self) -> usize;
    fn get(&self, i: usize) -> ItemStack;
    fn set(&mut self, i: usize, s: ItemStack);
}
