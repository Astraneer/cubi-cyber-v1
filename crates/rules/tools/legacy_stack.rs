//! Only for the independent donor reference generator; original pure stack values.


use crate::registry::items::{idef, ItemId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ItemStack {
    pub(crate) item: ItemId,
    pub(crate) count: i32,
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
