//! Durable finite per-tick cohort. Preparing never executes a recipe; Machines
//! finish before Resources, and Resources finish before any item transit.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwnerStage {
    Prepare,
    Machines,
    Resources,
    Transit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerState {
    pub tick: Tick,
    pub stage: OwnerStage,
    /// Last due-index row visited during bounded cohort preparation.
    pub prepared_after: Option<(Tick, u8, u64)>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnerSnapshot {
    pub state: Option<OwnerState>,
    /// Sorted by phase and creation order, independent of original due age.
    pub ready: Vec<Pending>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct OwnerDelta {
    pub before: Option<OwnerState>,
    pub after: Option<OwnerState>,
    pub ready: Vec<QueuePatch>,
}
impl OwnerDelta {
    pub fn is_empty(&self) -> bool {
        self.before == self.after && self.ready.is_empty()
    }
}
#[derive(Default)]
pub(super) struct OwnerBarrier {
    state: Option<OwnerState>,
    ready: BTreeMap<(u8, u64), Pending>,
}
pub(super) struct Cohort<'a> {
    base: &'a OwnerBarrier,
    pub state: Option<OwnerState>,
    changes: BTreeMap<(u8, u64), Option<Pending>>,
    scanned: [Option<u64>; 2],
}
pub(super) fn ready_key(p: Pending) -> (u8, u64) {
    (phase(p.event), p.order)
}
impl OwnerBarrier {
    pub fn project(&self) -> Cohort<'_> {
        Cohort {
            base: self,
            state: self.state,
            changes: BTreeMap::new(),
            scanned: [None, None],
        }
    }
    pub fn snapshot(&self) -> OwnerSnapshot {
        OwnerSnapshot {
            state: self.state,
            ready: self.ready.values().copied().collect(),
        }
    }
    pub fn restore(
        runtime: &MachineRuntime,
        snapshot: &OwnerSnapshot,
    ) -> Result<Self, MachineError> {
        if snapshot.ready.len() > runtime.config.events
            || snapshot
                .ready
                .windows(2)
                .any(|p| ready_key(p[0]) >= ready_key(p[1]))
        {
            return Err(MachineError::Encoding);
        }
        validate_state(snapshot.state, runtime.next_order)?;
        let mut out = Self {
            state: snapshot.state,
            ready: BTreeMap::new(),
        };
        for &p in &snapshot.ready {
            if runtime.pending(p.event) != Some(p) || !eligible(snapshot.state, p) {
                return Err(MachineError::Encoding);
            }
            out.ready.insert(ready_key(p), p);
        }
        if let Some(state) = snapshot.state {
            for &p in runtime.events.values().take_while(|p| p.due <= state.tick) {
                let captured = state.stage != OwnerStage::Prepare
                    || state
                        .prepared_after
                        .is_some_and(|cursor| queue_key(p) <= cursor);
                if captured && out.ready.get(&ready_key(p)).copied() != Some(p) {
                    return Err(MachineError::Encoding);
                }
            }
        }
        Ok(out)
    }
    pub fn validate(
        &self,
        runtime: &MachineRuntime,
        delta: &RuntimeDelta,
    ) -> Result<(), MachineError> {
        let patch = &delta.owner;
        if patch.before != self.state {
            return Err(MachineError::Stale);
        }
        validate_state(patch.after, delta.next_order_after)?;
        if let (Some(a), Some(b)) = (patch.before, patch.after)
            && (a.tick != b.tick
                || b.stage < a.stage
                || a.stage == OwnerStage::Prepare
                    && b.stage == OwnerStage::Prepare
                    && b.prepared_after < a.prepared_after)
        {
            return Err(MachineError::Encoding);
        }
        let mut keys = BTreeSet::new();
        let mut count = self.ready.len();
        let pending_after: BTreeMap<_, _> = delta
            .queue
            .iter()
            .filter_map(|q| q.before.or(q.after).map(|p| (p.event, q.after)))
            .collect();
        for p in &patch.ready {
            let row = p.before.or(p.after).ok_or(MachineError::Encoding)?;
            let key = ready_key(row);
            if !keys.insert(key)
                || self.ready.get(&key).copied() != p.before
                || p.before
                    .zip(p.after)
                    .is_some_and(|(a, b)| ready_key(a) != ready_key(b) || a.event != b.event)
            {
                return Err(MachineError::Stale);
            }
            count = count + usize::from(p.after.is_some()) - usize::from(p.before.is_some());
            if let Some(after) = p.after {
                let actual = pending_after
                    .get(&after.event)
                    .copied()
                    .unwrap_or_else(|| runtime.pending(after.event));
                if actual != Some(after) || !eligible(patch.after, after) {
                    return Err(MachineError::Encoding);
                }
            }
        }
        if count > runtime.config.events || patch.after.is_none() && count != 0 {
            return Err(MachineError::Bounds);
        }
        if let Some(s) = patch.after {
            let minimum_phase = match s.stage {
                OwnerStage::Prepare | OwnerStage::Machines => 0,
                OwnerStage::Resources => 1,
                OwnerStage::Transit => 2,
            };
            if self
                .ready
                .range(..(minimum_phase, 0))
                .any(|(key, _)| !keys.contains(key))
            {
                return Err(MachineError::Encoding);
            }
        }
        // Each changed queue row that was in the ready cohort must update its
        // cohort row too. This prevents stale replay or external removal ABA.
        for q in &delta.queue {
            let row = q.before.or(q.after).ok_or(MachineError::Encoding)?;
            let key = ready_key(row);
            if self.ready.contains_key(&key) && !keys.contains(&key) {
                return Err(MachineError::Encoding);
            }
        }
        Ok(())
    }
    pub fn apply(&mut self, delta: OwnerDelta) {
        for p in delta.ready {
            let row = p.before.or(p.after).expect("validated cohort patch");
            if let Some(after) = p.after {
                self.ready.insert(ready_key(row), after);
            } else {
                self.ready.remove(&ready_key(row));
            }
        }
        self.state = delta.after;
    }
}
fn validate_state(state: Option<OwnerState>, next_order: u64) -> Result<(), MachineError> {
    if let Some(s) = state
        && let Some((due, phase, order)) = s.prepared_after
        && (s.stage != OwnerStage::Prepare || due > s.tick || phase > 1 || order >= next_order)
    {
        return Err(MachineError::Encoding);
    }
    Ok(())
}
fn eligible(state: Option<OwnerState>, p: Pending) -> bool {
    state.is_some_and(|s| {
        p.due <= s.tick
            && match s.stage {
                OwnerStage::Prepare | OwnerStage::Machines => true,
                OwnerStage::Resources => phase(p.event) == 1,
                OwnerStage::Transit => false,
            }
    })
}
impl Cohort<'_> {
    pub fn put(&mut self, p: Pending) {
        self.changes.insert(ready_key(p), Some(p));
    }
    pub fn remove(&mut self, p: Pending) {
        self.changes.insert(ready_key(p), None);
    }
    pub fn next(&mut self, phase: u8) -> Option<Pending> {
        let lower = self.scanned[usize::from(phase)]
            .map_or(std::ops::Bound::Included((phase, 0)), |o| {
                std::ops::Bound::Excluded((phase, o))
            });
        let range = (lower, std::ops::Bound::Included((phase, u64::MAX)));
        let base = self
            .base
            .ready
            .range(range)
            .find(|(key, _)| !self.changes.contains_key(key))
            .map(|(_, p)| *p);
        let changed = self.changes.range(range).find_map(|(_, p)| *p);
        let next = match (base, changed) {
            (Some(a), Some(b)) => Some(if a.order < b.order { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some(p) = next {
            self.scanned[usize::from(phase)] = Some(p.order);
        }
        next
    }
    pub fn delta(self) -> OwnerDelta {
        OwnerDelta {
            before: self.base.state,
            after: self.state,
            ready: self
                .changes
                .into_iter()
                .filter_map(|(key, after)| {
                    let before = self.base.ready.get(&key).copied();
                    (before != after).then_some(QueuePatch { before, after })
                })
                .collect(),
        }
    }
}
impl MachineRuntime {
    pub fn owner_pending(&self) -> bool {
        self.owner.state.is_some()
    }
    pub fn owner_tick(&self) -> Option<Tick> {
        self.owner.state.map(|s| s.tick)
    }
    pub fn owner_stage(&self) -> Option<OwnerStage> {
        self.owner.state.map(|s| s.stage)
    }
}
impl PhasePlan {
    /// Later global phases may be staged in the same atomic closing WAL batch.
    /// Publishing them or advancing the global clock still requires its fsync.
    pub fn owner_complete(&self) -> bool {
        self.delta.owner.after.is_none()
    }
}
