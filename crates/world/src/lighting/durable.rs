//! Private exact continuation. Cell input records contain only semantic light
//! obligations; process-local journal cursor IDs never enter this schema.
use super::*;
use std::collections::{BTreeSet, VecDeque};
mod codec;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_OPERATIONS: usize = 131_072;
#[derive(Clone, Debug)]
pub(super) enum FrontierOp {
    Insert(BlockPos, LightPriority, Tick),
    Remove(usize),
    Rotate(usize),
}
#[derive(Clone, Debug)]
pub(super) struct InputRecord {
    tx: u64,
    pages: Vec<(PageId, u8)>,
    // page index in upper bits, interested bit12 and LocalIdx in low12bits.
    cells: Vec<u32>,
    bytes: usize,
}
impl InputRecord {
    fn new(tx: u64, pages: Vec<(PageId, u8)>, cells: Vec<u32>) -> Self {
        let bytes = 16
            + pages.len() * 17
            + cells.len() * 2
            + cells
                .iter()
                .enumerate()
                .filter(|(i, cell)| *i == 0 || (cells[*i - 1] >> 13) != (**cell >> 13))
                .count()
                * 8;
        Self {
            tx,
            pages,
            cells,
            bytes,
        }
    }
    pub(super) fn encoded_len(&self) -> usize {
        self.bytes
    }
}
pub(super) struct Owner {
    revision: u64,
    watermark: Option<u64>,
    pub(super) inputs: VecDeque<Arc<InputRecord>>,
    pub(super) input_bytes: usize,
    appended: Vec<Arc<InputRecord>>,
    pub(super) consumed: usize,
    pub(super) dirty: bool,
    pub(super) seeds_dirty: bool,
}
impl Owner {
    fn new() -> Self {
        Self {
            revision: 0,
            watermark: None,
            inputs: VecDeque::new(),
            input_bytes: 0,
            appended: Vec::new(),
            consumed: 0,
            dirty: false,
            seeds_dirty: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
struct RecordCursor {
    page: usize,
    cell: usize,
    neighbor: usize,
    interested: Option<bool>,
    queued_at: Tick,
}
#[derive(Clone)]
pub struct Snapshot {
    limits: LightingLimits,
    revision: u64,
    watermark: Option<u64>,
    entries: Vec<Option<Entry>>,
    free: Vec<usize>,
    chains: [Chain; 3],
    seeds: Vec<PageSeed>,
    inputs: Vec<Arc<InputRecord>>,
    record: Option<RecordCursor>,
}
#[derive(Clone)]
pub struct Delta {
    before: u64,
    after: u64,
    watermark: Option<u64>,
    operations: Vec<FrontierOp>,
    seeds: Option<Vec<PageSeed>>,
    appended: Vec<Arc<InputRecord>>,
    consumed: usize,
    record: Option<RecordCursor>,
}
fn invalid(detail: &'static str) -> LightingError {
    LightingError::Persistence(detail)
}
fn record_cursor(record: &PendingRecord) -> RecordCursor {
    RecordCursor {
        page: record.page,
        cell: record.cell,
        neighbor: record.neighbor,
        interested: record.interested,
        queued_at: record.queued_at,
    }
}
fn pending(record: RecordCursor, input: Arc<InputRecord>) -> Result<PendingRecord, LightingError> {
    if record.page > input.pages.len()
        || record.cell > input.cells.len()
        || record.neighbor > 6
        || (record.cell == input.cells.len() && record.neighbor != 0)
        || (record.interested != Some(true) && record.neighbor != 0)
    {
        return Err(invalid("input cursor"));
    }
    Ok(PendingRecord {
        change: InputChange::Durable(input),
        next: 0,
        page: record.page,
        cell: record.cell,
        neighbor: record.neighbor,
        interested: record.interested,
        queued_at: record.queued_at,
    })
}
impl LightingEngine {
    /// Fresh migration starts with the caller's existing bootstrap obligations.
    pub fn enable_durable_inputs(&mut self) -> Result<(), LightingError> {
        if self.record.is_some()
            || self.durable.is_some()
            || self.limits.max_frontier_cells > 65_536
            || self.limits.max_seed_pages > 65_536
        {
            return Err(invalid("input mode/limits"));
        }
        self.frontier.recording = true;
        self.durable = Some(Owner::new());
        Ok(())
    }
    pub fn has_durable_inputs(&self) -> bool {
        self.durable.is_some()
    }
    /// Every canonical batch supplies its accepted World changes, including a
    /// machine portion which does not yet run the light solver. TxId dedup makes
    /// the before-step and final-submit calls composable.
    pub fn observe_durable_change(
        &mut self,
        world: &World,
        change: &WorldChange,
    ) -> Result<(), LightingError> {
        let owner = self.durable.as_mut().ok_or(invalid("input mode"))?;
        if owner.watermark.is_some_and(|tx| change.tx.0 <= tx)
            || !change.channels.intersects(LIGHT_INPUT_CHANNELS)
        {
            return Ok(());
        }
        if change.pages.len() > 65_536 {
            return Err(invalid("input pages"));
        }
        let pages: Vec<_> = change
            .pages
            .iter()
            .map(|p| (p.page, u8::from(p.created) | (u8::from(p.removed) << 1)))
            .collect();
        let mut cells = Vec::with_capacity(change.cells.len());
        let mut page_index = 0;
        for cell in &change.cells {
            while page_index < pages.len() && pages[page_index].0 < cell.page {
                page_index += 1;
            }
            if pages.get(page_index).is_none_or(|p| p.0 != cell.page) {
                return Err(invalid("input page index"));
            }
            let interested = world
                .registry
                .properties(cell.old)
                .changed_channels(world.registry.properties(cell.new))
                .intersects(LIGHT_INPUT_CHANNELS);
            cells.push(
                (page_index as u32) << 13
                    | u32::from(interested) << 12
                    | u32::from(cell.index.get()),
            );
        }
        let input = Arc::new(InputRecord::new(change.tx.0, pages, cells));
        let bytes = input.encoded_len();
        if owner
            .input_bytes
            .checked_add(bytes)
            .is_none_or(|n| n > MAX_INPUT_BYTES)
        {
            return Err(invalid("input capacity"));
        }
        owner.input_bytes += bytes;
        owner.inputs.push_back(Arc::clone(&input));
        owner.appended.push(input);
        owner.watermark = Some(change.tx.0);
        owner.dirty = true;
        Ok(())
    }
    pub(super) fn ingest_owned_record(
        &mut self,
        world: &World,
        record: &mut PendingRecord,
        budget: LightIngestBudget,
        work: &mut LightWork,
    ) -> Result<RecordStatus, LightingError> {
        let InputChange::Durable(input) = &record.change else {
            return Err(invalid("input record"));
        };
        while record.page < input.pages.len() {
            if work.journal_entries_examined >= budget.max_entries {
                return Ok(RecordStatus::Budget);
            }
            let (page, flags) = input.pages[record.page];
            work.journal_entries_examined += 1;
            if flags != 0 {
                page_origin(page)?;
                if self
                    .queue_page_kind(
                        page,
                        flags & 1 != 0,
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
        while record.cell < input.cells.len() {
            let cell = input.cells[record.cell];
            if record.interested.is_none() {
                if work.journal_entries_examined >= budget.max_entries {
                    return Ok(RecordStatus::Budget);
                }
                work.journal_entries_examined += 1;
                record.interested = Some(cell & 4096 != 0);
            }
            if record.interested == Some(true) {
                let position = cell_position(
                    input.pages[(cell >> 13) as usize].0,
                    LocalIdx::new((cell & 4095) as u16).expect("packed local index"),
                )?;
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
                    {
                        if self.admission_deferred(target) {
                            return Ok(RecordStatus::Budget);
                        }
                        if self
                            .queue_cell(target, record.queued_at, LightPriority::Interactive)
                            .is_err()
                        {
                            return Ok(RecordStatus::Backpressure);
                        }
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
    pub fn durable_snapshot(&self) -> Result<Snapshot, LightingError> {
        let owner = self.durable.as_ref().ok_or(invalid("input mode"))?;
        if owner.dirty || owner.seeds_dirty || !self.frontier.operations.is_empty() {
            return Err(invalid("snapshot before durable delta boundary"));
        }
        Ok(Snapshot {
            limits: self.limits,
            revision: owner.revision,
            watermark: owner.watermark,
            entries: self.frontier.entries.clone(),
            free: self.frontier.free.clone(),
            chains: self.frontier.chains,
            seeds: self.seeds.values().copied().collect(),
            inputs: owner.inputs.iter().cloned().collect(),
            record: self.record.as_ref().map(record_cursor),
        })
    }
    pub fn restore_durable(
        world: &mut World,
        limits: LightingLimits,
        snapshot: Snapshot,
    ) -> Result<Self, LightingError> {
        let mut engine = Self::attach(world, limits)?;
        engine.enable_durable_inputs()?;
        if let Err(error) = engine.install_snapshot(snapshot) {
            engine.detach(world);
            return Err(error);
        }
        Ok(engine)
    }
    fn install_snapshot(&mut self, snapshot: Snapshot) -> Result<(), LightingError> {
        if snapshot.limits != self.limits
            || snapshot.entries.len() != self.limits.max_frontier_cells
            || snapshot.seeds.len() > self.limits.max_seed_pages
        {
            return Err(invalid("snapshot limits"));
        }
        let mut frontier = Frontier::new(snapshot.entries.len());
        frontier.entries = snapshot.entries;
        frontier.free = snapshot.free;
        frontier.chains = snapshot.chains;
        let mut seen = BTreeSet::new();
        for &index in &frontier.free {
            if index >= frontier.entries.len()
                || frontier.entries[index].is_some()
                || !seen.insert(index)
            {
                return Err(invalid("free slots"));
            }
        }
        for priority in 0..3 {
            let mut cursor = frontier.chains[priority].first;
            let mut previous = None;
            while let Some(index) = cursor {
                if index >= frontier.entries.len() || !seen.insert(index) {
                    return Err(invalid("frontier chain"));
                }
                let entry = frontier.entries[index].ok_or(invalid("frontier hole"))?;
                if entry.priority.index() != priority
                    || entry.previous != previous
                    || frontier.positions.insert(entry.position, index).is_some()
                {
                    return Err(invalid("frontier entry"));
                }
                frontier.add_age(entry.queued_at);
                frontier.noninteractive +=
                    usize::from(entry.priority != LightPriority::Interactive);
                previous = Some(index);
                cursor = entry.next;
            }
            if previous != frontier.chains[priority].last {
                return Err(invalid("frontier tail"));
            }
        }
        if seen.len() != frontier.entries.len()
            || frontier.noninteractive
                > self.limits.max_frontier_cells - self.limits.reserved_interactive_cells
        {
            return Err(invalid("frontier coverage"));
        }
        frontier.recording = true;
        let mut seeds = BTreeMap::new();
        for seed in snapshot.seeds {
            validate_seed(seed)?;
            if seeds.insert(seed.page, seed).is_some() {
                return Err(invalid("duplicate seed"));
            }
        }
        let mut owner = Owner::new();
        owner.revision = snapshot.revision;
        owner.watermark = snapshot.watermark;
        for input in snapshot.inputs {
            validate_input(&input)?;
            if owner.inputs.back().is_some_and(|old| old.tx >= input.tx)
                || owner.watermark.is_none_or(|tx| input.tx > tx)
            {
                return Err(invalid("input order"));
            }
            owner.input_bytes += input.encoded_len();
            owner.inputs.push_back(input);
        }
        if owner.input_bytes > MAX_INPUT_BYTES {
            return Err(invalid("input capacity"));
        }
        let record = snapshot
            .record
            .map(|r| {
                pending(
                    r,
                    Arc::clone(owner.inputs.front().ok_or(invalid("input cursor head"))?),
                )
            })
            .transpose()?;
        self.frontier = frontier;
        self.seeds = seeds;
        self.record = record;
        self.durable = Some(owner);
        Ok(())
    }
    /// Only changed queues create a delta. No clock/work-counter-only records.
    pub fn take_durable_delta(&mut self) -> Result<Option<Delta>, LightingError> {
        let owner = self.durable.as_mut().ok_or(invalid("input mode"))?;
        if !owner.dirty && !owner.seeds_dirty && self.frontier.operations.is_empty() {
            return Ok(None);
        }
        if self.frontier.operations.len() > MAX_OPERATIONS {
            return Err(invalid("operation capacity"));
        }
        let after = owner
            .revision
            .checked_add(1)
            .ok_or(invalid("revision overflow"))?;
        let delta = Delta {
            before: owner.revision,
            after,
            watermark: owner.watermark,
            operations: self.frontier.operations.clone(),
            seeds: owner
                .seeds_dirty
                .then(|| self.seeds.values().copied().collect()),
            appended: owner.appended.clone(),
            consumed: owner.consumed,
            record: self.record.as_ref().map(record_cursor),
        };
        owner.revision = after;
        owner.dirty = false;
        owner.seeds_dirty = false;
        owner.appended.clear();
        owner.consumed = 0;
        self.frontier.operations.clear();
        Ok(Some(delta))
    }
    pub fn apply_durable_delta(&mut self, delta: Delta) -> Result<(), LightingError> {
        let owner = self.durable.as_ref().ok_or(invalid("input mode"))?;
        if delta.before != owner.revision
            || delta.before.checked_add(1) != Some(delta.after)
            || delta.operations.len() > MAX_OPERATIONS
            || delta.watermark < owner.watermark
            || owner.dirty
            || owner.seeds_dirty
            || !self.frontier.operations.is_empty()
        {
            return Err(invalid("stale delta"));
        }
        // Recovery input is checksummed by the enclosing WAL. Validate all source
        // and seed metadata before applying queue operations.
        let mut previous = owner.watermark;
        for input in &delta.appended {
            validate_input(input)?;
            if previous.is_some_and(|tx| tx >= input.tx)
                || delta.watermark.is_none_or(|tx| input.tx > tx)
            {
                return Err(invalid("delta input order"));
            }
            previous = Some(input.tx);
        }
        if delta.consumed > owner.inputs.len() + delta.appended.len() {
            return Err(invalid("delta input count"));
        }
        if let Some(seeds) = &delta.seeds {
            if seeds.len() > self.limits.max_seed_pages {
                return Err(invalid("delta seeds"));
            }
            let mut pages = BTreeSet::new();
            for &seed in seeds {
                validate_seed(seed)?;
                if !pages.insert(seed.page) {
                    return Err(invalid("duplicate seed"));
                }
            }
        }
        let front = owner
            .inputs
            .iter()
            .chain(delta.appended.iter())
            .nth(delta.consumed);
        let record = delta
            .record
            .map(|r| pending(r, Arc::clone(front.ok_or(invalid("delta record head"))?)))
            .transpose()?;
        let appended_bytes: usize = delta.appended.iter().map(|r| r.encoded_len()).sum();
        let removed_bytes: usize = owner
            .inputs
            .iter()
            .chain(delta.appended.iter())
            .take(delta.consumed)
            .map(|r| r.encoded_len())
            .sum();
        let input_bytes = owner
            .input_bytes
            .checked_add(appended_bytes)
            .and_then(|n| n.checked_sub(removed_bytes))
            .ok_or(invalid("input byte count"))?;
        if input_bytes > MAX_INPUT_BYTES {
            return Err(invalid("input capacity"));
        }
        let mut undo = Vec::new();
        self.frontier.recording = false;
        let result = (|| {
            for op in &delta.operations {
                match *op {
                    FrontierOp::Insert(position, priority, at) => {
                        if !self.frontier.can_enqueue(position, priority, self.limits) {
                            return Err(invalid("delta frontier full"));
                        }
                        self.frontier
                            .insert(position, priority, at, Some(&mut undo));
                    }
                    FrontierOp::Remove(index) => {
                        if self.frontier.entries.get(index).is_none_or(Option::is_none) {
                            return Err(invalid("delta remove"));
                        }
                        self.frontier.remove(index, Some(&mut undo));
                    }
                    FrontierOp::Rotate(index) => {
                        if self.frontier.entries.get(index).is_none_or(Option::is_none) {
                            return Err(invalid("delta rotate"));
                        }
                        self.frontier.rotate(index, &mut undo);
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.frontier.rollback(undo);
        }
        self.frontier.recording = true;
        result?;
        if let Some(seeds) = delta.seeds {
            self.seeds = seeds.into_iter().map(|s| (s.page, s)).collect();
        }
        let owner = self.durable.as_mut().expect("owned mode");
        owner.inputs.extend(delta.appended);
        for _ in 0..delta.consumed {
            owner.inputs.pop_front();
        }
        owner.input_bytes = input_bytes;
        owner.watermark = delta.watermark;
        owner.revision = delta.after;
        self.record = record;
        Ok(())
    }
}
fn validate_seed(seed: PageSeed) -> Result<(), LightingError> {
    let own = if seed.whole {
        4096
    } else if seed.top {
        256
    } else {
        0
    };
    let maximum = own + if seed.halo { 1536 } else { 0 };
    if maximum == 0 || seed.next > maximum {
        return Err(invalid("seed cursor"));
    }
    page_origin(seed.page)?;
    Ok(())
}
fn validate_input(input: &InputRecord) -> Result<(), LightingError> {
    if input.pages.len() > 65_536 || input.encoded_len() > MAX_INPUT_BYTES {
        return Err(invalid("input bound"));
    }
    let mut last_page = None;
    for &(page, flags) in &input.pages {
        if flags > 2 || last_page.is_some_and(|old| old >= page) {
            return Err(invalid("input pages"));
        }
        page_origin(page)?;
        last_page = Some(page);
    }
    let mut last_cell = None;
    for &cell in &input.cells {
        let key = (cell >> 13, cell & 4095);
        if key.0 as usize >= input.pages.len() || last_cell.is_some_and(|old| old >= key) {
            return Err(invalid("input cells"));
        }
        last_cell = Some(key);
    }
    Ok(())
}
