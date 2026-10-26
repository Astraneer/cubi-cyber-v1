//! Bounded vanilla furnace value transition. The owning simulation registers
//! only `needs_tick()` furnaces in its active set and wakes them on slot edits.

use super::SurvivalError;
use crate::{ids::*, items::*, stack::ItemStack};

pub const COOK_TICKS: u16 = 200;
pub const FURNACE_BYTES: usize = 30;
pub const CHEST_BYTES: usize = 174;
pub const FURNACE_MAX_BYTES: usize = FURNACE_BYTES + 6 + 3 * 10;
pub const CHEST_MAX_BYTES: usize = CHEST_BYTES + 6 + 27 * 10;

/// Pure approved donor tables (world/blockentity.rs), stable registry IDs.
pub fn fuel_ticks(item: ItemId) -> u32 {
    match item {
        I_LAVA_BUCKET => 20_000,
        COAL_BLOCK => 16_000,
        I_COAL | I_CHARCOAL => 1600,
        LOG | BIRCH_LOG | SPRUCE_LOG | PLANKS | BIRCH_PLANKS | SPRUCE_PLANKS | CRAFTING_TABLE
        | CHEST | LADDER => 300,
        x if (I_WOODEN_PICKAXE..I_WOODEN_PICKAXE + 5).contains(&x) => 200,
        I_STICK | DEAD_BUSH => 100,
        x if (WHITE_WOOL..=PINK_WOOL).contains(&x) => 100,
        _ => 0,
    }
}
pub fn smelt(item: ItemId) -> Option<ItemId> {
    Some(match item {
        I_RAW_IRON | IRON_ORE => I_IRON_INGOT,
        COAL_ORE => I_COAL,
        COBBLE => STONE,
        SAND => GLASS,
        LOG | BIRCH_LOG | SPRUCE_LOG => I_CHARCOAL,
        x if (I_WOODEN_PICKAXE + 10..I_WOODEN_PICKAXE + 15).contains(&x)
            || (I_LEATHER_HELMET + 4..I_LEATHER_HELMET + 8).contains(&x) =>
        {
            I_IRON_NUGGET
        }
        I_PORKCHOP => I_COOKED_PORKCHOP,
        I_BEEF => I_COOKED_BEEF,
        I_CHICKEN => I_COOKED_CHICKEN,
        I_MUTTON => I_COOKED_MUTTON,
        I_POTATO => I_BAKED_POTATO,
        _ => return crate::machine_recipes::smelt(item),
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Furnace {
    slots: [ItemStack; 3],
    burn: u16,
    fuel_total: u16,
    cook: u16,
    cook_total: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FurnaceStep {
    pub lit_change: Option<bool>,
    pub smelted: Option<ItemId>,
    pub active: bool,
}

impl Furnace {
    pub const fn slots(&self) -> &[ItemStack; 3] {
        &self.slots
    }
    pub const fn burn(&self) -> u16 {
        self.burn
    }
    pub const fn fuel_total(&self) -> u16 {
        self.fuel_total
    }
    pub const fn cook(&self) -> u16 {
        self.cook
    }
    pub const fn cook_total(&self) -> u16 {
        self.cook_total
    }
    pub fn set_trusted(&mut self, slot: usize, value: ItemStack) -> Result<(), SurvivalError> {
        let old = *self.slots.get(slot).ok_or(SurvivalError::InvalidSlot)?;
        if slot == 0 && (value.is_empty() || !old.same_kind(value)) {
            self.cook = 0;
            self.cook_total = COOK_TICKS;
        }
        self.slots[slot] = value;
        Ok(())
    }
    pub fn result_that_fits(&self) -> Option<ItemId> {
        if self.slots[0].is_empty() {
            return None;
        }
        let result = smelt(self.slots[0].item())?;
        let output = self.slots[2];
        (output.is_empty()
            || (output.item() == result
                && output.damage() == 0
                && output.count() < output.maximum()))
        .then_some(result)
    }
    pub fn needs_tick(&self) -> bool {
        self.burn > 0
            || self.cook > 0
            || (self.result_that_fits().is_some() && fuel_ticks(self.slots[1].item()) > 0)
    }
    /// Exactly one tick, independent of elapsed wall time. A cold blocked furnace
    /// consumes no fuel; burning fuel expires even if its output becomes blocked.
    pub fn tick(&mut self) -> FurnaceStep {
        let was_lit = self.burn > 0;
        self.burn = self.burn.saturating_sub(1);
        let mut smelted = None;
        if self.burn > 0 || (!self.slots[0].is_empty() && !self.slots[1].is_empty()) {
            let result = self.result_that_fits();
            if self.burn == 0 && result.is_some() {
                let fuel = self.slots[1];
                self.burn = fuel_ticks(fuel.item()) as u16;
                self.fuel_total = self.burn;
                if self.burn > 0 {
                    self.slots[1] = if fuel.count() > 1 {
                        fuel.with_count(fuel.count() - 1)
                            .expect("bounded fuel count")
                    } else {
                        let remainder = idef(fuel.item()).remainder;
                        if remainder == 0 {
                            ItemStack::EMPTY
                        } else {
                            ItemStack::new(remainder, 1).expect("registry remainder")
                        }
                    };
                }
            }
            if let Some(result) = result.filter(|_| self.burn > 0) {
                self.cook += 1;
                if self.cook == self.cook_total {
                    self.cook = 0;
                    self.cook_total = COOK_TICKS;
                    let output = self.slots[2];
                    self.slots[2] = ItemStack::new(result, output.count() + 1)
                        .expect("checked output capacity");
                    let input = self.slots[0];
                    self.slots[0] = input.with_count(input.count() - 1).expect("nonempty input");
                    smelted = Some(result);
                }
            } else {
                self.cook = 0;
            }
        } else {
            self.cook = self.cook.saturating_sub(2).min(self.cook_total);
        }
        FurnaceStep {
            lit_change: (was_lit != (self.burn > 0)).then_some(self.burn > 0),
            smelted,
            active: self.needs_tick(),
        }
    }
    pub fn encode(&self) -> crate::item_payload::Encoded<FURNACE_MAX_BYTES> {
        let mut bytes = [0; FURNACE_BYTES];
        bytes[..4].copy_from_slice(b"FUR1");
        for (stack, raw) in self.slots.iter().zip(bytes[4..22].chunks_exact_mut(6)) {
            raw[..2].copy_from_slice(&stack.item().to_le_bytes());
            raw[2..4].copy_from_slice(&stack.count().to_le_bytes());
            raw[4..].copy_from_slice(&stack.damage().to_le_bytes());
        }
        for (value, raw) in [self.burn, self.fuel_total, self.cook, self.cook_total]
            .into_iter()
            .zip(bytes[22..].chunks_exact_mut(2))
        {
            raw.copy_from_slice(&value.to_le_bytes());
        }
        let mut encoded = crate::item_payload::Encoded::from_prefix(&bytes);
        encoded
            .append_refs(self.slots.iter().enumerate().map(|(i, s)| (i, s.payload())))
            .expect("validated furnace references");
        encoded
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SurvivalError> {
        if !(FURNACE_BYTES..=FURNACE_MAX_BYTES).contains(&bytes.len()) || &bytes[..4] != b"FUR1" {
            return Err(SurvivalError::InvalidEncoding);
        }
        let mut furnace = Self::default();
        for (stack, raw) in furnace.slots.iter_mut().zip(bytes[4..22].chunks_exact(6)) {
            let read = |i| u16::from_le_bytes([raw[i], raw[i + 1]]);
            *stack = ItemStack::from_parts(read(0), read(2), read(4))
                .map_err(|_| SurvivalError::InvalidStack)?;
        }
        let read = |i| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        furnace.burn = read(22);
        furnace.fuel_total = read(24);
        furnace.cook = read(26);
        furnace.cook_total = read(28);
        if furnace.burn > furnace.fuel_total
            || furnace.fuel_total > 20_000
            || furnace.cook >= COOK_TICKS
            || ![0, COOK_TICKS].contains(&furnace.cook_total)
            || furnace.cook > furnace.cook_total
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let (refs, used) = crate::item_payload::refs::read(&bytes[FURNACE_BYTES..], 3)
            .map_err(|_| SurvivalError::InvalidEncoding)?;
        if FURNACE_BYTES + used != bytes.len() {
            return Err(SurvivalError::InvalidEncoding);
        }
        for (slot, id) in refs {
            furnace.slots[slot] = furnace.slots[slot]
                .with_payload(id)
                .map_err(|_| SurvivalError::InvalidStack)?;
        }
        Ok(furnace)
    }
}

/// Single chests own 27 slots; no implicit double-chest scan or world mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chest {
    slots: [ItemStack; 27],
    revision: u64,
}
impl Default for Chest {
    fn default() -> Self {
        Self {
            slots: [ItemStack::EMPTY; 27],
            revision: 0,
        }
    }
}
impl Chest {
    pub const fn slots(&self) -> &[ItemStack; 27] {
        &self.slots
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub fn set_trusted(&mut self, slot: usize, value: ItemStack) -> Result<(), SurvivalError> {
        let before = self.slots.get(slot).ok_or(SurvivalError::InvalidSlot)?;
        if *before != value {
            self.revision = self
                .revision
                .checked_add(1)
                .ok_or(SurvivalError::RevisionOverflow)?;
            self.slots[slot] = value;
        }
        Ok(())
    }
    pub fn encode(&self) -> crate::item_payload::Encoded<CHEST_MAX_BYTES> {
        let mut bytes = [0; CHEST_BYTES];
        bytes[..4].copy_from_slice(b"CHS1");
        bytes[4..12].copy_from_slice(&self.revision.to_le_bytes());
        for (value, raw) in self.slots.iter().zip(bytes[12..].chunks_exact_mut(6)) {
            raw[..2].copy_from_slice(&value.item().to_le_bytes());
            raw[2..4].copy_from_slice(&value.count().to_le_bytes());
            raw[4..].copy_from_slice(&value.damage().to_le_bytes());
        }
        let mut encoded = crate::item_payload::Encoded::from_prefix(&bytes);
        encoded
            .append_refs(self.slots.iter().enumerate().map(|(i, s)| (i, s.payload())))
            .expect("validated chest references");
        encoded
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SurvivalError> {
        if !(CHEST_BYTES..=CHEST_MAX_BYTES).contains(&bytes.len()) || &bytes[..4] != b"CHS1" {
            return Err(SurvivalError::InvalidEncoding);
        }
        let mut chest = Self {
            revision: u64::from_le_bytes(
                bytes[4..12]
                    .try_into()
                    .map_err(|_| SurvivalError::InvalidEncoding)?,
            ),
            ..Self::default()
        };
        for (slot, raw) in chest
            .slots
            .iter_mut()
            .zip(bytes[12..CHEST_BYTES].chunks_exact(6))
        {
            let read = |i| u16::from_le_bytes([raw[i], raw[i + 1]]);
            *slot = ItemStack::from_parts(read(0), read(2), read(4))
                .map_err(|_| SurvivalError::InvalidStack)?;
        }
        let (refs, used) = crate::item_payload::refs::read(&bytes[CHEST_BYTES..], 27)
            .map_err(|_| SurvivalError::InvalidEncoding)?;
        if CHEST_BYTES + used != bytes.len() {
            return Err(SurvivalError::InvalidEncoding);
        }
        for (slot, id) in refs {
            chest.slots[slot] = chest.slots[slot]
                .with_payload(id)
                .map_err(|_| SurvivalError::InvalidStack)?;
        }
        Ok(chest)
    }
    pub fn plan_transfer(
        &self,
        inventory: &super::Inventory,
        chest_slot: usize,
        inventory_slot: usize,
        count: u16,
        to_chest: bool,
    ) -> Result<ChestTransfer, SurvivalError> {
        let old_chest = *self
            .slots
            .get(chest_slot)
            .ok_or(SurvivalError::InvalidSlot)?;
        let old_inventory = inventory
            .get(inventory_slot)
            .ok_or(SurvivalError::InvalidSlot)?;
        let (source, target) = if to_chest {
            (old_inventory, old_chest)
        } else {
            (old_chest, old_inventory)
        };
        if source.is_empty()
            || count == 0
            || source.count() < count
            || (!target.is_empty()
                && (!target.same_kind(source)
                    || target.count().saturating_add(count) > target.maximum()))
        {
            return Err(SurvivalError::NoEntitlement);
        }
        let remaining = source
            .with_count(source.count() - count)
            .map_err(|_| SurvivalError::InvalidStack)?;
        let merged = source
            .with_count(target.count() + count)
            .map_err(|_| SurvivalError::InvalidStack)?;
        let mut chest = *self;
        let mut next_inventory = *inventory;
        if to_chest {
            chest.set_trusted(chest_slot, merged)?;
            next_inventory.set_trusted(inventory_slot, remaining)?;
        } else {
            chest.set_trusted(chest_slot, remaining)?;
            next_inventory.set_trusted(inventory_slot, merged)?;
        }
        Ok(ChestTransfer {
            before: *self,
            after: chest,
            inventory: inventory.plan_replace_trusted(next_inventory)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "Persist chest component and player inventory in the same commit"]
pub struct ChestTransfer {
    before: Chest,
    after: Chest,
    pub inventory: super::InventoryPlan,
}
impl ChestTransfer {
    pub const fn chest_after(&self) -> &Chest {
        &self.after
    }
    pub fn apply(
        self,
        inventory: &mut super::Inventory,
        chest: &mut Chest,
    ) -> Result<(), SurvivalError> {
        if *chest != self.before || inventory != self.inventory.before() {
            return Err(SurvivalError::StalePlan);
        }
        self.inventory.apply(inventory)?;
        *chest = self.after;
        Ok(())
    }
}
