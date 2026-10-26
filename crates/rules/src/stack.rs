//! Validated value stacks, independent of inventory ownership and persistence.

use crate::item_payload::PayloadId;
use crate::{ItemId, item};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemStack {
    item: ItemId,
    count: u16,
    damage: u16,
    payload: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    InvalidEmpty,
    InvalidPayload,
    UnknownItem(ItemId),
    CountExceedsMaximum { count: u16, maximum: u16 },
    DamageExceedsMaximum { damage: u16, maximum: u16 },
}

impl ItemStack {
    pub const EMPTY: Self = Self {
        item: 0,
        count: 0,
        damage: 0,
        payload: 0,
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
            payload: 0,
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
    pub const fn payload(self) -> Option<PayloadId> {
        if self.payload == 0 {
            None
        } else {
            Some(PayloadId(self.payload))
        }
    }
    /// Only trusted owners may attach resolved data. A client reference alone
    /// does not grant possession or access to the immutable ledger.
    pub fn with_payload(self, id: PayloadId) -> Result<Self, StackError> {
        if id.0 == 0 || self.is_empty() || self.count != 1 {
            return Err(StackError::InvalidPayload);
        }
        Ok(Self {
            payload: id.0,
            ..self
        })
    }
    pub const fn without_payload(self) -> Self {
        Self { payload: 0, ..self }
    }
    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn maximum(self) -> u16 {
        if self.is_empty() {
            0
        } else if self.payload != 0 {
            1
        } else {
            u16::from(crate::items::idef(self.item).max_stack)
        }
    }

    pub const fn same_kind(self, other: Self) -> bool {
        self.item == other.item && self.damage == other.damage && self.payload == other.payload
    }

    pub fn with_count(self, count: u16) -> Result<Self, StackError> {
        if count == 0 {
            return Ok(Self::EMPTY);
        }
        let stack = Self::from_parts(self.item, count, self.damage)?;
        self.payload()
            .map_or(Ok(stack), |id| stack.with_payload(id))
    }

    pub fn with_damage(self, damage: u16) -> Result<Self, StackError> {
        let stack = Self::from_parts(self.item, self.count, damage)?;
        self.payload()
            .map_or(Ok(stack), |id| stack.with_payload(id))
    }
}
