//! Detached survival phase planning. The single world owner persists the world
//! transactions and RuntimeDelta, then calls acknowledge after WAL completion.
//! No I/O, network publication, or world commits occur in this module.
#[path = "survival/registration.rs"]
mod registration;
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, SpaceId, Tick, Version};
use rules::{
    ids::*,
    survival::{
        CellChange, SurvivalError, farm,
        fluid::{self, Liquid},
        furnace::{Chest, Furnace},
    },
};
use std::collections::{BTreeMap, BTreeSet};
use world::{CellRead, ComponentData, Transaction, World};

pub const FURNACE_COMPONENT: u16 = 0x8101;
pub const CHEST_COMPONENT: u16 = 0x8102;
pub const DROP_COMPONENT: u16 = 0x8103;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Event {
    Fluid(BlockPos, Liquid),
    Furnace(ComponentId),
    Farm(BlockPos),
    FarmSample(BlockPos, u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingEvent {
    pub event: Event,
    pub due: Tick,
    pub first_due: Tick,
    pub order: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub events: usize,
    pub cells: u64,
    pub writes: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            events: 128,
            cells: 16_384,
            writes: 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub events: u64,
    pub reads: u64,
    pub writes: u64,
    pub component_updates: u64,
    pub unknown_deferred: u64,
    pub oldest_overdue: u64,
}

#[derive(Debug)]
pub struct SurvivalRuntime {
    seed: u64,
    revision: u64,
    next_order: u64,
    capacity: usize,
    events: BTreeMap<(Tick, u64), PendingEvent>,
    keys: BTreeMap<Event, (Tick, u64)>,
    ages: BTreeMap<Tick, usize>,
}

#[derive(Clone, Debug)]
pub struct RuntimeDelta {
    before: u64,
    after: u64,
    next_order: u64,
    remove: Vec<(Tick, u64)>,
    add: Vec<PendingEvent>,
}
pub struct PhasePlan {
    pub transactions: Vec<Transaction>,
    pub delta: RuntimeDelta,
    pub work: Work,
}
impl SurvivalRuntime {
    pub fn new(capacity: usize) -> Self {
        Self {
            seed: 0,
            revision: 0,
            next_order: 0,
            capacity,
            events: BTreeMap::new(),
            keys: BTreeMap::new(),
            ages: BTreeMap::new(),
        }
    }
    pub fn with_seed(capacity: usize, seed: u64) -> Self {
        Self {
            seed,
            ..Self::new(capacity)
        }
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn register(&mut self, event: Event, due: Tick) -> Result<bool, SurvivalError> {
        validate_event(event)?;
        if self.keys.contains_key(&event) {
            return Ok(false);
        }
        if self.len() >= self.capacity {
            return Err(SurvivalError::Capacity);
        }
        let next = self
            .next_order
            .checked_add(1)
            .ok_or(SurvivalError::RevisionOverflow)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(SurvivalError::RevisionOverflow)?;
        self.insert(PendingEvent {
            event,
            due,
            first_due: due,
            order: self.next_order,
        });
        self.next_order = next;
        self.revision = revision;
        Ok(true)
    }
    /// Action-side scheduling is detached too: include this delta in the same
    /// WAL batch as the accepted placement/edit, then acknowledge after fsync.
    /// `register` is only a convenience for startup/fixture state construction.
    pub fn plan_register(
        &self,
        events: impl IntoIterator<Item = (Event, Tick)>,
    ) -> Result<RuntimeDelta, SurvivalError> {
        let mut pending = BTreeMap::new();
        for (event, due) in events {
            validate_event(event)?;
            if !self.keys.contains_key(&event) {
                schedule(&mut pending, event, due);
            }
        }
        if self.len().saturating_add(pending.len()) > self.capacity {
            return Err(SurvivalError::Capacity);
        }
        let mut next_order = self.next_order;
        let mut add = Vec::with_capacity(pending.len());
        for (_, mut event) in pending {
            event.order = next_order;
            next_order = next_order
                .checked_add(1)
                .ok_or(SurvivalError::RevisionOverflow)?;
            add.push(event);
        }
        Ok(RuntimeDelta {
            before: self.revision,
            after: self
                .revision
                .checked_add(1)
                .ok_or(SurvivalError::RevisionOverflow)?,
            next_order,
            remove: Vec::new(),
            add,
        })
    }
    /// After the owner has staged one edit, read its new world view to reserve
    /// the seven affected fluid-frontier entries before the accepted ACK.
    pub fn plan_wake(
        &self,
        world: &World,
        p: BlockPos,
        now: Tick,
    ) -> Result<RuntimeDelta, SurvivalError> {
        let mut entries = Vec::with_capacity(7);
        for delta in [
            [0, 0, 0],
            [-1, 0, 0],
            [1, 0, 0],
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [0, 0, 1],
        ] {
            let q = BlockPos {
                space: p.space,
                x: p.x
                    .checked_add(delta[0])
                    .ok_or(SurvivalError::UnknownInput)?,
                y: p.y
                    .checked_add(delta[1])
                    .ok_or(SurvivalError::UnknownInput)?,
                z: p.z
                    .checked_add(delta[2])
                    .ok_or(SurvivalError::UnknownInput)?,
            };
            let CellRead::Known(cell) = world.read(q) else {
                return Err(SurvivalError::UnknownInput);
            };
            if let Some(value) = fluid::fluid(cell) {
                entries.push((
                    Event::Fluid(q, value.liquid),
                    Tick(
                        now.0
                            .checked_add(value.liquid.delay())
                            .ok_or(SurvivalError::RevisionOverflow)?,
                    ),
                ));
            }
        }
        self.plan_register(entries)
    }
    fn insert(&mut self, event: PendingEvent) {
        let key = (event.due, event.order);
        self.events.insert(key, event);
        self.keys.insert(event.event, key);
        *self.ages.entry(event.first_due).or_default() += 1;
    }
    fn remove(&mut self, key: (Tick, u64)) -> Option<PendingEvent> {
        let event = self.events.remove(&key)?;
        self.keys.remove(&event.event);
        let count = self.ages.get_mut(&event.first_due).expect("age index");
        *count -= 1;
        if *count == 0 {
            self.ages.remove(&event.first_due);
        }
        Some(event)
    }
    pub fn oldest_overdue(&self, now: Tick) -> u64 {
        self.ages
            .first_key_value()
            .map_or(0, |(due, _)| now.0.saturating_sub(due.0))
    }
    /// Full-state snapshots belong to checkpoints, outside the tick hot path.
    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            seed: self.seed,
            revision: self.revision,
            next_order: self.next_order,
            events: self.events.values().copied().collect(),
        }
    }
    pub fn restore(capacity: usize, snapshot: &RuntimeSnapshot) -> Result<Self, SurvivalError> {
        if snapshot.events.len() > capacity {
            return Err(SurvivalError::Capacity);
        }
        let mut runtime = Self::new(capacity);
        runtime.revision = snapshot.revision;
        runtime.seed = snapshot.seed;
        runtime.next_order = snapshot.next_order;
        let mut orders = BTreeSet::new();
        for &event in &snapshot.events {
            validate_event(event.event)?;
            if event.first_due > event.due
                || event.order >= snapshot.next_order
                || !orders.insert(event.order)
                || runtime.keys.contains_key(&event.event)
            {
                return Err(SurvivalError::InvalidEncoding);
            }
            runtime.insert(event);
        }
        Ok(runtime)
    }
    /// Apply only after the corresponding world transactions are durable. Exact
    /// revision guards reject duplicate or reordered WAL deltas without mutation.
    pub fn acknowledge(&mut self, delta: RuntimeDelta) -> Result<(), SurvivalError> {
        self.validate_delta(&delta)?;
        for key in delta.remove {
            self.remove(key);
        }
        for event in delta.add {
            self.insert(event);
        }
        self.next_order = delta.next_order;
        self.revision = delta.after;
        Ok(())
    }
    pub fn plan_tick(
        &self,
        world: &World,
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, SurvivalError> {
        self.plan_tick_with_policy(world, now, budget, |_| true)
    }
    pub fn plan_tick_with_policy(
        &self,
        world: &World,
        now: Tick,
        budget: Budget,
        authorize: impl Fn(BlockPos) -> bool,
    ) -> Result<PhasePlan, SurvivalError> {
        let mut stage = Stage::new(world, &authorize, self.seed);
        let mut consumed = Vec::new();
        let mut pending = BTreeMap::<Event, PendingEvent>::new();
        let mut work = Work {
            oldest_overdue: self.oldest_overdue(now),
            ..Work::default()
        };
        for (&key, &event) in self.events.range(..=(now, u64::MAX)).take(budget.events) {
            let reserve = match event.event {
                Event::Fluid(..) => 2048,
                Event::Furnace(..) => 1,
                Event::Farm(..) | Event::FarmSample(..) => 256,
            };
            let writes = match event.event {
                Event::Fluid(..) => 48,
                _ => 1,
            };
            // Reserve the entire next event before starting it, then refund
            // unused reads. Stable fluid sources must not consume the maximum
            // hardening/wake cost of events which actually change neighbors.
            if stage.reads.get().saturating_add(reserve) > budget.cells
                || stage.cells.len().saturating_add(writes) > budget.writes
            {
                break;
            }
            consumed.push(key);
            work.events += 1;
            let result = match event.event {
                Event::Fluid(position, _) => {
                    stage.fluid_tick(position, now, &mut pending, &mut work)
                }
                Event::Furnace(id) => stage.furnace_tick(id, now, &mut pending, &mut work),
                Event::Farm(position) | Event::FarmSample(position, _) => {
                    stage.farm_tick(position, now, event.order, &mut pending, &mut work)
                }
            };
            match result {
                Ok(()) => {}
                Err(SurvivalError::UnknownInput) => {
                    work.unknown_deferred += 1;
                    let mut retry = event;
                    retry.due = Tick(
                        now.0
                            .checked_add(1)
                            .ok_or(SurvivalError::RevisionOverflow)?,
                    );
                    pending.insert(event.event, retry);
                }
                Err(error) => return Err(error),
            }
        }
        let removed: BTreeSet<_> = consumed.iter().copied().collect();
        let mut next_order = self.next_order;
        let mut additions = Vec::new();
        for (_, mut event) in pending {
            if self
                .keys
                .get(&event.event)
                .is_some_and(|key| !removed.contains(key))
            {
                continue;
            }
            if event.order == u64::MAX {
                event.order = next_order;
                next_order = next_order
                    .checked_add(1)
                    .ok_or(SurvivalError::RevisionOverflow)?;
            }
            additions.push(event);
        }
        if self
            .len()
            .saturating_sub(consumed.len())
            .saturating_add(additions.len())
            > self.capacity
        {
            return Err(SurvivalError::Capacity);
        }
        work.writes = stage.cells.len() as u64;
        work.reads = stage.reads.get();
        work.component_updates = stage.components.len() as u64;
        Ok(PhasePlan {
            transactions: stage.transactions(now),
            delta: RuntimeDelta {
                before: self.revision,
                after: self
                    .revision
                    .checked_add(1)
                    .ok_or(SurvivalError::RevisionOverflow)?,
                next_order,
                remove: consumed,
                add: additions,
            },
            work,
        })
    }
}

struct Stage<'a, P> {
    seed: u64,
    world: &'a World,
    authorize: &'a P,
    cells: BTreeMap<BlockPos, Cell>,
    components: BTreeMap<ComponentId, ComponentData>,
    inputs: std::cell::RefCell<BTreeMap<core_types::PageId, Version>>,
    reads: std::cell::Cell<u64>,
}
impl<'a, P: Fn(BlockPos) -> bool> Stage<'a, P> {
    fn new(world: &'a World, authorize: &'a P, seed: u64) -> Self {
        Self {
            seed,
            world,
            authorize,
            cells: BTreeMap::new(),
            components: BTreeMap::new(),
            inputs: std::cell::RefCell::new(BTreeMap::new()),
            reads: std::cell::Cell::new(0),
        }
    }
    fn read(&self, p: BlockPos) -> Option<Cell> {
        self.reads.set(self.reads.get() + 1);
        if !(self.authorize)(p) {
            return Some(Cell::from_parts(STONE, 0));
        }
        if let Some(&value) = self.cells.get(&p) {
            return Some(value);
        }
        self.inputs
            .borrow_mut()
            .insert(p.page(), self.world.page_version(p.page()));
        match self.world.read(p) {
            CellRead::Unknown => None,
            CellRead::Known(value) => Some(value),
        }
    }
    fn change(&mut self, space: SpaceId, change: CellChange) -> Result<(), SurvivalError> {
        let p = position(space, change.position);
        if !(self.authorize)(p) || self.read(p) != Some(change.before) {
            return Err(SurvivalError::StalePlan);
        }
        self.cells.insert(p, change.after);
        Ok(())
    }
    fn wake(
        &self,
        p: BlockPos,
        now: Tick,
        pending: &mut BTreeMap<Event, PendingEvent>,
    ) -> Result<(), SurvivalError> {
        for coords in std::iter::once(Some([p.x, p.y, p.z])).chain(
            [
                [-1, 0, 0],
                [1, 0, 0],
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
            ]
            .into_iter()
            .map(|offset| {
                Some([
                    p.x.checked_add(offset[0])?,
                    p.y.checked_add(offset[1])?,
                    p.z.checked_add(offset[2])?,
                ])
            }),
        ) {
            let coords = coords.ok_or(SurvivalError::UnknownInput)?;
            let target = position(p.space, coords);
            let cell = self.read(target).ok_or(SurvivalError::UnknownInput)?;
            if let Some(value) = fluid::fluid(cell) {
                schedule(
                    pending,
                    Event::Fluid(target, value.liquid),
                    Tick(
                        now.0
                            .checked_add(value.liquid.delay())
                            .ok_or(SurvivalError::RevisionOverflow)?,
                    ),
                );
            }
        }
        Ok(())
    }
    fn fluid_tick(
        &mut self,
        p: BlockPos,
        now: Tick,
        pending: &mut BTreeMap<Event, PendingEvent>,
        work: &mut Work,
    ) -> Result<(), SurvivalError> {
        let plan = fluid::plan_tick(
            |coords| self.read(position(p.space, coords)),
            [p.x, p.y, p.z],
        )?;
        work.reads += u64::from(plan.reads);
        // Preflight both neighbor-hardening and wake dependencies before adding
        // any staged writes. Unknown reads cannot leak a partial event.
        for change in plan.changes() {
            let q = position(p.space, change.position);
            for dx in -2i32..=2 {
                for dy in -2i32..=2 {
                    for dz in -2i32..=2 {
                        if dx.abs() + dy.abs() + dz.abs() > 2 {
                            continue;
                        }
                        let delta = [dx, dy, dz];
                        self.read(BlockPos {
                            space: q.space,
                            x: q.x
                                .checked_add(delta[0])
                                .ok_or(SurvivalError::UnknownInput)?,
                            y: q.y
                                .checked_add(delta[1])
                                .ok_or(SurvivalError::UnknownInput)?,
                            z: q.z
                                .checked_add(delta[2])
                                .ok_or(SurvivalError::UnknownInput)?,
                        })
                        .ok_or(SurvivalError::UnknownInput)?;
                    }
                }
            }
        }
        for &change in plan.changes() {
            self.change(p.space, change)?;
        }
        let mut changed: Vec<_> = plan
            .changes()
            .iter()
            .map(|change| position(p.space, change.position))
            .collect();
        for change in plan.changes() {
            let hardening = fluid::plan_neighbor_hardening(
                |coords| self.read(position(p.space, coords)),
                change.position,
            )?;
            for &change in hardening.changes() {
                self.change(p.space, change)?;
                changed.push(position(p.space, change.position));
            }
        }
        for position in changed {
            self.wake(position, now, pending)?;
        }
        Ok(())
    }
    fn furnace_tick(
        &mut self,
        id: ComponentId,
        now: Tick,
        pending: &mut BTreeMap<Event, PendingEvent>,
        work: &mut Work,
    ) -> Result<(), SurvivalError> {
        let Some(record) = self.world.component(id) else {
            return Ok(());
        };
        let data = self.components.get(&id).unwrap_or(&record.data).clone();
        if data.kind != FURNACE_COMPONENT {
            return Ok(());
        }
        let cell = self.read(data.anchor).ok_or(SurvivalError::UnknownInput)?;
        if cell.id() != FURNACE {
            return Ok(());
        }
        let mut furnace = Furnace::decode(&data.payload)?;
        let step = furnace.tick();
        work.reads += 1;
        if let Some(lit) = step.lit_change {
            self.cells.insert(
                data.anchor,
                Cell::from_parts(
                    FURNACE,
                    if lit {
                        cell.state() | rules::blocks::ST_LIT
                    } else {
                        cell.state() & !rules::blocks::ST_LIT
                    },
                ),
            );
        }
        self.components.insert(
            id,
            ComponentData {
                payload: furnace.encode().to_vec(),
                ..data
            },
        );
        if step.active {
            schedule(
                pending,
                Event::Furnace(id),
                Tick(
                    now.0
                        .checked_add(1)
                        .ok_or(SurvivalError::RevisionOverflow)?,
                ),
            );
        }
        Ok(())
    }
    fn farm_tick(
        &mut self,
        p: BlockPos,
        now: Tick,
        order: u64,
        pending: &mut BTreeMap<Event, PendingEvent>,
        work: &mut Work,
    ) -> Result<(), SurvivalError> {
        let light = self
            .world
            .page(p.page())
            .ok_or(SurvivalError::UnknownInput)?
            .light(p.local_index());
        let cell = self.read(p).ok_or(SurvivalError::UnknownInput)?;
        let mut rng = core_types::DeterministicRng::from_key(self.seed, 0x4641524d, order, now.0);
        let roll = if farm::is_crop(cell.id()) {
            let (moisture, reads) = farm::moisture_factor(
                |coords| self.read(position(p.space, coords)),
                [p.x, p.y, p.z],
                cell.id(),
            )?;
            work.reads += u64::from(reads);
            rng.below(u64::from(farm::growth_denominator(moisture)))
                .expect("positive bound") as u32
        } else {
            0
        };
        let plan = farm::random_tick(
            |coords| self.read(position(p.space, coords)),
            [p.x, p.y, p.z],
            light
                .sky
                .saturating_sub(crate::mob_spawn::ambient_darkness(now.0 % 24000))
                .max(light.block),
            roll,
        )?;
        work.reads += u64::from(plan.reads);
        if let Some(change) = plan.change {
            // Read wake inputs before changing any staged cell.
            let mut preflight = BTreeMap::new();
            self.wake(p, now, &mut preflight)?;
            self.change(p.space, change)?;
            self.wake(p, now, pending)?;
        }
        Ok(())
    }
    fn transactions(self, now: Tick) -> Vec<Transaction> {
        let mut transactions = BTreeMap::<SpaceId, Transaction>::new();
        for (p, value) in self.cells {
            transactions
                .entry(p.space)
                .or_insert_with(|| Transaction::new(now, p.space))
                .set_cell(p, value);
        }
        for (id, data) in self.components {
            transactions
                .entry(data.anchor.space)
                .or_insert_with(|| Transaction::new(now, data.anchor.space))
                .set_component(id, data, ChannelMask::CONTENTS);
        }
        for (page, version) in self.inputs.into_inner() {
            if let Some(tx) = transactions.get_mut(&page.space) {
                tx.expect_page(page, version);
            }
        }
        transactions.into_values().collect()
    }
}
fn position(space: SpaceId, p: [i32; 3]) -> BlockPos {
    BlockPos {
        space,
        x: p[0],
        y: p[1],
        z: p[2],
    }
}
fn schedule(pending: &mut BTreeMap<Event, PendingEvent>, event: Event, due: Tick) {
    pending
        .entry(event)
        .and_modify(|value| {
            value.due = value.due.min(due);
            value.first_due = value.first_due.min(due);
        })
        .or_insert(PendingEvent {
            event,
            due,
            first_due: due,
            order: u64::MAX,
        });
}

pub fn furnace_component(anchor: BlockPos, furnace: &Furnace) -> ComponentData {
    ComponentData {
        anchor,
        kind: FURNACE_COMPONENT,
        payload: furnace.encode().to_vec(),
    }
}
pub fn chest_component(anchor: BlockPos, chest: &Chest) -> ComponentData {
    ComponentData {
        anchor,
        kind: CHEST_COMPONENT,
        payload: chest.encode().to_vec(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub seed: u64,
    pub revision: u64,
    pub next_order: u64,
    pub events: Vec<PendingEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropEntry {
    pub entity_id: u64,
    pub player: [u8; 16],
    pub action: u64,
    pub index: u8,
    pub spawned: Tick,
    pub pickup_at: Tick,
    pub stack: rules::stack::ItemStack,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DropBundle {
    pub entries: Vec<DropEntry>,
}
impl DropBundle {
    pub fn encode(&self) -> Result<Vec<u8>, SurvivalError> {
        if self.entries.len() > 64 {
            return Err(SurvivalError::Capacity);
        }
        let mut bytes = vec![0; 8 + self.entries.len() * 56];
        bytes[..4].copy_from_slice(b"DRP1");
        bytes[4] = self.entries.len() as u8;
        for (entry, raw) in self.entries.iter().zip(bytes[8..].chunks_exact_mut(56)) {
            raw[..8].copy_from_slice(&entry.entity_id.to_le_bytes());
            raw[8..24].copy_from_slice(&entry.player);
            raw[24..32].copy_from_slice(&entry.action.to_le_bytes());
            raw[32] = entry.index;
            raw[33..41].copy_from_slice(&entry.spawned.0.to_le_bytes());
            raw[41..49].copy_from_slice(&entry.pickup_at.0.to_le_bytes());
            raw[49..51].copy_from_slice(&entry.stack.item().to_le_bytes());
            raw[51..53].copy_from_slice(&entry.stack.count().to_le_bytes());
            raw[53..55].copy_from_slice(&entry.stack.damage().to_le_bytes());
        }
        rules::item_payload::refs::append(
            &mut bytes,
            self.entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i, e.stack.payload())),
        )
        .map_err(|_| SurvivalError::InvalidEncoding)?;
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SurvivalError> {
        if bytes.len() < 8
            || &bytes[..4] != b"DRP1"
            || bytes[4] > 64
            || bytes[5..8].iter().any(|&value| value != 0)
            || bytes.len() < 8 + usize::from(bytes[4]) * 56
            || bytes.len() > 8 + 64 * 56 + 6 + 64 * 10
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let mut entries = Vec::with_capacity(usize::from(bytes[4]));
        let mut ids = BTreeSet::new();
        let prefix = 8 + usize::from(bytes[4]) * 56;
        for raw in bytes[8..prefix].chunks_exact(56) {
            let read64 = |i| u64::from_le_bytes(raw[i..i + 8].try_into().expect("fixed drop"));
            let read16 = |i| u16::from_le_bytes([raw[i], raw[i + 1]]);
            let entry = DropEntry {
                entity_id: read64(0),
                player: raw[8..24].try_into().expect("fixed player"),
                action: read64(24),
                index: raw[32],
                spawned: Tick(read64(33)),
                pickup_at: Tick(read64(41)),
                stack: rules::stack::ItemStack::from_parts(read16(49), read16(51), read16(53))
                    .map_err(|_| SurvivalError::InvalidStack)?,
            };
            if raw[55] != 0
                || entry.stack.is_empty()
                || entry.pickup_at < entry.spawned
                || !ids.insert(entry.entity_id)
                || entry.entity_id != drop_entity_id(entry.player, entry.action, entry.index)
            {
                return Err(SurvivalError::InvalidEncoding);
            }
            entries.push(entry);
        }
        let (refs, used) = rules::item_payload::refs::read(&bytes[prefix..], entries.len())
            .map_err(|_| SurvivalError::InvalidEncoding)?;
        if prefix + used != bytes.len() {
            return Err(SurvivalError::InvalidEncoding);
        }
        for (slot, id) in refs {
            entries[slot].stack = entries[slot]
                .stack
                .with_payload(id)
                .map_err(|_| SurvivalError::InvalidStack)?;
        }
        Ok(Self { entries })
    }
}
pub struct DropPlan {
    pub id: ComponentId,
    pub data: ComponentData,
    remove: Option<ComponentId>,
    page_version: Version,
}
impl DropPlan {
    pub fn append_to(self, transaction: &mut Transaction) {
        transaction.expect_page(self.data.anchor.page(), self.page_version);
        if let Some(remove) = self.remove {
            transaction.remove_component(remove, ChannelMask::CONTENTS);
        }
        transaction.set_component(
            self.id,
            self.data,
            ChannelMask::CONTENTS | ChannelMask::EVENTS,
        );
    }
}
fn drop_entity_id(player: [u8; 16], action: u64, index: u8) -> u64 {
    let mut hash = blake3::Hasher::new();
    hash.update(b"survival-item-v1");
    hash.update(&player);
    hash.update(&action.to_le_bytes());
    hash.update(&[index]);
    u64::from_le_bytes(
        hash.finalize().as_bytes()[..8]
            .try_into()
            .expect("hash prefix"),
    )
}
/// One component holds co-located drops because World has one anchor per cell.
/// Each contained item retains its own stable action-derived entity identity.
/// The M7 owner may unbundle these after durability without inventing new IDs.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit durable provenance and anchor-removal authorization stay visible to the world owner"
)]
pub fn plan_drops(
    world: &World,
    anchor: BlockPos,
    player: [u8; 16],
    action: u64,
    now: Tick,
    stacks: &[rules::stack::ItemStack],
    pickup_delay: u64,
    remove_block_component: Option<ComponentId>,
) -> Result<DropPlan, SurvivalError> {
    if stacks.len() > 64 {
        return Err(SurvivalError::Capacity);
    }
    let page = world
        .page(anchor.page())
        .ok_or(SurvivalError::UnknownInput)?;
    let mut remove = None;
    let (id, mut bundle) = if let Some(id) = page.component(anchor.local_index()) {
        let record = world.component(id).ok_or(SurvivalError::InvalidEncoding)?;
        if record.data.kind == DROP_COMPONENT {
            (id, DropBundle::decode(&record.data.payload)?)
        } else if remove_block_component == Some(id) {
            remove = Some(id);
            (drop_bundle_id(anchor), DropBundle::default())
        } else {
            return Err(SurvivalError::NoEntitlement);
        }
    } else {
        (drop_bundle_id(anchor), DropBundle::default())
    };
    if world
        .component(id)
        .is_some_and(|record| record.data.anchor != anchor || record.data.kind != DROP_COMPONENT)
        && Some(id) != remove
    {
        return Err(SurvivalError::NoEntitlement);
    }
    let pickup_at = Tick(
        now.0
            .checked_add(pickup_delay)
            .ok_or(SurvivalError::RevisionOverflow)?,
    );
    for (index, &stack) in stacks.iter().enumerate() {
        if stack.is_empty() {
            continue;
        }
        let entity_id = drop_entity_id(player, action, index as u8);
        if let Some(existing) = bundle
            .entries
            .iter()
            .find(|entry| entry.entity_id == entity_id)
        {
            if existing.player != player
                || existing.action != action
                || existing.index != index as u8
                || existing.stack != stack
            {
                return Err(SurvivalError::NoEntitlement);
            }
            continue;
        }
        if bundle.entries.len() == 64 {
            return Err(SurvivalError::Capacity);
        }
        bundle.entries.push(DropEntry {
            entity_id,
            player,
            action,
            index: index as u8,
            spawned: now,
            pickup_at,
            stack,
        });
    }
    Ok(DropPlan {
        id,
        data: ComponentData {
            anchor,
            kind: DROP_COMPONENT,
            payload: bundle.encode()?,
        },
        remove,
        page_version: page.version(),
    })
}
fn drop_bundle_id(anchor: BlockPos) -> ComponentId {
    let mut hash = blake3::Hasher::new();
    hash.update(b"survival-bundle-v1");
    hash.update(&anchor.space.0.to_le_bytes());
    for value in [anchor.x, anchor.y, anchor.z] {
        hash.update(&value.to_le_bytes());
    }
    ComponentId(u64::from_le_bytes(
        hash.finalize().as_bytes()[..8]
            .try_into()
            .expect("hash prefix"),
    ))
}
impl RuntimeSnapshot {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(32 + self.events.len() * 48);
        bytes.extend_from_slice(b"SVR1");
        bytes.extend_from_slice(&self.revision.to_le_bytes());
        bytes.extend_from_slice(&self.next_order.to_le_bytes());
        bytes.extend_from_slice(&self.seed.to_le_bytes());
        bytes.extend_from_slice(&(self.events.len() as u32).to_le_bytes());
        for &event in &self.events {
            bytes.extend_from_slice(&encode_event(event));
        }
        bytes
    }
    pub fn decode(bytes: &[u8], capacity: usize) -> Result<Self, SurvivalError> {
        if bytes.len() < 32 || &bytes[..4] != b"SVR1" {
            return Err(SurvivalError::InvalidEncoding);
        }
        let count = u32::from_le_bytes(
            bytes[28..32]
                .try_into()
                .map_err(|_| SurvivalError::InvalidEncoding)?,
        ) as usize;
        if count > capacity {
            return Err(SurvivalError::Capacity);
        }
        if count
            .checked_mul(48)
            .and_then(|value| value.checked_add(32))
            != Some(bytes.len())
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let snapshot = Self {
            seed: u64::from_le_bytes(
                bytes[20..28]
                    .try_into()
                    .map_err(|_| SurvivalError::InvalidEncoding)?,
            ),
            revision: u64::from_le_bytes(
                bytes[4..12]
                    .try_into()
                    .map_err(|_| SurvivalError::InvalidEncoding)?,
            ),
            next_order: u64::from_le_bytes(
                bytes[12..20]
                    .try_into()
                    .map_err(|_| SurvivalError::InvalidEncoding)?,
            ),
            events: bytes[32..]
                .chunks_exact(48)
                .map(decode_event)
                .collect::<Result<_, _>>()?,
        };
        Ok(SurvivalRuntime::restore(capacity, &snapshot)?.snapshot())
    }
}
impl RuntimeDelta {
    /// Bounded WAL payload proportional to this phase's events. The checkpoint
    /// owns the full queue; never serialize all 100k pending events every tick.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(36 + self.remove.len() * 16 + self.add.len() * 48);
        bytes.extend_from_slice(b"SVD1");
        for value in [self.before, self.after, self.next_order] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&(self.remove.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.add.len() as u32).to_le_bytes());
        for &(due, order) in &self.remove {
            bytes.extend_from_slice(&due.0.to_le_bytes());
            bytes.extend_from_slice(&order.to_le_bytes());
        }
        for &event in &self.add {
            bytes.extend_from_slice(&encode_event(event));
        }
        bytes
    }
    pub fn decode(bytes: &[u8], maximum_events: usize) -> Result<Self, SurvivalError> {
        if bytes.len() < 36 || &bytes[..4] != b"SVD1" {
            return Err(SurvivalError::InvalidEncoding);
        }
        let read64 =
            |start| u64::from_le_bytes(bytes[start..start + 8].try_into().expect("checked header"));
        let count = |start| {
            u32::from_le_bytes(bytes[start..start + 4].try_into().expect("checked header")) as usize
        };
        let removals = count(28);
        let additions = count(32);
        if removals > maximum_events || additions > maximum_events {
            return Err(SurvivalError::Capacity);
        }
        let boundary = removals
            .checked_mul(16)
            .and_then(|v| v.checked_add(36))
            .ok_or(SurvivalError::InvalidEncoding)?;
        if additions
            .checked_mul(48)
            .and_then(|v| v.checked_add(boundary))
            != Some(bytes.len())
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let remove = bytes[36..boundary]
            .chunks_exact(16)
            .map(|raw| {
                (
                    Tick(u64::from_le_bytes(
                        raw[..8].try_into().expect("fixed event"),
                    )),
                    u64::from_le_bytes(raw[8..].try_into().expect("fixed event")),
                )
            })
            .collect();
        let add = bytes[boundary..]
            .chunks_exact(48)
            .map(decode_event)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            before: read64(4),
            after: read64(12),
            next_order: read64(20),
            remove,
            add,
        })
    }
}
fn encode_event(event: PendingEvent) -> [u8; 48] {
    let mut bytes = [0; 48];
    bytes[..8].copy_from_slice(&event.due.0.to_le_bytes());
    bytes[8..16].copy_from_slice(&event.first_due.0.to_le_bytes());
    bytes[16..24].copy_from_slice(&event.order.to_le_bytes());
    let (tag, position, liquid) = match event.event {
        Event::Fluid(position, liquid) => (0, Some(position), Some(liquid)),
        Event::Furnace(id) => {
            bytes[25..33].copy_from_slice(&id.0.to_le_bytes());
            (1, None, None)
        }
        Event::Farm(position) => (2, Some(position), None),
        Event::FarmSample(position, token) => {
            debug_assert!(token <= FARM_SAMPLE_TOKEN_MAX);
            bytes[41..48].copy_from_slice(&token.to_le_bytes()[..7]);
            (3, Some(position), None)
        }
    };
    bytes[24] = tag;
    if let Some(position) = position {
        bytes[25..29].copy_from_slice(&position.space.0.to_le_bytes());
        for (value, raw) in [position.x, position.y, position.z]
            .into_iter()
            .zip(bytes[29..41].chunks_exact_mut(4))
        {
            raw.copy_from_slice(&value.to_le_bytes());
        }
    }
    if let Some(liquid) = liquid {
        bytes[41] = match liquid {
            Liquid::Water => 0,
            Liquid::Lava => 1,
        };
    }
    bytes
}
fn decode_event(bytes: &[u8]) -> Result<PendingEvent, SurvivalError> {
    if bytes.len() != 48 {
        return Err(SurvivalError::InvalidEncoding);
    }
    let read64 =
        |index| u64::from_le_bytes(bytes[index..index + 8].try_into().expect("fixed event"));
    let position = || {
        let mut coords = [0; 3];
        for (value, raw) in coords.iter_mut().zip(bytes[29..41].chunks_exact(4)) {
            *value = i32::from_le_bytes(raw.try_into().expect("fixed position"));
        }
        position(
            SpaceId(u32::from_le_bytes(
                bytes[25..29].try_into().expect("fixed space"),
            )),
            coords,
        )
    };
    let (event, reserved) = match bytes[24] {
        0 => (
            Event::Fluid(
                position(),
                match bytes[41] {
                    0 => Liquid::Water,
                    1 => Liquid::Lava,
                    _ => return Err(SurvivalError::InvalidEncoding),
                },
            ),
            42,
        ),
        1 => (Event::Furnace(ComponentId(read64(25))), 33),
        2 => (Event::Farm(position()), 41),
        3 => {
            let mut raw = [0; 8];
            raw[..7].copy_from_slice(&bytes[41..48]);
            (Event::FarmSample(position(), u64::from_le_bytes(raw)), 48)
        }
        _ => return Err(SurvivalError::InvalidEncoding),
    };
    if bytes[reserved..].iter().any(|&value| value != 0) {
        return Err(SurvivalError::InvalidEncoding);
    }
    Ok(PendingEvent {
        due: Tick(read64(0)),
        first_due: Tick(read64(8)),
        order: read64(16),
        event,
    })
}

pub const FARM_SAMPLE_TOKEN_MAX: u64 = (1u64 << 56) - 1;
pub fn farm_sample_token(now: Tick, ordinal: u16) -> Result<u64, SurvivalError> {
    if ordinal >= 4096 {
        return Err(SurvivalError::Capacity);
    }
    if now.0 > (FARM_SAMPLE_TOKEN_MAX >> 12) {
        return Err(SurvivalError::RevisionOverflow);
    }
    Ok((now.0 << 12) | u64::from(ordinal))
}
fn validate_event(event: Event) -> Result<(), SurvivalError> {
    if matches!(event,Event::FarmSample(_,token) if token>FARM_SAMPLE_TOKEN_MAX) {
        Err(SurvivalError::InvalidEncoding)
    } else {
        Ok(())
    }
}
