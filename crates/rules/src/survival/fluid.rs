//! Event-frontier water/lava rules, rewritten around bounded detached edits.
//! Reading never generates; an unknown dependency defers the entire event.
use super::{CellChange, Inventory, InventoryPlan, SurvivalError};
use crate::{
    blocks::{Shape, bdef, collision_of},
    ids::*,
    items::{I_BUCKET, I_LAVA_BUCKET, I_WATER_BUCKET, ItemKind, idef},
    stack::ItemStack,
};
use core_types::Cell;
use std::{
    cmp::Reverse,
    collections::{BTreeSet, BinaryHeap},
};

const DIRECTIONS: [[i32; 3]; 6] = [
    [-1, 0, 0],
    [1, 0, 0],
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
];
const HORIZONTAL: [usize; 4] = [4, 1, 5, 0];
const WRITE_ORDER: [usize; 4] = [4, 5, 0, 1];
const CACHE_EDGE: usize = 13;
const CACHE_CELLS: usize = CACHE_EDGE * CACHE_EDGE * 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Liquid {
    Water,
    Lava,
}
impl Liquid {
    pub const fn block(self) -> u16 {
        match self {
            Self::Water => WATER,
            Self::Lava => LAVA,
        }
    }
    pub const fn delay(self) -> u64 {
        match self {
            Self::Water => 5,
            Self::Lava => 30,
        }
    }
    const fn decrease(self) -> u8 {
        match self {
            Self::Water => 1,
            Self::Lava => 2,
        }
    }
    const fn search(self) -> u8 {
        match self {
            Self::Water => 4,
            Self::Lava => 2,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fluid {
    pub liquid: Liquid,
    pub amount: u8,
    pub source: bool,
    pub falling: bool,
}
impl Fluid {
    pub const fn source(liquid: Liquid) -> Self {
        Self {
            liquid,
            amount: 8,
            source: true,
            falling: false,
        }
    }
    pub const fn falling(liquid: Liquid) -> Self {
        Self {
            liquid,
            amount: 8,
            source: false,
            falling: true,
        }
    }
    pub const fn cell(self) -> Cell {
        Cell::from_parts(
            self.liquid.block(),
            if self.source {
                0
            } else if self.falling {
                8
            } else {
                8 - self.amount as u16
            },
        )
    }
}
pub fn fluid(cell: Cell) -> Option<Fluid> {
    let liquid = match cell.id() {
        WATER => Liquid::Water,
        LAVA => Liquid::Lava,
        _ => return None,
    };
    Some(match cell.state() & 15 {
        0 => Fluid::source(liquid),
        1..=7 => Fluid {
            liquid,
            amount: 8 - (cell.state() & 15) as u8,
            source: false,
            falling: false,
        },
        _ => Fluid::falling(liquid),
    })
}
pub fn solid(cell: Cell) -> bool {
    let boxes = collision_of(cell.raw());
    if boxes.is_empty() {
        return false;
    }
    let mut low = [f64::MAX; 3];
    let mut high = [f64::MIN; 3];
    for shape in boxes {
        for axis in 0..3 {
            low[axis] = low[axis].min(shape.lo[axis]);
            high[axis] = high[axis].max(shape.hi[axis]);
        }
    }
    let lengths = std::array::from_fn::<_, 3, _>(|axis| high[axis] - low[axis]);
    lengths.iter().sum::<f64>() / 3.0 >= 35.0 / 48.0 - 1e-9 || lengths[1] >= 1.0 - 1e-9
}
fn fillable(cell: Cell) -> bool {
    !solid(cell) && !matches!(bdef(cell.id()).shape, Shape::Cable | Shape::Large)
}
fn offset(position: [i32; 3], direction: usize) -> Result<[i32; 3], SurvivalError> {
    let mut out = position;
    for (axis, value) in out.iter_mut().enumerate() {
        *value = value
            .checked_add(DIRECTIONS[direction][axis])
            .ok_or(SurvivalError::UnknownInput)?;
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluidPlan {
    changes: [CellChange; 6],
    len: u8,
    pub reads: u16,
    pub water_washed: [bool; 6],
}
impl FluidPlan {
    pub fn changes(&self) -> &[CellChange] {
        &self.changes[..self.len as usize]
    }
}
struct Context<Q> {
    query: Q,
    origin: [i32; 3],
    cache: [Option<Cell>; CACHE_CELLS],
    plan: FluidPlan,
}
impl<Q: FnMut([i32; 3]) -> Option<Cell>> Context<Q> {
    fn read(&mut self, position: [i32; 3]) -> Result<Cell, SurvivalError> {
        let dx = i64::from(position[0]) - i64::from(self.origin[0]) + 6;
        let dy = i64::from(position[1]) - i64::from(self.origin[1]) + 2;
        let dz = i64::from(position[2]) - i64::from(self.origin[2]) + 6;
        if !(0..13).contains(&dx) || !(0..5).contains(&dy) || !(0..13).contains(&dz) {
            return Err(SurvivalError::Capacity);
        }
        let index = dx as usize + dz as usize * CACHE_EDGE + dy as usize * CACHE_EDGE * CACHE_EDGE;
        if let Some(value) = self.cache[index] {
            return Ok(value);
        }
        let value = (self.query)(position).ok_or(SurvivalError::UnknownInput)?;
        self.plan.reads += 1;
        self.cache[index] = Some(value);
        Ok(value)
    }
    fn write(
        &mut self,
        position: [i32; 3],
        after: Cell,
        washed: bool,
    ) -> Result<(), SurvivalError> {
        let before = self.read(position)?;
        if before == after {
            return Ok(());
        }
        if self.plan.len as usize == self.plan.changes.len() {
            return Err(SurvivalError::Capacity);
        }
        let index = self.plan.len as usize;
        self.plan.changes[index] = CellChange {
            position,
            before,
            after,
        };
        self.plan.water_washed[index] = washed;
        self.plan.len += 1;
        let dx = (i64::from(position[0]) - i64::from(self.origin[0]) + 6) as usize;
        let dy = (i64::from(position[1]) - i64::from(self.origin[1]) + 2) as usize;
        let dz = (i64::from(position[2]) - i64::from(self.origin[2]) + 6) as usize;
        self.cache[dx + dz * CACHE_EDGE + dy * CACHE_EDGE * CACHE_EDGE] = Some(after);
        Ok(())
    }
    fn at(&mut self, position: [i32; 3]) -> Result<Option<Fluid>, SurvivalError> {
        Ok(fluid(self.read(position)?))
    }
    fn next(&mut self, liquid: Liquid, position: [i32; 3]) -> Result<Option<Fluid>, SurvivalError> {
        let mut best = 0;
        let mut sources = 0;
        for direction in HORIZONTAL {
            if let Some(value) = self
                .at(offset(position, direction)?)?
                .filter(|value| value.liquid == liquid)
            {
                best = best.max(value.amount);
                sources += u8::from(value.source);
            }
        }
        if liquid == Liquid::Water && sources >= 2 {
            let below = self.read(offset(position, 2)?)?;
            if solid(below)
                || fluid(below).is_some_and(|value| value.liquid == liquid && value.source)
            {
                return Ok(Some(Fluid::source(liquid)));
            }
        }
        if self
            .at(offset(position, 3)?)?
            .is_some_and(|value| value.liquid == liquid)
        {
            return Ok(Some(Fluid::falling(liquid)));
        }
        let amount = best.saturating_sub(liquid.decrease());
        Ok((amount > 0).then_some(Fluid {
            liquid,
            amount,
            source: false,
            falling: false,
        }))
    }
    fn can_enter(
        &mut self,
        liquid: Liquid,
        position: [i32; 3],
        direction: usize,
    ) -> Result<bool, SurvivalError> {
        let cell = self.read(position)?;
        let replace = match fluid(cell) {
            None => true,
            Some(value) if value.liquid == Liquid::Water => {
                direction == 2 && liquid != Liquid::Water
            }
            Some(value) => value.amount >= 4 && liquid == Liquid::Water,
        };
        Ok(replace && fillable(cell))
    }
    fn through(&mut self, liquid: Liquid, position: [i32; 3]) -> Result<bool, SurvivalError> {
        let cell = self.read(position)?;
        Ok(
            !fluid(cell).is_some_and(|value| value.liquid == liquid && value.source)
                && fillable(cell),
        )
    }
    fn hole(&mut self, liquid: Liquid, position: [i32; 3]) -> Result<bool, SurvivalError> {
        let cell = self.read(offset(position, 2)?)?;
        Ok(fluid(cell).is_some_and(|value| value.liquid == liquid) || fillable(cell))
    }
    fn down_distance(
        &mut self,
        liquid: Liquid,
        position: [i32; 3],
        distance: u8,
        came: usize,
    ) -> Result<u16, SurvivalError> {
        let mut best = 1000;
        for direction in HORIZONTAL {
            if direction != came {
                let target = offset(position, direction)?;
                if self.through(liquid, target)? {
                    if self.hole(liquid, target)? {
                        return Ok(u16::from(distance));
                    }
                    if distance < liquid.search() {
                        best = best.min(self.down_distance(
                            liquid,
                            target,
                            distance + 1,
                            direction ^ 1,
                        )?);
                    }
                }
            }
        }
        Ok(best)
    }
    fn flow_into(
        &mut self,
        position: [i32; 3],
        direction: usize,
        value: Fluid,
    ) -> Result<(), SurvivalError> {
        let current = self.read(position)?;
        let after = if value.liquid == Liquid::Lava
            && direction == 2
            && fluid(current).is_some_and(|v| v.liquid == Liquid::Water)
        {
            Cell::from_parts(STONE, 0)
        } else {
            value.cell()
        };
        self.write(
            position,
            after,
            value.liquid == Liquid::Water && current.id() != AIR && fluid(current).is_none(),
        )
    }
    fn sides(&mut self, position: [i32; 3], value: Fluid) -> Result<(), SurvivalError> {
        let amount = if value.falling {
            7
        } else {
            value.amount.saturating_sub(value.liquid.decrease())
        };
        if amount == 0 {
            return Ok(());
        }
        let mut best = 1000;
        let mut candidates = [None; 6];
        for direction in HORIZONTAL {
            let target = offset(position, direction)?;
            if !self.through(value.liquid, target)? {
                continue;
            }
            let Some(next) = self.next(value.liquid, target)? else {
                continue;
            };
            let distance = if self.hole(value.liquid, target)? {
                0
            } else {
                self.down_distance(value.liquid, target, 1, direction ^ 1)?
            };
            if distance < best {
                candidates.fill(None);
                best = distance;
            }
            if distance <= best {
                candidates[direction] = Some(next);
            }
        }
        for direction in WRITE_ORDER {
            if let Some(next) = candidates[direction] {
                let target = offset(position, direction)?;
                if self.can_enter(value.liquid, target, direction)? {
                    self.flow_into(target, direction, next)?;
                }
            }
        }
        Ok(())
    }
}

/// Caller guards the read snapshot and authorizes *every* returned write before
/// committing once. World bounds/spawn-protected cells should be supplied as
/// known solid barriers by the query adapter, never silently writable air.
pub fn plan_tick(
    query: impl FnMut([i32; 3]) -> Option<Cell>,
    position: [i32; 3],
) -> Result<FluidPlan, SurvivalError> {
    let mut context = Context {
        query,
        origin: position,
        cache: [None; CACHE_CELLS],
        plan: FluidPlan {
            changes: [CellChange::default(); 6],
            len: 0,
            reads: 0,
            water_washed: [false; 6],
        },
    };
    let Some(mut value) = context.at(position)? else {
        return Ok(context.plan);
    };
    if value.liquid == Liquid::Lava {
        for direction in [3, 4, 5, 0, 1] {
            if context
                .at(offset(position, direction)?)?
                .is_some_and(|v| v.liquid == Liquid::Water)
            {
                context.write(
                    position,
                    Cell::from_parts(if value.source { OBSIDIAN } else { COBBLE }, 0),
                    false,
                )?;
                return Ok(context.plan);
            }
        }
    }
    if !value.source {
        let Some(next) = context.next(value.liquid, position)? else {
            context.write(position, Cell::AIR, false)?;
            return Ok(context.plan);
        };
        if next != value {
            value = next;
            context.write(position, value.cell(), false)?;
        }
    }
    let below = offset(position, 2)?;
    if let Some(next) = context.next(value.liquid, below)?
        && context.can_enter(value.liquid, below, 2)?
    {
        context.flow_into(below, 2, next)?;
        let mut sources = 0;
        for direction in HORIZONTAL {
            sources += u8::from(
                context
                    .at(offset(position, direction)?)?
                    .is_some_and(|v| v.liquid == value.liquid && v.source),
            );
        }
        if sources >= 3 {
            context.sides(position, value)?;
        }
    } else if value.source || !context.hole(value.liquid, position)? {
        context.sides(position, value)?;
    }
    Ok(context.plan)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardeningPlan {
    changes: [CellChange; 7],
    len: u8,
    pub reads: u16,
}
impl HardeningPlan {
    pub fn changes(&self) -> &[CellChange] {
        &self.changes[..self.len as usize]
    }
}
/// Immediate neighbor update after a proposed mutation, before scheduling fluid
/// delay. The query must expose the proposed changed cell. Commit these writes
/// with the triggering mutation; no lava waits 30 ticks to notice adjacent water.
pub fn plan_neighbor_hardening(
    mut query: impl FnMut([i32; 3]) -> Option<Cell>,
    position: [i32; 3],
) -> Result<HardeningPlan, SurvivalError> {
    let mut plan = HardeningPlan {
        changes: [CellChange::default(); 7],
        len: 0,
        reads: 0,
    };
    for target in
        std::iter::once(Ok(position)).chain((0..6).map(|direction| offset(position, direction)))
    {
        let target = target?;
        plan.reads += 1;
        let before = query(target).ok_or(SurvivalError::UnknownInput)?;
        let Some(value) = fluid(before).filter(|value| value.liquid == Liquid::Lava) else {
            continue;
        };
        for direction in [3, 4, 5, 0, 1] {
            plan.reads += 1;
            let neighbor = query(offset(target, direction)?).ok_or(SurvivalError::UnknownInput)?;
            if fluid(neighbor).is_some_and(|value| value.liquid == Liquid::Water) {
                plan.changes[plan.len as usize] = CellChange {
                    position: target,
                    before,
                    after: Cell::from_parts(if value.source { OBSIDIAN } else { COBBLE }, 0),
                };
                plan.len += 1;
                break;
            }
        }
    }
    Ok(plan)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "Commit cell change, inventory exchange and explicit spill together"]
pub struct BucketPlan {
    pub cell: CellChange,
    pub inventory: InventoryPlan,
    pub spill: ItemStack,
}
pub fn plan_bucket(
    inventory: &Inventory,
    position: [i32; 3],
    before: Cell,
) -> Result<BucketPlan, SurvivalError> {
    let held = inventory.hand();
    let ItemKind::Bucket { fluid: kind } = idef(held.item()).kind else {
        return Err(SurvivalError::NoEntitlement);
    };
    let (after, replacement) = if kind == 0 {
        let source = fluid(before)
            .filter(|value| value.source)
            .ok_or(SurvivalError::NoEntitlement)?;
        (
            Cell::AIR,
            if source.liquid == Liquid::Water {
                I_WATER_BUCKET
            } else {
                I_LAVA_BUCKET
            },
        )
    } else {
        if !fillable(before) {
            return Err(SurvivalError::NoEntitlement);
        }
        let liquid = if kind == crate::blocks::FLUID_WATER {
            Liquid::Water
        } else {
            Liquid::Lava
        };
        (Fluid::source(liquid).cell(), I_BUCKET)
    };
    let consume = inventory.plan_consume(inventory.selected(), held, 1)?;
    let mut result = *consume.after();
    let give = ItemStack::new(replacement, 1).map_err(|_| SurvivalError::InvalidStack)?;
    // ItemUsage.exchangeStack: last empty bucket keeps full bucket in hand;
    // a remaining empty stack keeps its slot and offers the full bucket normally.
    let spill = if result.hand().is_empty() {
        result.set_trusted(result.selected(), give)?;
        ItemStack::EMPTY
    } else {
        let pickup = result.plan_pickup(give)?;
        let spill = pickup.remainder();
        pickup.apply(&mut result)?;
        spill
    };
    Ok(BucketPlan {
        cell: CellChange {
            position,
            before,
            after,
        },
        inventory: inventory.plan_replace_trusted(result)?,
        spill,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScheduledFluid {
    pub due: u64,
    pub order: u64,
    pub position: [i32; 3],
    pub liquid: Liquid,
}
/// Count-bounded stable queue. No generated ocean cells are registered merely
/// because they are loaded. Saturation is explicit backpressure to the owner.
#[derive(Debug)]
pub struct FluidQueue {
    heap: BinaryHeap<Reverse<ScheduledFluid>>,
    keys: BTreeSet<([i32; 3], Liquid)>,
    capacity: usize,
    next_order: u64,
}
impl FluidQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity),
            keys: BTreeSet::new(),
            capacity,
            next_order: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.heap.len()
    }
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
    /// Atomic queue-side wake reservation after one accepted cell write. Existing
    /// entries retain their deadline and order. Callers preflight world edits and
    /// queue capacity together; a full queue never silently loses a frontier.
    pub fn wake(
        &mut self,
        now: u64,
        position: [i32; 3],
        mut query: impl FnMut([i32; 3]) -> Option<Cell>,
    ) -> Result<usize, SurvivalError> {
        let mut candidates = [None; 7];
        for (index, target) in std::iter::once(Ok(position))
            .chain((0..6).map(|direction| offset(position, direction)))
            .enumerate()
        {
            let target = target?;
            let value = query(target).ok_or(SurvivalError::UnknownInput)?;
            if let Some(value) = fluid(value)
                && !self.keys.contains(&(target, value.liquid))
            {
                candidates[index] = Some((target, value.liquid));
            }
        }
        let count = candidates.iter().flatten().count();
        if self.len().saturating_add(count) > self.capacity {
            return Err(SurvivalError::Capacity);
        }
        now.checked_add(30).ok_or(SurvivalError::RevisionOverflow)?;
        self.next_order
            .checked_add(count as u64)
            .ok_or(SurvivalError::RevisionOverflow)?;
        for (target, liquid) in candidates.into_iter().flatten() {
            self.schedule(now, target, liquid)?;
        }
        Ok(count)
    }
    /// Keep the original overdue deadline when an event cannot yet read its
    /// complete known snapshot. Call before admitting replacement events.
    pub fn retry(&mut self, event: ScheduledFluid) -> Result<(), SurvivalError> {
        if self.heap.len() >= self.capacity {
            return Err(SurvivalError::Capacity);
        }
        if event.order >= self.next_order || !self.keys.insert((event.position, event.liquid)) {
            return Err(SurvivalError::InvalidEncoding);
        }
        self.heap.push(Reverse(event));
        Ok(())
    }
    pub fn schedule(
        &mut self,
        now: u64,
        position: [i32; 3],
        liquid: Liquid,
    ) -> Result<bool, SurvivalError> {
        let key = (position, liquid);
        if self.keys.contains(&key) {
            return Ok(false);
        }
        if self.heap.len() >= self.capacity {
            return Err(SurvivalError::Capacity);
        }
        let due = now
            .checked_add(liquid.delay())
            .ok_or(SurvivalError::RevisionOverflow)?;
        let order = self.next_order;
        self.next_order = self
            .next_order
            .checked_add(1)
            .ok_or(SurvivalError::RevisionOverflow)?;
        self.keys.insert(key);
        self.heap.push(Reverse(ScheduledFluid {
            due,
            order,
            position,
            liquid,
        }));
        Ok(true)
    }
    pub fn pop_due(&mut self, now: u64) -> Option<ScheduledFluid> {
        if self.heap.peek().is_none_or(|event| event.0.due > now) {
            return None;
        }
        let event = self.heap.pop()?.0;
        self.keys.remove(&(event.position, event.liquid));
        Some(event)
    }
    pub fn oldest_overdue(&self, now: u64) -> u64 {
        self.heap
            .peek()
            .map_or(0, |event| now.saturating_sub(event.0.due))
    }
    pub fn snapshot(&self) -> Vec<ScheduledFluid> {
        let mut events: Vec<_> = self.heap.iter().map(|event| event.0).collect();
        events.sort_unstable();
        events
    }
    pub fn restore(
        capacity: usize,
        next_order: u64,
        events: &[ScheduledFluid],
    ) -> Result<Self, SurvivalError> {
        if events.len() > capacity {
            return Err(SurvivalError::Capacity);
        }
        let mut queue = Self::new(capacity);
        queue.next_order = next_order;
        for &event in events {
            if event.order >= next_order || !queue.keys.insert((event.position, event.liquid)) {
                return Err(SurvivalError::InvalidEncoding);
            }
            queue.heap.push(Reverse(event));
        }
        Ok(queue)
    }
    pub const fn next_order(&self) -> u64 {
        self.next_order
    }
    /// Checkpoint serialization is outside the tick hot path. Fixed-size events
    /// are emitted by (due,order), so byte output is deterministic.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16 + self.len() * 29);
        bytes.extend_from_slice(b"FLQ1");
        bytes.extend_from_slice(&self.next_order.to_le_bytes());
        bytes.extend_from_slice(&(self.len() as u32).to_le_bytes());
        for event in self.snapshot() {
            bytes.extend_from_slice(&event.due.to_le_bytes());
            bytes.extend_from_slice(&event.order.to_le_bytes());
            for value in event.position {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes.push(match event.liquid {
                Liquid::Water => 0,
                Liquid::Lava => 1,
            });
        }
        bytes
    }
    pub fn decode(bytes: &[u8], capacity: usize) -> Result<Self, SurvivalError> {
        if bytes.len() < 16 || &bytes[..4] != b"FLQ1" {
            return Err(SurvivalError::InvalidEncoding);
        }
        let count = u32::from_le_bytes(
            bytes[12..16]
                .try_into()
                .map_err(|_| SurvivalError::InvalidEncoding)?,
        ) as usize;
        if count > capacity {
            return Err(SurvivalError::Capacity);
        }
        if count
            .checked_mul(29)
            .and_then(|value| value.checked_add(16))
            != Some(bytes.len())
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let next_order = u64::from_le_bytes(
            bytes[4..12]
                .try_into()
                .map_err(|_| SurvivalError::InvalidEncoding)?,
        );
        let mut events = Vec::with_capacity(count);
        let mut orders = BTreeSet::new();
        for raw in bytes[16..].chunks_exact(29) {
            let due = u64::from_le_bytes(
                raw[..8]
                    .try_into()
                    .map_err(|_| SurvivalError::InvalidEncoding)?,
            );
            let order = u64::from_le_bytes(
                raw[8..16]
                    .try_into()
                    .map_err(|_| SurvivalError::InvalidEncoding)?,
            );
            if !orders.insert(order) {
                return Err(SurvivalError::InvalidEncoding);
            }
            let mut position = [0; 3];
            for (value, part) in position.iter_mut().zip(raw[16..28].chunks_exact(4)) {
                *value = i32::from_le_bytes(
                    part.try_into()
                        .map_err(|_| SurvivalError::InvalidEncoding)?,
                );
            }
            let liquid = match raw[28] {
                0 => Liquid::Water,
                1 => Liquid::Lava,
                _ => return Err(SurvivalError::InvalidEncoding),
            };
            events.push(ScheduledFluid {
                due,
                order,
                position,
                liquid,
            });
        }
        Self::restore(capacity, next_order, &events)
    }
}
