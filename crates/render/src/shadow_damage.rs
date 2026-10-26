//! Fixed-capacity model damage retained until the directional shadow owner commits.
use super::WorldBounds;
#[derive(Clone, Copy, Default)]
struct Slot {
    old: Option<WorldBounds>,
    marked: bool,
}
pub(crate) struct ShadowDamageTracker {
    slots: Vec<Slot>,
    dirty: Vec<u32>,
    base: u64,
}
impl ShadowDamageTracker {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            slots: vec![Slot::default(); capacity],
            dirty: Vec::with_capacity(capacity),
            base: 0,
        }
    }
    /// Keep the first displayed footprint; intermediate unpublished poses need
    /// not expand damage. The current footprint comes from the final GPU record.
    pub(crate) fn record(&mut self, index: usize, old: Option<WorldBounds>) {
        let slot = &mut self.slots[index];
        if !slot.marked {
            slot.marked = true;
            slot.old = old;
            self.dirty.push(index as u32);
        }
    }
    pub(crate) fn visit(
        &self,
        previous: u64,
        current: u64,
        mut bounds: impl FnMut(usize) -> Option<WorldBounds>,
        visit: &mut dyn FnMut(WorldBounds),
    ) -> bool {
        if previous == current {
            return true;
        }
        if previous != self.base {
            return false;
        }
        for &index in &self.dirty {
            if let Some(old) = self.slots[index as usize].old {
                visit(old);
            }
            if let Some(new) = bounds(index as usize) {
                visit(new);
            }
        }
        true
    }
    pub(crate) fn commit(&mut self, current: u64) {
        for index in self.dirty.drain(..) {
            self.slots[index as usize] = Slot::default();
        }
        self.base = current;
    }
}
