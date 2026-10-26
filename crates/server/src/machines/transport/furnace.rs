use super::*;
use crate::machines::view::Stage;
pub(in crate::machines) fn put_furnace(
    furnace: &mut Furnace,
    face: Face,
    stack: ItemStack,
) -> Result<u16, MachineError> {
    let slot = if face == Face::Up {
        0
    } else if rules::survival::furnace::fuel_ticks(stack.item()) > 0
        || stack.item() == rules::items::I_BUCKET
    {
        1
    } else {
        return Ok(0);
    };
    let old = furnace.slots()[slot];
    if !old.is_empty() && !old.same_kind(stack) {
        return Ok(0);
    }
    let count = stack
        .count()
        .min(stack.maximum().saturating_sub(old.count()));
    if count > 0 {
        furnace
            .set_trusted(
                slot,
                stack
                    .with_count(old.count() + count)
                    .map_err(|_| MachineError::Encoding)?,
            )
            .map_err(|_| MachineError::Encoding)?;
    }
    Ok(count)
}
impl Stage<'_> {
    pub(in crate::machines) fn ensure_furnace(
        &mut self,
        id: ComponentId,
    ) -> Result<(), MachineError> {
        if self.local.furnaces.contains_key(&id) {
            return Ok(());
        }
        let bytes = 2 * rules::survival::furnace::FURNACE_MAX_BYTES as u64;
        if self.work.bytes.saturating_add(bytes) > self.budget.bytes {
            return Err(MachineError::Bounds);
        }
        self.work.bytes += bytes;
        let change = if let Some(change) = self.parent.furnaces.get(&id) {
            *change
        } else {
            let record = self.world.component(id).ok_or(MachineError::Missing)?;
            if record.data.kind != crate::survival::FURNACE_COMPONENT {
                return Err(MachineError::Component);
            }
            let anchor = record.data.anchor;
            let version = record.version;
            let before =
                Furnace::decode(&record.data.payload).map_err(|_| MachineError::Encoding)?;
            if self.cell(anchor)?.id() != FURNACE {
                return Err(MachineError::Component);
            }
            FurnaceChange {
                version,
                before,
                after: before,
                anchor,
            }
        };
        self.local.furnaces.insert(id, change);
        Ok(())
    }
}
