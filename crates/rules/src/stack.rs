//! Validated value stacks, independent of inventory ownership and persistence.

use crate::{ItemId, item};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemStack {
    item: ItemId,
    count: u16,
    damage: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    InvalidEmpty,
    UnknownItem(ItemId),
    CountExceedsMaximum { count: u16, maximum: u16 },
    DamageExceedsMaximum { damage: u16, maximum: u16 },
}

impl ItemStack {
    pub const EMPTY: Self = Self {
        item: 0,
        count: 0,
        damage: 0,
    };

    pub fn new(item: ItemId, count: u16) -> Result<Self, StackError> {
        Self::from_parts(item, count, 0)
    }

    /// Empty values have one canonical representation. Durable items allow the
    /// maximum damage value for repair/command input; larger values are rejected.
    pub fn from_parts(id: ItemId, count: u16, damage: u16) -> Result<Self, StackError> {
        if id == 0 || count == 0 {
            return if id == 0 && count == 0 && damage == 0 {
                Ok(Self::EMPTY)
            } else {
                Err(StackError::InvalidEmpty)
            };
        }
        let definition = item(id).ok_or(StackError::UnknownItem(id))?;
        let maximum = u16::from(definition.max_stack);
        if count > maximum {
            return Err(StackError::CountExceedsMaximum { count, maximum });
        }
        if damage > definition.max_damage {
            return Err(StackError::DamageExceedsMaximum {
                damage,
                maximum: definition.max_damage,
            });
        }
        Ok(Self {
            item: id,
            count,
            damage,
        })
    }

    pub const fn item(self) -> ItemId {
        self.item
    }
    pub const fn count(self) -> u16 {
        self.count
    }
    pub const fn damage(self) -> u16 {
        self.damage
    }
    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn maximum(self) -> u16 {
        if self.is_empty() {
            0
        } else {
            u16::from(crate::items::idef(self.item).max_stack)
        }
    }

    pub const fn same_kind(self, other: Self) -> bool {
        self.item == other.item && self.damage == other.damage
    }

    pub fn with_count(self, count: u16) -> Result<Self, StackError> {
        if count == 0 {
            return Ok(Self::EMPTY);
        }
        Self::from_parts(self.item, count, self.damage)
    }

    pub fn with_damage(self, damage: u16) -> Result<Self, StackError> {
        Self::from_parts(self.item, self.count, damage)
    }
}
