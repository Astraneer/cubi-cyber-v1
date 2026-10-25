//! Authoritative value rules for survival. Plans are detached values: the world
//! owner publishes their complete result with the corresponding world transaction
//! only after durable commit. These types contain no network or world access.

pub mod farm;
pub mod fluid;
pub mod furnace;
pub mod health;
mod inventory;
pub mod loot;

pub use inventory::{INVENTORY_BYTES, Inventory, InventoryCraftPlan, InventoryPlan, OFFHAND};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellChange {
    pub position: [i32; 3],
    pub before: core_types::Cell,
    pub after: core_types::Cell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurvivalError {
    InvalidSlot,
    InvalidArmorSlot,
    InvalidStack,
    NoEntitlement,
    NotDurableTool,
    StalePlan,
    RevisionOverflow,
    InvalidEncoding,
    UnknownInput,
    Capacity,
}
