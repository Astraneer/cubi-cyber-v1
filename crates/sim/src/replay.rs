//! Bounded, local movement reproduction. Records only the public cells/flow
//! actually queried by a simulation step, its input, and exact continuations.
//! External changes (camera, permissions, authority correction) are explicit
//! checkpoints. This is not a renderer, server, inventory or secret-state dump.
use crate::{Keys, Player, QueryWorld, TickOutcome, prediction::PredictionState};
use core_types::Cell;
use std::{cell::RefCell, fmt::Write};
use world::CellRead;
mod codec;
pub use codec::Archive;

pub const WINDOW_NS: u64 = 10_000_000_000;
pub const MAX_CELLS: usize = 256;
pub const MAX_FLOWS: usize = 32;
pub const DEFAULT_STEPS: usize = 512;
pub const MAX_STEPS: usize = 4096;
pub const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;
const TABLE: usize = MAX_CELLS * 2;
const EMPTY: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellWitness {
    position: [i32; 3],
    value: CellRead,
}
impl Default for CellWitness {
    fn default() -> Self {
        Self {
            position: [0; 3],
            value: CellRead::Unknown,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct FlowWitness {
    position: [i32; 3],
    value: Option<[f64; 3]>,
}
#[derive(Clone)]
struct Step {
    time_ns: u64,
    sequence: u64,
    before: Option<PredictionState>,
    after: Option<PredictionState>,
    external: bool,
    keys: u8,
    using: bool,
    outcome: TickOutcome,
    cells: [CellWitness; MAX_CELLS],
    flows: [FlowWitness; MAX_FLOWS],
    cell_count: u16,
    flow_count: u8,
    invalid: bool,
}
impl Default for Step {
    fn default() -> Self {
        Self {
            time_ns: 0,
            sequence: 0,
            before: None,
            after: None,
            external: true,
            keys: 0,
            using: false,
            outcome: TickOutcome::default(),
            cells: [CellWitness::default(); MAX_CELLS],
            flows: [FlowWitness::default(); MAX_FLOWS],
            cell_count: 0,
            flow_count: 0,
            invalid: false,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub steps: usize,
    pub cell_witnesses: usize,
    pub flow_witnesses: usize,
    pub checkpoints: usize,
    pub capacity_losses: u64,
    pub invalid_steps: u64,
    pub complete_window: bool,
}
/// Allocates its fixed ring at loading time. Recording, expiry and overwriting
/// do not allocate. A bound failure never changes simulation behavior and is
/// reported as an incomplete capture instead of fabricating missing data.
pub struct Recorder {
    steps: Vec<Step>,
    head: usize,
    len: usize,
    time_ns: u64,
    started_ns: Option<u64>,
    last_loss_ns: Option<u64>,
    next_sequence: u64,
    losses: u64,
    invalid_steps: u64,
    previous: Option<PredictionState>,
}
impl Recorder {
    pub fn new(capacity: usize) -> Result<Self, &'static str> {
        if capacity == 0 || capacity > MAX_STEPS {
            return Err("invalid movement replay capacity");
        }
        Ok(Self {
            steps: vec![Step::default(); capacity],
            head: 0,
            len: 0,
            time_ns: 0,
            started_ns: None,
            last_loss_ns: None,
            next_sequence: 1,
            losses: 0,
            invalid_steps: 0,
            previous: None,
        })
    }
    pub fn set_time(&mut self, monotonic_ns: u64) -> Result<(), &'static str> {
        if monotonic_ns < self.time_ns {
            return Err("movement replay clock went backwards");
        }
        self.time_ns = monotonic_ns;
        self.started_ns.get_or_insert(monotonic_ns);
        let threshold = monotonic_ns.saturating_sub(WINDOW_NS);
        while self.len != 0 && self.steps[self.head].time_ns < threshold {
            self.head = (self.head + 1) % self.steps.len();
            self.len -= 1;
        }
        Ok(())
    }
    pub fn tick(
        &mut self,
        player: &mut Player,
        world: &impl QueryWorld,
        keys: Keys,
        using: bool,
    ) -> TickOutcome {
        let before = PredictionState::capture(player);
        let Ok(before) = before else {
            self.invalid_steps += 1;
            self.last_loss_ns = Some(self.time_ns);
            self.previous = None;
            return player.tick_using(world, keys, using);
        };
        if self.len == self.steps.len() {
            self.head = (self.head + 1) % self.steps.len();
            self.len -= 1;
            self.losses += 1;
            self.last_loss_ns = Some(self.time_ns);
        }
        let index = (self.head + self.len) % self.steps.len();
        let step = &mut self.steps[index];
        step.time_ns = self.time_ns;
        step.sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        step.before = Some(before);
        step.after = None;
        step.external = self.previous != Some(before);
        step.keys = encode_keys(keys);
        step.using = using;
        step.cell_count = 0;
        step.flow_count = 0;
        step.invalid = false;
        let trace = RecordingQuery {
            world,
            trace: RefCell::new(Trace {
                step,
                table: [EMPTY; TABLE],
            }),
        };
        let outcome = player.tick_using(&trace, keys, using);
        let after = PredictionState::capture(player).ok();
        let trace = trace.trace.into_inner();
        trace.step.after = after;
        trace.step.outcome = outcome;
        if after.is_none() || trace.step.invalid {
            self.invalid_steps += 1;
            self.last_loss_ns = Some(self.time_ns);
        }
        self.previous = after;
        self.len += 1;
        outcome
    }
    fn iter(&self) -> impl ExactSizeIterator<Item = &Step> {
        (0..self.len).map(|offset| &self.steps[(self.head + offset) % self.steps.len()])
    }
    pub fn stats(&self) -> Stats {
        let mut stats = Stats {
            steps: self.len,
            capacity_losses: self.losses,
            invalid_steps: self.invalid_steps,
            complete_window: self
                .started_ns
                .is_some_and(|start| self.time_ns.saturating_sub(start) >= WINDOW_NS)
                && self
                    .last_loss_ns
                    .is_none_or(|loss| self.time_ns.saturating_sub(loss) > WINDOW_NS),
            ..Stats::default()
        };
        for step in self.iter() {
            stats.cell_witnesses += usize::from(step.cell_count);
            stats.flow_witnesses += usize::from(step.flow_count);
            stats.checkpoints += usize::from(step.external);
        }
        stats
    }
    /// Export only on explicit local F8/file actions, outside a measured frame.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        codec::encode(self)
    }
    /// Adds one object member to an existing JSON object. All data is generated
    /// bounded hexadecimal; no credentials, strings from packets or resources.
    pub fn append_json(&self, json: &mut String) -> Result<(), &'static str> {
        if !json.ends_with('}') {
            return Err("report must end with an object");
        }
        let data = self.encode()?;
        let stats = self.stats();
        json.pop();
        if !json.ends_with('{') {
            json.push(',');
        }
        write!(json, "\"movementReplay\":{{\"format\":\"CUBR1\",\"scope\":\"public movement ticks and explicit external checkpoints\",\"rendererReplay\":false,\"steps\":{},\"completeWindow\":{},\"capacityLosses\":{},\"invalidSteps\":{},\"dataHex\":\"", stats.steps, stats.complete_window, stats.capacity_losses, stats.invalid_steps).map_err(|_| "replay JSON formatting")?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        json.reserve(data.len() * 2 + 4);
        for byte in data {
            json.push(HEX[usize::from(byte >> 4)] as char);
            json.push(HEX[usize::from(byte & 15)] as char);
        }
        json.push_str("\"}}");
        Ok(())
    }
}
struct Trace<'a> {
    step: &'a mut Step,
    table: [u16; TABLE],
}
impl Trace<'_> {
    fn cell(&mut self, position: [i32; 3], value: CellRead) {
        let mut hash = 0xcbf29ce484222325u64;
        for axis in position {
            hash = (hash ^ u64::from(axis as u32)).wrapping_mul(0x100000001b3);
        }
        let mut at = hash as usize & (TABLE - 1);
        loop {
            let index = self.table[at];
            if index == EMPTY {
                if usize::from(self.step.cell_count) == MAX_CELLS {
                    self.step.invalid = true;
                    return;
                }
                let index = self.step.cell_count;
                self.step.cells[usize::from(index)] = CellWitness { position, value };
                self.step.cell_count += 1;
                self.table[at] = index;
                return;
            }
            let old = self.step.cells[usize::from(index)];
            if old.position == position {
                self.step.invalid |= old.value != value;
                return;
            }
            at = (at + 1) & (TABLE - 1);
        }
    }
    fn flow(&mut self, position: [i32; 3], value: Option<[f64; 3]>) {
        let flows = &self.step.flows[..usize::from(self.step.flow_count)];
        if let Some(old) = flows.iter().find(|old| old.position == position) {
            self.step.invalid |= !same_flow(old.value, value);
        } else if flows.len() == MAX_FLOWS {
            self.step.invalid = true;
        } else {
            self.step.flows[usize::from(self.step.flow_count)] = FlowWitness { position, value };
            self.step.flow_count += 1;
        }
    }
}
fn same_flow(a: Option<[f64; 3]>, b: Option<[f64; 3]>) -> bool {
    a.map(|v| v.map(f64::to_bits)) == b.map(|v| v.map(f64::to_bits))
}
struct RecordingQuery<'a, W> {
    world: &'a W,
    trace: RefCell<Trace<'a>>,
}
impl<W: QueryWorld> QueryWorld for RecordingQuery<'_, W> {
    fn cell(&self, position: [i32; 3]) -> CellRead {
        let value = self.world.cell(position);
        self.trace.borrow_mut().cell(position, value);
        value
    }
    fn flow_override(&self, position: [i32; 3]) -> Option<[f64; 3]> {
        let value = self.world.flow_override(position);
        self.trace.borrow_mut().flow(position, value);
        value
    }
}
fn encode_keys(keys: Keys) -> u8 {
    [
        keys.fwd,
        keys.back,
        keys.left,
        keys.right,
        keys.jump,
        keys.sneak,
        keys.sprint,
    ]
    .into_iter()
    .enumerate()
    .fold(0, |bits, (i, enabled)| bits | (u8::from(enabled) << i))
}
fn decode_keys(bits: u8) -> Keys {
    Keys {
        fwd: bits & 1 != 0,
        back: bits & 2 != 0,
        left: bits & 4 != 0,
        right: bits & 8 != 0,
        jump: bits & 16 != 0,
        sneak: bits & 32 != 0,
        sprint: bits & 64 != 0,
    }
}
