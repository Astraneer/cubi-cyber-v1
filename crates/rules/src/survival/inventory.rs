use super::SurvivalError;
use crate::{
    items::{ItemKind, equip_slot, idef},
    stack::ItemStack,
};
use core_types::Cell;

pub const OFFHAND: usize = 40;
pub const INVENTORY_BYTES: usize = 4 + 1 + 1 + 8 + 41 * 6;
pub const INVENTORY_MAX_BYTES: usize = INVENTORY_BYTES + 6 + 41 * 10;

/// Flat indices preserve the donor contract: main 0..36, boots..helmet 36..40,
/// offhand 40. New players own no items; grants are explicit server operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inventory {
    slots: [ItemStack; 41],
    selected: u8,
    revision: u64,
}

impl Default for Inventory {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "Publish the complete inventory result with the accepted world action"]
pub struct InventoryPlan {
    before: Inventory,
    after: Inventory,
    remainder: ItemStack,
    broken: bool,
}

impl InventoryPlan {
    pub const fn before(&self) -> &Inventory {
        &self.before
    }
    pub const fn after(&self) -> &Inventory {
        &self.after
    }
    pub const fn remainder(&self) -> ItemStack {
        self.remainder
    }
    pub const fn broken(&self) -> bool {
        self.broken
    }
    pub fn apply(self, inventory: &mut Inventory) -> Result<(), SurvivalError> {
        if *inventory != self.before {
            return Err(SurvivalError::StalePlan);
        }
        *inventory = self.after;
        Ok(())
    }
}

impl Inventory {
    pub const fn empty() -> Self {
        Self {
            slots: [ItemStack::EMPTY; 41],
            selected: 0,
            revision: 0,
        }
    }
    pub const fn selected(&self) -> usize {
        self.selected as usize
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub fn get(&self, slot: usize) -> Option<ItemStack> {
        self.slots.get(slot).copied()
    }
    pub fn hand(&self) -> ItemStack {
        self.slots[self.selected()]
    }
    pub fn main(&self) -> &[ItemStack] {
        &self.slots[..36]
    }
    pub fn slots(&self) -> &[ItemStack; 41] {
        &self.slots
    }
    fn bump(&mut self) -> Result<(), SurvivalError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(SurvivalError::RevisionOverflow)?;
        Ok(())
    }
    pub fn select(&mut self, slot: usize) -> Result<(), SurvivalError> {
        if slot >= 9 {
            return Err(SurvivalError::InvalidSlot);
        }
        if self.selected() != slot {
            self.bump()?;
            self.selected = slot as u8;
        }
        Ok(())
    }
    pub fn scroll(&mut self, direction: i8) -> Result<(), SurvivalError> {
        let next = (self.selected as i16 - i16::from(direction.signum())).rem_euclid(9);
        self.select(next as usize)
    }
    /// Trusted owner only: client requests must instead use the detached plans.
    pub fn set_trusted(&mut self, slot: usize, value: ItemStack) -> Result<(), SurvivalError> {
        if slot >= 41 {
            return Err(SurvivalError::InvalidSlot);
        }
        if (36..40).contains(&slot)
            && !value.is_empty()
            && equip_slot(value.item()) != Some((slot - 36) as u8)
        {
            return Err(SurvivalError::InvalidArmorSlot);
        }
        if self.slots[slot] != value {
            self.bump()?;
            self.slots[slot] = value;
        }
        Ok(())
    }
    fn plan(&self) -> InventoryPlan {
        InventoryPlan {
            before: *self,
            after: *self,
            remainder: ItemStack::EMPTY,
            broken: false,
        }
    }
    pub(crate) fn plan_replace_trusted(&self, after: Self) -> Result<InventoryPlan, SurvivalError> {
        if after.revision < self.revision {
            return Err(SurvivalError::StalePlan);
        }
        Ok(InventoryPlan {
            before: *self,
            after,
            remainder: ItemStack::EMPTY,
            broken: false,
        })
    }
    pub fn plan_place(
        &self,
        expected: ItemStack,
        cell: Cell,
    ) -> Result<InventoryPlan, SurvivalError> {
        let held = self.hand();
        if held.is_empty()
            || held != expected
            || !matches!(idef(held.item()).kind, ItemKind::Block(block) if block == cell.id())
        {
            return Err(SurvivalError::NoEntitlement);
        }
        self.plan_consume(self.selected(), expected, 1)
    }
    pub fn plan_consume(
        &self,
        slot: usize,
        expected: ItemStack,
        count: u16,
    ) -> Result<InventoryPlan, SurvivalError> {
        let held = self.get(slot).ok_or(SurvivalError::InvalidSlot)?;
        if held != expected || count == 0 || held.count() < count {
            return Err(SurvivalError::NoEntitlement);
        }
        let mut plan = self.plan();
        plan.after.bump()?;
        plan.after.slots[slot] = held
            .with_count(held.count() - count)
            .map_err(|_| SurvivalError::InvalidStack)?;
        Ok(plan)
    }
    pub fn plan_wear_tool(
        &self,
        expected: ItemStack,
        amount: u16,
    ) -> Result<InventoryPlan, SurvivalError> {
        let held = self.hand();
        if held.is_empty() || held != expected {
            return Err(SurvivalError::NoEntitlement);
        }
        let definition = idef(held.item());
        if definition.max_damage == 0
            || !matches!(
                definition.kind,
                ItemKind::Tool { .. } | ItemKind::Sword { .. } | ItemKind::Shears
            )
        {
            return Err(SurvivalError::NotDurableTool);
        }
        let mut plan = self.plan();
        if amount == 0 {
            return Ok(plan);
        }
        plan.after.bump()?;
        let damage = held.damage().saturating_add(amount);
        plan.broken = damage >= definition.max_damage;
        plan.after.slots[self.selected()] = if plan.broken {
            ItemStack::EMPTY
        } else {
            held.with_damage(damage)
                .map_err(|_| SurvivalError::InvalidStack)?
        };
        Ok(plan)
    }
    /// Match donor pickup order: selected matching stack, offhand matching stack,
    /// then matching main slots, then first empty main. Armor is never auto-filled.
    pub fn plan_pickup(&self, mut value: ItemStack) -> Result<InventoryPlan, SurvivalError> {
        let mut plan = self.plan();
        let before = value;
        if value.damage() == 0 {
            for slot in [self.selected(), OFFHAND].into_iter().chain(0..36) {
                let current = plan.after.slots[slot];
                if current.is_empty() || !current.same_kind(value) || current.maximum() <= 1 {
                    continue;
                }
                let take = value.count().min(current.maximum() - current.count());
                plan.after.slots[slot] = current
                    .with_count(current.count() + take)
                    .map_err(|_| SurvivalError::InvalidStack)?;
                value = value
                    .with_count(value.count() - take)
                    .map_err(|_| SurvivalError::InvalidStack)?;
                if value.is_empty() {
                    break;
                }
            }
        }
        if !value.is_empty()
            && let Some(slot) = plan.after.slots[..36]
                .iter()
                .position(|stack| stack.is_empty())
        {
            plan.after.slots[slot] = value;
            value = ItemStack::EMPTY;
        }
        if value != before {
            plan.after.bump()?;
        }
        plan.remainder = value;
        Ok(plan)
    }
    pub fn plan_drop_selected(&self, all: bool) -> Result<InventoryPlan, SurvivalError> {
        let held = self.hand();
        if held.is_empty() {
            return Ok(self.plan());
        }
        let count = if all { held.count() } else { 1 };
        let mut plan = self.plan_consume(self.selected(), held, count)?;
        plan.remainder = held
            .with_count(count)
            .map_err(|_| SurvivalError::InvalidStack)?;
        Ok(plan)
    }
    /// The owner persists this complete state and creates exactly these 41 stacks
    /// as durable item entities in the same transaction; empty values are ignored.
    pub fn plan_death(&self) -> Result<InventoryPlan, SurvivalError> {
        let mut plan = self.plan();
        if self.slots.iter().any(|stack| !stack.is_empty()) {
            plan.after.bump()?;
            plan.after.slots.fill(ItemStack::EMPTY);
        }
        Ok(plan)
    }
    pub fn armor(&self) -> (u8, f32) {
        let mut armor = 0u8;
        let mut toughness = 0.0;
        for value in &self.slots[36..40] {
            if !value.is_empty()
                && let ItemKind::Armor { points, .. } = idef(value.item()).kind
            {
                armor = armor.saturating_add(points);
                toughness += crate::items::armor_toughness(value.item());
            }
        }
        (armor, toughness)
    }
    /// Crafting is an explicit action, so its detached recipe plan may allocate.
    /// No ingredient/output can be published separately from this inventory.
    pub fn plan_craft(
        &self,
        book: &crate::crafting::RecipeBook,
        grid: &crate::crafting::Grid,
        crafts: u16,
    ) -> Result<InventoryCraftPlan, crate::crafting::CraftError> {
        let recipe = crate::crafting::CraftPlan::prepare_exact(
            book,
            grid,
            self.main(),
            crafts,
            crate::crafting::RemainderPolicy::RejectOverflow,
        )?;
        let mut after = *self;
        let mut after_grid = *grid;
        let drops = recipe.apply(&mut after_grid, &mut after.slots[..36])?;
        debug_assert!(drops.is_empty());
        after
            .bump()
            .map_err(|_| crate::crafting::CraftError::StalePlan)?;
        Ok(InventoryCraftPlan {
            before: *self,
            after,
            before_grid: *grid,
            after_grid,
        })
    }
    pub fn plan_armor_wear(&self, amount: u16) -> Result<InventoryPlan, SurvivalError> {
        let mut plan = self.plan();
        if amount == 0 {
            return Ok(plan);
        }
        let mut changed = false;
        for slot in 36..40 {
            let held = self.slots[slot];
            if held.is_empty() {
                continue;
            }
            let damage = held.damage().saturating_add(amount);
            plan.after.slots[slot] = if damage >= idef(held.item()).max_damage {
                ItemStack::EMPTY
            } else {
                held.with_damage(damage)
                    .map_err(|_| SurvivalError::InvalidStack)?
            };
            changed = true;
        }
        if changed {
            plan.after.bump()?;
        }
        Ok(plan)
    }
    pub fn encode(&self) -> crate::item_payload::Encoded<INVENTORY_MAX_BYTES> {
        let mut out = [0u8; INVENTORY_BYTES];
        out[..4].copy_from_slice(b"INV1");
        out[4] = self.selected;
        out[6..14].copy_from_slice(&self.revision.to_le_bytes());
        for (stack, bytes) in self.slots.iter().zip(out[14..].chunks_exact_mut(6)) {
            bytes[..2].copy_from_slice(&stack.item().to_le_bytes());
            bytes[2..4].copy_from_slice(&stack.count().to_le_bytes());
            bytes[4..].copy_from_slice(&stack.damage().to_le_bytes());
        }
        let mut encoded = crate::item_payload::Encoded::from_prefix(&out);
        encoded
            .append_refs(self.slots.iter().enumerate().map(|(i, s)| (i, s.payload())))
            .expect("validated inventory references");
        encoded
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SurvivalError> {
        if !(INVENTORY_BYTES..=INVENTORY_MAX_BYTES).contains(&bytes.len())
            || &bytes[..4] != b"INV1"
            || bytes[4] >= 9
            || bytes[5] != 0
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let mut out = Self::empty();
        out.selected = bytes[4];
        out.revision = u64::from_le_bytes(
            bytes[6..14]
                .try_into()
                .map_err(|_| SurvivalError::InvalidEncoding)?,
        );
        for (slot, raw) in bytes[14..INVENTORY_BYTES].chunks_exact(6).enumerate() {
            let read = |index| u16::from_le_bytes([raw[index], raw[index + 1]]);
            let stack = ItemStack::from_parts(read(0), read(2), read(4))
                .map_err(|_| SurvivalError::InvalidStack)?;
            if (36..40).contains(&slot)
                && !stack.is_empty()
                && equip_slot(stack.item()) != Some((slot - 36) as u8)
            {
                return Err(SurvivalError::InvalidArmorSlot);
            }
            out.slots[slot] = stack;
        }
        let (refs, used) = crate::item_payload::refs::read(&bytes[INVENTORY_BYTES..], 41)
            .map_err(|_| SurvivalError::InvalidEncoding)?;
        if INVENTORY_BYTES + used != bytes.len() {
            return Err(SurvivalError::InvalidEncoding);
        }
        for (slot, id) in refs {
            out.slots[slot] = out.slots[slot]
                .with_payload(id)
                .map_err(|_| SurvivalError::InvalidStack)?;
        }
        Ok(out)
    }
}

#[derive(Debug)]
#[must_use = "Publish the inventory and crafting grid together"]
pub struct InventoryCraftPlan {
    before: Inventory,
    after: Inventory,
    before_grid: crate::crafting::Grid,
    after_grid: crate::crafting::Grid,
}
impl InventoryCraftPlan {
    pub const fn inventory_after(&self) -> &Inventory {
        &self.after
    }
    pub const fn grid_after(&self) -> &crate::crafting::Grid {
        &self.after_grid
    }
    pub fn apply(
        self,
        inventory: &mut Inventory,
        grid: &mut crate::crafting::Grid,
    ) -> Result<(), SurvivalError> {
        if *inventory != self.before || *grid != self.before_grid {
            return Err(SurvivalError::StalePlan);
        }
        *inventory = self.after;
        *grid = self.after_grid;
        Ok(())
    }
}
