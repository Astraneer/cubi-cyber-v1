//! Bounded durable machine continuation before the late simulation phases.
//! Core Machines -> Resources -> Transit barrier/cursors belong to kind4.
use super::*;
mod wakes;

pub(super) struct MachineTick {
    pub(super) spawns: Vec<rules::entities::item::ItemEntity>,
    pub(super) complete: bool,
}
impl MachineTick {
    pub(super) fn idle() -> Self {
        Self {
            spawns: Vec::new(),
            complete: true,
        }
    }
}
impl Server {
    /// One core portion only. All world/runtime effects share one writer ticket.
    /// The current global tick stays fixed until the durable core closes.
    pub(super) fn finish_machine_phase(&mut self) -> Result<bool, ServerError> {
        if !self.runtimes.machines.owner_pending() {
            return Ok(false);
        }
        let mut batch = Batch::empty(self.tick);
        let step = self.tick_machines(&mut batch)?;
        self.finish_machine_portion(&mut batch, step)?;
        if batch.is_empty() {
            // Quota pressure cannot authorize advancing an unfinished tick.
            return Ok(self.runtimes.machines.owner_pending());
        }
        self.submit_batch(batch, Vec::new());
        Ok(true)
    }
    pub(super) fn finish_machine_portion(
        &mut self,
        batch: &mut Batch,
        step: MachineTick,
    ) -> Result<(), ServerError> {
        if step.complete {
            // Closing kind4 and all late-phase effects become durable together.
            self.late_tick_phases(batch, step.spawns)
        } else {
            // Transit may spill an item before the portion budget is exhausted.
            // Persist the birth with packet retirement, without stepping physics
            // or sampling farms twice in the held global tick.
            self.stage_machine_spawns(batch, step.spawns)?;
            self.stage_partial_survival_wakes(batch)?;
            if let Some(height) = self
                .runtimes
                .height
                .plan_batch(&batch.changes)
                .map_err(|_| ServerError::Invalid("partial machine height admission"))?
            {
                batch.runtimes.push(save::RuntimeRecord {
                    kind: save::RUNTIME_HEIGHT,
                    payload: height
                        .encode()
                        .map_err(|_| ServerError::Invalid("partial machine height codec"))?,
                });
            }
            Ok(())
        }
    }
    fn late_tick_phases(
        &mut self,
        batch: &mut Batch,
        spawns: Vec<rules::entities::item::ItemEntity>,
    ) -> Result<(), ServerError> {
        if let Err(error) = self.tick_survival(batch) {
            self.fail_error("tick_survival", &error);
            return Err(error);
        }
        if let Err(error) = self.tick_items(batch, spawns) {
            self.fail_error("tick_items", &error);
            return Err(error);
        }
        if let Err(error) = self.tick_mobs(batch) {
            self.fail_error("tick_mobs", &error);
            return Err(error);
        }
        if let Err(error) = self.tick_lighting(batch) {
            self.fail_error("tick_lighting", &error);
            return Err(error);
        }
        if let Err(error) = self.schedule_natural() {
            self.fail_error("schedule_natural", &error);
            return Err(error);
        }
        Ok(())
    }
    fn stage_machine_spawns(
        &mut self,
        batch: &mut Batch,
        spawns: Vec<rules::entities::item::ItemEntity>,
    ) -> Result<(), ServerError> {
        if spawns.is_empty() {
            return Ok(());
        }
        let existing = batch
            .runtimes
            .iter()
            .position(|r| r.kind == save::RUNTIME_ITEMS);
        let delta = if let Some(index) = existing {
            entities::items::ItemDelta::decode(&batch.runtimes[index].payload, 8192)
                .map_err(|_| ServerError::Invalid("partial machine pending item codec"))?
        } else {
            self.runtimes
                .items
                .plan_activity(&[])
                .map_err(|_| ServerError::Invalid("partial machine item boundary"))?
        };
        let delta = self
            .runtimes
            .items
            .extend_spawn(delta, spawns, true)
            .map_err(|_| ServerError::Invalid("partial machine item births"))?;
        let record = save::RuntimeRecord {
            kind: save::RUNTIME_ITEMS,
            payload: delta
                .encode()
                .map_err(|_| ServerError::Invalid("partial machine item codec"))?,
        };
        if let Some(index) = existing {
            batch.runtimes[index] = record;
        } else {
            batch.runtimes.push(record);
        }
        Ok(())
    }
}
