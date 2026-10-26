//! Legacy index reconstruction uses durable Arc pages and one checkpoint barrier.
use super::*;
use std::sync::Arc;
pub(super) struct LegacyMigration {
    worker: height_migration::LegacyWorker,
    pending: VecDeque<(PageId, Option<Arc<world::Page>>)>,
    complete: bool,
    pub(super) checkpoint_requested: bool,
}
impl LegacyMigration {
    pub(super) fn new(
        world: &World,
        height: &height_authority::HeightAuthority,
        generator: Arc<gen_authority::ServerGenerator>,
    ) -> Result<Option<Self>, ServerError> {
        if height.is_ready() {
            return Ok(None);
        }
        let pending = height
            .index()
            .pending_legacy_keys()
            .map(|page| (page, world.snapshot(page)))
            .collect();
        Ok(Some(Self {
            worker: height_migration::LegacyWorker::new(generator)
                .map_err(|_| ServerError::Invalid("height legacy worker"))?,
            pending,
            complete: false,
            checkpoint_requested: false,
        }))
    }
}
impl Server {
    pub(super) fn poll_height_migration(&mut self) -> Result<bool, ServerError> {
        let Some(migration) = &mut self.height_migration else {
            return Ok(false);
        };
        if migration.checkpoint_requested {
            return Ok(false);
        }
        if !migration.complete {
            for _ in 0..2 {
                let Some(result) = migration
                    .worker
                    .try_recv()
                    .map_err(|_| ServerError::Invalid("height baseline worker result"))?
                else {
                    break;
                };
                self.runtimes
                    .topology_bootstrap
                    .extend(result.topology_positions);
                self.runtimes
                    .height
                    .ingest_legacy_page(result.page, &result.differences)
                    .map_err(|_| ServerError::Invalid("height baseline capacity/state"))?;
                self.counters.height_migration_cells += u64::from(result.compared_cells);
                self.counters.height_migration_pages += 1;
            }
            for _ in 0..height_migration::CAPACITY {
                if migration.worker.inflight() >= height_migration::CAPACITY {
                    break;
                }
                let Some((page, snapshot)) = migration.pending.pop_front() else {
                    break;
                };
                let Some(snapshot) = snapshot else {
                    // No saved page is available to prove its overrides. Keep
                    // the migration unavailable; do not substitute generation.
                    migration.pending.push_back((page, None));
                    continue;
                };
                match migration
                    .worker
                    .try_submit(page, snapshot.clone(), self.tick)
                {
                    Ok(_) => {}
                    Err(height_migration::Error::Capacity) => {
                        migration.pending.push_front((page, Some(snapshot)));
                        break;
                    }
                    Err(_) => {
                        return Err(ServerError::Invalid("height baseline worker submission"));
                    }
                }
            }
            if migration.pending.is_empty() && migration.worker.inflight() == 0 {
                self.runtimes
                    .height
                    .finish_legacy()
                    .map_err(|_| ServerError::Invalid("height baseline completion"))?;
                migration.complete = true;
            }
        }
        if migration.complete {
            // The index becomes publishable only after its full rebuilt snapshot
            // is durable. LIGHT and tile workers remain guarded until then.
            if self.request_checkpoint() {
                self.height_migration
                    .as_mut()
                    .expect("active migration")
                    .checkpoint_requested = true;
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn height_ready(&self) -> bool {
        self.height_migration.is_none() && self.runtimes.height.is_ready()
    }
}
