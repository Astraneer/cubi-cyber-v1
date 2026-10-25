//! Typed item entities, spatial ownership and detached conservative transfers.
use super::{
    EntityError, EntityId, combat,
    mob::validate_position,
    spatial::{QueryBudget, QueryWork, SpatialHash},
};
use crate::{
    stack::ItemStack,
    survival::{Inventory, InventoryPlan},
};
use core_types::{DeterministicRng, SpaceId, Tick};
use std::collections::{BTreeMap, BTreeSet};
pub const ITEM_BYTES: usize = 160;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemEntity {
    pub id: EntityId,
    pub space: SpaceId,
    pub stack: ItemStack,
    pub position: [f64; 3],
    pub previous: [f64; 3],
    pub velocity: [f64; 3],
    pub on_ground: bool,
    pub no_clip: bool,
    pub in_water: bool,
    pub age: u32,
    pub item_age: u32,
    pub spin_age: u32,
    pub pickup_delay: u16,
    pub thrown: bool,
    pub offset: f32,
    pub last_tick: Tick,
}
impl ItemEntity {
    pub fn new(
        id: EntityId,
        space: SpaceId,
        stack: ItemStack,
        position: [f64; 3],
        seed: u64,
        delay: u16,
    ) -> Result<Self, EntityError> {
        validate_position(position)?;
        if stack.is_empty() || delay > 32767 {
            return Err(EntityError::InvalidEncoding);
        }
        let mut rng = DeterministicRng::from_key(seed, 0x4954454d, id.0, 0);
        let unit = |rng: &mut DeterministicRng| crate::survival::loot::unit(rng);
        Ok(Self {
            id,
            space,
            stack,
            position,
            previous: position,
            velocity: [
                f64::from(unit(&mut rng)) * 0.2 - 0.1,
                0.2,
                f64::from(unit(&mut rng)) * 0.2 - 0.1,
            ],
            on_ground: false,
            no_clip: false,
            in_water: false,
            age: 0,
            item_age: 0,
            spin_age: 0,
            pickup_delay: delay,
            thrown: false,
            offset: unit(&mut rng) * std::f32::consts::TAU,
            last_tick: Tick(0),
        })
    }
    pub fn bbox(&self) -> crate::shapes::Aabb {
        let [x, y, z] = self.position;
        crate::shapes::Aabb {
            lo: [x - 0.125, y, z - 0.125],
            hi: [x + 0.125, y + 0.25, z + 0.125],
        }
    }
    pub fn validate(&self) -> Result<(), EntityError> {
        validate_position(self.position)?;
        validate_position(self.previous)?;
        if self.stack.is_empty()
            || self
                .velocity
                .iter()
                .any(|value| !value.is_finite() || value.abs() > 64.0)
            || !self.offset.is_finite()
            || !(0.0..std::f32::consts::TAU).contains(&self.offset)
            || self.pickup_delay > 32767
            || self.item_age > 6000
        {
            return Err(EntityError::InvalidEncoding);
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<[u8; ITEM_BYTES], EntityError> {
        self.validate()?;
        let mut bytes = [0; ITEM_BYTES];
        bytes[..4].copy_from_slice(b"ITM1");
        bytes[4..12].copy_from_slice(&self.id.0.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.space.0.to_le_bytes());
        for (value, raw) in [
            self.stack.item(),
            self.stack.count(),
            self.stack.damage(),
            self.pickup_delay,
        ]
        .into_iter()
        .zip(bytes[16..24].chunks_exact_mut(2))
        {
            raw.copy_from_slice(&value.to_le_bytes());
        }
        for (value, raw) in self
            .position
            .into_iter()
            .chain(self.previous)
            .chain(self.velocity)
            .zip(bytes[24..96].chunks_exact_mut(8))
        {
            raw.copy_from_slice(&value.to_bits().to_le_bytes());
        }
        for (value, raw) in [
            self.age,
            self.item_age,
            self.spin_age,
            self.offset.to_bits(),
        ]
        .into_iter()
        .zip(bytes[96..112].chunks_exact_mut(4))
        {
            raw.copy_from_slice(&value.to_le_bytes());
        }
        bytes[112] = u8::from(self.on_ground)
            | (u8::from(self.no_clip) << 1)
            | (u8::from(self.in_water) << 2)
            | (u8::from(self.thrown) << 3);
        bytes[113..121].copy_from_slice(&self.last_tick.0.to_le_bytes());
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, EntityError> {
        if bytes.len() != ITEM_BYTES
            || &bytes[..4] != b"ITM1"
            || bytes[121..].iter().any(|&value| value != 0)
            || bytes[112] & !15 != 0
        {
            return Err(EntityError::InvalidEncoding);
        }
        let read16 = |i| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let read32 = |i| u32::from_le_bytes(bytes[i..i + 4].try_into().expect("fixed item"));
        let read64 = |i| u64::from_le_bytes(bytes[i..i + 8].try_into().expect("fixed item"));
        let mut vectors = [[0.0; 3]; 3];
        for (value, raw) in vectors
            .iter_mut()
            .flatten()
            .zip(bytes[24..96].chunks_exact(8))
        {
            *value = f64::from_bits(u64::from_le_bytes(raw.try_into().expect("fixed vector")));
        }
        let item = Self {
            id: EntityId(read64(4)),
            space: SpaceId(read32(12)),
            stack: ItemStack::from_parts(read16(16), read16(18), read16(20))
                .map_err(|_| EntityError::InvalidEncoding)?,
            pickup_delay: read16(22),
            position: vectors[0],
            previous: vectors[1],
            velocity: vectors[2],
            age: read32(96),
            item_age: read32(100),
            spin_age: read32(104),
            offset: f32::from_bits(read32(108)),
            on_ground: bytes[112] & 1 != 0,
            no_clip: bytes[112] & 2 != 0,
            in_water: bytes[112] & 4 != 0,
            thrown: bytes[112] & 8 != 0,
            last_tick: Tick(read64(113)),
        };
        item.validate()?;
        Ok(item)
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemPatch {
    pub before: Option<ItemEntity>,
    pub after: Option<ItemEntity>,
    pub before_active: bool,
    pub after_active: bool,
}
impl ItemPatch {
    pub fn id(&self) -> Result<EntityId, EntityError> {
        match (self.before, self.after) {
            (Some(before), Some(after)) if before.id == after.id => Ok(before.id),
            (Some(before), None) => Ok(before.id),
            (None, Some(after)) => Ok(after.id),
            _ => Err(EntityError::InvalidEncoding),
        }
    }
}
pub struct ItemStore {
    rows: Vec<ItemEntity>,
    ids: BTreeMap<EntityId, usize>,
    active: BTreeSet<EntityId>,
    spatial: SpatialHash,
    capacity: usize,
}
impl ItemStore {
    pub fn new(capacity: usize) -> Result<Self, EntityError> {
        Ok(Self {
            rows: Vec::with_capacity(capacity),
            ids: BTreeMap::new(),
            active: BTreeSet::new(),
            spatial: SpatialHash::new(capacity, 8.0)?,
            capacity,
        })
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn get(&self, id: EntityId) -> Option<&ItemEntity> {
        self.ids.get(&id).map(|&index| &self.rows[index])
    }
    pub fn rows(&self) -> &[ItemEntity] {
        &self.rows
    }
    pub fn active_ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.active.iter().copied()
    }
    pub fn is_active(&self, id: EntityId) -> bool {
        self.active.contains(&id)
    }
    pub fn insert(&mut self, item: ItemEntity, active: bool) -> Result<(), EntityError> {
        item.validate()?;
        if self.rows.len() == self.capacity {
            return Err(EntityError::Capacity);
        }
        if self.ids.contains_key(&item.id) {
            return Err(EntityError::DuplicateId);
        }
        let index = self.rows.len();
        self.spatial.update(index, item.position)?;
        self.rows.push(item);
        self.ids.insert(item.id, index);
        if active {
            self.active.insert(item.id);
        }
        Ok(())
    }
    pub fn remove(&mut self, id: EntityId) -> Result<ItemEntity, EntityError> {
        let index = *self.ids.get(&id).ok_or(EntityError::UnknownId)?;
        self.spatial.remove(index)?;
        let last = self.rows.len() - 1;
        if index != last {
            self.spatial.move_index(last, index)?;
        }
        self.ids.remove(&id);
        self.active.remove(&id);
        let item = self.rows.swap_remove(index);
        if index < self.rows.len() {
            self.ids.insert(self.rows[index].id, index);
        }
        Ok(item)
    }
    pub fn validate_patches(&self, patches: &[ItemPatch]) -> Result<(), EntityError> {
        let mut seen = BTreeSet::new();
        let mut count = self.len();
        for patch in patches {
            let id = patch.id()?;
            if !seen.insert(id)
                || self.get(id).copied() != patch.before
                || self.is_active(id) != patch.before_active
                || (patch.before.is_none() && patch.before_active)
                || (patch.after.is_none() && patch.after_active)
            {
                return Err(EntityError::StalePlan);
            }
            if let Some(after) = patch.after {
                after.validate()?;
                if patch.before.is_none() {
                    count += 1;
                }
            } else {
                count = count.checked_sub(1).ok_or(EntityError::InvalidEncoding)?;
            }
        }
        if count > self.capacity {
            return Err(EntityError::Capacity);
        }
        Ok(())
    }
    pub fn apply_patches(&mut self, patches: &[ItemPatch]) -> Result<(), EntityError> {
        self.validate_patches(patches)?;
        for patch in patches {
            if patch.after.is_none() {
                self.remove(patch.id()?)?;
            }
        }
        for patch in patches {
            if let Some(after) = patch.after {
                if let Some(before) = patch.before {
                    let index = self.ids[&before.id];
                    self.spatial.update(index, after.position)?;
                    self.rows[index] = after;
                    if patch.after_active {
                        self.active.insert(after.id);
                    } else {
                        self.active.remove(&after.id);
                    }
                } else {
                    self.insert(after, patch.after_active)?;
                }
            }
        }
        Ok(())
    }
    pub fn query_ids(
        &self,
        low: [f64; 3],
        high: [f64; 3],
        budget: QueryBudget,
        out: &mut [EntityId],
    ) -> Result<(usize, QueryWork), EntityError> {
        let mut len = 0;
        let mut overflow = false;
        let mut work = self.spatial.query(low, high, budget, |index| {
            if len == out.len() {
                overflow = true;
            } else {
                out[len] = self.rows[index].id;
                len += 1;
            }
        })?;
        work.overflow |= overflow;
        Ok((len, work))
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use = "Commit inventory and item entity remainder/removal in one WAL batch"]
pub struct PickupPlan {
    pub item: ItemPatch,
    pub inventory: InventoryPlan,
    pub picked: u16,
}
impl PickupPlan {
    pub fn prepare(
        item: ItemEntity,
        active: bool,
        inventory: &Inventory,
        player_space: SpaceId,
        player_position: [f64; 3],
        crouching: bool,
        alive: bool,
    ) -> Result<Self, EntityError> {
        if !alive || item.space != player_space || item.pickup_delay != 0 || item.item_age >= 6000 {
            return Err(EntityError::NoEntitlement);
        }
        let body = combat::body_box(player_position, crouching);
        let zone = crate::shapes::Aabb {
            lo: [body.lo[0] - 1.0, body.lo[1] - 0.5, body.lo[2] - 1.0],
            hi: [body.hi[0] + 1.0, body.hi[1] + 0.5, body.hi[2] + 1.0],
        };
        if !zone.intersects(&item.bbox()) {
            return Err(EntityError::NoEntitlement);
        }
        let plan = inventory
            .plan_pickup(item.stack)
            .map_err(|_| EntityError::NoEntitlement)?;
        let left = plan.remainder();
        if left == item.stack {
            return Err(EntityError::NoEntitlement);
        }
        let after = (!left.is_empty()).then_some(ItemEntity {
            stack: left,
            ..item
        });
        Ok(Self {
            item: ItemPatch {
                before: Some(item),
                after,
                before_active: active,
                after_active: after.is_some() && active,
            },
            inventory: plan,
            picked: item.stack.count() - left.count(),
        })
    }
    pub fn apply(
        self,
        items: &mut ItemStore,
        inventory: &mut Inventory,
    ) -> Result<(), EntityError> {
        items.validate_patches(&[self.item])?;
        if inventory != self.inventory.before() {
            return Err(EntityError::StalePlan);
        }
        items.apply_patches(&[self.item])?;
        self.inventory
            .apply(inventory)
            .map_err(|_| EntityError::StalePlan)
    }
}
/// Donor merge: the larger stack survives in place; a tie chooses the other
/// argument. No overflow split. Delay=max, despawn age=min, spin age unchanged.
pub fn plan_merge(
    a: ItemEntity,
    b: ItemEntity,
    active_a: bool,
    active_b: bool,
) -> Result<[ItemPatch; 2], EntityError> {
    if a.id == b.id
        || a.space != b.space
        || !a.stack.same_kind(b.stack)
        || a.stack.maximum() <= 1
        || a.stack.count() + b.stack.count() > a.stack.maximum()
        || a.pickup_delay == 32767
        || b.pickup_delay == 32767
        || a.item_age >= 6000
        || b.item_age >= 6000
    {
        return Err(EntityError::NoEntitlement);
    }
    let bounds = a.bbox();
    let zone = crate::shapes::Aabb {
        lo: [bounds.lo[0] - 0.5, bounds.lo[1], bounds.lo[2] - 0.5],
        hi: [bounds.hi[0] + 0.5, bounds.hi[1], bounds.hi[2] + 0.5],
    };
    if !b.bbox().intersects(&zone) {
        return Err(EntityError::NoEntitlement);
    }
    let (source, target, source_active, target_active) = if b.stack.count() < a.stack.count() {
        (b, a, active_b, active_a)
    } else {
        (a, b, active_a, active_b)
    };
    let after = ItemEntity {
        stack: target
            .stack
            .with_count(a.stack.count() + b.stack.count())
            .map_err(|_| EntityError::InvalidEncoding)?,
        pickup_delay: a.pickup_delay.max(b.pickup_delay),
        item_age: a.item_age.min(b.item_age),
        ..target
    };
    Ok([
        ItemPatch {
            before: Some(source),
            after: None,
            before_active: source_active,
            after_active: false,
        },
        ItemPatch {
            before: Some(target),
            after: Some(after),
            before_active: target_active,
            after_active: target_active,
        },
    ])
}
