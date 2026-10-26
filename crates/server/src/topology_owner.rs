//! Durable connection queue reconciliation precedes the ordinary resource phase.
use super::*;
impl Server {
    pub(super) fn reconcile_topology(&mut self) -> Result<bool, ServerError> {
        let positions: Vec<_> = self
            .runtimes
            .machines
            .pending_topology()
            .due(self.tick, 64)
            .map(|e| e.position)
            .collect();
        if positions.is_empty() {
            return Ok(false);
        }
        // Canonical generated display blocks receive typed empty components
        // before port discovery; cells stay unchanged and nothing is broadcast.
        let mut births = BTreeMap::new();
        for p in &positions {
            for d in [
                [0, 0, 0],
                [-1, 0, 0],
                [1, 0, 0],
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
            ] {
                let (Some(x), Some(y), Some(z)) = (
                    p.x.checked_add(d[0]),
                    p.y.checked_add(d[1]),
                    p.z.checked_add(d[2]),
                ) else {
                    continue;
                };
                let at = BlockPos {
                    space: p.space,
                    x,
                    y,
                    z,
                };
                if self
                    .world
                    .page(at.page())
                    .and_then(|page| page.component(at.local_index()))
                    .is_some()
                {
                    continue;
                }
                if let CellRead::Known(cell) = self.world.read(at)
                    && rules::blocks::bdef(cell.id()).be.is_some()
                    && let Some((id, data)) =
                        block_loot::initial_component(&self.world, at, cell)
                            .map_err(|_| ServerError::Invalid("generated component birth"))?
                {
                    births.insert(at, (id, data));
                }
            }
        }
        let mut batch = Batch::empty(self.tick);
        let mut transactions: BTreeMap<SpaceId, Transaction> = BTreeMap::new();
        for (_, (id, data)) in births {
            transactions
                .entry(data.anchor.space)
                .or_insert_with(|| Transaction::new(self.tick, data.anchor.space))
                .set_component(
                    id,
                    data,
                    core_types::ChannelMask::CONTENTS | core_types::ChannelMask::CONNECTIONS,
                );
        }
        for tx in transactions.into_values() {
            batch.changes.push(
                self.world
                    .commit(tx)
                    .map_err(|_| ServerError::Invalid("generated component transaction"))?,
            );
        }
        let mut delta = match self.runtimes.machines.plan_topology(
            &self.world,
            &positions,
            self.tick,
            machines::Budget::default(),
        ) {
            Ok(mut plan) => {
                self.runtimes
                    .machines
                    .validate(&self.world, &plan)
                    .map_err(|_| ServerError::Invalid("topology version guard"))?;
                plan.delta = self
                    .runtimes
                    .machines
                    .extend_topology_completion(plan.delta, positions.iter().copied())
                    .map_err(|_| ServerError::Invalid("topology completion guard"))?;
                for tx in plan.transactions {
                    batch.changes.push(
                        self.world
                            .commit(tx)
                            .map_err(|_| ServerError::Invalid("topology arm transaction"))?,
                    );
                }
                self.counters.topology_completed += positions.len() as u64;
                plan.delta
            }
            Err(machines::MachineError::Unknown | machines::MachineError::Bounds) => {
                for p in &positions {
                    for d in rules::cell::DIRS {
                        if let (Some(x), Some(y), Some(z)) = (
                            p.x.checked_add(d[0]),
                            p.y.checked_add(d[1]),
                            p.z.checked_add(d[2]),
                        ) {
                            self.request_generation(
                                BlockPos {
                                    space: p.space,
                                    x,
                                    y,
                                    z,
                                }
                                .page(),
                            );
                        }
                    }
                }
                let delta = self
                    .runtimes
                    .machines
                    .noop_delta()
                    .map_err(|_| ServerError::Invalid("topology retry boundary"))?;
                self.counters.topology_retried += positions.len() as u64;
                self.runtimes
                    .machines
                    .extend_topology_retry(
                        delta,
                        positions.iter().copied(),
                        Tick(self.tick.0.saturating_add(20)),
                    )
                    .map_err(|_| ServerError::Invalid("topology retry guard"))?
            }
            Err(_) => return Err(ServerError::Invalid("topology reconciliation")),
        };
        let changed = batch
            .changes
            .iter()
            .flat_map(|c| &c.components)
            .filter(|c| {
                c.after
                    .as_ref()
                    .is_some_and(|r| r.data.kind == machines::MACHINE_COMPONENT)
            })
            .map(|c| c.id);
        delta = self
            .runtimes
            .machines
            .extend_registration(delta, &self.world, changed, self.tick)
            .map_err(|_| ServerError::Invalid("topology component registration"))?;
        batch.runtimes.push(save::RuntimeRecord {
            kind: save::RUNTIME_MACHINES,
            payload: delta
                .encode()
                .map_err(|_| ServerError::Invalid("topology WAL capacity"))?,
        });
        if let Some(plan) = self
            .runtimes
            .height
            .plan_batch(&batch.changes)
            .map_err(|_| ServerError::Invalid("topology height admission"))?
        {
            batch.runtimes.push(save::RuntimeRecord {
                kind: save::RUNTIME_HEIGHT,
                payload: plan
                    .encode()
                    .map_err(|_| ServerError::Invalid("topology height codec"))?,
            });
        }
        self.submit_batch(batch, Vec::new());
        Ok(true)
    }
}
