//! Durable finite per-tick cohort. Preparing never executes a recipe; Machines
//! finish before Resources, and Resources finish before any item transit.
use super::*;
use std::sync::Arc;
pub const MAX_ACTIVITY_PAGES: usize = 64 * 125;

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
    /// Exact captured simulation AOI; None belongs to source-neutral fixtures.
    pub active_pages: Option<Vec<PageId>>,
    pub context_tick: Option<Tick>,
    pub observers: Vec<net::PlayerId>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct OwnerDelta {
    pub before: Option<OwnerState>,
    pub after: Option<OwnerState>,
    pub ready: Vec<QueuePatch>,
    /// Present only when a partial cohort opens. Closing clears its image.
    pub active_pages: Option<Vec<PageId>>,
    pub context_tick: Option<Tick>,
    pub observers: Option<Vec<net::PlayerId>>,
    /// Live planning hint only; not part of the WAL schema.
    pub prior_context_tick: Option<Tick>,
}
impl OwnerDelta {
    pub fn is_empty(&self) -> bool {
        self.before == self.after
            && self.ready.is_empty()
            && self.active_pages.is_none()
            && self.context_tick.is_none()
            && self.observers.is_none()
    }
}
#[derive(Default)]
pub(super) struct OwnerBarrier {
    state: Option<OwnerState>,
    ready: BTreeMap<(u8, u64), Pending>,
    pub active_pages: Option<Arc<[PageId]>>,
    pub context_tick: Option<Tick>,
    observers: Arc<[net::PlayerId]>,
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
            active_pages: self.active_pages.as_deref().map(<[PageId]>::to_vec),
            context_tick: self.context_tick,
            observers: self.observers.to_vec(),
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
        if let Some(pages) = snapshot.active_pages.as_ref() {
            if snapshot.context_tick.is_none() {
                return Err(MachineError::Encoding);
            }
            validate_pages(pages)?;
        }
        if snapshot.context_tick.is_some() != snapshot.active_pages.is_some()
            || snapshot
                .context_tick
                .zip(snapshot.state)
                .is_some_and(|(tick, state)| tick > state.tick)
            || snapshot.context_tick.is_none() && !snapshot.observers.is_empty()
        {
            return Err(MachineError::Encoding);
        }
        validate_observers(&snapshot.observers)?;
        let mut out = Self {
            state: snapshot.state,
            ready: BTreeMap::new(),
            active_pages: snapshot
                .active_pages
                .as_ref()
                .map(|p| Arc::from(p.as_slice())),
            context_tick: snapshot.context_tick,
            observers: Arc::from(snapshot.observers.as_slice()),
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
        if let Some(tick) = patch.context_tick {
            if self.context_tick.is_some_and(|old| old > tick)
                || patch.after.is_some_and(|s| s.tick != tick)
            {
                return Err(MachineError::Stale);
            }
            let pages = patch.active_pages.as_ref().ok_or(MachineError::Encoding)?;
            validate_pages(pages)?;
            validate_observers(patch.observers.as_deref().unwrap_or(&[]))?;
            if self.context_tick == Some(tick)
                && (self.active_pages.as_deref() != Some(pages.as_slice())
                    || self.observers.as_ref() != patch.observers.as_deref().unwrap_or(&[]))
            {
                return Err(MachineError::Stale);
            }
        } else if patch.active_pages.is_some() || patch.observers.is_some() {
            return Err(MachineError::Encoding);
        }
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
        if let Some(tick) = delta.context_tick {
            self.context_tick = Some(tick);
            let pages = delta.active_pages.expect("validated captured pages");
            self.active_pages = Some(Arc::from(pages));
            self.observers = Arc::from(delta.observers.unwrap_or_default());
        }
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
fn validate_pages(pages: &[PageId]) -> Result<(), MachineError> {
    if pages.len() > MAX_ACTIVITY_PAGES || pages.windows(2).any(|p| p[0] >= p[1]) {
        Err(MachineError::Encoding)
    } else {
        Ok(())
    }
}
fn validate_observers(ids: &[net::PlayerId]) -> Result<(), MachineError> {
    if ids.len() > 64 || ids.windows(2).any(|p| p[0] >= p[1]) {
        Err(MachineError::Encoding)
    } else {
        Ok(())
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
            active_pages: None,
            context_tick: None,
            observers: None,
            prior_context_tick: self.base.context_tick,
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
    pub fn owner_context_tick(&self) -> Option<Tick> {
        self.owner.context_tick
    }
    pub fn owner_active_pages(&self) -> Option<&[PageId]> {
        self.owner.active_pages.as_deref()
    }
    pub fn owner_observers(&self) -> Option<&[net::PlayerId]> {
        self.owner
            .context_tick
            .map(|_| self.owner.observers.as_ref())
    }
}
impl PhasePlan {
    /// Capture the SAME AOI used to plan the opening portion. The image is
    /// encoded once, only if that portion cannot finish the logical tick.
    pub fn capture_activity(&mut self, pages: &BTreeSet<PageId>) -> Result<bool, MachineError> {
        if self.delta.owner.before.is_some() || self.delta.owner.after.is_none() {
            return Ok(false);
        }
        if pages.len() > MAX_ACTIVITY_PAGES {
            return Err(MachineError::Bounds);
        }
        if let Some(existing) = self.delta.owner.active_pages.as_ref() {
            if existing.iter().copied().ne(pages.iter().copied()) {
                return Err(MachineError::Stale);
            }
            return Ok(false);
        }
        self.delta.owner.active_pages = Some(pages.iter().copied().collect());
        self.delta.owner.context_tick = self.delta.owner.after.map(|s| s.tick);
        Ok(true)
    }
    pub fn capture_observers(
        &mut self,
        ids: &BTreeSet<net::PlayerId>,
    ) -> Result<bool, MachineError> {
        if self.delta.owner.before.is_some() {
            return Ok(false);
        }
        if self.delta.owner.context_tick.is_none() {
            return Err(MachineError::Encoding);
        }
        if ids.len() > 64 {
            return Err(MachineError::Bounds);
        }
        let ids: Vec<_> = ids.iter().copied().collect();
        if self.delta.owner.observers.as_ref() == Some(&ids) {
            return Ok(false);
        }
        self.delta.owner.observers = Some(ids);
        Ok(true)
    }
    /// Later global phases may be staged in the same atomic closing WAL batch.
    /// Publishing them or advancing the global clock still requires its fsync.
    pub fn owner_complete(&self) -> bool {
        self.delta.owner.after.is_none()
    }
}
impl RuntimeDelta {
    /// Capture only when this logical tick yields (core or a late actor phase).
    /// Historical context is retained through closing the core; later idle
    /// ticks ignore its old tick and need no kind4 write.
    pub fn capture_context(
        &mut self,
        tick: Tick,
        pages: &BTreeSet<PageId>,
        observers: &BTreeSet<net::PlayerId>,
    ) -> Result<bool, MachineError> {
        if self.owner.prior_context_tick == Some(tick) && self.owner.context_tick.is_none() {
            return Ok(false);
        }
        if self.owner.prior_context_tick.is_some_and(|old| old > tick)
            || self.owner.after.is_some_and(|s| s.tick != tick)
        {
            return Err(MachineError::Stale);
        }
        if pages.len() > MAX_ACTIVITY_PAGES || observers.len() > 64 {
            return Err(MachineError::Bounds);
        }
        let pages: Vec<_> = pages.iter().copied().collect();
        let observers: Vec<_> = observers.iter().copied().collect();
        if self.owner.context_tick == Some(tick)
            && self.owner.active_pages.as_ref() == Some(&pages)
            && self.owner.observers.as_ref() == Some(&observers)
        {
            return Ok(false);
        }
        if self.owner.context_tick.is_some() && self.owner.context_tick != Some(tick) {
            return Err(MachineError::Stale);
        }
        self.owner.context_tick = Some(tick);
        self.owner.active_pages = Some(pages);
        self.owner.observers = Some(observers);
        Ok(true)
    }
}
