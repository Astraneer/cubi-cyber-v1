//! Incremental game light, RGB and skylight. All stored samples are 0..=15.
//!
//! Missing neighbors never generate pages. In particular, a missing page above
//! is dark unless [`SkyAccess`] explicitly certifies a sky boundary. Local
//! attenuation is positive except direct level-15 skylight moving downward in
//! transparent air. Every graph cycle therefore has positive attenuation: local
//! relaxation converges after removal as well as addition, without a world scan.
//!
//! This owner-thread solver reads current cells and publishes one bounded batch
//! through `Transaction::set_light`; it does not build or invalidate geometry.

use crate::{
    CellRead, CommitError, CursorError, CursorId, LightSample, Transaction, World, WorldChange,
};
use core_types::{BlockPos, ChannelMask, LocalIdx, PAGE_VOLUME, PageId, SpaceId, Tick};
use std::{collections::BTreeMap, sync::Arc};

pub const LIGHT_INPUT_CHANNELS: ChannelMask =
    ChannelMask(ChannelMask::OPACITY.0 | ChannelMask::EMISSION.0);

/// Certifies light arriving from an unknown cell directly above this known cell.
/// The callback must describe an immutable boundary for a whole `step` call.
pub trait SkyAccess {
    fn incoming_from_unknown_above(&self, known_cell: BlockPos) -> Option<u8>;
}
impl<F: Fn(BlockPos) -> Option<u8>> SkyAccess for F {
    fn incoming_from_unknown_above(&self, position: BlockPos) -> Option<u8> {
        self(position)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LightPriority {
    Interactive,
    Normal,
    Background,
}
impl LightPriority {
    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LightingLimits {
    pub max_frontier_cells: usize,
    pub reserved_interactive_cells: usize,
    pub max_seed_pages: usize,
    pub max_queue_age_ticks: u64,
}
impl Default for LightingLimits {
    fn default() -> Self {
        Self {
            max_frontier_cells: 65_536,
            reserved_interactive_cells: 4096,
            max_seed_pages: 64,
            max_queue_age_ticks: 40,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LightBudget {
    pub max_cells: u64,
    pub max_neighbor_reads: u64,
    pub max_seed_cells: u64,
    pub max_updates: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct LightIngestBudget {
    pub max_records: usize,
    pub max_entries: u64,
    pub max_seeds: u64,
}
impl Default for LightIngestBudget {
    fn default() -> Self {
        Self {
            max_records: 8,
            max_entries: 1024,
            max_seeds: 1024,
        }
    }
}
impl Default for LightBudget {
    fn default() -> Self {
        Self {
            max_cells: 1024,
            max_neighbor_reads: 6144,
            max_seed_cells: 1024,
            max_updates: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightWork {
    pub cells_examined: u64,
    pub neighbor_reads: u64,
    pub registry_queries: u64,
    pub unknown_neighbor_reads: u64,
    pub seed_cells_examined: u64,
    pub light_updates: u64,
    pub transactions: u64,
    pub journal_records: u64,
    pub journal_entries_examined: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LightProgress {
    pub work: LightWork,
    pub pending_cells: usize,
    pub pending_seed_pages: usize,
    pub backpressure: bool,
    pub oldest_age_ticks: u64,
    pub overdue: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightingBackpressure {
    FrontierFull,
    SeedPagesFull,
}

#[derive(Debug)]
pub enum LightingError {
    InvalidLimits,
    TickWentBackwards,
    CoordinateOverflow(PageId),
    Cursor(CursorError),
    Commit(CommitError),
}
impl From<CursorError> for LightingError {
    fn from(error: CursorError) -> Self {
        Self::Cursor(error)
    }
}
impl From<CommitError> for LightingError {
    fn from(error: CommitError) -> Self {
        Self::Commit(error)
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    position: BlockPos,
    priority: LightPriority,
    queued_at: Tick,
    previous: Option<usize>,
    next: Option<usize>,
}
#[derive(Clone, Copy, Debug, Default)]
struct Chain {
    first: Option<usize>,
    last: Option<usize>,
}
enum Undo {
    Inserted(usize),
    Removed(usize, Entry),
    Changed(usize, Entry),
}

/// Intrusive FIFO lists avoid an O(frontier length) priority search per cell and
/// cannot accumulate stale queue tokens when a pending cell is promoted.
struct Frontier {
    entries: Vec<Option<Entry>>,
    free: Vec<usize>,
    positions: BTreeMap<BlockPos, usize>,
    ages: BTreeMap<Tick, usize>,
    chains: [Chain; 3],
    noninteractive: usize,
}
impl Frontier {
    fn new(capacity: usize) -> Self {
        Self {
            entries: vec![None; capacity],
            free: (0..capacity).rev().collect(),
            positions: BTreeMap::new(),
            ages: BTreeMap::new(),
            chains: [Chain::default(); 3],
            noninteractive: 0,
        }
    }
    fn len(&self) -> usize {
        self.positions.len()
    }
    fn can_enqueue(
        &self,
        position: BlockPos,
        priority: LightPriority,
        limits: LightingLimits,
    ) -> bool {
        self.positions.contains_key(&position)
            || (self.len() < limits.max_frontier_cells
                && (priority == LightPriority::Interactive
                    || self.noninteractive
                        < limits.max_frontier_cells - limits.reserved_interactive_cells))
    }
    fn insert(
        &mut self,
        position: BlockPos,
        priority: LightPriority,
        queued_at: Tick,
        undo: Option<&mut Vec<Undo>>,
    ) {
        if let Some(&index) = self.positions.get(&position) {
            let old = self.entries[index].expect("indexed cell");
            if priority < old.priority || queued_at < old.queued_at {
                if let Some(undo) = undo {
                    undo.push(Undo::Changed(index, old));
                }
                if priority < old.priority {
                    self.detach(index);
                    if old.priority != LightPriority::Interactive
                        && priority == LightPriority::Interactive
                    {
                        self.noninteractive -= 1;
                    }
                    self.entries[index].as_mut().expect("indexed cell").priority = priority;
                    self.append(index);
                }
                self.remove_age(old.queued_at);
                self.add_age(Tick(old.queued_at.0.min(queued_at.0)));
                self.entries[index]
                    .as_mut()
                    .expect("indexed cell")
                    .queued_at = Tick(old.queued_at.0.min(queued_at.0));
            }
            return;
        }
        let index = self.free.pop().expect("capacity prechecked");
        self.entries[index] = Some(Entry {
            position,
            priority,
            queued_at,
            previous: None,
            next: None,
        });
        self.positions.insert(position, index);
        self.add_age(queued_at);
        if priority != LightPriority::Interactive {
            self.noninteractive += 1;
        }
        self.append(index);
        if let Some(undo) = undo {
            undo.push(Undo::Inserted(index));
        }
    }
    fn append(&mut self, index: usize) {
        let priority = self.entries[index].expect("active cell").priority.index();
        let previous = self.chains[priority].last;
        if let Some(previous) = previous {
            self.entries[previous].as_mut().expect("previous cell").next = Some(index);
        } else {
            self.chains[priority].first = Some(index);
        }
        let entry = self.entries[index].as_mut().expect("active cell");
        entry.previous = previous;
        entry.next = None;
        self.chains[priority].last = Some(index);
    }
    fn detach(&mut self, index: usize) {
        let entry = self.entries[index].expect("active cell");
        let chain = &mut self.chains[entry.priority.index()];
        if let Some(previous) = entry.previous {
            self.entries[previous].as_mut().expect("previous cell").next = entry.next;
        } else {
            chain.first = entry.next;
        }
        if let Some(next) = entry.next {
            self.entries[next].as_mut().expect("next cell").previous = entry.previous;
        } else {
            chain.last = entry.previous;
        }
    }
    fn remove(&mut self, index: usize, undo: Option<&mut Vec<Undo>>) -> Entry {
        let entry = self.entries[index].expect("active cell");
        self.detach(index);
        self.entries[index] = None;
        self.positions.remove(&entry.position);
        self.remove_age(entry.queued_at);
        self.free.push(index);
        if entry.priority != LightPriority::Interactive {
            self.noninteractive -= 1;
        }
        if let Some(undo) = undo {
            undo.push(Undo::Removed(index, entry));
        }
        entry
    }
    fn restore_entry(&mut self, index: usize, entry: Entry) {
        self.entries[index] = Some(entry);
        self.positions.insert(entry.position, index);
        self.add_age(entry.queued_at);
        if let Some(previous) = entry.previous {
            self.entries[previous].as_mut().expect("previous cell").next = Some(index);
        } else {
            self.chains[entry.priority.index()].first = Some(index);
        }
        if let Some(next) = entry.next {
            self.entries[next].as_mut().expect("next cell").previous = Some(index);
        } else {
            self.chains[entry.priority.index()].last = Some(index);
        }
    }
    fn rollback(&mut self, mut undo: Vec<Undo>) {
        while let Some(operation) = undo.pop() {
            match operation {
                Undo::Inserted(index) => {
                    let _ = self.remove(index, None);
                }
                Undo::Removed(index, entry) => {
                    assert_eq!(self.free.pop(), Some(index));
                    if entry.priority != LightPriority::Interactive {
                        self.noninteractive += 1;
                    }
                    self.restore_entry(index, entry);
                }
                Undo::Changed(index, entry) => {
                    let current = self.entries[index].expect("changed cell");
                    self.detach(index);
                    self.remove_age(current.queued_at);
                    if current.priority == LightPriority::Interactive
                        && entry.priority != LightPriority::Interactive
                    {
                        self.noninteractive += 1;
                    }
                    self.restore_entry(index, entry);
                }
            }
        }
    }
    fn next(&self, now: Tick, max_age: u64, space: Option<SpaceId>) -> Option<usize> {
        self.chains
            .iter()
            .filter_map(|chain| chain.first)
            .filter(|index| {
                space
                    .is_none_or(|space| self.entries[*index].expect("head").position.space == space)
            })
            .min_by_key(|index| {
                let entry = self.entries[*index].expect("head");
                (
                    if now.0.saturating_sub(entry.queued_at.0) >= max_age / 2 {
                        0
                    } else {
                        entry.priority.index() + 1
                    },
                    entry.queued_at,
                    *index,
                )
            })
    }
    fn oldest_age(&self, now: Tick) -> u64 {
        self.ages
            .first_key_value()
            .map_or(0, |(tick, _)| now.0.saturating_sub(tick.0))
    }
    fn add_age(&mut self, tick: Tick) {
        *self.ages.entry(tick).or_default() += 1;
    }
    fn remove_age(&mut self, tick: Tick) {
        let count = self.ages.get_mut(&tick).expect("queued age");
        *count -= 1;
        if *count == 0 {
            self.ages.remove(&tick);
        }
    }
    fn grow(&mut self, capacity: usize) {
        let previous = self.entries.len();
        if capacity > previous {
            self.entries.resize(capacity, None);
            self.free.extend((previous..capacity).rev());
        }
    }
}

#[derive(Clone, Copy)]
struct PageSeed {
    page: PageId,
    whole: bool,
    next: usize,
    queued_at: Tick,
    priority: LightPriority,
}
struct PendingRecord {
    change: Arc<WorldChange>,
    next: u64,
    page: usize,
    cell: usize,
    neighbor: usize,
    interested: Option<bool>,
    queued_at: Tick,
}

pub struct LightingEngine {
    cursor: CursorId,
    next_sequence: u64,
    limits: LightingLimits,
    frontier: Frontier,
    seeds: BTreeMap<PageId, PageSeed>,
    record: Option<PendingRecord>,
}

impl LightingEngine {
    pub fn attach(world: &mut World, limits: LightingLimits) -> Result<Self, LightingError> {
        validate_limits(limits)?;
        Ok(Self {
            cursor: world.register_cursor(),
            next_sequence: world.journal_next(),
            limits,
            frontier: Frontier::new(limits.max_frontier_cells),
            seeds: BTreeMap::new(),
            record: None,
        })
    }
    pub fn cursor(&self) -> CursorId {
        self.cursor
    }
    pub fn detach(self, world: &mut World) -> bool {
        world.remove_cursor(self.cursor)
    }
    pub fn pending_cells(&self) -> usize {
        self.frontier.len()
    }
    pub fn pending_seed_pages(&self) -> usize {
        self.seeds.len()
    }
    pub fn is_idle(&self) -> bool {
        self.frontier.len() == 0 && self.seeds.is_empty() && self.record.is_none()
    }
    pub fn set_limits(&mut self, limits: LightingLimits) -> Result<(), LightingError> {
        validate_limits(limits)?;
        if limits.max_frontier_cells < self.frontier.len()
            || limits.max_seed_pages < self.seeds.len()
            || limits.max_frontier_cells - limits.reserved_interactive_cells
                < self.frontier.noninteractive
        {
            return Err(LightingError::InvalidLimits);
        }
        self.frontier.grow(limits.max_frontier_cells);
        self.limits = limits;
        Ok(())
    }
    /// Explicit bootstrap or changed sky boundary, never a scan of all loaded pages.
    pub fn queue_page(
        &mut self,
        page: PageId,
        tick: Tick,
        priority: LightPriority,
    ) -> Result<(), LightingBackpressure> {
        self.queue_page_kind(page, true, tick, priority)
    }
    pub fn queue_cell(
        &mut self,
        position: BlockPos,
        tick: Tick,
        priority: LightPriority,
    ) -> Result<(), LightingBackpressure> {
        if !self.frontier.can_enqueue(position, priority, self.limits) {
            return Err(LightingBackpressure::FrontierFull);
        }
        self.frontier.insert(position, priority, tick, None);
        Ok(())
    }
    /// Immediate near-edit invalidation, including neighbors when opacity changes
    /// but the edited cell's own light value happens to stay unchanged.
    pub fn invalidate_cell(
        &mut self,
        world: &World,
        position: BlockPos,
        tick: Tick,
        priority: LightPriority,
    ) -> Result<(), LightingBackpressure> {
        let mut positions = vec![position];
        positions.extend((0..6).filter_map(|direction| neighbor(position, direction)));
        positions.retain(|position| world.page(position.page()).is_some());
        if !self.can_add_positions(&positions, priority, None) {
            return Err(LightingBackpressure::FrontierFull);
        }
        for position in positions {
            self.frontier.insert(position, priority, tick, None);
        }
        Ok(())
    }

    /// Consume only opacity/emission changes. Journal acknowledgement occurs only
    /// after all its seeds have a retained representation in this engine.
    pub fn ingest(
        &mut self,
        world: &mut World,
        tick: Tick,
        budget: LightIngestBudget,
    ) -> Result<LightProgress, LightingError> {
        let mut work = LightWork::default();
        let mut processed = 0;
        let mut backpressure = false;
        while processed < budget.max_records {
            if self.record.is_none() && self.next_sequence == world.journal_next() {
                break;
            }
            let mut record = if let Some(record) = self.record.take() {
                record
            } else {
                let batch = world.read_cursor(self.cursor, 1)?;
                let Some(change) = batch.changes.into_iter().next() else {
                    break;
                };
                work.journal_records += 1;
                PendingRecord {
                    change,
                    next: batch.next,
                    page: 0,
                    cell: 0,
                    neighbor: 0,
                    interested: None,
                    queued_at: tick,
                }
            };
            let result = self.ingest_record(world, &mut record, budget, &mut work);
            match result {
                Ok(RecordStatus::Done) => {
                    // On an explicit cursor lag the record stays retained; callers
                    // install/reseed a snapshot before seeking the cursor again.
                    if let Err(error) = world.acknowledge_cursor(self.cursor, record.next) {
                        self.record = Some(record);
                        return Err(error.into());
                    }
                    self.next_sequence = record.next;
                    processed += 1;
                }
                Ok(status) => {
                    backpressure = matches!(status, RecordStatus::Backpressure);
                    self.record = Some(record);
                    break;
                }
                Err(error) => {
                    self.record = Some(record);
                    return Err(error);
                }
            }
        }
        Ok(self.progress(tick, work, backpressure))
    }

    /// Explicit recovery after the owner has queued a complete current snapshot.
    /// The owner must not skip intervening opacity/emission edits during recovery.
    pub fn recover_cursor(
        &mut self,
        world: &mut World,
        sequence: u64,
    ) -> Result<(), LightingError> {
        world.recover_cursor(self.cursor, sequence)?;
        self.next_sequence = sequence;
        self.record = None;
        Ok(())
    }

    fn ingest_record(
        &mut self,
        world: &World,
        record: &mut PendingRecord,
        budget: LightIngestBudget,
        work: &mut LightWork,
    ) -> Result<RecordStatus, LightingError> {
        if !record.change.channels.intersects(LIGHT_INPUT_CHANNELS) {
            return Ok(RecordStatus::Done);
        }
        while record.page < record.change.pages.len() {
            if work.journal_entries_examined >= budget.max_entries {
                return Ok(RecordStatus::Budget);
            }
            let page = &record.change.pages[record.page];
            work.journal_entries_examined += 1;
            if page.created || page.removed {
                page_origin(page.page)?;
                if self
                    .queue_page_kind(
                        page.page,
                        page.created,
                        record.queued_at,
                        LightPriority::Background,
                    )
                    .is_err()
                {
                    return Ok(RecordStatus::Backpressure);
                }
            }
            record.page += 1;
        }
        while record.cell < record.change.cells.len() {
            let cell = &record.change.cells[record.cell];
            if record.interested.is_none() {
                if work.journal_entries_examined >= budget.max_entries {
                    return Ok(RecordStatus::Budget);
                }
                let old = world.registry.properties(cell.old);
                let new = world.registry.properties(cell.new);
                work.registry_queries += 2;
                work.journal_entries_examined += 1;
                record.interested =
                    Some(old.changed_channels(new).intersects(LIGHT_INPUT_CHANNELS));
            }
            if record.interested == Some(true) {
                let position = cell_position(cell.page, cell.index)?;
                while record.neighbor < 7 {
                    if work.seed_cells_examined >= budget.max_seeds {
                        return Ok(RecordStatus::Budget);
                    }
                    let target = if record.neighbor == 0 {
                        Some(position)
                    } else {
                        neighbor(position, record.neighbor - 1)
                    };
                    work.seed_cells_examined += 1;
                    if let Some(target) = target
                        && world.page(target.page()).is_some()
                        && self
                            .queue_cell(target, record.queued_at, LightPriority::Interactive)
                            .is_err()
                    {
                        return Ok(RecordStatus::Backpressure);
                    }
                    record.neighbor += 1;
                }
            }
            record.cell += 1;
            record.neighbor = 0;
            record.interested = None;
        }
        Ok(RecordStatus::Done)
    }

    /// Advance a bounded deterministic frontier. If admission cannot retain every
    /// affected neighbor the cell remains queued; increase capacity or relieve
    /// pressure before retrying. No dropped edge is disguised as convergence.
    pub fn step(
        &mut self,
        world: &mut World,
        tick: Tick,
        budget: LightBudget,
        sky: &impl SkyAccess,
    ) -> Result<LightProgress, LightingError> {
        if tick < world.tick() {
            return Err(LightingError::TickWentBackwards);
        }
        let mut work = LightWork::default();
        let mut backpressure = self.pump_seeds(world, budget.max_seed_cells, &mut work)?;
        let mut overlay = BTreeMap::<BlockPos, LightSample>::new();
        let mut undo = Vec::new();
        let mut active_space = None;
        while work.cells_examined < budget.max_cells {
            let Some(index) =
                self.frontier
                    .next(tick, self.limits.max_queue_age_ticks, active_space)
            else {
                break;
            };
            let entry = self.frontier.entries[index].expect("queued cell");
            active_space = Some(entry.position.space);
            work.cells_examined += 1;
            let CellRead::Known(cell) = world.read(entry.position) else {
                self.frontier.remove(index, Some(&mut undo));
                continue;
            };
            if budget
                .max_neighbor_reads
                .saturating_sub(work.neighbor_reads)
                < 6
            {
                break;
            }
            let current = sample(world, &overlay, entry.position).expect("known cell sample");
            let properties = world.registry.properties(cell);
            work.registry_queries += 1;
            let opacity = properties.light_opacity.min(15);
            let attenuation = opacity.max(1);
            let rgb = properties.emission.map(|channel| channel.min(15));
            let mut next = LightSample {
                sky: 0,
                block: *rgb.iter().max().expect("RGB"),
                rgb,
            };
            let mut neighbors = [entry.position; 6];
            let mut neighbor_count = 0;
            for direction in 0..6 {
                work.neighbor_reads += 1;
                let position = neighbor(entry.position, direction);
                let incoming = position.and_then(|position| sample(world, &overlay, position));
                if let Some(incoming) = incoming {
                    neighbors[neighbor_count] = position.expect("known neighbor");
                    neighbor_count += 1;
                    next.block = next
                        .block
                        .max(incoming.block.min(15).saturating_sub(attenuation));
                    for channel in 0..3 {
                        next.rgb[channel] = next.rgb[channel]
                            .max(incoming.rgb[channel].min(15).saturating_sub(attenuation));
                    }
                    let direct = direction == 3 && incoming.sky == 15 && opacity == 0;
                    next.sky = next.sky.max(if direct {
                        15
                    } else {
                        incoming.sky.min(15).saturating_sub(attenuation)
                    });
                } else {
                    work.unknown_neighbor_reads += 1;
                    if direction == 3
                        && let Some(incoming) = sky.incoming_from_unknown_above(entry.position)
                    {
                        let incoming = incoming.min(15);
                        next.sky = next.sky.max(if incoming == 15 && opacity == 0 {
                            15
                        } else {
                            incoming.saturating_sub(attenuation)
                        });
                    }
                }
            }
            if next != current {
                if work.light_updates >= budget.max_updates {
                    break;
                }
                if !self.can_add_positions(
                    &neighbors[..neighbor_count],
                    entry.priority,
                    Some(index),
                ) {
                    backpressure = true;
                    break;
                }
                overlay.insert(entry.position, next);
                work.light_updates += 1;
                self.frontier.remove(index, Some(&mut undo));
                for position in neighbors[..neighbor_count].iter().copied() {
                    self.frontier.insert(
                        position,
                        entry.priority,
                        entry.queued_at,
                        Some(&mut undo),
                    );
                }
            } else {
                self.frontier.remove(index, Some(&mut undo));
            }
        }
        if let Some(space) = active_space {
            let mut transaction = Transaction::new(tick, space);
            let mut changed = false;
            for (position, light) in &overlay {
                let before = world
                    .page(position.page())
                    .expect("known overlay page")
                    .light(position.local_index());
                if *light != before {
                    transaction.expect_page(position.page(), world.page_version(position.page()));
                    transaction.set_light(*position, *light);
                    changed = true;
                }
            }
            if changed {
                if let Err(error) = world.commit(transaction) {
                    self.frontier.rollback(undo);
                    return Err(error.into());
                }
                work.transactions = 1;
            }
        }
        Ok(self.progress(tick, work, backpressure))
    }

    fn pump_seeds(
        &mut self,
        world: &World,
        budget: u64,
        work: &mut LightWork,
    ) -> Result<bool, LightingError> {
        while work.seed_cells_examined < budget {
            let Some(key) = self
                .seeds
                .values()
                .min_by_key(|seed| (seed.priority, seed.queued_at, seed.page))
                .map(|seed| seed.page)
            else {
                break;
            };
            let mut seed = self.seeds.remove(&key).expect("selected page seed");
            while work.seed_cells_examined < budget {
                let position = match page_seed_position(seed) {
                    Ok(SeedPosition::End) => break,
                    Ok(SeedPosition::Outside) => {
                        seed.next += 1;
                        work.seed_cells_examined += 1;
                        continue;
                    }
                    Ok(SeedPosition::Cell(position)) => position,
                    Err(error) => {
                        self.seeds.insert(key, seed);
                        return Err(error);
                    }
                };
                work.seed_cells_examined += 1;
                if world.page(position.page()).is_some()
                    && self
                        .queue_cell(position, seed.queued_at, seed.priority)
                        .is_err()
                {
                    self.seeds.insert(key, seed);
                    return Ok(true);
                }
                seed.next += 1;
            }
            if !matches!(page_seed_position(seed), Ok(SeedPosition::End)) {
                self.seeds.insert(key, seed);
                break;
            }
        }
        Ok(false)
    }

    fn can_add_positions(
        &self,
        positions: &[BlockPos],
        priority: LightPriority,
        remove: Option<usize>,
    ) -> bool {
        let missing = positions
            .iter()
            .filter(|position| !self.frontier.positions.contains_key(position))
            .count();
        let normal_removed = usize::from(remove.is_some_and(|index| {
            self.frontier.entries[index].expect("removed cell").priority
                != LightPriority::Interactive
        }));
        self.frontier.len() - usize::from(remove.is_some()) + missing
            <= self.limits.max_frontier_cells
            && (priority == LightPriority::Interactive
                || self.frontier.noninteractive - normal_removed + missing
                    <= self.limits.max_frontier_cells - self.limits.reserved_interactive_cells)
    }

    fn progress(&self, tick: Tick, work: LightWork, backpressure: bool) -> LightProgress {
        let age = self
            .frontier
            .oldest_age(tick)
            .max(
                self.seeds
                    .values()
                    .map(|seed| tick.0.saturating_sub(seed.queued_at.0))
                    .max()
                    .unwrap_or(0),
            )
            .max(
                self.record
                    .as_ref()
                    .map_or(0, |record| tick.0.saturating_sub(record.queued_at.0)),
            );
        LightProgress {
            work,
            pending_cells: self.frontier.len(),
            pending_seed_pages: self.seeds.len(),
            backpressure,
            oldest_age_ticks: age,
            overdue: age >= self.limits.max_queue_age_ticks,
        }
    }
    fn queue_page_kind(
        &mut self,
        page: PageId,
        whole: bool,
        tick: Tick,
        priority: LightPriority,
    ) -> Result<(), LightingBackpressure> {
        if let Some(seed) = self.seeds.get_mut(&page) {
            seed.whole |= whole;
            seed.next = 0;
            seed.queued_at = Tick(seed.queued_at.0.min(tick.0));
            seed.priority = seed.priority.min(priority);
            return Ok(());
        }
        if self.seeds.len() >= self.limits.max_seed_pages {
            return Err(LightingBackpressure::SeedPagesFull);
        }
        self.seeds.insert(
            page,
            PageSeed {
                page,
                whole,
                next: 0,
                queued_at: tick,
                priority,
            },
        );
        Ok(())
    }
}

enum RecordStatus {
    Done,
    Budget,
    Backpressure,
}

fn sample(
    world: &World,
    overlay: &BTreeMap<BlockPos, LightSample>,
    position: BlockPos,
) -> Option<LightSample> {
    if let Some(sample) = overlay.get(&position) {
        return Some(*sample);
    }
    world
        .page(position.page())
        .map(|page| page.light(position.local_index()))
}

fn neighbor(position: BlockPos, direction: usize) -> Option<BlockPos> {
    let [dx, dy, dz] = [
        [-1, 0, 0],
        [1, 0, 0],
        [0, -1, 0],
        [0, 1, 0],
        [0, 0, -1],
        [0, 0, 1],
    ][direction];
    Some(BlockPos {
        space: position.space,
        x: position.x.checked_add(dx)?,
        y: position.y.checked_add(dy)?,
        z: position.z.checked_add(dz)?,
    })
}

fn page_origin(page: PageId) -> Result<BlockPos, LightingError> {
    Ok(BlockPos {
        space: page.space,
        x: page
            .x
            .checked_mul(16)
            .ok_or(LightingError::CoordinateOverflow(page))?,
        y: page
            .y
            .checked_mul(16)
            .ok_or(LightingError::CoordinateOverflow(page))?,
        z: page
            .z
            .checked_mul(16)
            .ok_or(LightingError::CoordinateOverflow(page))?,
    })
}
fn cell_position(page: PageId, index: LocalIdx) -> Result<BlockPos, LightingError> {
    let origin = page_origin(page)?;
    let (x, y, z) = index.xyz();
    Ok(BlockPos {
        space: page.space,
        x: origin
            .x
            .checked_add(i32::from(x))
            .ok_or(LightingError::CoordinateOverflow(page))?,
        y: origin
            .y
            .checked_add(i32::from(y))
            .ok_or(LightingError::CoordinateOverflow(page))?,
        z: origin
            .z
            .checked_add(i32::from(z))
            .ok_or(LightingError::CoordinateOverflow(page))?,
    })
}
enum SeedPosition {
    End,
    Outside,
    Cell(BlockPos),
}
fn page_seed_position(seed: PageSeed) -> Result<SeedPosition, LightingError> {
    let own = if seed.whole { PAGE_VOLUME } else { 0 };
    if seed.next < own {
        return cell_position(
            seed.page,
            LocalIdx::new(seed.next as u16).expect("page range"),
        )
        .map(SeedPosition::Cell);
    }
    let face_index = seed.next - own;
    if face_index >= 6 * 256 {
        return Ok(SeedPosition::End);
    }
    let face = face_index / 256;
    let a = (face_index % 16) as i32;
    let b = ((face_index % 256) / 16) as i32;
    let [x, y, z] = match face {
        0 => [-1, a, b],
        1 => [16, a, b],
        2 => [a, -1, b],
        3 => [a, 16, b],
        4 => [a, b, -1],
        _ => [a, b, 16],
    };
    let origin = page_origin(seed.page)?;
    Ok(origin
        .x
        .checked_add(x)
        .zip(origin.y.checked_add(y))
        .zip(origin.z.checked_add(z))
        .map(|((x, y), z)| {
            SeedPosition::Cell(BlockPos {
                space: seed.page.space,
                x,
                y,
                z,
            })
        })
        .unwrap_or(SeedPosition::Outside))
}

fn validate_limits(limits: LightingLimits) -> Result<(), LightingError> {
    if limits.max_frontier_cells == 0
        || limits.max_seed_pages == 0
        || limits.max_queue_age_ticks == 0
        || limits.reserved_interactive_cells > limits.max_frontier_cells
    {
        Err(LightingError::InvalidLimits)
    } else {
        Ok(())
    }
}
