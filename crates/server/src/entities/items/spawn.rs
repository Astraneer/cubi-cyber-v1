//! Trusted spawns join an already planned item phase at one revision boundary.
use super::*;
impl ItemRuntime {
    pub fn spawn_capacity(&self, delta: &ItemDelta) -> Result<usize, EntityError> {
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(EntityError::Capacity)?
            || delta.cursor_before != self.cursor
        {
            return Err(EntityError::StalePlan);
        }
        self.store.validate_patches(&delta.patches)?;
        let added = delta.patches.iter().filter(|p| p.before.is_none()).count();
        let removed = delta.patches.iter().filter(|p| p.after.is_none()).count();
        self.maximum
            .checked_sub(
                self.store
                    .len()
                    .saturating_add(added)
                    .saturating_sub(removed),
            )
            .ok_or(EntityError::Capacity)
    }
    pub fn extend_spawn(
        &self,
        mut delta: ItemDelta,
        rows: impl IntoIterator<Item = ItemEntity>,
        active: bool,
    ) -> Result<ItemDelta, EntityError> {
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(EntityError::Capacity)?
            || delta.cursor_before != self.cursor
        {
            return Err(EntityError::StalePlan);
        }
        self.store.validate_patches(&delta.patches)?;
        let mut patches: BTreeMap<_, _> = delta
            .patches
            .into_iter()
            .map(|p| Ok((p.id()?, p)))
            .collect::<Result<_, EntityError>>()?;
        let mut count = 0;
        for item in rows {
            count += 1;
            if count > self.maximum {
                return Err(EntityError::Capacity);
            }
            item.validate()?;
            if self.store.get(item.id).is_some() || patches.contains_key(&item.id) {
                return Err(EntityError::DuplicateId);
            }
            patches.insert(
                item.id,
                ItemPatch {
                    before: None,
                    after: Some(item),
                    before_active: false,
                    after_active: active,
                },
            );
        }
        delta.patches = patches.into_values().collect();
        self.store.validate_patches(&delta.patches)?;
        Ok(delta)
    }
}
