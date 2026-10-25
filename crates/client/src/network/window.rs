//! One bounded authoritative window image. A different inventory revision is
//! retained but cannot be presented as a coherent actionable window yet.
use net::window::{WindowCodecError, WindowSnapshot, validate_snapshot};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowCounters {
    pub updates: u64,
    pub stale: u64,
}
#[derive(Default)]
pub struct WindowCache {
    latest: Option<WindowSnapshot>,
    counters: WindowCounters,
}
impl WindowCache {
    pub fn clear(&mut self) {
        self.latest = None;
    }
    pub fn latest(&self) -> Option<&WindowSnapshot> {
        self.latest.as_ref()
    }
    pub fn matching_inventory(&self, revision: u64) -> Option<&WindowSnapshot> {
        self.latest
            .as_ref()
            .filter(|state| state.guard.inventory_revision == revision)
    }
    pub fn counters(&self) -> WindowCounters {
        self.counters
    }
    pub fn receive(&mut self, state: WindowSnapshot) -> Result<bool, WindowCodecError> {
        validate_snapshot(&state)?;
        if let Some(old) = &self.latest {
            if state.guard.id < old.guard.id
                || (state.guard.id == old.guard.id
                    && (state.guard.revision < old.guard.revision
                        || state.guard.inventory_revision < old.guard.inventory_revision))
            {
                self.counters.stale += 1;
                return Ok(false);
            }
            if old == &state {
                return Ok(false);
            }
        }
        self.latest = Some(state);
        self.counters.updates += 1;
        Ok(true)
    }
}
