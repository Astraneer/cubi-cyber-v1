//! Page activity touches only indexed component IDs and already staged patches.
//! Sleeping recipes retain their exact progress; activation starts one tick
//! before `now`, so the first active tick runs without a wall-clock catch-up.
use super::*;
impl MachineRuntime {
    pub fn extend_machine_page_activity(
        &self,
        mut delta: RuntimeDelta,
        changes: &[(PageId, bool)],
        now: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        self.validate_delta(&delta)?;
        if changes.is_empty() {
            return Ok(delta);
        }
        if changes.len() > self.config.machines.saturating_add(self.config.packets) {
            return Err(MachineError::Bounds);
        }
        let mut pages = BTreeMap::new();
        let mut selected = BTreeMap::new();
        for &(page, active) in changes {
            if pages.insert(page, active).is_some() {
                return Err(MachineError::Encoding);
            }
            if let Some(ids) = self.machine_pages.get(&page) {
                selected.extend(ids.iter().map(|&id| (id, active)));
            }
        }
        for patch in &delta.machines {
            if let Some(meta) = patch.after
                && let Some(&active) = pages.get(&meta.anchor.page())
            {
                selected.insert(meta.id, active);
            }
        }
        let mut metadata: BTreeMap<_, _> = delta
            .machines
            .into_iter()
            .map(|p| Ok((p.after.or(p.before).ok_or(MachineError::Encoding)?.id, p)))
            .collect::<Result<_, MachineError>>()?;
        let mut queue: BTreeMap<_, _> = delta
            .queue
            .into_iter()
            .map(|p| Ok((p.after.or(p.before).ok_or(MachineError::Encoding)?.event, p)))
            .collect::<Result<_, MachineError>>()?;
        for (id, active) in selected {
            let before = self.machine_meta(id);
            let Some(meta) = metadata.get(&id).map(|p| p.after).unwrap_or(before) else {
                continue;
            };
            if meta.active == active {
                continue;
            }
            let after = MachineMeta {
                active,
                last_tick: if active {
                    Tick(now.0.saturating_sub(1))
                } else {
                    meta.last_tick
                },
                ..meta
            };
            metadata.insert(
                id,
                MetaPatch {
                    before,
                    after: Some(after),
                },
            );
            let event = Event::Machine(id);
            let after = active.then_some(Pending {
                event,
                due: now,
                first_due: now,
                order: meta.order,
            });
            queue.insert(
                event,
                QueuePatch {
                    before: self.pending(event),
                    after,
                },
            );
        }
        delta.machines = metadata
            .into_values()
            .filter(|p| p.before != p.after)
            .collect();
        delta.queue = queue
            .into_values()
            .filter(|p| p.before != p.after)
            .collect();
        self.validate_delta(&delta)?;
        Ok(delta)
    }
}
