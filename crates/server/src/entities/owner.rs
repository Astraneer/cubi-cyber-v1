//! Detached owner composition for trusted births/activity at one mob revision.
use super::*;
impl MobRuntime {
    pub fn validate_owner_delta(&self, delta: &MobDelta) -> Result<(), EntityError> {
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(EntityError::Capacity)?
            || delta.cursor_before != self.store.active_cursor()
        {
            return Err(EntityError::StalePlan);
        }
        self.store.validate_patches(&delta.patches)
    }
    pub fn extend_owned(
        &self,
        mut delta: MobDelta,
        spawns: impl IntoIterator<Item = (Mob, bool)>,
        activity: &[(EntityId, bool)],
        removals: &[EntityId],
    ) -> Result<MobDelta, EntityError> {
        self.validate_owner_delta(&delta)?;
        if activity.len() > 1024 || removals.len() > 1024 {
            return Err(EntityError::Capacity);
        }
        let mut spawns = spawns.into_iter().peekable();
        if spawns.peek().is_none() && activity.is_empty() && removals.is_empty() {
            // Ordinary AI/body phases already own their exact patch vector.
            // Avoid rebuilding large inline Mob before/after records in a tree
            // just to compose an empty owner extension.
            delta
                .patches
                .sort_unstable_by_key(|p| p.id().expect("validated patch id"));
            delta
                .patches
                .retain(|p| p.before != p.after || p.before_active != p.after_active);
            return Ok(delta);
        }
        let mut patches: BTreeMap<_, _> = delta
            .patches
            .into_iter()
            .map(|p| Ok((p.id()?, p)))
            .collect::<Result<_, EntityError>>()?;
        for (count, (mob, active)) in spawns.enumerate() {
            if count >= 1024 || self.store.get(mob.id).is_some() || patches.contains_key(&mob.id) {
                return Err(EntityError::DuplicateId);
            }
            Mob::decode(&mob.encode()?)?;
            patches.insert(
                mob.id,
                EntityPatch {
                    before: None,
                    after: Some(mob),
                    before_active: false,
                    after_active: active,
                },
            );
        }
        for &(id, active) in activity {
            if let Some(patch) = patches.get_mut(&id) {
                if patch.after.is_some() {
                    patch.after_active = active;
                }
            } else {
                let before = *self.store.get(id).ok_or(EntityError::UnknownId)?;
                patches.insert(
                    id,
                    EntityPatch {
                        before: Some(before),
                        after: Some(before),
                        before_active: self.store.is_active(id),
                        after_active: active,
                    },
                );
            }
        }
        for &id in removals {
            if let Some(patch) = patches.get_mut(&id) {
                patch.after = None;
                patch.after_active = false;
                if patch.before.is_none() {
                    patches.remove(&id);
                }
            } else {
                let before = *self.store.get(id).ok_or(EntityError::UnknownId)?;
                patches.insert(
                    id,
                    EntityPatch {
                        before: Some(before),
                        after: None,
                        before_active: self.store.is_active(id),
                        after_active: false,
                    },
                );
            }
        }
        delta.patches = patches
            .into_values()
            .filter(|p| p.before != p.after || p.before_active != p.after_active)
            .collect();
        self.validate_owner_delta(&delta)?;
        Ok(delta)
    }
}
impl MobDelta {
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty() && self.cursor_before == self.cursor_after
    }
}
