//! Compose bounded action/dependency wakes into an existing owner-tick delta.
//! The cold queue is only probed by key, never copied or traversed.
use super::*;

impl RuntimeDelta {
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.add.is_empty()
    }
}
impl SurvivalRuntime {
    pub fn validate_delta(&self, delta: &RuntimeDelta) -> Result<(), SurvivalError> {
        if delta.before != self.revision || delta.before.checked_add(1) != Some(delta.after) {
            return Err(SurvivalError::StalePlan);
        }
        let removed: BTreeSet<_> = delta.remove.iter().copied().collect();
        if removed.len() != delta.remove.len()
            || removed.iter().any(|key| !self.events.contains_key(key))
            || self
                .len()
                .saturating_sub(removed.len())
                .saturating_add(delta.add.len())
                > self.capacity
            || delta.next_order < self.next_order
        {
            return Err(SurvivalError::InvalidEncoding);
        }
        let mut events = BTreeSet::new();
        let mut orders = BTreeSet::new();
        for event in &delta.add {
            validate_event(event.event)?;
            let old = self
                .keys
                .get(&event.event)
                .and_then(|key| removed.contains(key).then(|| self.events.get(key)))
                .flatten();
            if !events.insert(event.event)
                || !orders.insert(event.order)
                || event.first_due > event.due
                || event.order >= delta.next_order
                || self
                    .keys
                    .get(&event.event)
                    .is_some_and(|key| !removed.contains(key))
                || event.order < self.next_order
                    && old.is_none_or(|old| {
                        old.order != event.order || old.first_due != event.first_due
                    })
            {
                return Err(SurvivalError::InvalidEncoding);
            }
        }
        Ok(())
    }
    /// Extends the SAME revision transition returned by plan_tick/plan_register.
    /// Existing scheduled events retain their due/order/age, matching register.
    pub fn extend_registration(
        &self,
        mut delta: RuntimeDelta,
        events: impl IntoIterator<Item = (Event, Tick)>,
    ) -> Result<RuntimeDelta, SurvivalError> {
        self.validate_delta(&delta)?;
        let removed: BTreeSet<_> = delta.remove.iter().copied().collect();
        let mut additions: BTreeMap<_, _> = delta
            .add
            .into_iter()
            .map(|event| (event.event, event))
            .collect();
        let mut new = BTreeMap::<Event, Tick>::new();
        let base = self
            .len()
            .saturating_sub(removed.len())
            .saturating_add(additions.len());
        for (event, due) in events {
            validate_event(event)?;
            if additions.contains_key(&event)
                || self
                    .keys
                    .get(&event)
                    .is_some_and(|key| !removed.contains(key))
            {
                continue;
            }
            if let Some(old) = new.get_mut(&event) {
                *old = (*old).min(due);
                continue;
            }
            if base.saturating_add(new.len()) >= self.capacity {
                return Err(SurvivalError::Capacity);
            }
            new.insert(event, due);
        }
        for (event, due) in new {
            let order = delta.next_order;
            delta.next_order = delta
                .next_order
                .checked_add(1)
                .ok_or(SurvivalError::RevisionOverflow)?;
            additions.insert(
                event,
                PendingEvent {
                    event,
                    due,
                    first_due: due,
                    order,
                },
            );
        }
        delta.add = additions.into_values().collect();
        self.validate_delta(&delta)?;
        Ok(delta)
    }
}
