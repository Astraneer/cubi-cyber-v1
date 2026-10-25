//! Durable dropped-item owner: confirmed bundle import, bounded physics/merges,
//! and pickup plans which publish inventory and entity changes together.
use super::{CHANNELS, Tracked};
use crate::survival::{DROP_COMPONENT, DropBundle};
use core_types::{ChannelMask, ComponentId, SpaceId, Tick, Version};
use rules::{
    entities::{
        EntityError, EntityId,
        item::{self, ItemEntity, ItemPatch, ItemSnapshot, ItemStore, PickupPlan},
        spatial::{QueryBudget, SpatialHash},
    },
    stack::ItemStack,
    survival::Inventory,
};
use sim::{collision::Bufs, entities::item as physics};
use std::{
    cell::{Cell as Counter, RefCell},
    collections::{BTreeMap, BTreeSet},
};
use world::{Transaction, World};

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub entities: usize,
    pub cells: u64,
    pub merge_checks: u64,
    pub pickup_checks: u64,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            entities: 1024,
            cells: 1_000_000,
            merge_checks: 16_384,
            pickup_checks: 16_384,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub entities: u64,
    pub queries: u64,
    pub merge_candidates: u64,
    pub merged: u64,
    pub picked_items: u64,
    pub waiting_world: u64,
    pub deferred: u64,
    pub query_overflows: u64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickupPlayer {
    pub id: EntityId,
    pub space: SpaceId,
    pub position: [f64; 3],
    pub crouching: bool,
    pub alive: bool,
    pub inventory: Inventory,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryPatch {
    pub player: EntityId,
    pub before: Inventory,
    pub after: Inventory,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickupEvent {
    pub item: EntityId,
    pub player: EntityId,
    pub before: ItemStack,
    pub picked: u16,
    pub position: [f64; 3],
}
#[derive(Clone, Debug)]
pub struct ItemDelta {
    revision: u64,
    next_revision: u64,
    cursor_before: Option<EntityId>,
    cursor_after: Option<EntityId>,
    pub patches: Vec<ItemPatch>,
}
pub struct PhasePlan {
    pub delta: ItemDelta,
    pub inventories: Vec<InventoryPatch>,
    pub pickups: Vec<PickupEvent>,
    pub inputs: Vec<jobs::InputVersion>,
    pub work: Work,
}
#[must_use = "Persist imported item rows and remove confirmed source components in one WAL batch"]
pub struct ImportPlan {
    pub delta: ItemDelta,
    pub transactions: Vec<Transaction>,
    sources: Vec<(ComponentId, Version)>,
}
pub struct ItemRuntime {
    store: ItemStore,
    seed: u64,
    revision: u64,
    cursor: Option<EntityId>,
    bufs: Bufs,
    scratch: SpatialHash,
    scratch_ids: Vec<EntityId>,
    maximum: usize,
}
impl ItemRuntime {
    pub fn new(maximum: usize, seed: u64) -> Result<Self, EntityError> {
        Ok(Self {
            store: ItemStore::new(maximum)?,
            seed,
            revision: 0,
            cursor: None,
            bufs: Bufs::default(),
            scratch: SpatialHash::new(maximum, 8.0)?,
            scratch_ids: Vec::with_capacity(maximum),
            maximum,
        })
    }
    pub fn entities(&self) -> &ItemStore {
        &self.store
    }
    pub fn insert_initial(&mut self, item: ItemEntity, active: bool) -> Result<(), EntityError> {
        self.store.insert(item, active)
    }
    pub fn plan_activity(&self, changes: &[(EntityId, bool)]) -> Result<ItemDelta, EntityError> {
        if changes.len() > 1024 {
            return Err(EntityError::Capacity);
        }
        let patches = changes
            .iter()
            .map(|&(id, active)| {
                let item = *self.store.get(id).ok_or(EntityError::UnknownId)?;
                Ok(ItemPatch {
                    before: Some(item),
                    after: Some(item),
                    before_active: self.store.is_active(id),
                    after_active: active,
                })
            })
            .collect::<Result<Vec<_>, EntityError>>()?;
        self.store.validate_patches(&patches)?;
        self.delta(patches, self.cursor)
    }
    fn delta(
        &self,
        patches: Vec<ItemPatch>,
        cursor: Option<EntityId>,
    ) -> Result<ItemDelta, EntityError> {
        Ok(ItemDelta {
            revision: self.revision,
            next_revision: self.revision.checked_add(1).ok_or(EntityError::Capacity)?,
            cursor_before: self.cursor,
            cursor_after: cursor,
            patches,
        })
    }
    pub fn acknowledge(&mut self, delta: ItemDelta) -> Result<(), EntityError> {
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(EntityError::Capacity)?
            || delta.cursor_before != self.cursor
        {
            return Err(EntityError::StalePlan);
        }
        self.store.apply_patches(&delta.patches)?;
        self.revision = delta.next_revision;
        self.cursor = delta.cursor_after;
        Ok(())
    }
    /// IDs must come from the owner's already durable component-change stream.
    /// There is no scan over every component each tick.
    pub fn plan_import(
        &self,
        world: &World,
        components: &[ComponentId],
        now: Tick,
        active: bool,
    ) -> Result<ImportPlan, EntityError> {
        if components.len() > 64 {
            return Err(EntityError::Capacity);
        }
        let mut patches = Vec::new();
        let mut transactions = BTreeMap::<SpaceId, Transaction>::new();
        let mut sources = Vec::new();
        let mut seen = BTreeSet::new();
        for &id in components {
            if !seen.insert(id) {
                return Err(EntityError::DuplicateId);
            }
            let record = world.component(id).ok_or(EntityError::UnknownId)?;
            if record.data.kind != DROP_COMPONENT {
                return Err(EntityError::NoEntitlement);
            }
            let bundle = DropBundle::decode(&record.data.payload)
                .map_err(|_| EntityError::InvalidEncoding)?;
            for entry in bundle.entries {
                if self.store.get(EntityId(entry.entity_id)).is_some() {
                    return Err(EntityError::DuplicateId);
                }
                let mut rng = core_types::DeterministicRng::from_key(
                    self.seed,
                    0x44524f50504f53,
                    entry.entity_id,
                    0,
                );
                let mut jitter = || f64::from(rules::survival::loot::unit(&mut rng)) * 0.5 - 0.25;
                let position = [
                    f64::from(record.data.anchor.x) + 0.5 + jitter(),
                    f64::from(record.data.anchor.y) + 0.375 + jitter(),
                    f64::from(record.data.anchor.z) + 0.5 + jitter(),
                ];
                let remaining = entry.pickup_at.0.saturating_sub(now.0).min(32767) as u16;
                let mut entity = ItemEntity::new(
                    EntityId(entry.entity_id),
                    record.data.anchor.space,
                    entry.stack,
                    position,
                    self.seed,
                    remaining,
                )?;
                entity.last_tick = now;
                patches.push(ItemPatch {
                    before: None,
                    after: Some(entity),
                    before_active: false,
                    after_active: active,
                });
            }
            sources.push((id, record.version));
            transactions
                .entry(record.data.anchor.space)
                .or_insert_with(|| Transaction::new(now, record.data.anchor.space))
                .expect_page(
                    record.data.anchor.page(),
                    world.page_version(record.data.anchor.page()),
                )
                .remove_component(id, ChannelMask::CONTENTS | ChannelMask::EVENTS);
        }
        self.store.validate_patches(&patches)?;
        Ok(ImportPlan {
            delta: self.delta(patches, self.cursor)?,
            transactions: transactions.into_values().collect(),
            sources,
        })
    }
    pub fn validate_import(&self, world: &World, plan: &ImportPlan) -> Result<(), EntityError> {
        if plan.delta.revision != self.revision
            || plan.sources.iter().any(|&(id, version)| {
                world
                    .component(id)
                    .is_none_or(|record| record.version != version)
            })
        {
            return Err(EntityError::StalePlan);
        }
        self.store.validate_patches(&plan.delta.patches)
    }
    fn candidates(
        &self,
        low: [f64; 3],
        high: [f64; 3],
        maximum: u64,
    ) -> Result<(BTreeSet<EntityId>, bool, u64), EntityError> {
        let mut ids = [EntityId(0); 256];
        let (count, work) = self.store.query_ids(
            low,
            high,
            QueryBudget {
                buckets: 128,
                probes: 1024,
                candidates: maximum.min(256),
            },
            &mut ids,
        )?;
        let mut candidates: BTreeSet<_> = ids[..count].iter().copied().collect();
        let mut scratch_ids = 0u64;
        let scratch = self.scratch.query(
            low,
            high,
            QueryBudget {
                buckets: 128,
                probes: 1024,
                candidates: maximum.saturating_sub(work.candidates).min(256),
            },
            |index| {
                candidates.insert(self.scratch_ids[index]);
                scratch_ids += 1;
            },
        )?;
        Ok((
            candidates,
            work.overflow || scratch.overflow,
            work.candidates + scratch_ids,
        ))
    }
    /// Players are trusted private server snapshots. Their resulting inventories
    /// and this ledger delta are one publication unit; the input slice is intact.
    pub fn plan_tick(
        &mut self,
        world: &World,
        now: Tick,
        players: &[PickupPlayer],
        budget: Budget,
    ) -> Result<PhasePlan, EntityError> {
        if players.len() > 64 {
            return Err(EntityError::Capacity);
        }
        for index in 0..self.scratch_ids.len() {
            self.scratch.remove(index)?;
        }
        self.scratch_ids.clear();
        let mut selected = vec![EntityId(0); budget.entities.min(self.store.active_len())];
        let count = self.store.active_batch(self.cursor, &mut selected);
        selected.truncate(count);
        let mut cursor = self.cursor;
        let mut staged = BTreeMap::<EntityId, Option<ItemEntity>>::new();
        let reads = Counter::new(0u64);
        let exhausted = Counter::new(false);
        let inputs = RefCell::new(BTreeMap::new());
        let mut work = Work {
            deferred: self.store.active_len().saturating_sub(count) as u64,
            ..Work::default()
        };
        let mut merge_due = Vec::new();
        for &id in &selected {
            cursor = Some(id);
            let before = *self.store.get(id).ok_or(EntityError::UnknownId)?;
            if before.last_tick >= now {
                continue;
            }
            let query = Tracked {
                world,
                space: before.space,
                reads: &reads,
                maximum: budget.cells,
                exhausted: &exhausted,
                inputs: &inputs,
            };
            match physics::plan_item(&before, &query, self.seed, &mut self.bufs) {
                Ok(plan) => {
                    work.entities += 1;
                    let after = plan.after.map(|mut item| {
                        item.last_tick = now;
                        item
                    });
                    if plan.merge_due && after.is_some() {
                        merge_due.push(id);
                    }
                    if let Some(after) = after {
                        let index = self.scratch_ids.len();
                        self.scratch.update(index, after.position)?;
                        self.scratch_ids.push(id);
                    }
                    staged.insert(id, after);
                }
                Err(EntityError::UnknownInput) => work.waiting_world += 1,
                Err(error) => return Err(error),
            }
            if exhausted.get() {
                break;
            }
        }
        let mut merge_remaining = budget.merge_checks;
        for id in merge_due {
            if merge_remaining == 0 {
                break;
            }
            let Some(item) = value(&self.store, &staged, id) else {
                continue;
            };
            let bounds = item.bbox();
            let (candidates, overflow, checked) = self.candidates(
                [bounds.lo[0] - 0.5, bounds.lo[1], bounds.lo[2] - 0.5],
                [bounds.hi[0] + 0.5, bounds.hi[1], bounds.hi[2] + 0.5],
                merge_remaining,
            )?;
            merge_remaining = merge_remaining.saturating_sub(checked);
            work.merge_candidates += checked;
            work.query_overflows += u64::from(overflow);
            if !overflow {
                staged.insert(
                    id,
                    Some(ItemEntity {
                        merge_debt: false,
                        ..item
                    }),
                );
            }
            for other in candidates {
                if other == id || !self.store.is_active(other) {
                    continue;
                }
                let Some(a) = value(&self.store, &staged, id) else {
                    break;
                };
                let Some(b) = value(&self.store, &staged, other) else {
                    continue;
                };
                if let Ok(patches) = item::plan_merge(a, b, true, true) {
                    for patch in patches {
                        staged.insert(patch.id()?, patch.after);
                    }
                    work.merged += 1;
                }
            }
        }
        let mut owned: Vec<_> = players.to_vec();
        owned.sort_unstable_by_key(|player| player.id);
        if owned.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(EntityError::DuplicateId);
        }
        let mut pickup_remaining = budget.pickup_checks;
        let mut pickups = Vec::new();
        for player in &mut owned {
            if !player.alive || pickup_remaining == 0 {
                continue;
            }
            let bounds = rules::entities::combat::body_box(player.position, player.crouching);
            let (candidates, overflow, checked) = self.candidates(
                [bounds.lo[0] - 1.0, bounds.lo[1] - 0.5, bounds.lo[2] - 1.0],
                [bounds.hi[0] + 1.0, bounds.hi[1] + 0.5, bounds.hi[2] + 1.0],
                pickup_remaining,
            )?;
            pickup_remaining = pickup_remaining.saturating_sub(checked);
            work.query_overflows += u64::from(overflow);
            for id in candidates {
                let Some(item) = value(&self.store, &staged, id) else {
                    continue;
                };
                let Ok(plan) = PickupPlan::prepare(
                    item,
                    self.store.is_active(id),
                    &player.inventory,
                    player.space,
                    player.position,
                    player.crouching,
                    player.alive,
                ) else {
                    continue;
                };
                pickups.push(PickupEvent {
                    item: id,
                    player: player.id,
                    before: item.stack,
                    picked: plan.picked,
                    position: item.position,
                });
                work.picked_items += u64::from(plan.picked);
                player.inventory = *plan.inventory.after();
                staged.insert(id, plan.item.after);
            }
        }
        let inventories = owned
            .into_iter()
            .filter_map(|player| {
                let before = players
                    .iter()
                    .find(|old| old.id == player.id)
                    .expect("same trusted players")
                    .inventory;
                (before != player.inventory).then_some(InventoryPatch {
                    player: player.id,
                    before,
                    after: player.inventory,
                })
            })
            .collect();
        let patches = staged
            .into_iter()
            .filter_map(|(id, after)| {
                let before = self.store.get(id).copied();
                (before != after).then_some(ItemPatch {
                    before,
                    after,
                    before_active: self.store.is_active(id),
                    after_active: after.is_some() && self.store.is_active(id),
                })
            })
            .collect();
        work.queries = reads.get();
        let inputs = inputs
            .into_inner()
            .into_iter()
            .map(|(page, version)| jobs::InputVersion { page, version })
            .collect();
        Ok(PhasePlan {
            delta: self.delta(patches, cursor)?,
            inventories,
            pickups,
            inputs,
            work,
        })
    }
    pub fn validate(
        &self,
        world: &World,
        players: &[PickupPlayer],
        plan: &PhasePlan,
    ) -> Result<(), EntityError> {
        if plan.delta.revision != self.revision
            || plan.inputs.iter().any(|input| {
                world
                    .page(input.page)
                    .map(|_| world.input_version(input.page, CHANNELS))
                    != input.version
            })
            || plan.inventories.iter().any(|patch| {
                players
                    .iter()
                    .find(|player| player.id == patch.player)
                    .is_none_or(|player| player.inventory != patch.before)
            })
        {
            return Err(EntityError::StalePlan);
        }
        self.store.validate_patches(&plan.delta.patches)
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            seed: self.seed,
            revision: self.revision,
            cursor: self.cursor,
            items: self.store.snapshot(),
        }
    }
    pub fn restore(maximum: usize, snapshot: &Snapshot) -> Result<Self, EntityError> {
        let mut runtime = Self::new(maximum, snapshot.seed)?;
        runtime.store = ItemStore::restore(maximum, &snapshot.items)?;
        runtime.revision = snapshot.revision;
        runtime.cursor = snapshot.cursor;
        Ok(runtime)
    }
    pub const fn capacity(&self) -> usize {
        self.maximum
    }
}
fn value(
    store: &ItemStore,
    staged: &BTreeMap<EntityId, Option<ItemEntity>>,
    id: EntityId,
) -> Option<ItemEntity> {
    staged
        .get(&id)
        .copied()
        .unwrap_or_else(|| store.get(id).copied())
}
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub seed: u64,
    pub revision: u64,
    pub cursor: Option<EntityId>,
    pub items: ItemSnapshot,
}
fn put_cursor(bytes: &mut Vec<u8>, cursor: Option<EntityId>) {
    bytes.push(u8::from(cursor.is_some()));
    bytes.extend_from_slice(&cursor.map_or(0, |id| id.0).to_le_bytes());
}
fn get_cursor(bytes: &[u8]) -> Result<Option<EntityId>, EntityError> {
    if bytes.len() != 9 || bytes[0] > 1 {
        return Err(EntityError::InvalidEncoding);
    }
    let id = u64::from_le_bytes(bytes[1..9].try_into().expect("fixed cursor"));
    if bytes[0] == 0 && id != 0 {
        return Err(EntityError::InvalidEncoding);
    }
    Ok((bytes[0] == 1).then_some(EntityId(id)))
}
impl Snapshot {
    pub fn encode(&self) -> Result<Vec<u8>, EntityError> {
        let items = self.items.encode()?;
        let mut bytes = Vec::with_capacity(33 + items.len());
        bytes.extend_from_slice(b"ITR1");
        bytes.extend_from_slice(&self.seed.to_le_bytes());
        bytes.extend_from_slice(&self.revision.to_le_bytes());
        put_cursor(&mut bytes, self.cursor);
        bytes.extend_from_slice(&(items.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&items);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], maximum: usize) -> Result<Self, EntityError> {
        if bytes.len() < 33 || &bytes[..4] != b"ITR1" {
            return Err(EntityError::InvalidEncoding);
        }
        let length = u32::from_le_bytes(bytes[29..33].try_into().expect("header length")) as usize;
        if length.checked_add(33) != Some(bytes.len()) {
            return Err(EntityError::InvalidEncoding);
        }
        Ok(Self {
            seed: u64::from_le_bytes(bytes[4..12].try_into().expect("header length")),
            revision: u64::from_le_bytes(bytes[12..20].try_into().expect("header length")),
            cursor: get_cursor(&bytes[20..29])?,
            items: ItemSnapshot::decode(&bytes[33..], maximum)?,
        })
    }
}
impl ItemDelta {
    pub fn encode(&self) -> Result<Vec<u8>, EntityError> {
        if self.patches.len() > 1_000_000 {
            return Err(EntityError::Capacity);
        }
        let mut bytes = Vec::with_capacity(42 + self.patches.len() * 324);
        bytes.extend_from_slice(b"ITD1");
        bytes.extend_from_slice(&self.revision.to_le_bytes());
        bytes.extend_from_slice(&self.next_revision.to_le_bytes());
        put_cursor(&mut bytes, self.cursor_before);
        put_cursor(&mut bytes, self.cursor_after);
        bytes.extend_from_slice(&(self.patches.len() as u32).to_le_bytes());
        for patch in &self.patches {
            patch.id()?;
            bytes.extend_from_slice(&[
                u8::from(patch.before.is_some())
                    | (u8::from(patch.after.is_some()) << 1)
                    | (u8::from(patch.before_active) << 2)
                    | (u8::from(patch.after_active) << 3),
                0,
                0,
                0,
            ]);
            if let Some(before) = patch.before {
                bytes.extend_from_slice(&before.encode()?);
            }
            if let Some(after) = patch.after {
                bytes.extend_from_slice(&after.encode()?);
            }
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], maximum: usize) -> Result<Self, EntityError> {
        if bytes.len() < 42 || &bytes[..4] != b"ITD1" {
            return Err(EntityError::InvalidEncoding);
        }
        let count = u32::from_le_bytes(bytes[38..42].try_into().expect("header length")) as usize;
        if count > maximum {
            return Err(EntityError::Capacity);
        }
        if count
            .checked_mul(4)
            .is_none_or(|minimum| minimum > bytes.len() - 42)
        {
            return Err(EntityError::InvalidEncoding);
        }
        let mut cursor = 42usize;
        let mut patches = Vec::with_capacity(count);
        for _ in 0..count {
            let header = bytes
                .get(cursor..cursor + 4)
                .ok_or(EntityError::InvalidEncoding)?;
            cursor += 4;
            let flags = header[0];
            if flags & !15 != 0 || flags & 3 == 0 || header[1..].iter().any(|&value| value != 0) {
                return Err(EntityError::InvalidEncoding);
            }
            let mut values = [None; 2];
            for (index, value) in values.iter_mut().enumerate() {
                if flags & (1 << index) != 0 {
                    let end = cursor
                        .checked_add(item::ITEM_BYTES)
                        .ok_or(EntityError::InvalidEncoding)?;
                    *value = Some(ItemEntity::decode(
                        bytes.get(cursor..end).ok_or(EntityError::InvalidEncoding)?,
                    )?);
                    cursor = end;
                }
            }
            let patch = ItemPatch {
                before: values[0],
                after: values[1],
                before_active: flags & 4 != 0,
                after_active: flags & 8 != 0,
            };
            patch.id()?;
            if (patch.before.is_none() && patch.before_active)
                || (patch.after.is_none() && patch.after_active)
            {
                return Err(EntityError::InvalidEncoding);
            }
            patches.push(patch);
        }
        if cursor != bytes.len() {
            return Err(EntityError::InvalidEncoding);
        }
        Ok(Self {
            revision: u64::from_le_bytes(bytes[4..12].try_into().expect("header length")),
            next_revision: u64::from_le_bytes(bytes[12..20].try_into().expect("header length")),
            cursor_before: get_cursor(&bytes[20..29])?,
            cursor_after: get_cursor(&bytes[29..38])?,
            patches,
        })
    }
}
