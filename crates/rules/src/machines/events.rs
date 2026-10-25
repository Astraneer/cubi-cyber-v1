//! Exact event projection for stable recipe machines. Topology changes, jams,
//! inventory mutations and outages are explicit owner events: advance to their
//! tick before applying the mutation. Unsupported side effects use the ordinary
//! machine interpreter, never an approximate event model.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventWork {
    pub transitions: u64,
    pub elapsed_ticks: u64,
    pub completed_operations: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    Unsupported,
    TickOverflow,
}

/// This kernel deliberately does not own a clock or mutate the world. The
/// caller schedules exact dependency wakes and coalesces their affected machines.
/// Stable operation interiors use O(1) arithmetic rather than per-tick scanning.
pub fn advance(machine: &mut Machine, ticks: u64, work: &mut EventWork) -> Result<(), EventError> {
    let d = definition(machine.kind);
    if !matches!(
        machine.kind,
        MekKind::Enrich | MekKind::Smelter | MekKind::Crusher | MekKind::Infuser
    ) || machine.eject
        || d.energy_slot
            .is_some_and(|i| !machine.slots[i].stack.is_empty())
        || d.upgrade_slot
            .is_some_and(|i| !machine.slots[i].stack.is_empty())
        || (machine.kind == MekKind::Infuser && !machine.slots[0].stack.is_empty())
    {
        return Err(EventError::Unsupported);
    }
    work.elapsed_ticks = work
        .elapsed_ticks
        .checked_add(ticks)
        .ok_or(EventError::TickOverflow)?;
    let mut remaining = ticks;
    while remaining > 0 {
        work.transitions = work.transitions.saturating_add(1);
        match machine.prepare_operation() {
            Err(step) => {
                machine.last = step;
                machine.active.countdown();
                machine.active.set(false);
                let bulk = remaining - 1;
                machine.active.delay = machine.active.delay.saturating_sub(bulk.min(255) as u8);
                if machine.active.delay == 0 {
                    machine.active.shown = machine.active.logic;
                }
                break;
            }
            Ok(op) => {
                let until_finish =
                    u64::from(machine.ticks().saturating_sub(machine.progress).max(1));
                let span = remaining
                    .min(until_finish)
                    .min(machine.energy / machine.per_tick());
                machine.energy -= span * machine.per_tick();
                machine.progress += span as u32;
                machine.active.countdown();
                machine.active.set(true);
                machine.active.delay = machine
                    .active
                    .delay
                    .saturating_sub((span - 1).min(255) as u8);
                if machine.active.delay == 0 {
                    machine.active.shown = machine.active.logic;
                }
                remaining -= span;
                if u64::from(machine.progress) >= u64::from(machine.ticks()) {
                    machine.finish(op);
                    machine.last = Step::Finished;
                    work.completed_operations = work.completed_operations.saturating_add(1);
                } else {
                    machine.last = Step::Worked;
                }
            }
        }
    }
    Ok(())
}

/// A fixed-capacity dependency wake queue; duplicate machine IDs coalesce to
/// their earliest tick. No world-sized tick scan and no allocation after setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wake {
    pub at: u64,
    pub machine: u32,
    pub generation: u32,
}
pub struct WakeQueue {
    entries: Vec<Wake>,
    positions: Vec<usize>,
}
impl WakeQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            positions: vec![usize::MAX; capacity],
        }
    }
    pub fn schedule(&mut self, wake: Wake) -> Result<(), Wake> {
        let Some(&position) = self.positions.get(wake.machine as usize) else {
            return Err(wake);
        };
        if position != usize::MAX {
            let old = self.entries[position];
            if wake.generation < old.generation {
                return Ok(());
            }
            if old.generation != wake.generation || wake.at < old.at {
                self.entries[position] = wake;
                self.sift_up(position);
                self.sift_down(self.positions[wake.machine as usize]);
            }
            return Ok(());
        }
        self.positions[wake.machine as usize] = self.entries.len();
        self.entries.push(wake);
        self.sift_up(self.entries.len() - 1);
        Ok(())
    }
    pub fn pop_due(&mut self, tick: u64) -> Option<Wake> {
        let first = *self.entries.first()?;
        if first.at > tick {
            return None;
        }
        self.positions[first.machine as usize] = usize::MAX;
        self.entries.swap_remove(0);
        if let Some(next) = self.entries.first() {
            self.positions[next.machine as usize] = 0;
            self.sift_down(0);
        }
        Some(first)
    }
    fn swap(&mut self, a: usize, b: usize) {
        self.entries.swap(a, b);
        self.positions[self.entries[a].machine as usize] = a;
        self.positions[self.entries[b].machine as usize] = b;
    }
    fn key(&self, i: usize) -> (u64, u32) {
        let w = self.entries[i];
        (w.at, w.machine)
    }
    fn sift_up(&mut self, mut i: usize) {
        while i > 0 {
            let parent = (i - 1) / 2;
            if self.key(parent) <= self.key(i) {
                break;
            }
            self.swap(parent, i);
            i = parent;
        }
    }
    fn sift_down(&mut self, mut i: usize) {
        loop {
            let left = i * 2 + 1;
            if left >= self.entries.len() {
                break;
            }
            let right = left + 1;
            let child = if right < self.entries.len() && self.key(right) < self.key(left) {
                right
            } else {
                left
            };
            if self.key(i) <= self.key(child) {
                break;
            }
            self.swap(i, child);
            i = child;
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
