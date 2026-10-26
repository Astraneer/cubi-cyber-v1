//! Admit dependencies from partial world portions without executing another
//! survival/random-tick phase in the same global tick.
use super::*;
impl Server {
    pub(crate) fn component_survival_wakes(
        &self,
        batch: &Batch,
        events: &mut BTreeMap<survival::Event, Tick>,
    ) {
        for component in batch.changes.iter().flat_map(|c| &c.components) {
            if component
                .after
                .as_ref()
                .is_some_and(|r| r.data.kind == survival::FURNACE_COMPONENT)
            {
                events.insert(
                    survival::Event::Furnace(component.id),
                    Tick(self.tick.0.saturating_add(1)),
                );
            }
        }
    }
    pub(crate) fn cell_survival_wakes(
        &mut self,
        batch: &Batch,
        events: &mut BTreeMap<survival::Event, Tick>,
    ) -> Result<(), ServerError> {
        let mut positions = BTreeSet::new();
        for cell in batch.changes.iter().flat_map(|c| &c.cells) {
            let (x, y, z) = cell.index.xyz();
            let p = BlockPos {
                space: cell.page.space,
                x: cell
                    .page
                    .x
                    .checked_mul(16)
                    .and_then(|v| v.checked_add(i32::from(x)))
                    .ok_or(ServerError::Invalid("cell x bound"))?,
                y: cell
                    .page
                    .y
                    .checked_mul(16)
                    .and_then(|v| v.checked_add(i32::from(y)))
                    .ok_or(ServerError::Invalid("cell y bound"))?,
                z: cell
                    .page
                    .z
                    .checked_mul(16)
                    .and_then(|v| v.checked_add(i32::from(z)))
                    .ok_or(ServerError::Invalid("cell z bound"))?,
            };
            for d in [
                [0, 0, 0],
                [-1, 0, 0],
                [1, 0, 0],
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
            ] {
                if let (Some(x), Some(y), Some(z)) = (
                    p.x.checked_add(d[0]),
                    p.y.checked_add(d[1]),
                    p.z.checked_add(d[2]),
                ) {
                    positions.insert(BlockPos {
                        space: p.space,
                        x,
                        y,
                        z,
                    });
                }
            }
        }
        for position in positions {
            self.counters.survival_wake_reads += 1;
            if let CellRead::Known(cell) = self.world.read(position)
                && let Some(fluid) = rules::survival::fluid::fluid(cell)
            {
                events.insert(
                    survival::Event::Fluid(position, fluid.liquid),
                    Tick(self.tick.0.saturating_add(fluid.liquid.delay())),
                );
            }
        }
        Ok(())
    }
    pub(super) fn stage_partial_survival_wakes(
        &mut self,
        batch: &mut Batch,
    ) -> Result<(), ServerError> {
        let mut events = BTreeMap::new();
        self.component_survival_wakes(batch, &mut events);
        self.cell_survival_wakes(batch, &mut events)?;
        if events.is_empty() {
            return Ok(());
        }
        let existing = batch
            .runtimes
            .iter()
            .position(|r| r.kind == save::RUNTIME_SURVIVAL);
        let delta = if let Some(index) = existing {
            let delta = survival::RuntimeDelta::decode(&batch.runtimes[index].payload, 100_000)
                .map_err(|_| ServerError::Invalid("partial survival pending codec"))?;
            self.runtimes.survival.extend_registration(delta, events)
        } else {
            self.runtimes.survival.plan_register(events)
        }
        .map_err(|_| ServerError::Invalid("partial survival wake capacity/guard"))?;
        if !delta.is_empty() {
            let record = save::RuntimeRecord {
                kind: save::RUNTIME_SURVIVAL,
                payload: delta.encode(),
            };
            if let Some(index) = existing {
                batch.runtimes[index] = record;
            } else {
                batch.runtimes.push(record);
            }
        }
        Ok(())
    }
}
